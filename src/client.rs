use std::{
    mem::size_of,
    sync::{atomic::AtomicUsize, Arc},
};

use anyhow::{bail, Error};
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

use crate::message::{ClientMessage, IntoMessage, ServerMessage};

pub async fn start_client() -> Result<(), Error> {
    let server = Server::bind("127.0.0.1:5000", Arc::new(NoAuth)).await?;

    let client = Arc::new(Client::new("127.0.0.1:4433").await?);
    println!("[client] building client<->server connection done!");
    while let Ok((conn, _)) = server.accept().await {
        let client = client.clone();
        tokio::spawn(async move {
            match handle(client, conn).await {
                Ok(()) => {}
                Err(err) => eprintln!("[client] ->{err}"),
            }
        });
    }

    Ok(())
}

struct Client {
    tx: Arc<tokio::sync::mpsc::Sender<Vec<u8>>>,

    next_conn_id: AtomicUsize,

    receiver_channels: Arc<chashmap::CHashMap<usize, tokio::sync::mpsc::Sender<Vec<u8>>>>,
}

struct StubVerifier;

impl ServerCertVerifier for StubVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::Certificate,
        intermediates: &[rustls::Certificate],
        server_name: &rustls::ServerName,
        scts: &mut dyn Iterator<Item = &[u8]>,
        ocsp_response: &[u8],
        now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

impl Client {
    #[track_caller]
    async fn create_connection(server: &str) -> Result<TlsStream<TcpStream>, Error> {
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
        config
            .dangerous()
            .set_certificate_verifier(Arc::new(StubVerifier));
        let connector = TlsConnector::from(Arc::new(config));
        let stream = TcpStream::connect(&server).await?;
        let domain = rustls::ServerName::try_from("domain.com")?;

        let mut stream = connector.connect(domain, stream).await?;
        let content = format!("POST / HTTP/1.0\r\nHost: {}\r\n\r\n", "example.orgxx");
        stream.write_all(content.as_bytes()).await?;
        stream.flush().await?;
        Ok(stream)
    }

    pub async fn new(server: &str) -> Result<Client, Error> {
        let receiver_channels: Arc<chashmap::CHashMap<usize, tokio::sync::mpsc::Sender<Vec<u8>>>> =
            Default::default();
        let receiver_channels_ref = receiver_channels.clone();

        println!("[client] creating rx_stream");
        let (uuid, rx_stream) = {
            // connect to server and get a new uuid
            let mut stream = Self::create_connection(server).await?;

            let init_msg = ClientMessage::InitRx { new_session: true };
            stream.write_all(init_msg.as_message()?.as_slice()).await?;
            stream.flush().await?;
            println!("[client] waiting for stub response body...");
            wait_for_body(&mut stream).await?;
            println!("[client] waiting for stub response done");
            match read_server_message(&mut stream).await? {
                ServerMessage::SessionCreated { uuid } => (uuid, stream),
                x => bail!("unexpected message received: {:?}", x),
            }
        };
        println!("[client] rx_stream created");

        tokio::spawn(async move {
            let mut rx_stream = rx_stream;
            loop {
                let msg = match read_server_message(&mut rx_stream).await {
                    Ok(x) => x,
                    Err(e) => {
                        println!("[client] [WARN] hit error while reading server message {:?}", e);
                        continue;
                    }
                };

                match msg {
                    ServerMessage::Data { id, data } => {
                        if let Some(chan) = receiver_channels.get(&id) {
                            if let Err(e) = chan.send(data).await {
                                println!("[client] [WARN] {}", e);
                            }
                        } else {
                            println!("[client] [WARN] no channel listening for {}", id);
                        }
                    }
                    x => print!("[WARN] unexpected message received: {:?}", x),
                }
            }
        });

        println!("[client] creating tx_stream");
        let mut tx_stream = {
            // connect to server and init tx
            let mut stream = Self::create_connection(server).await?;

            let init_msg = ClientMessage::InitTx { uuid };
            stream.write_all(init_msg.as_message()?.as_slice()).await?;
            stream.flush().await?;
            stream
        };

        let (tx, mut chan_rx) = mpsc::channel::<Vec<u8>>(10000);

        tokio::spawn(async move {
            while let Some(x) = chan_rx.recv().await {
                if let Err(e) = tx_stream.write_all(&x).await {
                    println!("[client] [Err] {:?}", e)
                }
                let _ = tx_stream.flush().await;
            }
        });
        println!("[client] tx_stream created");

        Ok(Client {
            tx: Arc::new(tx),
            next_conn_id: Default::default(),
            receiver_channels: receiver_channels_ref,
        })
    }

    pub async fn create_new_connection(
        &self,
        addr: Address,
    ) -> Result<(tokio::sync::mpsc::Receiver<Vec<u8>>, usize), Error> {
        let id = self
            .next_conn_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let (tx, rx) = mpsc::channel::<Vec<u8>>(10000);

        if self.receiver_channels.insert(id, tx).is_some() {
            println!("[client] [WARN] id collided!");
        }

        let msg = match addr {
            Address::SocketAddress(addr) => ClientMessage::NewConnectionWithIp { addr, id },
            x => bail!("{:?} not supported", x),
        };

        self.tx.send(msg.as_message()?).await?;
        Ok((rx, id))
    }
}

async fn wait_for_body(stream: &mut TlsStream<TcpStream>) -> Result<(), Error> {
    let mut sep = [0u8; 4];
    loop {
        let n = stream.read_exact(&mut sep).await?;
        match (n, sep) {
            (4, [b'\r', b'\n', b'\r', b'\n']) => break,
            (4, _) => {}
            _ => bail!("!"),
        }
    }
    Ok(())
}

async fn read_server_message(stream: &mut TlsStream<TcpStream>) -> Result<ServerMessage, Error> {
    assert!(bincode::serialized_size(&0u64)? == size_of::<u64>() as u64);
    println!("[client] waiting for server message...");
    let mut len_hdr = [0u8; 64 / 8];
    stream.read_exact(&mut len_hdr).await?;
    let len: usize = bincode::deserialize(len_hdr.as_slice())?;
    println!("[client] got a server message with len {}", len);
    let mut buffer = vec![0u8; len];

    let n = stream.read_exact(&mut buffer).await?;
    if n != len {
        bail!("n != len");
    }
    Ok(bincode::deserialize(&buffer)?)
}

async fn handle(client: Arc<Client>, conn: IncomingConnection) -> Result<(), Error> {
    println!("[client] new socks5 conn");
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

            let read_task = tokio::task::spawn(async move {
                let mut buff = [0u8; 1024];
                loop {
                    match socks_read.read(buff.as_mut_slice()).await {
                        Ok(n) => {
                            if n == 0 {
                                println!("[client] n == 0");
                                break;
                            }
                            let data = buff[0..n].to_vec(); // TODO: double check
                            let msg = ClientMessage::Data { id, data };
                            client.tx.send(msg.as_message().unwrap()).await.unwrap();
                        }
                        Err(e) => {
                            println!("[client] err {:?}", e);
                            break;
                        }
                    };
                }
            });

            let write_task = tokio::task::spawn(async move {
                while let Some(data) = proxy_server_stream.recv().await {
                    socks_write.write_all(data.as_slice()).await.unwrap();
                    socks_write.flush().await.unwrap();
                }
            });

            let (a, b) = tokio::join!(read_task, write_task);
            a?;
            b?;
            println!("[client]!");
        }
    }

    Ok(())
}
