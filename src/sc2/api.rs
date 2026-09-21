use std::net::TcpStream;

use protobuf::Message;
use sc2_proto::sc2api::{Request, Response};
use tungstenite::{Bytes, Message::Binary, WebSocket, stream::MaybeTlsStream};

use crate::sc2::error::Sc2Error;

pub struct Api {
    pub(crate) ws: WebSocket<MaybeTlsStream<TcpStream>>,
}

impl Api {
    pub(crate) fn send(&mut self, msg: Request) -> Result<Response, Sc2Error> {
        self.ws.send(Binary(Bytes::from(msg.write_to_bytes()?)))?;
        let message = self.ws.read()?;
        let response = Response::parse_from_bytes(&message.into_data())?;
        // println!("Received response {:?}", response);
        Ok(response)
    }
}
