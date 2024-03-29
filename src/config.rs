use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: Option<ServerConfig>,
    pub client: Option<ClientConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub http_response: String,
    pub buffer_size: usize,

    pub inbound_addr: String,
    pub cert_pem_path: String,
    pub key_pem_path: String,

    pub separator: Vec<char>,

    pub udp_bind_addr: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientConfig {
    pub http_request: String,
    pub buffer_size: usize,

    pub tx_sni: String,
    pub rx_sni: String,

    pub insecure_tls: bool,

    pub inbound_addr: String,
    pub outbound_addr: String,

    pub separator: Vec<char>,

    pub client_pool: usize,

    pub udp_bind_addr: String,
}
