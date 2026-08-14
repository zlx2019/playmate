//! TCP pairing handshake: the host listens while the client connects and submits a code.
//!
//! Handshake flow:
//!
//! ```text
//! Client ──── Hello{version, name, role} ─→ Host
//! Client ←─── Challenge (or Reject) ─────── Host   # Version validation
//! Client ──── PairCode{code} ────────────→ Host
//! Client ←─── Welcome{rom} (or Reject) ──── Host   # Code validation + seat claim
//! ```
//!
//! Every handshake read has a timeout so a half-open connection cannot stall the task.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::NetError;
use crate::protocol::{JoinRole, Message, PROTOCOL_VERSION};

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
    /// Seat granted to the client.
    pub role: JoinRole,
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

/// Completes the host-side handshake with one client.
///
/// `seat_check` runs with the client's requested role after the pairing code
/// validates; returning `Err(reason)` rejects the client. The caller claims a
/// seat atomically inside the closure, before `Welcome` is sent.
pub async fn pair_with_client(
    stream: &mut TcpStream,
    pair_code: &str,
    rom_name: &str,
    seat_check: impl FnOnce(JoinRole) -> Result<(), String>,
) -> Result<(String, JoinRole), NetError> {
    stream.set_nodelay(true)?;

    // Step 1: validate the protocol version.
    let (version, peer_name, role) = match read_timed(stream).await? {
        Message::Hello {
            version,
            name,
            role,
        } => (version, name, role),
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

    // Step 3: let the caller claim a seat for the requested role.
    if let Err(reason) = seat_check(role) {
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
    Ok((peer_name, role))
}

/// Rejects one incoming connection with `reason` after reading its `Hello`.
///
/// Used by a host whose room is already occupied: the newcomer gets an
/// immediate answer instead of timing out in the accept backlog.
pub async fn reject_client(stream: &mut TcpStream, reason: &str) -> Result<(), NetError> {
    stream.set_nodelay(true)?;
    match read_timed(stream).await? {
        Message::Hello { .. } => {}
        other => return Err(NetError::Protocol(format!("预期 Hello，收到 {other:?}"))),
    }
    Message::Reject {
        reason: reason.to_owned(),
    }
    .write_to(stream)
    .await?;
    Ok(())
}

/// Connects to a host and completes client-side pairing for the given role.
///
/// `code_provider` is called only after the host sends `Challenge`.
pub async fn client_connect(
    addr: SocketAddr,
    client_name: &str,
    role: JoinRole,
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
        role,
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

    use tokio::net::TcpListener;

    use super::*;

    /// Test host that accepts one connection and pairs it with the given seat policy.
    async fn host_pair_once(
        listener: &TcpListener,
        seat_check: impl FnOnce(JoinRole) -> Result<(), String>,
    ) -> Result<HostSession, NetError> {
        let (mut stream, peer_addr) = listener.accept().await?;
        pair_with_client(&mut stream, "1234", "test-rom.nes", seat_check)
            .await
            .map(|(peer_name, role)| HostSession {
                stream,
                peer_name,
                peer_addr,
                role,
            })
    }

    /// Runs a complete handshake with a real listener and concurrent host/client tasks.
    async fn run_handshake(
        client_code: &str,
        role: JoinRole,
    ) -> (
        Result<HostSession, NetError>,
        Result<ClientSession, NetError>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let host = tokio::spawn(async move { host_pair_once(&listener, |_| Ok(())).await });
        let code = client_code.to_string();
        let client = client_connect(addr, "test-client", role, move || code).await;
        (host.await.unwrap(), client)
    }

    /// The correct code succeeds on both sides, carrying role and ROM name across.
    #[tokio::test]
    async fn pairing_succeeds_with_correct_code() {
        let (host, client) = run_handshake("1234", JoinRole::Spectator).await;
        let host = host.unwrap();
        let client = client.unwrap();
        assert_eq!(host.peer_name, "test-client");
        assert_eq!(host.role, JoinRole::Spectator);
        assert_eq!(client.rom_name, "test-rom.nes");
    }

    /// A failing seat check rejects the client with the given reason.
    #[tokio::test]
    async fn seat_check_rejection_reaches_client() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let host = tokio::spawn(async move {
            host_pair_once(&listener, |_| Err("玩家位已满".to_string())).await
        });
        let client =
            client_connect(addr, "test-client", JoinRole::Player, || "1234".to_string()).await;
        assert!(matches!(host.await.unwrap(), Err(NetError::Rejected(_))));
        let Err(err) = client else {
            panic!("expected the seat check to reject the client");
        };
        assert!(matches!(err, NetError::Rejected(reason) if reason == "玩家位已满"));
    }

    /// An incorrect code is rejected by the host and reported to the client.
    #[tokio::test]
    async fn pairing_fails_with_wrong_code() {
        let (host, client) = run_handshake("0000", JoinRole::Player).await;
        assert!(matches!(host, Err(NetError::Rejected(_))));
        assert!(matches!(client, Err(NetError::Rejected(_))));
    }

    /// A full room rejects the newcomer right after its `Hello`, with the reason intact.
    #[tokio::test]
    async fn full_room_rejects_newcomer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let host = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            reject_client(&mut stream, "房间已满").await.unwrap();
        });
        let Err(err) =
            client_connect(addr, "latecomer", JoinRole::Player, || "1234".to_string()).await
        else {
            panic!("expected the full room to reject the newcomer");
        };
        assert!(matches!(err, NetError::Rejected(reason) if reason == "房间已满"));
        host.await.unwrap();
    }
}
