use std::{
    mem::size_of,
    sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc,
    },
    time::Duration,
};

use color_eyre::{eyre::eyre, Result};
use rustls::{client::ServerCertVerifier, OwnedTrustAnchor};
use socks5_proto::{Address, Reply};
use socks5_server::{auth::NoAuth, Connection, IncomingConnection, Server};
use tokio::{
    self,
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{
        mpsc::{self, channel},
        Notify, RwLock,
    },
};
use tokio_rustls::{client::TlsStream, TlsConnector};

use crate::{
    config::ClientConfig,
    gate::Gate,
    instance::Instance,
    message::{ClientMessage, IntoMessage, ServerMessage},
};

pub async fn start_client(instance: &Arc<Instance>, config: ClientConfig) -> Result<()> {
    let config = Arc::new(config);

    log::info!("creating socks5 server ...");
    let server = Server::bind(config.inbound_addr.as_str(), Arc::new(NoAuth)).await?;
    log::info!("creating socks5 server done");
    log::info!("creating 2ra client ...");

    let pool =
        vec![Arc::new(Client::new(config.clone(), instance.clone()).await?); config.client_pool];
    let mut next_client_idx = 0;

    while let Ok((conn, _)) = tokio::select! {
        _ = instance.shutdown_signal.notified() => {
            log::info!("shutting down client...");
            return Ok(());
        },
        x = server.accept() => x,
    } {
        let client = pool[next_client_idx].clone();
        next_client_idx += 1;
        if next_client_idx >= pool.len() {
            next_client_idx = 0;
        }
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
    tx: RwLock<Arc<tokio::sync::mpsc::Sender<Vec<u8>>>>,
    config: Arc<ClientConfig>,
    next_conn_id: AtomicUsize,

    receiver_channels: RwLock<Arc<chashmap::CHashMap<usize, tokio::sync::mpsc::Sender<Vec<u8>>>>>,

    kill_notification: RwLock<Arc<tokio::sync::Notify>>,
    killing: AtomicBool,

    gate: Gate,

    instance: Arc<Instance>,
}

unsafe impl Sync for Client {}

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

    pub async fn new(config: Arc<ClientConfig>, instance: Arc<Instance>) -> Result<Client> {
        // stub tx channel
        let (tx, _) = channel(10000);
        Ok(Client {
            tx: RwLock::new(Arc::new(tx)),
            next_conn_id: Default::default(),
            receiver_channels: Default::default(),
            config,
            gate: Gate::new(true),
            kill_notification: Default::default(),
            killing: AtomicBool::new(true),
            instance,
        })
    }

    pub async fn recreate_pipes(self: &Arc<Self>) -> Result<()> {
        log::info!("creating pipes!");

        self.killing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        {
            let x = self.kill_notification.read().await.clone();
            x.notify_waiters();
        }

        let new_kill_notification = Arc::new(Notify::new());

        let receiver_channels: Arc<chashmap::CHashMap<usize, tokio::sync::mpsc::Sender<Vec<u8>>>> =
            Default::default();
        let receiver_channels_ref = receiver_channels.clone();

        let mut tx_ref = self.tx.write().await;
        let mut receiver_channels_mut_ref = self.receiver_channels.write().await;
        let mut old_kill_notification_ref = self.kill_notification.write().await;

        log::info!("creating rx_stream");
        let (uuid, rx_stream) = {
            // connect to server and get a new uuid
            let mut stream = Self::create_connection(
                &self.config.outbound_addr,
                &self.config.rx_sni,
                &self.config.http_request,
                self.config.insecure_tls,
            )
            .await?;
            log::trace!("connection made");

            let init_msg = ClientMessage::InitRx { new_session: true };
            stream.write_all(init_msg.as_message()?.as_slice()).await?;
            stream.flush().await?;
            wait_for_body(&self.config.separator, &mut stream).await?;
            log::debug!("waiting for stub response done");
            match read_server_message(&mut stream).await? {
                ServerMessage::SessionCreated { uuid } => (uuid, stream),
                x => return Err(eyre!("unexpected message received: {:?}", x)),
            }
        };
        log::info!("rx_stream created");

        let client_ref = self.clone();
        let new_kill_notification_ref = new_kill_notification.clone();

        self.killing
            .store(false, std::sync::atomic::Ordering::SeqCst);

        tokio::spawn(async move {
            let mut rx_stream = rx_stream;
            loop {
                let msg = match tokio::select! {
                    _ = new_kill_notification_ref.notified() => {
                        log::info!("killing rx_stream...");
                        return;
                    },
                    x = read_server_message(&mut rx_stream) => x,
                } {
                    Ok(x) => x,
                    Err(e) => {
                        log::error!("hit error while reading server message {:?}", e);
                        client_ref
                            .killing
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        return;
                    }
                };

                match msg {
                    ServerMessage::Data { id, data } => {
                        if let Some(chan) = receiver_channels.get(&id) {
                            if client_ref
                                .killing
                                .load(std::sync::atomic::Ordering::Relaxed)
                            {
                                return;
                            }
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
                &self.config.outbound_addr,
                &self.config.tx_sni,
                &self.config.http_request,
                self.config.insecure_tls,
            )
            .await?;

            let init_msg = ClientMessage::InitTx { uuid };
            stream.write_all(init_msg.as_message()?.as_slice()).await?;
            stream.flush().await?;
            stream
        };

        let (tx, mut chan_rx) = mpsc::channel::<Vec<u8>>(10000);

        let client_ref = self.clone();
        let new_kill_notification_ref = new_kill_notification.clone();
        tokio::spawn(async move {
            while let Some(x) = tokio::select! {
                _ = new_kill_notification_ref.notified() => {
                    log::info!("killing tx_stream...");
                    return;
                },
                x = chan_rx.recv() => x,
            } {
                if let Err(e) = tokio::select! {
                _ = new_kill_notification_ref.notified() => {
                    log::info!("killing tx_stream...");
                    return;
                },
                x =  tx_stream.write_all(&x) => x, }
                {
                    log::error!("{:?}", e);
                    client_ref
                        .killing
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
                let _ = tx_stream.flush().await;
            }
        });
        log::info!("tx_stream created");

        let inst = self.instance.clone();
        let self_ref = self.clone();
        tokio::spawn(async move {
            inst.shutdown_signal.notified().await;
            self_ref.gate.set_open(false);
            self_ref
                .kill_notification
                .read()
                .await
                .clone()
                .notify_waiters();
        });

        *tx_ref = Arc::new(tx);

        *receiver_channels_mut_ref = receiver_channels_ref;
        *old_kill_notification_ref = new_kill_notification;
        drop(tx_ref);
        drop(receiver_channels_mut_ref);

        Ok(())
    }

    pub async fn restart(self: &Arc<Self>) {
        log::info!("restarting pipes...");
        self.gate.set_open(false);
        while let Err(e) = self.recreate_pipes().await {
            log::error!("failed to create pipes... {e}");
            log::info!("retrying in 5 seconds...");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        self.gate.set_open(true);
    }

    pub async fn create_new_connection(
        &self,
        addr: Address,
    ) -> Result<(tokio::sync::mpsc::Receiver<Vec<u8>>, usize)> {
        let id = self
            .next_conn_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let (tx, rx) = mpsc::channel::<Vec<u8>>(10000);

        if self.receiver_channels.read().await.insert(id, tx).is_some() {
            log::warn!("id collided!");
        }

        let msg = match addr {
            Address::SocketAddress(addr) => ClientMessage::NewConnectionWithIp { addr, id },
            Address::DomainAddress(domain, port) => {
                ClientMessage::NewConnectionWithDomain { domain, port, id }
            }
        };

        self.tx.read().await.send(msg.as_message()?).await?;
        Ok((rx, id))
    }

    pub async fn send_data(&self, id: usize, data: Vec<u8>) -> Result<()> {
        let msg = ClientMessage::Data { id, data };
        let tx = self.tx.read().await;
        tx.send(msg.as_message()?).await?;
        Ok(())
    }
}

async fn wait_for_body(target: &[char], stream: &mut TlsStream<TcpStream>) -> Result<()> {
    log::debug!("waiting for stub response message");
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

async fn read_server_message(stream: &mut TlsStream<TcpStream>) -> Result<ServerMessage> {
    assert!(bincode::serialized_size(&0u64)? == size_of::<u64>() as u64);
    log::debug!("waiting for server message...");
    let mut len_hdr = [0u8; 64 / 8];
    stream.read_exact(&mut len_hdr).await?;
    let len: usize = bincode::deserialize(len_hdr.as_slice())?;

    if len > 100_000 {
        return Err(eyre!("invalid message len size. {len} is too big"));
    }

    log::debug!("got a server message with len {}", len);
    let mut buffer = vec![0u8; len];

    let n = stream.read_exact(&mut buffer).await?;
    if n != len {
        return Err(eyre!("n != len"));
    }
    Ok(bincode::deserialize(&buffer)?)
}

async fn handle(client: Arc<Client>, conn: IncomingConnection) -> Result<()> {
    client.gate.wait_or_pass().await;
    if client
        .killing
        .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        client.restart().await;
    }

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

            let client_ref = client.clone();
            let read_task = tokio::task::spawn(async move {
                let mut buff = vec![0u8; client_ref.config.buffer_size];
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
                            if let Err(e) = client_ref.send_data(id, data).await {
                                log::error!("error while sending data to server {e}");
                                bp.store(true, std::sync::atomic::Ordering::Relaxed);
                                return;
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
            let kill_notification = client.kill_notification.read().await.clone();

            tokio::select! {
                _ = kill_notification.notified() => {
                    broken_pipe.store(true, std::sync::atomic::Ordering::Relaxed);

                    return Err(eyre!("kill notification received while handling socks5 connection"));
                },
                x = async { tokio::join!(read_task, write_task) } => {
                    x.0?;
                    x.1?;

                    log::debug!("socks5 job done");
                    return Ok(());
                }
            };
        }
    }

    Ok(())
}
