use std::{
    mem::size_of,
    sync::{atomic::AtomicUsize, Arc},
};

use anyhow::{bail, Error};
use rustls::OwnedTrustAnchor;
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

    let client = Arc::new(Client::new("127.0.0.1:1234").await?);

    while let Ok((conn, _)) = server.accept().await {
        let client = client.clone();
        tokio::spawn(async move {
            match handle(client, conn).await {
                Ok(()) => {}
                Err(err) => eprintln!("{err}"),
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

impl Client {
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

        let config = rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth(); // i guess this was previously the default?
        let connector = TlsConnector::from(Arc::new(config));
        let stream = TcpStream::connect(&server).await?;
        let domain = rustls::ServerName::try_from(server)?;

        let mut stream = connector.connect(domain, stream).await?;
        let content = format!("POST / HTTP/1.0\r\nHost: {}\r\n\r\n", "example.org");
        stream.write_all(content.as_bytes()).await?;
        stream.flush().await?;
        wait_for_body(&mut stream).await?;
        Ok(stream)
    }

    pub async fn new(server: &str) -> Result<Client, Error> {
        let receiver_channels: Arc<chashmap::CHashMap<usize, tokio::sync::mpsc::Sender<Vec<u8>>>> =
            Default::default();
        let receiver_channels_ref = receiver_channels.clone();
        let (uuid, rx_stream) = {
            // connect to server and get a new uuid
            let mut stream = Self::create_connection(server).await?;

            let init_msg = ClientMessage::InitRx { new_session: true };
            stream.write_all(init_msg.as_message()?.as_slice()).await?;
            stream.flush().await?;
            match read_server_message(&mut stream).await? {
                ServerMessage::SessionCreated { uuid } => (uuid, stream),
                x => bail!("unexpected message received: {:?}", x),
            }
        };

        tokio::spawn(async move {
            let mut rx_stream = rx_stream;
            loop {
                let msg = match read_server_message(&mut rx_stream).await {
                    Ok(x) => x,
                    Err(e) => {
                        print!("[WARN] hit error while reading server message {:?}", e);
                        continue;
                    }
                };

                match msg {
                    ServerMessage::Data { id, data } => {
                        if let Some(chan) = receiver_channels.get(&id) {
                            if let Err(e) = chan.send(data).await {
                                println!("[WARN] {}", e);
                            }
                        } else {
                            println!("[WARN] no channel listening for {}", id);
                        }
                    }
                    x => print!("[WARN] unexpected message received: {:?}", x),
                }
            }
        });

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
                    println!("[Err] {:?}", e)
                }
                let _ = tx_stream.flush().await;
            }
        });

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
            println!("[WARN] id collided!");
        }

        let msg = match addr {
            Address::SocketAddress(addr) => {
                ClientMessage::NewConnectionWithIp { addr, id }
            }
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

    let len = stream.read_u64().await? as usize;
    let mut buffer = vec![0u8; len];

    let n = stream.read_exact(&mut buffer).await?;
    if n != len {
        bail!("n != len");
    }
    Ok(bincode::deserialize(&buffer)?)
}

async fn handle(client: Arc<Client>, conn: IncomingConnection) -> Result<(), Error> {
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
                                println!("n == 0");
                                break;
                            }
                            let data = buff[0..n].to_vec(); // TODO: double check
                            let msg = ClientMessage::Data { id, data };
                            client.tx.send(msg.as_message().unwrap()).await.unwrap();
                        }
                        Err(e) => {
                            println!("err {:?}", e);
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
        }
    }

    Ok(())
}
