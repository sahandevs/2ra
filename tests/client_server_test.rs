use fast_socks5::client::Config;
use httpmock::prelude::*;
use std::{
    sync::{atomic::Ordering::Acquire, Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CONFIG: &str = r#"
[client]
http_request = """
POST / HTTP/1.0
Host: fronted-domain.com
Content-Length: 99999999999999999
X-2ra-version: <VER>
"""
buffer_size = 1024
tx_sni = "tx.com"
rx_sni = "rx.com"
insecure_tls = true
# socks5 addr
inbound_addr = "127.0.0.1:8989"
# 2ra server addr
outbound_addr = "127.0.0.1:9999"
separator = ["\r", "\n", "\r", "\n"]
client_pool = 5

[server]
http_response = """
HTTP/1.0 200 OK
Content-Type: text/html; charset=UTF-8
Server: gws
"""
buffer_size = 1024
inbound_addr = "127.0.0.1:9999"
cert_pem_path = "./cert/cert.pem"
key_pem_path = "./cert/key.pem"
separator = ["\r", "\n", "\r", "\n"]
"#;

#[allow(dead_code)]
pub struct Env {
    instance: Arc<lib2ra::instance::Instance>,
    mock_server: MockServer,
    http_client: reqwest::Client,
    udp_upstream_addr: String,

    socks5_addr: String,
}

async fn create_udp_upstream() -> String {
    let port = portpicker::pick_unused_port().expect("no free port");
    let addr = format!("127.0.0.16:{}", port);
    let socket = tokio::net::UdpSocket::bind(&addr).await.unwrap();
    tokio::spawn(async move {
        let mut buff = [0u8; 1024];
        while let Ok((n, addr)) = socket.recv_from(&mut buff).await {
            let _ = &buff[..n];
            socket.send_to(b"hi!", addr).await.unwrap();
        }
    });
    addr
}

async fn setup_environment() -> Arc<Env> {
    lazy_static::lazy_static! {
        pub static ref ENV: std::sync::Mutex<Option<Arc<Env>>> = Mutex::new(None);
    }

    let mut env_ref = ENV.lock().unwrap();

    if let Some(env) = &*env_ref {
        return env.clone();
    }
    // let _ = color_eyre::install();
    // pretty_env_logger::init();
    let config: lib2ra::config::Config = toml::from_str(CONFIG).unwrap();
    let instance = lib2ra::instance::Instance::new(config.clone()).unwrap();

    // create a server
    let instance_ref = instance.clone();
    tokio::spawn(async move {
        instance_ref.start().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // create an mock server
    let mock_server = MockServer::start();

    // create reqwest client that uses 2ra
    let addr = format!("socks5://{}", &config.client.as_ref().unwrap().inbound_addr);
    let proxy = reqwest::Proxy::http(&addr).unwrap();

    let http_client = reqwest::ClientBuilder::new().proxy(proxy).build().unwrap();

    let env = Arc::new(Env {
        instance,
        mock_server,
        http_client,
        udp_upstream_addr: create_udp_upstream().await,
        socks5_addr: config.client.as_ref().unwrap().inbound_addr.clone(),
    });

    *env_ref = Some(env.clone());

    return env;
}

fn create_mock(env: &Env) -> httpmock::Mock<'_> {
    let hello_mock = env.mock_server.mock(|when, then| {
        when.method(GET);
        then.status(200).header("X-2ra", "Yes");
    });
    hello_mock
}

#[tokio::test]
async fn test_simple_http_client() {
    let env = setup_environment().await;
    let _ = create_mock(&env);
    assert_eq!(env.instance.stats.client_handled_tcp_conn.load(Acquire), 0);
    assert_eq!(env.instance.stats.server_handled_tcp_conn.load(Acquire), 0);
    let response = env
        .http_client
        .execute(
            env.http_client
                .get(env.mock_server.url("/"))
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let headers = response.headers();
    assert_eq!(headers.get("X-2ra").unwrap(), "Yes");

    assert_ne!(env.instance.stats.client_handled_tcp_conn.load(Acquire), 0);
    assert_ne!(env.instance.stats.server_handled_tcp_conn.load(Acquire), 0);
}

#[tokio::test]
async fn test_simple_tcp() {
    let env = setup_environment().await;
    let _ = create_mock(&env);
    let mut socks5 = fast_socks5::client::Socks5Stream::connect(
        &env.socks5_addr,
        env.mock_server.address().ip().to_string(),
        env.mock_server.address().port(),
        Config::default(),
    )
    .await
    .unwrap();

    socks5
        .write_all(
            b"GET / HTTP/1.0
Host: fronted-domain.com
\r\n\r\n\r\n\r\n",
        )
        .await
        .unwrap();
    let mut buff = [0u8; 1024 * 64];

    loop {
        match socks5.read(&mut buff).await {
            Ok(n) => {
                if n == 0 {
                    break;
                }

                let data = &buff[..n];
                let chunk = std::str::from_utf8(data).unwrap();
                assert!(chunk.contains("x-2ra: Yes"));
                if chunk.ends_with("\r\n") {
                    break;
                }
            }
            Err(e) => {
                panic!("{}", e);
            }
        }
    }
}
