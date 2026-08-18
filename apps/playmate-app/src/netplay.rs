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
use std::time::{Duration, Instant};

use playmate_core::{FRAME_BYTES, Player};
use playmate_net::codec::{FrameDecoder, FrameEncoder, f32_to_i16_bytes, i16_bytes_to_f32};
use playmate_net::{ClientSession, JoinRole, Message, MessageReader, NetError, pair_with_client};
use tokio::io::AsyncRead;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
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
    /// Asks the peer for a P1/P2 slot swap.
    RequestSwap,
    /// Answers the peer's pending swap request.
    RespondSwap(bool),
    /// Spectator asks to take the player seat.
    RequestSeat,
    /// Answers the pending seat-change request (seated player or host).
    RespondSeat(bool),
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
    /// Round trip to the host measured by the heartbeat (guest side).
    Latency {
        /// Milliseconds between sending `Ping` and receiving `Pong`.
        rtt_ms: u32,
    },
    /// A member's round trip measured by the broadcast heartbeat (host side).
    MemberLatency {
        /// Member display name as shown in the roster.
        name: String,
        /// Milliseconds between the broadcast `Ping` and this member's `Pong`.
        rtt_ms: u32,
    },
    /// The peer asked for a slot swap and awaits the local user's answer.
    SwapRequested,
    /// The peer declined the local user's swap request.
    SwapDeclined,
    /// Room membership changed; carries the full roster for display.
    Roster {
        /// Guest player seat display name, when occupied.
        player: Option<String>,
        /// Connected spectator names.
        spectators: Vec<String>,
    },
    /// A spectator asked to take the player seat; awaits the local user's answer.
    SeatRequested {
        /// Requesting spectator's display name.
        spectator: String,
    },
    /// The seat-change request of the local spectator was declined.
    SeatDeclined,
    /// The local user's own role changed after an approved seat change.
    RoleChanged {
        /// Whether the local user is now a spectator.
        is_spectator: bool,
    },
    /// The peer left or disconnected; the host continues waiting for another player.
    PeerLeft,
    /// Session failure with a human-readable connection, rejection, or retry reason.
    Failed(String),
}

/// Which side initiated the swap negotiation currently in flight.
enum SwapInitiator {
    /// The local UI asked and awaits the peer's answer.
    Local,
    /// The peer asked and awaits the local UI's answer.
    Remote,
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

/// Starts the client room task for the given role and returns its UI handle.
pub fn spawn_guest(
    rt: &tokio::runtime::Runtime,
    addr: std::net::SocketAddr,
    pin: String,
    my_name: String,
    role: JoinRole,
) -> RoomHandle {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = std_mpsc::channel();
    rt.spawn(guest_task(addr, pin, my_name, role, cmd_rx, event_tx));
    RoomHandle { cmd_tx, event_rx }
}

/// Error representing a peer that stopped responding.
fn idle_timeout_error() -> NetError {
    NetError::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "对端超时无响应",
    ))
}

/// Reads a message with a timeout; timeout is treated as disconnection.
///
/// Only for dedicated reader tasks that loop on this call alone: the timeout
/// restarts on every call, so racing it inside a `select!` would let other
/// arms (heartbeat, commands) reset it before it can ever fire. Select loops
/// must use [`read_alive`] plus an explicit liveness check instead.
async fn read_idle(
    reader: &mut MessageReader,
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<Message, NetError> {
    match timeout(IDLE_TIMEOUT, reader.next(stream)).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(idle_timeout_error()),
    }
}

