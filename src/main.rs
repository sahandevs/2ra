use std::{env::args, time::Duration};

use lib2ra::{client::start_client, config, server::start_server};
use tokio::fs::read_to_string;

macro_rules! err {
    ($expr:expr) => {{
        match $expr {
            Ok(x) => x,
            Err(e) => {
                eprintln!(
                    "[warn] {e} in {} {}",
                    stringify!($expr),
                    core::panic::Location::caller()
                );
                return;
            }
        }
    }};
}

#[tokio::main]
async fn main() {
    color_eyre::install().unwrap();
    pretty_env_logger::init();

    let mut args = args().into_iter();
    let config = loop {
        let x = args.next();
        if let Some(x) = x {
            if let Some(x) = x.strip_prefix("--config=") {
                break x.to_string();
            }
        } else {
            log::error!("abort. please provide a valid config.toml path. usage: ./2ra --config=\"path/to/config.toml\"");
            std::process::exit(1);
        }
    };
    let config = read_to_string(config).await.unwrap();
    let mut config: config::Config = toml::de::from_str(&config).unwrap();

    let mut tasks = vec![];

    if let Some(mut server_config) = config.server.take() {
        server_config.http_response = server_config.http_response.trim().to_string();
        server_config.http_response.push_str("\r\n\r\n");
        let task = tokio::task::spawn(async { err!(start_server(server_config).await) });
        tasks.push(task);
    }

    tokio::time::sleep(Duration::from_millis(100)).await;
    if let Some(mut client_config) = config.client.take() {
        client_config.http_request = client_config.http_request.trim().to_string();
        client_config.http_request.push_str("\r\n\r\n");
        let task = tokio::task::spawn(async { err!(start_client(client_config).await) });

        tasks.push(task);
    }

    for task in tasks {
        task.await.unwrap();
    }
}
