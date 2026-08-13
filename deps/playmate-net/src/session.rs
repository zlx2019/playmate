//! TCP pairing handshake: the host listens while the client connects and submits a code.
//!
//! Handshake flow:
//!
//! ```text
//! Client ──── Hello{version, name} ───→ Host
//! Client ←─── Challenge (or Reject) ──── Host   # Version validation
//! Client ──── PairCode{code} ─────────→ Host
//! Client ←─── Welcome{rom} (or Reject) ─ Host   # Pairing-code validation
//! ```
//!
//! Every handshake read has a timeout so a half-open connection cannot stall the task.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::NetError;
use crate::protocol::{Message, PROTOCOL_VERSION};

/// Timeout for each handshake step, allowing enough time to enter a code.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for a client-initiated TCP connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Host-side session after pairing completes.
pub struct HostSession {
    /// Connection after a successful handshake.
    pub stream: TcpStream,
    /// Client display name.
    pub peer_name: String,
    /// Client address.
    pub peer_addr: SocketAddr,
}

/// Client-side session after pairing completes.
pub struct ClientSession {
    /// Connection after a successful handshake.
    pub stream: TcpStream,
    /// Name of the ROM running on the host.
    pub rom_name: String,
}

/// Reads one message with a timeout mapped to an I/O `TimedOut` error.
async fn read_timed(stream: &mut TcpStream) -> Result<Message, NetError> {
    match timeout(HANDSHAKE_TIMEOUT, Message::read_from(stream)).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(NetError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "握手等待超时",
        ))),
    }
}

/// Waits until a client completes pairing with the host.
///
/// Connections that fail because of a version mismatch, invalid code, or timeout
/// are dropped before waiting for the next client. Only listener errors are returned.
pub async fn host_wait_for_peer(
    listener: &TcpListener,
    pair_code: &str,
    rom_name: &str,
) -> Result<HostSession, NetError> {
    loop {
        let (mut stream, peer_addr) = listener.accept().await?;
        log::info!("connection received: {peer_addr}");
        match pair_with_client(&mut stream, pair_code, rom_name).await {
            Ok(peer_name) => {
                log::info!("pairing succeeded: {peer_name} ({peer_addr})");
                return Ok(HostSession {
                    stream,
                    peer_name,
                    peer_addr,
                });
            }
            Err(e) => log::warn!("pairing failed ({peer_addr}): {e}; waiting for next connection"),
        }
    }
}

/// Completes the host-side handshake with one client and returns its display name.
async fn pair_with_client(
    stream: &mut TcpStream,
    pair_code: &str,
    rom_name: &str,
) -> Result<String, NetError> {
    stream.set_nodelay(true)?;

    // Step 1: validate the protocol version.
    let (version, peer_name) = match read_timed(stream).await? {
        Message::Hello { version, name } => (version, name),
        other => return Err(NetError::Protocol(format!("预期 Hello，收到 {other:?}"))),
    };
    if version != PROTOCOL_VERSION {
        let reason = format!("协议版本不匹配（主机 v{PROTOCOL_VERSION}，客户端 v{version}）");
        Message::Reject {
            reason: reason.clone(),
        }
        .write_to(stream)
        .await?;
        return Err(NetError::Rejected(reason));
    }

    // Step 2: validate the pairing code.
    Message::Challenge.write_to(stream).await?;
    let code = match read_timed(stream).await? {
        Message::PairCode { code } => code,
        other => {
            return Err(NetError::Protocol(format!("预期 PairCode，收到 {other:?}")));
        }
    };
    if code != pair_code {
        let reason = "配对码错误".to_string();
        Message::Reject {
            reason: reason.clone(),
        }
        .write_to(stream)
        .await?;
        return Err(NetError::Rejected(reason));
    }

    Message::Welcome {
        rom_name: rom_name.to_string(),
    }
    .write_to(stream)
    .await?;
    Ok(peer_name)
}

/// Connects to a host and completes client-side pairing.
///
/// `code_provider` is called only after the host sends `Challenge`.
pub async fn client_connect(
    addr: SocketAddr,
    client_name: &str,
    code_provider: impl FnOnce() -> String,
) -> Result<ClientSession, NetError> {
    let mut stream = match timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(NetError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("连接 {addr} 超时"),
            )));
        }
    };
    stream.set_nodelay(true)?;

    Message::Hello {
        version: PROTOCOL_VERSION,
        name: client_name.to_string(),
    }
    .write_to(&mut stream)
    .await?;

    match read_timed(&mut stream).await? {
        Message::Challenge => {}
        Message::Reject { reason } => return Err(NetError::Rejected(reason)),
        other => {
            return Err(NetError::Protocol(format!(
                "预期 Challenge，收到 {other:?}"
            )));
        }
    }

    Message::PairCode {
        code: code_provider(),
    }
    .write_to(&mut stream)
    .await?;

    match read_timed(&mut stream).await? {
        Message::Welcome { rom_name } => Ok(ClientSession { stream, rom_name }),
        Message::Reject { reason } => Err(NetError::Rejected(reason)),
        other => Err(NetError::Protocol(format!("预期 Welcome，收到 {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Test host that accepts only one connection, avoiding a retry loop on failure.
    async fn host_wait_for_peer_once(listener: &TcpListener) -> Result<HostSession, NetError> {
        let (mut stream, peer_addr) = listener.accept().await?;
        pair_with_client(&mut stream, "1234", "test-rom.nes")
            .await
            .map(|peer_name| HostSession {
                stream,
                peer_name,
                peer_addr,
            })
    }

    /// Runs a complete handshake with a real listener and concurrent host/client tasks.
    async fn run_handshake(
        client_code: &str,
    ) -> (
        Result<HostSession, NetError>,
        Result<ClientSession, NetError>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let host = tokio::spawn(async move { host_wait_for_peer_once(&listener).await });
        let code = client_code.to_string();
        let client = client_connect(addr, "test-client", move || code).await;
        (host.await.unwrap(), client)
    }

    /// The correct code succeeds on both sides and returns the ROM name to the client.
    #[tokio::test]
    async fn pairing_succeeds_with_correct_code() {
        let (host, client) = run_handshake("1234").await;
        let host = host.unwrap();
        let client = client.unwrap();
        assert_eq!(host.peer_name, "test-client");
        assert_eq!(client.rom_name, "test-rom.nes");
    }

    /// An incorrect code is rejected by the host and reported to the client.
    #[tokio::test]
    async fn pairing_fails_with_wrong_code() {
        let (host, client) = run_handshake("0000").await;
        assert!(matches!(host, Err(NetError::Rejected(_))));
        assert!(matches!(client, Err(NetError::Rejected(_))));
    }
}