/// Reads the next message and stamps the liveness clock on success.
/// Cancel-safe like `MessageReader::next`; the stamp only happens when this
/// future completes, i.e. when its `select!` arm wins.
async fn read_alive(
    reader: &mut MessageReader,
    stream: &mut (impl AsyncRead + Unpin),
    last_recv: &mut Instant,
) -> Result<Message, NetError> {
    let msg = reader.next(stream).await?;
    *last_recv = Instant::now();
    Ok(msg)
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

/// Returns the shared input cell for the remote player's slot.
fn remote_buttons_cell(ctx: &GameContext) -> &std::sync::atomic::AtomicU8 {
    match ctx.remote_slot {
        Player::One => &ctx.shared.p1_buttons,
        Player::Two => &ctx.shared.p2_buttons,
    }
}

/// Maximum simultaneous spectators.
const SPECTATOR_MAX: usize = 4;

/// Per-connection media queue length; overflow drops frames for that peer only.
const MEDIA_QUEUE: usize = 8;

/// Host task: accept and pair connections concurrently, then serve the room
/// and any active game to every registered connection from one select loop.
/// An active game survives a player disconnect so the next player can rejoin.
async fn host_task(
    listener: TcpListener,
    pin: String,
    room_name: String,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<RoomCmd>,
    event_tx: std_mpsc::Sender<RoomEvent>,
) {
    let seats = Arc::new(Mutex::new(Seats::default()));
    let (joined_tx, mut joined_rx) = tokio_mpsc::unbounded_channel::<NewPeer>();
    let (in_tx, mut in_rx) = tokio_mpsc::unbounded_channel::<ConnIn>();
    let mut game: Option<GameContext> = None;
    let mut room = HostRoom {
        conns: Vec::new(),
        next_id: 0,
        host_is_p1: true,
        swap_pending: None,
        seat_pending: None,
        event_tx,
        in_tx,
    };
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    // Broadcast time of the last heartbeat Ping, for per-member round trips.
    let mut ping_sent = Instant::now();

    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, addr)) => spawn_pairing(stream, addr, &pin, &room_name, &seats, &joined_tx),
                Err(e) => {
                    let _ = room.event_tx.send(RoomEvent::Failed(format!("等待玩家加入失败: {e}")));
                    return;
                }
            },
            Some(peer) = joined_rx.recv() => room.register(peer, game.as_ref()),
            Some(input) = in_rx.recv() => match input {
                ConnIn::Msg(id, Message::Pong) => room.on_pong(id, ping_sent),
                ConnIn::Msg(id, msg) => room.on_message(id, msg, game.as_ref(), &seats),
                ConnIn::Closed(id) => room.remove(id, &seats, game.as_ref()),
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(cmd) => room.on_command(cmd, &mut game, &seats),
                // The UI left the room; dropping the connections closes every peer.
                None => return,
            },
            media = next_media(&mut game) => match media {
                GameMedia::Frame(fb) => room.fan_out(MediaMsg::Frame(Arc::new(fb))),
                GameMedia::Audio(samples) => {
                    if !samples.is_empty() {
                        room.fan_out(MediaMsg::Audio(Arc::new(f32_to_i16_bytes(&samples))));
                    }
                }
                // The host closed the game session; return everyone to the room.
                GameMedia::Ended => {
                    game = None;
                    room.broadcast(Message::GameEnd);
                }
            },
            _ = heartbeat.tick() => {
                ping_sent = Instant::now();
                room.broadcast(Message::Ping);
            }
        }
    }
}

/// Seats claimed during pairing, shared with concurrent handshake tasks.
#[derive(Default)]
struct Seats {
    /// Whether the guest player seat is taken.
    player: bool,
    /// Number of connected spectators.
    spectators: usize,
}

/// Releases a claimed seat.
fn release_seat(seats: &Arc<Mutex<Seats>>, role: JoinRole) {
    if let Ok(mut s) = seats.lock() {
        match role {
            JoinRole::Player => s.player = false,
            JoinRole::Spectator => s.spectators = s.spectators.saturating_sub(1),
        }
    }
}

/// A paired connection handed from a handshake task to the host loop.
struct NewPeer {
    /// Established connection right after `Welcome`.
    stream: TcpStream,
    /// Peer display name.
    name: String,
    /// Seat granted during pairing.
    role: JoinRole,
}

/// Inbound traffic from one connection's reader task.
enum ConnIn {
    /// A decoded message from the peer.
    Msg(u64, Message),
    /// The connection failed, timed out, or closed.
    Closed(u64),
}

/// Outbound control command for one connection's writer task.
enum WriterCtrl {
    /// Reliable, in-order message.
    Send(Message),
    /// Resets the frame encoder so the next frame is a keyframe (new game).
    ResetEncoder,
}

/// Droppable media payload fanned out to every connection.
#[derive(Clone)]
enum MediaMsg {
    /// Raw RGBA frame; encoded per connection to keep every delta base consistent.
    Frame(Arc<Vec<u8>>),
    /// Mono i16 PCM chunk shared verbatim.
    Audio(Arc<Vec<u8>>),
}

/// Media pulled from the active game, or the end of the game session.
enum GameMedia {
    /// One raw RGBA frame.
    Frame(Vec<u8>),
    /// One chunk of f32 samples.
    Audio(Vec<f32>),
    /// The emulation session closed its channels.
    Ended,
}

