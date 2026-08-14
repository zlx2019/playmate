//! playmate-net: LAN discovery, pairing handshake, and session protocol.
//!
//! Layers:
//! - [`protocol`]: session messages and length-prefixed, tagged binary encoding
//! - [`discovery`]: host-side mDNS advertising and client-side room browsing
//! - [`session`]: TCP pairing handshake (Hello -> Challenge -> PairCode -> Welcome/Reject)
//!
//! The network model uses **tokio asynchronous I/O**, leaving room for future
//! multi-connection features such as spectators, voice chat, and lobbies.
//! The exception is mdns-sd, which uses synchronous worker threads.
//! [`browse_rooms`] blocks and should be called through `tokio::task::spawn_blocking`
//! from an async context.

pub mod codec;
pub mod discovery;
pub mod protocol;
pub mod session;

pub use codec::{FrameDecoder, FrameEncoder, f32_to_i16_bytes, i16_bytes_to_f32};
pub use discovery::{Announcer, Room, browse_rooms};
pub use protocol::{Message, PROTOCOL_VERSION};
pub use session::{ClientSession, HostSession, client_connect, host_wait_for_peer, reject_client};

/// Common error type for the networking layer.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// Low-level I/O error, such as a disconnect or timeout.
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// The peer sent a message that violates the protocol.
    #[error("协议错误: {0}")]
    Protocol(String),
    /// The host rejected pairing due to a version mismatch, invalid code, or similar issue.
    #[error("配对被拒绝: {0}")]
    Rejected(String),
    /// mDNS service discovery error.
    #[error("服务发现错误: {0}")]
    Discovery(String),
}

impl From<mdns_sd::Error> for NetError {
    fn from(e: mdns_sd::Error) -> Self {
        Self::Discovery(e.to_string())
    }
}
