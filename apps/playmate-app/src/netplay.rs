//! Network session for a room, bridging host/client background tasks with the UI.
//!
//! Thread and task model:
//! - Network tasks run on the tokio runtime, with [`RoomHandle`] owning command channels.
//! - The egui main thread sends commands through `cmd_tx` and drains `event_rx` each frame.
//! - Dropping [`RoomHandle`] closes the command channel and tells the task to disconnect.
//!
//! After a game starts, the task enters a **game loop**:
//! - Host: receives frames and audio through [`NetSink`](crate::emu::NetSink),
//!   sends delta/LZ4 video and i16 PCM audio, and writes `Input` into the remote slot.
//! - Client: decodes video into a shared frame buffer, pushes audio into the
//!   ring buffer, and sends local input changes back through `Input`.

use std::sync::atomic::Ordering;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use playmate_core::{FRAME_BYTES, Player};
use playmate_net::codec::{FrameDecoder, FrameEncoder, f32_to_i16_bytes, i16_bytes_to_f32};
use playmate_net::{ClientSession, Message, NetError, host_wait_for_peer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::time::timeout;

use crate::audio::AudioRing;
use crate::emu::SharedState;

/// Idle read timeout after which the peer is considered disconnected.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Keepalive interval used while idle and during client gameplay.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);

/// Commands from the UI to the network task.
pub enum RoomCmd {
    /// Requests a P1/P2 slot swap.
    SwapSlots,
    /// Host command to start a game with emulation media and shared state.
    StartGame {
        /// Game title displayed by the client.
        title: String,
        /// Host audio sample rate.
        sample_rate: u32,
        /// Raw RGBA frames produced by emulation.
        frame_rx: tokio_mpsc::Receiver<Vec<u8>>,
        /// Audio samples produced by emulation.
        audio_rx: tokio_mpsc::UnboundedReceiver<Vec<f32>>,
        /// Shared state used to write remote-player input.
        shared: Arc<SharedState>,
        /// Slot occupied by the remote player.
        remote_slot: Player,
    },
    /// Client command carrying a changed local input bitmap.
    Input(u8),
}

/// Events from the network task to the UI.
pub enum RoomEvent {
    /// A client joined the host.
    PeerJoined {
        /// Client display name.
        name: String,
    },
    /// The client connected to the room.
    Connected {
        /// Room display name.
        room_name: String,
    },
    /// The local player's slot changed.
    MySlot(Player),
    /// The host started a game; shared media buffers let the UI create the gameplay view.
    GameStarted {
        /// Game name.
        rom_name: String,
        /// Host sample rate used to configure client audio.
        sample_rate: u32,
        /// Latest RGBA frame continuously updated by the network task.
        framebuffer: Arc<Mutex<Vec<u8>>>,
        /// Audio samples continuously updated by the network task.
        ring: Arc<AudioRing>,
    },
    /// The host ended the game; both sides return to the room without disconnecting.
    GameEnded,
    /// The client disconnected and is attempting automatic reconnection.
    Reconnecting {
        /// One-based attempt number.
        attempt: u32,
    },
    /// Automatic reconnection succeeded; `GameStarted` follows if a game is still active.
    Reconnected,
    /// The peer left or disconnected; the host continues waiting for another player.
    PeerLeft,
    /// Session failure with a human-readable connection, rejection, or retry reason.
    Failed(String),
}

/// UI-owned room session handle; dropping it leaves the room.
pub struct RoomHandle {
    /// Sends commands to the network task.
    pub cmd_tx: tokio_mpsc::UnboundedSender<RoomCmd>,
    /// Receives network events drained by the UI each frame.
    pub event_rx: std_mpsc::Receiver<RoomEvent>,
}

impl RoomHandle {
    /// Drains and returns all events available for the current frame.
    pub fn poll_events(&self) -> Vec<RoomEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.event_rx.try_recv() {
            events.push(ev);
        }
        events
    }

    /// Sends a command, ignoring it if the task has already exited.
    pub fn send(&self, cmd: RoomCmd) {
        let _ = self.cmd_tx.send(cmd);
    }
}

