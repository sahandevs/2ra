use anyhow::{bail, Error};
use std::mem::size_of;
use std::sync::Arc;
use std::vec::Vec;
use std::{fs, io, sync};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{channel, Sender};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::server::TlsStream;

use crate::message::{ClientMessage, IntoMessage, ServerMessage};

pub async fn start_server() -> Result<(), Error> {
    // Build TLS configuration.
    let tls_cfg = {
        // Load public certificate.
        let certs = load_certs("cert/cert.pem")?;
        // Load private key.
        let key = load_private_key("cert/key.pem")?;
        // Do not use client certificate authentication.
        let mut cfg = rustls::ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| Error::msg(format!("{}", e)))?;
        // Configure ALPN to accept HTTP/2, HTTP/1.1 in that order.
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        sync::Arc::new(cfg)
    };

    start_tcp_listener(tls_cfg).await?;

    Ok(())
}

pub async fn start_tcp_listener(cfg: Arc<ServerConfig>) -> Result<(), Error> {
    let addr = "127.0.0.1:4433";

    let server_listener = tokio::net::TcpListener::bind(addr).await?;
    let server = Arc::new(Server::default());
    while let Ok((conn, _)) = server_listener.accept().await {
        let cfg = cfg.clone();
        let server = server.clone();
        tokio::spawn(async move {
            println!("[server] handshaking...");
            let acceptor = tokio_rustls::TlsAcceptor::from(cfg);

            match acceptor.accept(conn).await {
                Ok(tls_stream) => {
                    println!("[server] handshake done");
                    match handle(server, tls_stream).await {
                        Ok(()) => {}
                        Err(err) => println!("[server] {err}"),
                    }
                }
                Err(e) => println!("[server] {e}"),
            }
        });
    }

    Ok(())
}

#[derive(Default)]
pub struct Server {
    send_to_client: chashmap::CHashMap<String, Arc<Sender<ServerMessage>>>,
}

macro_rules! err {
    ($expr:expr) => {{
        match $expr {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[server] [warn] {e}");
                return;
            }
        }
    }};
}

async fn handle(server: Arc<Server>, mut tls_stream: TlsStream<TcpStream>) -> Result<(), Error> {
    wait_for_body(&mut tls_stream).await?;
    println!("[server] done waiting for body");
    let init_message = read_client_message(&mut tls_stream).await?;

    println!("[server] got an init message {:?}", init_message);

    match init_message {
        ClientMessage::InitRx { new_session: _ } => {
            let uuid = uuid::Uuid::new_v4().to_string();

            let (tx, mut rx) = channel(10000);
            server.send_to_client.insert(uuid.clone(), Arc::new(tx));

            let content = "HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n\r\n".to_string();
            tls_stream.write_all(content.as_bytes()).await?;
            let m = ServerMessage::SessionCreated { uuid };
            tls_stream.write_all(m.as_message()?.as_slice()).await?;
            tls_stream.flush().await?;

            while let Some(m) = rx.recv().await {
                tls_stream.write_all(m.as_message()?.as_slice()).await?;
                tls_stream.flush().await?;
            }
        }
        ClientMessage::InitTx { uuid } => {
            let connection_senders: chashmap::CHashMap<usize, Arc<Sender<Vec<u8>>>> =
                Default::default();

            loop {
                let Some(send_to_client_chan) = server.send_to_client.get(&uuid).map(|x| x.clone()) else { bail!("unknown uuid {uuid}")};
                println!("[server] entering {uuid} tx loop");
                let msg = read_client_message(&mut tls_stream).await?;
                
                match msg {
                    ClientMessage::NewConnectionWithIp { addr, id } => {
                        println!("[server] got a message in tx channel {:?}", msg);
                        let send_to_client_chan = send_to_client_chan.clone();
                        let (tx, mut rx) = channel(10000);
                        connection_senders.insert(id, Arc::new(tx));
                        tokio::task::spawn(async move {
                            let conn = err!(tokio::net::TcpStream::connect(addr).await);
                            let (mut read, mut write) = conn.into_split();
                            tokio::task::spawn(async move {
                                while let Some(data) = rx.recv().await {
                                    err!(write.write_all(data.as_slice()).await);
                                    err!(write.flush().await);
                                }
                            });
                            let mut buff = [0u8; 1024];
                            loop {
                                match read.read(buff.as_mut_slice()).await {
                                    Ok(n) => {
                                        if n == 0 {
                                            println!("[server] n == 0");
                                            break;
                                        }
                                        let data = buff[0..n].to_vec(); // TODO: double check
                                        let msg = ServerMessage::Data { id, data };
                                        send_to_client_chan.send(msg).await.unwrap();
                                    }
                                    Err(e) => {
                                        println!("[server] err {:?}", e);
                                        break;
                                    }
                                };
                            }
                        });
                    }
                    ClientMessage::Data { id, data } => {
                        println!("[server] got a message in tx channel Data");
                        let Some(tx) = connection_senders.get(&id).map(|x| x.clone()) else { bail!("no sender!") };
                        tx.send(data).await?;
                    }
                    x => bail!("unexpected message {:?}", x),
                }
            }
        }
        x => bail!("invalid first message: {:?}", x),
    };
    Ok(())
}

async fn read_client_message(stream: &mut TlsStream<TcpStream>) -> Result<ClientMessage, Error> {
    assert!(bincode::serialized_size(&0u64)? == size_of::<u64>() as u64);

    println!("[server] waiting for client message...");
    let mut len_hdr = [0u8; 64 / 8];
    stream.read_exact(&mut len_hdr).await?;
    let len: usize = bincode::deserialize(len_hdr.as_slice())?;
    println!("[server] got a client message with len {}", len);
    let mut buffer = vec![0u8; len];

    let n = stream.read_exact(&mut buffer).await?;
    if n != len {
        bail!("n != len");
    }
    Ok(bincode::deserialize(&buffer)?)
}

async fn wait_for_body(stream: &mut TlsStream<TcpStream>) -> Result<(), Error> {
    // TODO: this won't work!
    let mut sep = [0u8; 4];
    loop {
        let n = stream.read_exact(&mut sep).await?;
        // let joined: Vec<_> = sep.iter().cloned().map(char::from).collect();
        // println!("{:?}", joined);
        match (n, sep) {
            (4, [b'\r', b'\n', b'\r', b'\n']) => break,
            (4, _) => {}
            _ => bail!("!"),
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
        println!("[server] {:?}", keys);
        return Err(error("expected a single private key".into()));
    }

    Ok(rustls::PrivateKey(keys[0].clone()))
}
