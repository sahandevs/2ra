use color_eyre::{eyre::eyre, Result};
use std::mem::size_of;
use std::sync::atomic::Ordering::Release;
use std::sync::Arc;
use std::vec::Vec;
use std::{fs, io, sync};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{channel, Sender};
use tokio_rustls::server::TlsStream;

use crate::config::ServerConfig;
use crate::instance::Instance;
use crate::message::{ClientMessage, IntoMessage, ServerMessage};

pub async fn start_server(instance: &Arc<Instance>, config: ServerConfig) -> Result<()> {
    // Build TLS configuration.
    let tls_cfg = {
        // Load public certificate.
        let certs = load_certs(&config.cert_pem_path)?;
        // Load private key.
        let key = load_private_key(&config.key_pem_path)?;
        // Do not use client certificate authentication.
        let mut cfg = rustls::ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| eyre!("{}", e))?;
        // Configure ALPN to accept HTTP/2, HTTP/1.1 in that order.
        cfg.alpn_protocols = vec![b"http/1.0".to_vec()];
        sync::Arc::new(cfg)
    };

    start_tcp_listener(instance, config, tls_cfg).await?;

    Ok(())
}

pub async fn start_tcp_listener(
    instance: &Arc<Instance>,
    config: ServerConfig,
    cfg: Arc<rustls::ServerConfig>,
) -> Result<()> {
    let server_listener = tokio::net::TcpListener::bind(&config.inbound_addr).await?;
    let server = Arc::new(Server {
        send_to_client: Default::default(),
        config,
        instance: instance.clone(),
    });

    while let Ok((conn, _)) = tokio::select! {
        _ = instance.shutdown_signal.notified() => {
            log::info!("shutting down server tcp listener...");
            return Ok(());
        },
        x = server_listener.accept() => x,
    } {
        let cfg = cfg.clone();
        let server = server.clone();
        tokio::spawn(async move {
            log::info!("handshaking...");
            let acceptor = tokio_rustls::TlsAcceptor::from(cfg);

            match acceptor.accept(conn).await {
                Ok(tls_stream) => {
                    log::info!("handshake done");
                    match handle(server, tls_stream).await {
                        Ok(()) => {}
                        Err(err) => log::error!("{err}"),
                    }
                }
                Err(e) => log::error!("{e}"),
            }
        });
    }

    Ok(())
}

pub struct Server {
    send_to_client: chashmap::CHashMap<String, Arc<Sender<ServerMessage>>>,
    config: ServerConfig,
    instance: Arc<Instance>,
}

macro_rules! err {
    ($expr:expr) => {{
        match $expr {
            Ok(x) => x,
            Err(e) => {
                log::error!("[warn] {e}");
                return Err(eyre!("[warn] {e}"));
            }
        }
    }};
}

async fn handle(server: Arc<Server>, mut tls_stream: TlsStream<TcpStream>) -> Result<()> {
    wait_for_body(&server.config.separator, &mut tls_stream).await?;
    log::debug!("done waiting for body");
    let init_message = read_client_message(&mut tls_stream).await?;

    log::debug!("got an init message {:?}", init_message);

    match init_message {
        ClientMessage::InitRx { new_session: _ } => {
            let uuid = uuid::Uuid::new_v4().to_string();

            let (tx, mut rx) = channel(10000);
            server.send_to_client.insert(uuid.clone(), Arc::new(tx));

            tls_stream
                .write_all(server.config.http_response.as_bytes())
                .await?;
            let m = ServerMessage::SessionCreated { uuid };
            tls_stream.write_all(m.as_message()?.as_slice()).await?;
            tls_stream.flush().await?;

            while let Some(m) = tokio::select! {
                _ = server.instance.shutdown_signal.notified() => {
                    log::info!("shutting down rx..");
                    return Ok(());
                },
                x = rx.recv() => x,
            } {
                tls_stream.write_all(m.as_message()?.as_slice()).await?;
                tls_stream.flush().await?;
            }
        }
        ClientMessage::InitTx { uuid } => {
            let connection_senders: chashmap::CHashMap<usize, Arc<Sender<Vec<u8>>>> =
                Default::default();

            // TODO: handle shutdown signal
            loop {
                let Some(send_to_client_chan) = server.send_to_client.get(&uuid).map(|x| x.clone()) else { return Err(eyre!("unknown uuid {uuid}"));};
                log::debug!("entering {uuid} tx loop");
                let msg = read_client_message(&mut tls_stream).await?;

                match msg {
                    ClientMessage::NewConnectionWithDomain { domain, port, id } => {
                        log::debug!("got a message in tx channel NewConnectionWithDomain");
                        server
                            .instance
                            .stats
                            .server_handled_tcp_conn
                            .fetch_add(1, Release);
                        handle_new_connection_with_addr(
                            ClientMessage::NewConnectionWithDomain { domain, port, id },
                            send_to_client_chan,
                            &connection_senders,
                            id,
                        )
                        .await?;
                    }
                    ClientMessage::NewConnectionWithIp { addr, id } => {
                        log::debug!("got a message in tx channel {:?}", msg);
                        server
                            .instance
                            .stats
                            .server_handled_tcp_conn
                            .fetch_add(1, Release);
                        handle_new_connection_with_addr(
                            ClientMessage::NewConnectionWithIp { addr, id },
                            send_to_client_chan,
                            &connection_senders,
                            id,
                        )
                        .await?;
                    }
                    ClientMessage::Data { id, data } => {
                        log::debug!("got a message in tx channel Data");
                        let Some(tx) = connection_senders.get(&id).map(|x| x.clone()) else { return Err(eyre!("no sender!")); };
                        tx.send(data).await?;
                    }
                    x => return Err(eyre!("unexpected message {:?}", x)),
                }
            }
        }
        x => return Err(eyre!("invalid first message: {:?}", x)),
    };
    Ok(())
}