/// Yields the next media payload of the active game, or pends forever without one.
async fn next_media(game: &mut Option<GameContext>) -> GameMedia {
    match game {
        Some(ctx) => tokio::select! {
            frame = ctx.frame_rx.recv() => match frame {
                Some(fb) => GameMedia::Frame(fb),
                None => GameMedia::Ended,
            },
            audio = ctx.audio_rx.recv() => match audio {
                Some(samples) => GameMedia::Audio(samples),
                None => GameMedia::Ended,
            },
        },
        None => std::future::pending().await,
    }
}

/// Spawns a handshake task for one accepted connection. The seat is claimed
/// atomically inside the pairing exchange, so concurrent joiners cannot
/// overshoot the capacity, and a rejected joiner gets the reason immediately.
fn spawn_pairing(
    mut stream: TcpStream,
    addr: std::net::SocketAddr,
    pin: &str,
    room_name: &str,
    seats: &Arc<Mutex<Seats>>,
    joined_tx: &tokio_mpsc::UnboundedSender<NewPeer>,
) {
    let pin = pin.to_owned();
    let room_name = room_name.to_owned();
    let seats = Arc::clone(seats);
    let joined_tx = joined_tx.clone();
    tokio::spawn(async move {
        // Records the seat claimed inside the closure so every failure path
        // after the claim can give it back.
        let claimed = Arc::new(Mutex::new(None::<JoinRole>));
        let claimed_in = Arc::clone(&claimed);
        let check_seats = Arc::clone(&seats);
        let result = pair_with_client(&mut stream, &pin, &room_name, move |role| {
            let Ok(mut s) = check_seats.lock() else {
                return Err("主机内部错误".to_string());
            };
            match role {
                JoinRole::Player if s.player => Err("玩家位已满，可选择观战加入".to_string()),
                JoinRole::Spectator if s.spectators >= SPECTATOR_MAX => {
                    Err("观众位已满".to_string())
                }
                JoinRole::Player => {
                    s.player = true;
                    if let Ok(mut c) = claimed_in.lock() {
                        *c = Some(role);
                    }
                    Ok(())
                }
                JoinRole::Spectator => {
                    s.spectators += 1;
                    if let Ok(mut c) = claimed_in.lock() {
                        *c = Some(role);
                    }
                    Ok(())
                }
            }
        })
        .await;
        match result {
            Ok((name, role)) => {
                log::info!("pairing succeeded: {name} ({addr}, {role:?})");
                if joined_tx.send(NewPeer { stream, name, role }).is_err() {
                    // The host loop is gone; give the claimed seat back.
                    release_seat(&seats, role);
                }
            }
            Err(e) => {
                if let Ok(c) = claimed.lock()
                    && let Some(role) = *c
                {
                    release_seat(&seats, role);
                }
                log::info!("pairing failed ({addr}): {e}");
            }
        }
    });
}

/// Reader task: forwards decoded messages to the host loop until the
/// connection errors or times out, then reports the close.
async fn conn_reader(
    mut read_half: OwnedReadHalf,
    id: u64,
    in_tx: tokio_mpsc::UnboundedSender<ConnIn>,
) {
    let mut reader = MessageReader::new();
    loop {
        match read_idle(&mut reader, &mut read_half).await {
            Ok(msg) => {
                if in_tx.send(ConnIn::Msg(id, msg)).is_err() {
                    return; // The host loop exited.
                }
            }
            Err(_) => {
                let _ = in_tx.send(ConnIn::Closed(id));
                return;
            }
        }
    }
}

/// Writer task: serializes one connection's outbound traffic. Control messages
/// take priority over media, and frames are encoded here so every connection
/// keeps a consistent delta base even when its lagging queue drops frames.
async fn conn_writer(
    mut write_half: OwnedWriteHalf,
    mut ctrl_rx: tokio_mpsc::UnboundedReceiver<WriterCtrl>,
    mut media_rx: tokio_mpsc::Receiver<MediaMsg>,
) {
    let mut encoder = FrameEncoder::new();
    let mut seq: u32 = 0;
    loop {
        let write_result = tokio::select! {
            biased;
            ctrl = ctrl_rx.recv() => match ctrl {
                Some(WriterCtrl::Send(msg)) => msg.write_to(&mut write_half).await,
                Some(WriterCtrl::ResetEncoder) => {
                    encoder = FrameEncoder::new();
                    seq = 0;
                    Ok(())
                }
                None => return, // Deregistered by the host loop.
            },
            media = media_rx.recv() => match media {
                Some(MediaMsg::Frame(fb)) => {
                    let (keyframe, data) = encoder.encode(&fb);
                    let result = Message::Frame { seq, keyframe, data }.write_to(&mut write_half).await;
                    seq = seq.wrapping_add(1);
                    result
                }
                Some(MediaMsg::Audio(bytes)) => {
                    Message::AudioChunk { data: bytes.to_vec() }.write_to(&mut write_half).await
                }
                None => return,
            },
        };
        if write_result.is_err() {
            return; // The reader reports the close; just stop writing.
        }
    }
}

