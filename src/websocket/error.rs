//! Error type of the RiseX WebSocket client.

use nautilus_network::error::SendError;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum RisexWsError {
    /// The connection could not be established.
    #[error("network error: {0}")]
    Network(String),
    /// A frame could not be written.
    #[error("transport error: {0}")]
    Transport(#[from] SendError),
    /// The caller passed an invalid request.
    #[error("client error: {0}")]
    Client(String),
}
