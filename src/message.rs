use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Serialize, Deserialize, Debug)]
pub enum ClientMessage {
    InitRx {
        new_session: bool,
    },
    InitTx {
        uuid: String,
    },
    NewUdpSocket {
        id: usize,
    },
    DataUdp {
        id: usize,
        addr: SocketAddr,
        data: Vec<u8>,
    },
    NewConnectionWithIp {
        addr: SocketAddr,
        id: usize,
    },
    NewConnectionWithDomain {
        domain: String,
        port: u16,
        id: usize,
    },
    Data {
        id: usize,
        data: Vec<u8>,
    },
    CloseConnection {
        id: usize,
    },
    Heartbeat,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ServerMessage {
    SessionCreated {
        uuid: String,
    },
    Data {
        id: usize,
        data: Vec<u8>,
    },
    DataUdp {
        id: usize,
        data: Vec<u8>,
        addr: SocketAddr,
    },
    ConnectionClosed {
        id: usize,
    },
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