/// One registered connection served by dedicated reader and writer tasks.
struct PeerConn {
    /// Registration id used to route inbound traffic.
    id: u64,
    /// Peer display name.
    name: String,
    /// Seat occupied by the peer.
    role: JoinRole,
    /// Reliable control channel consumed by the writer task.
    ctrl_tx: tokio_mpsc::UnboundedSender<WriterCtrl>,
    /// Droppable media channel consumed by the writer task.
    media_tx: tokio_mpsc::Sender<MediaMsg>,
}

/// A seat-change negotiation in flight, keyed by the requesting spectator.
struct SeatPending {
    /// Requesting spectator's connection id.
    spectator_id: u64,
    /// Requesting spectator's display name.
    spectator_name: String,
    /// Whether the seated player arbitrates (`true`) or the host does for an
    /// empty seat (`false`). Each answer path only consumes its own kind.
    via_player: bool,
}

/// Room state owned by the host loop: registered connections, slot
/// assignment, and the swap negotiation.
struct HostRoom {
    /// Registered connections (at most one player plus spectators).
    conns: Vec<PeerConn>,
    /// Monotonic id source for registrations.
    next_id: u64,
    /// Whether the host currently occupies P1.
    host_is_p1: bool,
    /// Swap negotiation in flight with the player, if any.
    swap_pending: Option<SwapInitiator>,
    /// Seat-change negotiation in flight, if any.
    seat_pending: Option<SeatPending>,
    /// Events towards the UI.
    event_tx: std_mpsc::Sender<RoomEvent>,
    /// Inbound sender handed to every reader task.
    in_tx: tokio_mpsc::UnboundedSender<ConnIn>,
}

impl HostRoom {
    /// Returns the guest player connection, if seated.
    fn player(&self) -> Option<&PeerConn> {
        self.conns.iter().find(|c| c.role == JoinRole::Player)
    }

    /// Sends a reliable message to the player connection, if seated.
    fn send_player(&self, msg: Message) {
        if let Some(player) = self.player() {
            let _ = player.ctrl_tx.send(WriterCtrl::Send(msg));
        }
    }

    /// Sends a reliable message to connection `id`, if present.
    fn send_to(&self, id: u64, msg: Message) {
        if let Some(conn) = self.conns.iter().find(|c| c.id == id) {
            let _ = conn.ctrl_tx.send(WriterCtrl::Send(msg));
        }
    }

    /// Sends a reliable message to every connection.
    fn broadcast(&self, msg: Message) {
        for conn in &self.conns {
            let _ = conn.ctrl_tx.send(WriterCtrl::Send(msg.clone()));
        }
    }

    /// Records a heartbeat answer as that member's round trip for the UI.
    fn on_pong(&self, id: u64, ping_sent: Instant) {
        if let Some(conn) = self.conns.iter().find(|c| c.id == id) {
            let _ = self.event_tx.send(RoomEvent::MemberLatency {
                name: conn.name.clone(),
                rtt_ms: u32::try_from(ping_sent.elapsed().as_millis()).unwrap_or(u32::MAX),
            });
        }
    }

    /// Sends a droppable media payload to every connection. A full queue means
    /// that peer is lagging, so the payload is dropped for it alone.
    fn fan_out(&self, msg: MediaMsg) {
        for conn in &self.conns {
            let _ = conn.media_tx.try_send(msg.clone());
        }
    }