/// Generates a four-digit PIN when the user leaves the field empty.
/// The PIN prevents accidental joins rather than providing authentication, so
/// a nanosecond clock value modulo 10,000 is sufficient.
pub fn gen_pin() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(1_2345);
    format!("{:04}", nanos % 10_000)
}

/// Returns the local LAN IP for display, or a localized placeholder on failure.
/// Connecting a UDP socket does not send a packet, but asks the OS to select an
/// outbound interface whose local address can then be read.
pub fn local_ip_display() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "<本机IP>".to_string())
}

/// Starts the host room task and returns its UI handle.
pub fn spawn_host(
    rt: &tokio::runtime::Runtime,
    listener: TcpListener,
    pin: String,
    room_name: String,
) -> RoomHandle {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = std_mpsc::channel();
    rt.spawn(host_task(listener, pin, room_name, cmd_rx, event_tx));
    RoomHandle { cmd_tx, event_rx }
}

/// Starts the client room task and returns its UI handle.
pub fn spawn_guest(
    rt: &tokio::runtime::Runtime,
    addr: std::net::SocketAddr,
    pin: String,
    my_name: String,
) -> RoomHandle {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = std_mpsc::channel();
    rt.spawn(guest_task(addr, pin, my_name, cmd_rx, event_tx));
    RoomHandle { cmd_tx, event_rx }
}

/// Reads a message with a timeout; timeout is treated as disconnection.
async fn read_idle(stream: &mut TcpStream) -> Result<Message, NetError> {
    match timeout(IDLE_TIMEOUT, Message::read_from(stream)).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(NetError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "对端超时无响应",
        ))),
    }
}

/// Media context for an active game.
/// It belongs to the game rather than the connection, so the game survives a
/// peer disconnect and a newly paired client can rejoin it immediately.
struct GameContext {
    /// Game title.
    title: String,
    /// Host audio sample rate.
    sample_rate: u32,
    /// Raw frames from emulation.
    frame_rx: tokio_mpsc::Receiver<Vec<u8>>,
    /// Audio samples from emulation.
    audio_rx: tokio_mpsc::UnboundedReceiver<Vec<f32>>,
    /// Shared state used to write remote input.
    shared: Arc<SharedState>,
    /// Remote player slot, fixed when the game starts and retained across reconnects.
    remote_slot: Player,
}

/// Host task: wait for pairing, serve the room or game, then wait again after disconnect.
/// An active game context is retained so the next client can rejoin immediately.
async fn host_task(
    listener: TcpListener,
    pin: String,
    room_name: String,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<RoomCmd>,
    event_tx: std_mpsc::Sender<RoomEvent>,
) {
    let mut game: Option<GameContext> = None;
    loop {
        let session = tokio::select! {
            result = host_wait_for_peer(&listener, &pin, &room_name) => match result {
                Ok(s) => s,
                Err(e) => {
                    let _ = event_tx.send(RoomEvent::Failed(format!("等待玩家加入失败: {e}")));
                    return;
                }
            },
            // Exit if the UI closes the room while waiting; ignore other commands here.
            cmd = cmd_rx.recv() => match cmd {
                None => return,
                Some(_) => continue,
            },
            // With no client in an active game, drain media to prevent backlog.
            _ = drain_game_media(&mut game) => continue,
        };

        let _ = event_tx.send(RoomEvent::PeerJoined {
            name: session.peer_name.clone(),
        });

        if host_connection(session.stream, &mut cmd_rx, &event_tx, &mut game)
            .await
            .is_err()
        {
            let _ = event_tx.send(RoomEvent::PeerLeft);
        }
        if cmd_rx.is_closed() {
            return;
        }
        log::info!("returned to waiting state; ready for another player");
    }
}

/// Drains and discards game media while disconnected, clearing the context when
/// its channels close. With no active game, waits forever without competing in `select!`.
async fn drain_game_media(game: &mut Option<GameContext>) {
    match game {
        Some(ctx) => {
            tokio::select! {
                frame = ctx.frame_rx.recv() => if frame.is_none() { *game = None; },
                audio = ctx.audio_rx.recv() => if audio.is_none() { *game = None; },
            }
        }
        None => std::future::pending::<()>().await,
    }
}

