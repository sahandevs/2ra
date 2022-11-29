use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};

#[derive(Serialize, Deserialize, Debug)]
pub enum ClientMessage {
    InitRx { new_session: bool },
    InitTx { uuid: String },
    NewConnectionWithIp { addr: SocketAddr, id: usize },
    NewConnectionWithDomain { ip: IpAddr, port: u16, id: usize },
    Data { id: usize, data: Vec<u8> },
    CloseConnection { id: usize },
    Heartbeat,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ServerMessage {
    SessionCreated { uuid: String },
    Data { id: usize, data: Vec<u8> },
    ConnectionClosed { id: usize },
    Heartbeat,
}

pub trait IntoMessage {
    fn as_message(&self) -> Result<Vec<u8>>;
}

impl IntoMessage for ClientMessage {
    fn as_message(&self) -> Result<Vec<u8>> {
        // TODO: avoid reallocation
        let data = bincode::serialize(self)?;
        let mut result: Vec<u8> = vec![];
        result.extend(bincode::serialize(&(data.len() as u64))?);
        result.extend(data);

        Ok(result)
    }
}

impl IntoMessage for ServerMessage {
    fn as_message(&self) -> Result<Vec<u8>> {
        // TODO: avoid reallocation
        let data = bincode::serialize(self)?;
        let mut result: Vec<u8> = vec![];
        result.extend(bincode::serialize(&(data.len() as u64))?);
        result.extend(data);

        Ok(result)
    }
}