    /// Registers a paired connection: spawns its I/O tasks, synchronizes slot
    /// and game state, and publishes the new roster.
    fn register(&mut self, peer: NewPeer, game: Option<&GameContext>) {
        self.next_id += 1;
        let id = self.next_id;
        let (read_half, write_half) = peer.stream.into_split();
        let (ctrl_tx, ctrl_rx) = tokio_mpsc::unbounded_channel();
        let (media_tx, media_rx) = tokio_mpsc::channel(MEDIA_QUEUE);
        tokio::spawn(conn_reader(read_half, id, self.in_tx.clone()));
        tokio::spawn(conn_writer(write_half, ctrl_rx, media_rx));
        let conn = PeerConn {
            id,
            name: peer.name.clone(),
            role: peer.role,
            ctrl_tx,
            media_tx,
        };

        if peer.role == JoinRole::Player {
            // Reuse an active game's slot assignment; otherwise keep the current one.
            if let Some(ctx) = game {
                self.host_is_p1 = ctx.remote_slot == Player::Two;
            }
            let _ = self
                .event_tx
                .send(RoomEvent::MySlot(host_slot(self.host_is_p1)));
            let _ = self
                .event_tx
                .send(RoomEvent::PeerJoined { name: peer.name });
        }
        // Everyone learns the seat assignment; spectators use it for display only.
        let _ = conn.ctrl_tx.send(WriterCtrl::Send(Message::SlotState {
            host_is_p1: self.host_is_p1,
        }));
        self.conns.push(conn);
        // Publish the roster before any GameStart so the joiner still handles
        // it in the room loop rather than dropping it inside the game loop.
        self.roster_changed();
        // A joiner during an active game enters it immediately; the fresh
        // writer encoder guarantees its first frame is a keyframe.
        if let Some(ctx) = game {
            self.send_to(
                id,
                Message::GameStart {
                    rom_name: ctx.title.clone(),
                    sample_rate: ctx.sample_rate,
                },
            );
        }
    }

    /// Removes a closed connection, frees its seat, and publishes the roster.
    fn remove(&mut self, id: u64, seats: &Arc<Mutex<Seats>>, game: Option<&GameContext>) {
        let Some(pos) = self.conns.iter().position(|c| c.id == id) else {
            return;
        };
        let conn = self.conns.remove(pos);
        release_seat(seats, conn.role);
        log::info!("connection closed: {} ({:?})", conn.name, conn.role);
        if conn.role == JoinRole::Player {
            self.swap_pending = None;
            // Input messages replace the whole button state, so buttons held
            // at disconnect would stay pressed in emulation forever.
            if let Some(ctx) = game {
                remote_buttons_cell(ctx).store(0, Ordering::Relaxed);
            }
            let _ = self.event_tx.send(RoomEvent::PeerLeft);
        }
        // Cancel a seat negotiation that involves the leaving connection.
        if let Some(pending) = self.seat_pending.take() {
            let spectator_left = pending.spectator_id == conn.id;
            let approver_left = conn.role == JoinRole::Player;
            if approver_left && !spectator_left {
                self.send_to(
                    pending.spectator_id,
                    Message::SeatChangeResponse { accepted: false },
                );
            }
            if !spectator_left && !approver_left {
                self.seat_pending = Some(pending); // Unrelated close; keep waiting.
            }
        }
        self.roster_changed();
    }

    /// Routes one inbound message from connection `id`.
    fn on_message(
        &mut self,
        id: u64,
        msg: Message,
        game: Option<&GameContext>,
        seats: &Arc<Mutex<Seats>>,
    ) {
        let from_player = self.player().is_some_and(|c| c.id == id);
        match msg {
            Message::Input { buttons } => {
                // Spectators never control the game.
                if from_player && let Some(ctx) = game {
                    remote_buttons_cell(ctx).store(buttons, Ordering::Relaxed);
                }
            }
            Message::SwapRequest if from_player => {
                if matches!(self.swap_pending, Some(SwapInitiator::Local)) {
                    // Both sides asked at the same time: agreement is implicit.
                    self.swap_pending = None;
                    self.send_player(Message::SwapResponse { accepted: true });
                    self.flip_slots();
                } else {
                    self.swap_pending = Some(SwapInitiator::Remote);
                    let _ = self.event_tx.send(RoomEvent::SwapRequested);
                }
            }
            Message::SwapResponse { accepted } if from_player => {
                if matches!(self.swap_pending, Some(SwapInitiator::Local)) {
                    self.swap_pending = None;
                    if accepted {
                        self.flip_slots();
                    } else {
                        let _ = self.event_tx.send(RoomEvent::SwapDeclined);
                    }
                }
            }
            Message::SeatChangeRequest { .. } if !from_player => {
                self.on_seat_request(id, game.is_some());
            }
            Message::SeatChangeResponse { accepted } if from_player => {
                // Only consume a negotiation this player actually arbitrates.
                if let Some(pending) = self.seat_pending.take_if(|p| p.via_player) {
                    self.finish_seat_change(pending, accepted, true, seats);
                }
            }
            Message::Ping => self.send_to(id, Message::Pong),
            other => log::debug!("host loop ignored message: {other:?}"),
        }
    }

