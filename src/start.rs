use std::time::Duration;

use crate::{client::start_client, config, server::start_server};

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

pub async fn start(mut config: config::Config) {
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