/// Serves one client: synchronize slots, rejoin an active game if present, then run the room loop.
/// `Ok` means the UI left; `Err` means the peer disconnected and `game` is retained.
async fn host_connection(
    mut stream: TcpStream,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<RoomCmd>,
    event_tx: &std_mpsc::Sender<RoomEvent>,
    game: &mut Option<GameContext>,
) -> Result<(), NetError> {
    // Reuse an active game's slot assignment; otherwise default the host to P1.
    let mut host_is_p1 = match game.as_ref() {
        Some(ctx) => ctx.remote_slot == Player::Two,
        None => true,
    };
    Message::SlotState { host_is_p1 }
        .write_to(&mut stream)
        .await?;
    let _ = event_tx.send(RoomEvent::MySlot(host_slot(host_is_p1)));

    // Rejoin an active game immediately; a fresh encoder guarantees a keyframe first.
    if game.is_some() {
        run_game_on_connection(&mut stream, cmd_rx, game).await?;
        if cmd_rx.is_closed() {
            return Ok(());
        }
    }

    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    loop {
        tokio::select! {
            msg = read_idle(&mut stream) => match msg? {
                Message::SwapSlots => {
                    host_is_p1 = !host_is_p1;
                    Message::SlotState { host_is_p1 }.write_to(&mut stream).await?;
                    let _ = event_tx.send(RoomEvent::MySlot(host_slot(host_is_p1)));
                }
                Message::Ping => Message::Pong.write_to(&mut stream).await?,
                other => log::debug!("room loop ignored message: {other:?}"),
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(RoomCmd::SwapSlots) => {
                    host_is_p1 = !host_is_p1;
                    Message::SlotState { host_is_p1 }.write_to(&mut stream).await?;
                    let _ = event_tx.send(RoomEvent::MySlot(host_slot(host_is_p1)));
                }
                Some(RoomCmd::StartGame {
                    title,
                    sample_rate,
                    frame_rx,
                    audio_rx,
                    shared,
                    remote_slot,
                }) => {
                    *game = Some(GameContext {
                        title,
                        sample_rate,
                        frame_rx,
                        audio_rx,
                        shared,
                        remote_slot,
                    });
                    run_game_on_connection(&mut stream, cmd_rx, game).await?;
                    if cmd_rx.is_closed() {
                        return Ok(()); // The UI has completely left the room.
                    }
                }
                Some(RoomCmd::Input(_)) => {} // This command is client-only.
                None => return Ok(()),        // The UI left the room.
            },
            _ = heartbeat.tick() => {
                Message::Ping.write_to(&mut stream).await?;
            }
        }
    }
}

/// Runs a game on the current connection: send `GameStart`, enter the game loop,
/// then clear context and send `GameEnd`. An error retains context for reconnect.
async fn run_game_on_connection(
    stream: &mut TcpStream,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<RoomCmd>,
    game: &mut Option<GameContext>,
) -> Result<(), NetError> {
    let Some(ctx) = game.as_mut() else {
        return Ok(());
    };
    Message::GameStart {
        rom_name: ctx.title.clone(),
        sample_rate: ctx.sample_rate,
    }
    .write_to(stream)
    .await?;
    host_game_loop(stream, cmd_rx, ctx).await?;
    // A normal return means the host ended the game; clear it and return the client to the room.
    *game = None;
    if !cmd_rx.is_closed() {
        Message::GameEnd.write_to(stream).await?;
    }
    Ok(())
}

