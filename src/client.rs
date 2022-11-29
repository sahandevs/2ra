use std::{
    mem::size_of,
    sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc,
    },
};

use color_eyre::{eyre::eyre, Result};
use rustls::{client::ServerCertVerifier, OwnedTrustAnchor};
use socks5_proto::{Address, Reply};
use socks5_server::{auth::NoAuth, Connection, IncomingConnection, Server};
use tokio::{
    self,
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};
use tokio_rustls::{client::TlsStream, TlsConnector};

use crate::{
    config::ClientConfig,
    message::{ClientMessage, IntoMessage, ServerMessage},
};

macro_rules! err {
    ($expr:expr) => {{
        match $expr {
            Ok(x) => x,
            Err(e) => {
                log::error!("[warn] {e}");
                return;
            }
        }
    }};
}

pub async fn start_client(config: ClientConfig) -> Result<()> {
    let config = Arc::new(config);

    log::info!("creating socks5 server ...");
    let server = Server::bind(config.inbound_addr.as_str(), Arc::new(NoAuth)).await?;
    log::info!("creating socks5 server done");
    log::info!("creating 2ra client ...");
    let client = Arc::new(Client::new(config.clone()).await?);

    log::info!("building client<->server connection done!");
    while let Ok((conn, _)) = server.accept().await {
        let client = client.clone();
        tokio::spawn(async move {
            match handle(client, conn).await {
                Ok(()) => {}
                Err(err) => log::error!("->{err}"),
            }
        });
    }

    Ok(())
}

struct Client {
    tx: Arc<tokio::sync::mpsc::Sender<Vec<u8>>>,
    config: Arc<ClientConfig>,
    next_conn_id: AtomicUsize,

    receiver_channels: Arc<chashmap::CHashMap<usize, tokio::sync::mpsc::Sender<Vec<u8>>>>,
}

impl Client {
    #[track_caller]
    async fn create_connection(
        server: &str,
        sni: &str,
        content: &str,
        insecure: bool,
    ) -> Result<TlsStream<TcpStream>> {
        let mut root_cert_store = rustls::RootCertStore::empty();
        root_cert_store.add_server_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.0.iter().map(
            |ta| {
                OwnedTrustAnchor::from_subject_spki_name_constraints(
                    ta.subject,
                    ta.spki,
                    ta.name_constraints,
                )
            },
        ));

        let mut config = rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth(); // i guess this was previously the default?
        if insecure {
            struct StubVerifier;

            impl ServerCertVerifier for StubVerifier {
                fn verify_server_cert(
                    &self,
                    _end_entity: &rustls::Certificate,
                    _intermediates: &[rustls::Certificate],
                    _server_name: &rustls::ServerName,
                    _scts: &mut dyn Iterator<Item = &[u8]>,
                    _ocsp_response: &[u8],
                    _now: std::time::SystemTime,
                ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
                    Ok(rustls::client::ServerCertVerified::assertion())
                }
            }
            config
                .dangerous()
                .set_certificate_verifier(Arc::new(StubVerifier));
        }

        let connector = TlsConnector::from(Arc::new(config));
        let stream = TcpStream::connect(&server).await?;
        let domain = rustls::ServerName::try_from(sni)?;