async fn handle_new_connection_with_addr(
    msg: ClientMessage,
    send_to_client_chan: Arc<Sender<ServerMessage>>,
    connection_senders: &chashmap::CHashMap<usize, Arc<Sender<Vec<u8>>>>,
    id: usize,
) -> Result<()> {
    // TODO: handle shutdown signal
    let send_to_client_chan = send_to_client_chan.clone();
    let (tx, mut rx) = channel(10000);
    connection_senders.insert(id, Arc::new(tx));
    tokio::task::spawn(async move {
        let addr = match msg {
            ClientMessage::NewConnectionWithIp { addr, id: _ } => addr,
            ClientMessage::NewConnectionWithDomain {
                domain,
                port,
                id: _,
            } => {
                log::debug!("resolving {}:{}...", domain, port);
                let addrs = tokio::net::lookup_host(format!("{}:{}", domain, port))
                    .await?
                    .into_iter()
                    .next();
                let Some(addr) = addrs else { return Err(eyre!("failed to lookup {}:{}", domain, port) );};
                log::debug!("resolving {}:{} done", domain, port);

                addr
            }
            _ => {
                unreachable!("unreachable");
            }
        };
        let conn = err!(tokio::net::TcpStream::connect(addr).await);
        let (mut read, mut write) = conn.into_split();
        tokio::task::spawn(async move {
            while let Some(data) = rx.recv().await {
                err!(write.write_all(data.as_slice()).await);
                err!(write.flush().await);
            }
            Ok(())
        });
        let mut buff = [0u8; 1024];
        loop {
            match read.read(buff.as_mut_slice()).await {
                Ok(n) => {
                    if n == 0 {
                        log::debug!("n == 0");
                        break;
                    }
                    let data = buff[0..n].to_vec(); // TODO: double check
                    let msg = ServerMessage::Data { id, data };
                    send_to_client_chan.send(msg).await?;
                }
                Err(e) => {
                    log::error!("err {:?}", e);
                    break;
                }
            };
        }

        Ok(())
    });
    Ok(())
}

async fn read_client_message(stream: &mut TlsStream<TcpStream>) -> Result<ClientMessage> {
    assert!(bincode::serialized_size(&0u64)? == size_of::<u64>() as u64);

    log::debug!("waiting for client message...");
    let mut len_hdr = [0u8; 64 / 8];
    stream.read_exact(&mut len_hdr).await?;
    // log::trace!("{:?}", len_hdr.iter().map(|x| *x as char).collect::<Vec<char>>());
    let len: usize = bincode::deserialize(len_hdr.as_slice())?;

    if len > 100_000 {
        return Err(eyre!("invalid message len size. {len} is too big"));
    }

    log::debug!("got a client message with len {}", len);
    let mut buffer = vec![0u8; len];

    let n = stream.read_exact(&mut buffer).await?;
    if n != len {
        return Err(eyre!("n != len"));
    }
    Ok(bincode::deserialize(&buffer)?)
}

async fn wait_for_body(target: &[char], stream: &mut TlsStream<TcpStream>) -> Result<()> {
    log::debug!("waiting for stub request message");

    let mut matched_idx = 0;

    loop {
        let x = stream.read_u8().await?;
        // log::trace!("{:?}", [x as char]);
        if x as char == target[matched_idx] {
            matched_idx += 1;
        } else {
            matched_idx = 0;
        }

        if matched_idx >= target.len() {
            break;
        }
    }
    Ok(())
}

fn error(err: String) -> io::Error {
    io::Error::new(io::ErrorKind::Other, err)
}

// Load public certificate from file.
fn load_certs(filename: &str) -> io::Result<Vec<rustls::Certificate>> {
    // Open certificate file.
    let certfile = fs::File::open(filename)
        .map_err(|e| error(format!("failed to open {}: {}", filename, e)))?;
    let mut reader = io::BufReader::new(certfile);

    // Load and return certificate.
    let certs = rustls_pemfile::certs(&mut reader)
        .map_err(|_| error("failed to load certificate".into()))?;
    Ok(certs.into_iter().map(rustls::Certificate).collect())
}

// Load private key from file.
fn load_private_key(filename: &str) -> io::Result<rustls::PrivateKey> {
    // Open keyfile.
    let keyfile = fs::File::open(filename)
        .map_err(|e| error(format!("failed to open {}: {}", filename, e)))?;
    let mut reader = io::BufReader::new(keyfile);
    // Load and return a single private key.
    let keys = rustls_pemfile::pkcs8_private_keys(&mut reader)
        .map_err(|_| error("failed to load private key".into()))?;
    if keys.len() != 1 {
        log::error!("{:?}", keys);
        return Err(error("expected a single private key".into()));
    }

    Ok(rustls::PrivateKey(keys[0].clone()))
}
