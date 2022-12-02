use std::env::args;

use tokio::fs::read_to_string;


mod start;

pub mod client;
pub mod config;
pub mod gate;
pub mod message;
pub mod server;

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
    let config: config::Config = toml::de::from_str(&config).unwrap();
    
    start::start(config).await;
}