/// Host game loop forwarding media and receiving remote input.
async fn host_game_loop(
    stream: &mut TcpStream,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<RoomCmd>,
    ctx: &mut GameContext,
) -> Result<(), NetError> {
    let mut encoder = FrameEncoder::new();
    let mut seq: u32 = 0;
    loop {
        tokio::select! {
            frame = ctx.frame_rx.recv() => match frame {
                Some(fb) => {
                    let (keyframe, data) = encoder.encode(&fb);
                    Message::Frame { seq, keyframe, data }.write_to(stream).await?;
                    seq = seq.wrapping_add(1);
                }
                // The emulation session ended because the host left the game.
                None => return Ok(()),
            },
            audio = ctx.audio_rx.recv() => match audio {
                Some(samples) => {
                    if !samples.is_empty() {
                        Message::AudioChunk { data: f32_to_i16_bytes(&samples) }
                            .write_to(stream)
                            .await?;
                    }
                }
                None => return Ok(()),
            },
            msg = read_idle(stream) => match msg? {
                Message::Input { buttons } => {
                    let cell = match ctx.remote_slot {
                        Player::One => &ctx.shared.p1_buttons,
                        Player::Two => &ctx.shared.p2_buttons,
                    };
                    cell.store(buttons, Ordering::Relaxed);
                }
                Message::Ping => Message::Pong.write_to(stream).await?,
                other => log::debug!("game loop ignored message: {other:?}"),
            },
            cmd = cmd_rx.recv() => {
                // `None` means the UI left; other commands are irrelevant during gameplay.
                if cmd.is_none() {
                    return Ok(());
                }
            },
        }
    }
}

/// Returns the host's slot under the current assignment.
fn host_slot(host_is_p1: bool) -> Player {
    if host_is_p1 { Player::One } else { Player::Two }
}

/// Maximum number of automatic reconnection attempts.
const RECONNECT_MAX_ATTEMPTS: u32 = 6;

/// Maximum automatic reconnection backoff.
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(8);

/// Client task: make the initial connection, serve it, and reconnect automatically after loss.
/// Initial failures are not retried because invalid PINs and unavailable hosts need quick feedback.
async fn guest_task(
    addr: std::net::SocketAddr,
    pin: String,
    my_name: String,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<RoomCmd>,
    event_tx: std_mpsc::Sender<RoomEvent>,
) {
    let first_pin = pin.clone();
    let mut session = match playmate_net::client_connect(addr, &my_name, move || first_pin).await {
        Ok(s) => s,
        Err(NetError::Rejected(reason)) => {
            let _ = event_tx.send(RoomEvent::Failed(format!("加入被拒绝: {reason}")));
            return;
        }
        Err(e) => {
            let _ = event_tx.send(RoomEvent::Failed(format!("连接失败: {e}")));
            return;
        }
    };
    let _ = event_tx.send(RoomEvent::Connected {
        room_name: session.rom_name.clone(),
    });

    loop {
        // Serve the current connection until the UI leaves or the connection fails.
        match guest_connection(session.stream, &mut cmd_rx, &event_tx).await {
            Ok(()) => return,
            Err(e) => log::warn!("disconnected from host: {e}; starting automatic reconnect"),
        }
        // End the task when reconnect gives up because of limits, rejection, or UI exit.
        match auto_reconnect(addr, &pin, &my_name, &mut cmd_rx, &event_tx).await {
            Some(new_session) => {
                session = new_session;
                let _ = event_tx.send(RoomEvent::Reconnected);
                log::info!("automatic reconnect succeeded");
            }
            None => return,
        }
    }
}

/// Reconnects with exponential backoff: 1s, 2s, 4s, 8s, and so on.
/// Returns a new session on success or `None` after rejection, exhaustion, or UI exit.
async fn auto_reconnect(
    addr: std::net::SocketAddr,
    pin: &str,
    my_name: &str,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<RoomCmd>,
    event_tx: &std_mpsc::Sender<RoomEvent>,
) -> Option<ClientSession> {
    let mut delay = Duration::from_secs(1);
    for attempt in 1..=RECONNECT_MAX_ATTEMPTS {
        let _ = event_tx.send(RoomEvent::Reconnecting { attempt });

        // Wait through the backoff, discarding commands and stopping if the UI leaves.
        let deadline = tokio::time::Instant::now() + delay;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                // Channel closure means the UI left; discard all other commands.
                cmd = cmd_rx.recv() => {
                    cmd.as_ref()?;
                }
            }
        }

        let retry_pin = pin.to_string();
        match playmate_net::client_connect(addr, my_name, move || retry_pin).await {
            Ok(session) => return Some(session),
            // Explicit rejection, such as a changed PIN, makes further retries pointless.
            Err(NetError::Rejected(reason)) => {
                let _ = event_tx.send(RoomEvent::Failed(format!("重连被拒绝: {reason}")));
                return None;
            }
            Err(e) => {
                log::warn!("reconnect attempt {attempt} failed: {e}");
                delay = (delay * 2).min(RECONNECT_MAX_DELAY);
            }
        }
    }
    let _ = event_tx.send(RoomEvent::Failed(
        "自动重连失败，请回大厅手动重新加入".to_string(),
    ));
    None
}