    /// Handles a spectator's request to take the player seat: the seated
    /// player decides, or the host when the seat is empty.
    fn on_seat_request(&mut self, id: u64, game_active: bool) {
        let Some(name) = self
            .conns
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
        else {
            return;
        };
        // One negotiation at a time, and never while a game is running.
        if game_active || self.seat_pending.is_some() {
            self.send_to(id, Message::SeatChangeResponse { accepted: false });
            return;
        }
        let via_player = self.player().is_some();
        self.seat_pending = Some(SeatPending {
            spectator_id: id,
            spectator_name: name.clone(),
            via_player,
        });
        if via_player {
            self.send_player(Message::SeatChangeRequest { spectator: name });
        } else {
            let _ = self
                .event_tx
                .send(RoomEvent::SeatRequested { spectator: name });
        }
    }

    /// Applies the outcome of a seat-change negotiation. `via_player` marks
    /// the player-approved path; the host approves only an empty seat.
    fn finish_seat_change(
        &mut self,
        pending: SeatPending,
        accepted: bool,
        via_player: bool,
        seats: &Arc<Mutex<Seats>>,
    ) {
        let spectator_alive = self.conns.iter().any(|c| c.id == pending.spectator_id);
        // A player may have grabbed the seat while the host was deciding.
        let seat_raced = !via_player && self.player().is_some();
        if !accepted || !spectator_alive || seat_raced {
            self.send_to(
                pending.spectator_id,
                Message::SeatChangeResponse { accepted: false },
            );
            return;
        }
        // Demote the seated player (present only on the player-approved path).
        let demoted = self.player().map(|c| c.id);
        if let Some(pid) = demoted {
            if let Some(p) = self.conns.iter_mut().find(|c| c.id == pid) {
                p.role = JoinRole::Spectator;
            }
            self.send_to(
                pid,
                Message::RoleChanged {
                    role: JoinRole::Spectator,
                },
            );
        }
        // Promote the spectator; the slot mapping itself is unchanged.
        if let Some(s) = self.conns.iter_mut().find(|c| c.id == pending.spectator_id) {
            s.role = JoinRole::Player;
        }
        self.send_to(
            pending.spectator_id,
            Message::RoleChanged {
                role: JoinRole::Player,
            },
        );
        // Recompute claimed seats from the connection list.
        if let Ok(mut s) = seats.lock() {
            s.player = true;
            s.spectators = self
                .conns
                .iter()
                .filter(|c| c.role == JoinRole::Spectator)
                .count();
        }
        self.swap_pending = None;
        let _ = self.event_tx.send(RoomEvent::PeerJoined {
            name: pending.spectator_name,
        });
        self.roster_changed();
    }

    /// Applies one UI command.
    fn on_command(
        &mut self,
        cmd: RoomCmd,
        game: &mut Option<GameContext>,
        seats: &Arc<Mutex<Seats>>,
    ) {
        match cmd {
            RoomCmd::RespondSeat(accepted) => {
                // Empty-seat promotion arbitrated by the host. Always consume
                // the host-arbitrated pending state — gating on the seat still
                // being empty would leave it stuck forever when a new player
                // joined mid-decision; finish_seat_change already declines
                // that race via its seat_raced check.
                if let Some(pending) = self.seat_pending.take_if(|p| !p.via_player) {
                    self.finish_seat_change(pending, accepted, false, seats);
                }
            }
            RoomCmd::RequestSeat => {} // Client-only command.
            RoomCmd::RequestSwap => {
                // Ignored while another negotiation is in flight or without a player.
                if self.swap_pending.is_none() && self.player().is_some() {
                    self.swap_pending = Some(SwapInitiator::Local);
                    self.send_player(Message::SwapRequest);
                }
            }
            RoomCmd::RespondSwap(accepted) => {
                if matches!(self.swap_pending, Some(SwapInitiator::Remote)) {
                    self.swap_pending = None;
                    // Answer first so the client clears its pending state
                    // before the new SlotState lands.
                    self.send_player(Message::SwapResponse { accepted });
                    if accepted {
                        self.flip_slots();
                    }
                }
            }
            RoomCmd::StartGame {
                title,
                sample_rate,
                frame_rx,
                audio_rx,
                shared,
                remote_slot,
            } => {
                // Gameplay freezes negotiation UI on every side, so resolve
                // in-flight negotiations before switching to the game.
                self.swap_pending = None;
                if let Some(pending) = self.seat_pending.take() {
                    let via_player = pending.via_player;
                    self.finish_seat_change(pending, false, via_player, seats);
                }
                *game = Some(GameContext {
                    title: title.clone(),
                    sample_rate,
                    frame_rx,
                    audio_rx,
                    shared,
                    remote_slot,
                });
                for conn in &self.conns {
                    let _ = conn.ctrl_tx.send(WriterCtrl::Send(Message::GameStart {
                        rom_name: title.clone(),
                        sample_rate,
                    }));
                    // Keyframe boundary between two consecutive games.
                    let _ = conn.ctrl_tx.send(WriterCtrl::ResetEncoder);
                }
            }
            RoomCmd::Input(_) => {} // Client-only command.
        }
    }