        let mut stream = connector.connect(domain, stream).await?;
        stream.write_all(content.as_bytes()).await?;
        stream.flush().await?;
        Ok(stream)
    }

    pub async fn new(config: Arc<ClientConfig>) -> Result<Client> {
        let receiver_channels: Arc<chashmap::CHashMap<usize, tokio::sync::mpsc::Sender<Vec<u8>>>> =
            Default::default();
        let receiver_channels_ref = receiver_channels.clone();

        log::info!("creating rx_stream");
        let (uuid, rx_stream) = {
            // connect to server and get a new uuid
            let mut stream = Self::create_connection(
                &config.outbound_addr,
                &config.rx_sni,
                &config.http_request,
                config.insecure_tls,
            )
            .await?;
            log::trace!("connection made");

            let init_msg = ClientMessage::InitRx { new_session: true };
            stream.write_all(init_msg.as_message()?.as_slice()).await?;
            stream.flush().await?;
            wait_for_body(&config.separator, &mut stream).await?;
            log::debug!("waiting for stub response done");
            match read_server_message(&mut stream).await? {
                ServerMessage::SessionCreated { uuid } => (uuid, stream),
                x => return Err(eyre!("unexpected message received: {:?}", x)),
            }
        };
        log::info!("rx_stream created");

        tokio::spawn(async move {
            let mut rx_stream = rx_stream;
            loop {
                let msg = match read_server_message(&mut rx_stream).await {
                    Ok(x) => x,
                    Err(e) => {
                        log::error!("hit error while reading server message {:?}", e);
                        continue;
                    }
                };

                match msg {
                    ServerMessage::Data { id, data } => {
                        if let Some(chan) = receiver_channels.get(&id) {
                            if let Err(e) = chan.send(data).await {
                                log::error!("x -> {}", e);
                            }
                        } else {
                            log::error!("no channel listening for {}", id);
                        }
                    }
                    x => log::error!("unexpected message received: {:?}", x),
                }
            }
        });

        log::info!("creating tx_stream");
        let mut tx_stream = {
            // connect to server and init tx
            let mut stream = Self::create_connection(
                &config.outbound_addr,
                &config.tx_sni,
                &config.http_request,
                config.insecure_tls,
            )
            .await?;

            let init_msg = ClientMessage::InitTx { uuid };
            stream.write_all(init_msg.as_message()?.as_slice()).await?;
            stream.flush().await?;
            stream
        };

        let (tx, mut chan_rx) = mpsc::channel::<Vec<u8>>(10000);

        tokio::spawn(async move {
            while let Some(x) = chan_rx.recv().await {
                if let Err(e) = tx_stream.write_all(&x).await {
                    log::error!("{:?}", e)
                }
                let _ = tx_stream.flush().await;
            }
        });
        log::info!("tx_stream created");

        Ok(Client {
            tx: Arc::new(tx),
            next_conn_id: Default::default(),
            receiver_channels: receiver_channels_ref,
            config,
        })
    }

    pub async fn create_new_connection(
        &self,
        addr: Address,
    ) -> Result<(tokio::sync::mpsc::Receiver<Vec<u8>>, usize)> {
        let id = self
            .next_conn_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let (tx, rx) = mpsc::channel::<Vec<u8>>(10000);

        if self.receiver_channels.insert(id, tx).is_some() {
            log::warn!("id collided!");
        }

        let msg = match addr {
            Address::SocketAddress(addr) => ClientMessage::NewConnectionWithIp { addr, id },
            Address::DomainAddress(domain, port) => {
                ClientMessage::NewConnectionWithDomain { domain, port, id }
            }
        };

        self.tx.send(msg.as_message()?).await?;
        Ok((rx, id))
    }
}

async fn wait_for_body(target: &[char], stream: &mut TlsStream<TcpStream>) -> Result<()> {
    log::debug!("waiting for stub response message");
    let mut matched_idx = 0;

    loop {
        let x = stream.read_u8().await?;
        log::trace!("{:?}", [x as char]);
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

async fn read_server_message(stream: &mut TlsStream<TcpStream>) -> Result<ServerMessage> {
    assert!(bincode::serialized_size(&0u64)? == size_of::<u64>() as u64);
    log::debug!("waiting for server message...");
    let mut len_hdr = [0u8; 64 / 8];
    stream.read_exact(&mut len_hdr).await?;
    let len: usize = bincode::deserialize(len_hdr.as_slice())?;
    log::debug!("got a server message with len {}", len);
    let mut buffer = vec![0u8; len];

    let n = stream.read_exact(&mut buffer).await?;
    if n != len {
        return Err(eyre!("n != len"));
    }
    Ok(bincode::deserialize(&buffer)?)
}

async fn handle(client: Arc<Client>, conn: IncomingConnection) -> Result<()> {
    log::debug!("new socks5 conn");
    match conn.handshake().await? {
        Connection::Associate(associate, _) => {
            let mut conn = associate
                .reply(Reply::CommandNotSupported, Address::unspecified())
                .await?;
            conn.shutdown().await?;
        }
        Connection::Bind(bind, _) => {
            let mut conn = bind
                .reply(Reply::CommandNotSupported, Address::unspecified())
                .await?;
            conn.shutdown().await?;
        }
        Connection::Connect(connect, addr) => {
            let (mut proxy_server_stream, id) = client.create_new_connection(addr).await?;
            let conn = connect
                .reply(Reply::Succeeded, Address::unspecified())
                .await?;
            let (mut socks_read, mut socks_write) = conn.stream.into_split();
            let broken_pipe = Arc::new(AtomicBool::new(false));

            let bp = broken_pipe.clone();
            let read_task = tokio::task::spawn(async move {
                let mut buff = vec![0u8; client.config.buffer_size];
                loop {
                    if bp.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    match socks_read.read(buff.as_mut_slice()).await {
                        Ok(n) => {
                            if n == 0 {
                                log::debug!("n == 0");
                                break;
                            }
                            let data = buff[0..n].to_vec(); // TODO: double check
                            let msg = ClientMessage::Data { id, data };
                            if bp.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            if client.tx.send(err!(msg.as_message())).await.is_err() {
                                bp.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        Err(e) => {
                            log::error!("err {:?}", e);
                            break;
                        }
                    };
                }
            });

            let bp = broken_pipe.clone();
            let write_task = tokio::task::spawn(async move {
                while let Some(data) = proxy_server_stream.recv().await {
                    if socks_write.write_all(data.as_slice()).await.is_err() {
                        bp.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    let _ = socks_write.flush().await;
                }
            });

            let (a, b) = tokio::join!(read_task, write_task);
            a?;
            b?;
            log::debug!("socks5 job done");
        }
    }

    Ok(())
}