/// Serves an established connection with a room loop containing the game loop.
/// `Ok` means the UI left; `Err` delegates disconnection to automatic reconnect.
async fn guest_connection(
    mut stream: TcpStream,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<RoomCmd>,
    event_tx: &std_mpsc::Sender<RoomEvent>,
) -> Result<(), NetError> {
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    loop {
        tokio::select! {
            msg = read_idle(&mut stream) => match msg? {
                Message::SlotState { host_is_p1 } => {
                    // The client occupies the slot opposite the host.
                    let my_slot = if host_is_p1 { Player::Two } else { Player::One };
                    let _ = event_tx.send(RoomEvent::MySlot(my_slot));
                }
                Message::GameStart { rom_name, sample_rate } => {
                    // Create shared buffers and enter the game loop immediately to catch the keyframe.
                    let framebuffer = Arc::new(Mutex::new(vec![0u8; FRAME_BYTES]));
                    let ring = Arc::new(AudioRing::new());
                    let _ = event_tx.send(RoomEvent::GameStarted {
                        rom_name,
                        sample_rate,
                        framebuffer: Arc::clone(&framebuffer),
                        ring: Arc::clone(&ring),
                    });
                    match guest_game_loop(&mut stream, cmd_rx, &framebuffer, &ring).await? {
                        GuestGameEnd::UiLeft => return Ok(()),
                        GuestGameEnd::HostEnded => {
                            // The host ended the game; return to the room loop on the same connection.
                            let _ = event_tx.send(RoomEvent::GameEnded);
                        }
                    }
                }
                Message::Ping => Message::Pong.write_to(&mut stream).await?,
                other => log::debug!("room loop ignored message: {other:?}"),
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(RoomCmd::SwapSlots) => {
                    Message::SwapSlots.write_to(&mut stream).await?;
                }
                Some(_) => {}          // Other commands are irrelevant while the room is idle.
                None => return Ok(()), // The UI left the room.
            },
            _ = heartbeat.tick() => {
                Message::Ping.write_to(&mut stream).await?;
            }
        }
    }
}

/// Ways the client game loop can end.
enum GuestGameEnd {
    /// The UI left the room.
    UiLeft,
    /// The host ended the game; keep the connection and return to the room loop.
    HostEnded,
}

/// Client game loop that decodes media into shared buffers and sends local input.
/// Errors indicate host disconnection or a protocol failure.
async fn guest_game_loop(
    stream: &mut TcpStream,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<RoomCmd>,
    framebuffer: &Arc<Mutex<Vec<u8>>>,
    ring: &Arc<AudioRing>,
) -> Result<GuestGameEnd, NetError> {
    let mut decoder = FrameDecoder::new();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    loop {
        tokio::select! {
            msg = read_idle(stream) => match msg? {
                Message::Frame { keyframe, data, .. } => {
                    let fb = decoder.decode(keyframe, &data)?;
                    if let Ok(mut lock) = framebuffer.lock()
                        && lock.len() == fb.len()
                    {
                        lock.copy_from_slice(fb);
                    }
                }
                Message::AudioChunk { data } => {
                    ring.push(&i16_bytes_to_f32(&data));
                }
                Message::GameEnd => return Ok(GuestGameEnd::HostEnded),
                Message::Ping => Message::Pong.write_to(stream).await?,
                other => log::debug!("game loop ignored message: {other:?}"),
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(RoomCmd::Input(buttons)) => {
                    Message::Input { buttons }.write_to(stream).await?;
                }
                Some(_) => {}
                None => return Ok(GuestGameEnd::UiLeft),
            },
            _ = heartbeat.tick() => {
                Message::Ping.write_to(stream).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generated PINs contain exactly four digits.
    #[test]
    fn pin_is_four_digits() {
        let pin = gen_pin();
        assert_eq!(pin.len(), 4);
        assert!(pin.chars().all(|c| c.is_ascii_digit()));
    }
}
