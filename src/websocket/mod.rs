//! RiseX WebSocket transport and wire types.
//!
//! - [`client`]: connection, heartbeat, reconnect with backoff, subscription state.
//! - [`messages`]: every frame the venue sends and the client sends.
//! - [`enums`]: raw venue enums.
//! - [`error`]: the client error type.
//!
//! Parsing into Nautilus types, dispatch and account state are not here.

pub mod account_state;
pub mod client;
pub mod dispatch;
pub mod enums;
pub mod error;
pub mod handler;
pub mod messages;
pub mod parse;

pub use client::RisexWebSocketClient;
pub use enums::RisexWsChannel;
pub use error::RisexWsError;
pub use messages::RisexWsMessage;
