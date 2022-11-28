use std::time::Duration;

use lib2ra::{client::start_client, server::start_server};

macro_rules! err {
    ($expr:expr) => {{
        match $expr {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[warn] {e} in {} {}", stringify!($expr), core::panic::Location::caller());
                return;
            }
        }
    }};
}

#[tokio::main]
async fn main() {
    let a = tokio::task::spawn(async { err!(start_server().await) });
    
    tokio::time::sleep(Duration::from_millis(100)).await;
    let b = tokio::task::spawn(async { err!(start_client().await) });

    a.await.unwrap();
    b.await.unwrap();
}