    /// Flips P1/P2, informs every connection, and updates the local UI.
    fn flip_slots(&mut self) {
        self.host_is_p1 = !self.host_is_p1;
        self.broadcast(Message::SlotState {
            host_is_p1: self.host_is_p1,
        });
        let _ = self
            .event_tx
            .send(RoomEvent::MySlot(host_slot(self.host_is_p1)));
    }

    /// Broadcasts the roster to every peer and mirrors it to the local UI.
    fn roster_changed(&self) {
        let player = self.player().map(|c| c.name.clone());
        let spectators: Vec<String> = self
            .conns
            .iter()
            .filter(|c| c.role == JoinRole::Spectator)
            .map(|c| c.name.clone())
            .collect();
        self.broadcast(Message::Roster {
            player: player.clone(),
            spectators: spectators.clone(),
        });
        let _ = self.event_tx.send(RoomEvent::Roster { player, spectators });
    }
}

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
    role: JoinRole,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<RoomCmd>,
    event_tx: std_mpsc::Sender<RoomEvent>,
) {
    let first_pin = pin.clone();
    let mut session =
        match playmate_net::client_connect(addr, &my_name, role, move || first_pin).await {
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

    // Mutable because an approved seat change updates the reconnect role.
    let mut role = role;
    loop {
        // Serve the current connection until the UI leaves or the connection fails.
        match guest_connection(session.stream, &mut cmd_rx, &event_tx, &mut role).await {
            Ok(()) => return,
            Err(e) => log::warn!("disconnected from host: {e}; starting automatic reconnect"),
        }
        // End the task when reconnect gives up because of limits, rejection, or UI exit.
        match auto_reconnect(addr, &pin, &my_name, role, &mut cmd_rx, &event_tx).await {
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
    role: JoinRole,
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
        match playmate_net::client_connect(addr, my_name, role, move || retry_pin).await {
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
    role: &mut JoinRole,
) -> Result<(), NetError> {
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    // The game loop can occupy an arm body for a long time; firing every
    // missed tick afterwards would burst-send stale pings.
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut swap_pending: Option<SwapInitiator> = None;
    // One reader for the connection's whole lifetime: its buffer may hold a
    // prefix of the next message, so the game loop shares it too.
    let mut reader = MessageReader::new();
    let mut last_recv = Instant::now();
    // Send time of the last heartbeat Ping, for round-trip measurement.
    let mut ping_sent: Option<Instant> = None;
    loop {
        tokio::select! {
            msg = read_alive(&mut reader, &mut stream, &mut last_recv) => match msg? {
                Message::SlotState { host_is_p1 } => {
                    // The client occupies the slot opposite the host.
                    let my_slot = if host_is_p1 { Player::Two } else { Player::One };
                    let _ = event_tx.send(RoomEvent::MySlot(my_slot));
                }
                Message::SwapRequest => {
                    if matches!(swap_pending, Some(SwapInitiator::Local)) {
                        // Both sides asked at the same time: agree and let the
                        // host flip the slots and broadcast SlotState.
                        swap_pending = None;
                        Message::SwapResponse { accepted: true }.write_to(&mut stream).await?;
                    } else {
                        swap_pending = Some(SwapInitiator::Remote);
                        let _ = event_tx.send(RoomEvent::SwapRequested);
                    }
                }
                Message::SwapResponse { accepted } => {
                    if matches!(swap_pending, Some(SwapInitiator::Local)) {
                        swap_pending = None;
                        // On acceptance the host flips and broadcasts SlotState next.
                        if !accepted {
                            let _ = event_tx.send(RoomEvent::SwapDeclined);
                        }
                    }
                }
                Message::Roster { player, spectators } => {
                    let _ = event_tx.send(RoomEvent::Roster { player, spectators });
                }
                Message::SeatChangeRequest { spectator } => {
                    let _ = event_tx.send(RoomEvent::SeatRequested { spectator });
                }
                Message::SeatChangeResponse { accepted } => {
                    if !accepted {
                        let _ = event_tx.send(RoomEvent::SeatDeclined);
                    }
                }
                Message::RoleChanged { role: new_role } => {
                    // Keep the reconnect role in sync with the granted seat.
                    *role = new_role;
                    let _ = event_tx.send(RoomEvent::RoleChanged {
                        is_spectator: new_role == JoinRole::Spectator,
                    });
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
                    match guest_game_loop(
                        &mut reader,
                        &mut stream,
                        cmd_rx,
                        event_tx,
                        &framebuffer,
                        &ring,
                    )
                    .await?
                    {
                        GuestGameEnd::UiLeft => return Ok(()),
                        GuestGameEnd::HostEnded => {
                            // The host ended the game; return to the room loop
                            // on the same connection. The liveness clock is
                            // stale from before the game, so restart it.
                            last_recv = Instant::now();
                            let _ = event_tx.send(RoomEvent::GameEnded);
                        }
                    }
                }
                Message::Ping => Message::Pong.write_to(&mut stream).await?,
                Message::Pong => {
                    // Answer to our own heartbeat: surface the round trip.
                    if let Some(sent) = ping_sent.take() {
                        let _ = event_tx.send(RoomEvent::Latency {
                            rtt_ms: u32::try_from(sent.elapsed().as_millis()).unwrap_or(u32::MAX),
                        });
                    }
                }
                other => log::debug!("room loop ignored message: {other:?}"),
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(RoomCmd::RequestSwap) => {
                    // Ignored while another negotiation is in flight.
                    if swap_pending.is_none() {
                        swap_pending = Some(SwapInitiator::Local);
                        Message::SwapRequest.write_to(&mut stream).await?;
                    }
                }
                Some(RoomCmd::RespondSwap(accepted)) => {
                    if matches!(swap_pending, Some(SwapInitiator::Remote)) {
                        swap_pending = None;
                        // The host flips the slots and broadcasts SlotState on acceptance.
                        Message::SwapResponse { accepted }.write_to(&mut stream).await?;
                    }
                }
                Some(RoomCmd::RequestSeat) => {
                    // The host identifies the sender; the name field stays empty.
                    Message::SeatChangeRequest {
                        spectator: String::new(),
                    }
                    .write_to(&mut stream)
                    .await?;
                }
                Some(RoomCmd::RespondSeat(accepted)) => {
                    Message::SeatChangeResponse { accepted }
                        .write_to(&mut stream)
                        .await?;
                }
                Some(_) => {}          // Other commands are irrelevant while the room is idle.
                None => return Ok(()), // The UI left the room.
            },
            _ = heartbeat.tick() => {
                // Idle disconnection is detected here: a timeout inside the
                // read arm would be recreated (and thus reset) every time
                // another arm wins the select.
                if last_recv.elapsed() >= IDLE_TIMEOUT {
                    return Err(idle_timeout_error());
                }
                ping_sent = Some(Instant::now());
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
    reader: &mut MessageReader,
    stream: &mut TcpStream,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<RoomCmd>,
    event_tx: &std_mpsc::Sender<RoomEvent>,
    framebuffer: &Arc<Mutex<Vec<u8>>>,
    ring: &Arc<AudioRing>,
) -> Result<GuestGameEnd, NetError> {
    let mut decoder = FrameDecoder::new();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    let mut last_recv = Instant::now();
    // Send time of the last heartbeat Ping, for round-trip measurement.
    let mut ping_sent: Option<Instant> = None;
    loop {
        tokio::select! {
            msg = read_alive(reader, stream, &mut last_recv) => match msg? {
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
                Message::Roster { player, spectators } => {
                    // Membership can change mid-game (spectators joining or
                    // leaving); keep the room page state current for later.
                    let _ = event_tx.send(RoomEvent::Roster { player, spectators });
                }
                Message::GameEnd => return Ok(GuestGameEnd::HostEnded),
                Message::Ping => Message::Pong.write_to(stream).await?,
                Message::Pong => {
                    // Answer to our own heartbeat: surface the round trip.
                    if let Some(sent) = ping_sent.take() {
                        let _ = event_tx.send(RoomEvent::Latency {
                            rtt_ms: u32::try_from(sent.elapsed().as_millis()).unwrap_or(u32::MAX),
                        });
                    }
                }
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
                // See guest_connection: liveness must be checked outside the
                // read arm because select! recreates that future per iteration.
                if last_recv.elapsed() >= IDLE_TIMEOUT {
                    return Err(idle_timeout_error());
                }
                ping_sent = Some(Instant::now());
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
