use thiserror::Error;

#[derive(Error, Debug)]
pub enum Sc2Error {
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error(transparent)]
    WebSocketError(#[from] tungstenite::Error),
    #[error(transparent)]
    ProtobufError(#[from] protobuf::ProtobufError),
    #[error(transparent)]
    SerdeJsonError(#[from] serde_json::Error),
    #[error("{0}")]
    Protocol(String),
}
