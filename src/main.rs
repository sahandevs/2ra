use lib2ra::{client::start_client, server::start_server};

#[tokio::main]
async fn main() {
    start_server().await.unwrap();
}
