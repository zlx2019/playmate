//! Room page showing members and slots, slot swapping, and host game selection.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use playmate_core::Player;

use crate::audio::AudioRing;
use crate::netplay::{RoomCmd, RoomEvent, RoomHandle};
use crate::pages::game_select::GameEntry;
use crate::theme;

/// Game-start information used by the application to create client gameplay.
pub struct StartInfo {
    /// Game name.
    pub rom_name: String,
    /// Host audio sample rate.
    pub sample_rate: u32,
    /// Frame buffer continuously updated by the network task.
    pub framebuffer: Arc<Mutex<Vec<u8>>>,
    /// Audio buffer continuously updated by the network task.
    pub ring: Arc<AudioRing>,
}

/// Room page state.
pub struct RoomState {
    /// Network session handle; dropping it leaves the room.
    pub handle: RoomHandle,
    /// Whether the local user is the host.
    pub is_host: bool,
    /// Room display name.
    pub room_name: String,
    /// PIN shown by the host; empty on clients.
    pub pin: String,
    /// Direct address shown by the host; empty on clients.
    pub addr_display: String,
    /// Peer name, or `None` before another player joins.
    pub peer: Option<String>,
    /// Local player slot.
    pub my_slot: Player,
    /// Host game list scanned when entering the room.
    pub games: Vec<GameEntry>,
    /// Host's selected game index.
    pub selected: Option<usize>,
    /// User-facing error message, rendered as a red banner.
    pub error: Option<String>,
    /// Informational status message, kept separate from `error` so routine
    /// notices (declines, role changes) never render as failures.
    pub notice: Option<String>,
    /// Latest round-trip latency to the host in milliseconds (guest side only).
    pub latency_ms: Option<u32>,
    /// Waiting for the peer to answer the local swap request.
    pub swap_outgoing: bool,
    /// Auto-decline deadline of the peer's pending swap request.
    pub swap_incoming: Option<Instant>,
    /// Whether the local user joined as a watch-only spectator.
    pub is_spectator: bool,
    /// Waiting for the seat-change request to be answered (spectator side).
    pub seat_outgoing: bool,
    /// Pending seat-change prompt: requesting spectator and auto-decline deadline.
    pub seat_incoming: Option<(String, Instant)>,
    /// Guest player seat name from the latest roster (spectator view).
    pub roster_player: Option<String>,
    /// Connected spectator names from the latest roster.
    pub spectators: Vec<String>,
    /// Host mDNS advertisement retained until this state is dropped.
    pub _announcer: Option<playmate_net::Announcer>,
}

/// How long an incoming swap request waits before it is declined automatically.
const SWAP_PROMPT_TIMEOUT: Duration = Duration::from_secs(15);

/// Action triggered by the room page.
pub enum RoomAction {
    /// No action.
    None,
    /// Leave the room and return to the lobby.
    Leave,
    /// Host starts a game using a local ROM path and title.
    StartGame(PathBuf, String),
}

/// Signals summarized by [`apply_events`] for application-level handling.
#[derive(Default)]
pub struct RoomUpdates {
    /// The host started a game.
    pub start: Option<StartInfo>,
    /// The host ended the game and the client should return to the room.
    pub game_ended: bool,
    /// Reconnection succeeded; an active game may immediately provide `start`.
    pub reconnected: bool,
    /// Terminal session failure requiring the application to leave the room.
    pub fatal: Option<String>,
    /// The local role changed (`true` = now spectator); the application keeps
    /// its quick-rejoin entry in sync with it.
    pub role_changed: Option<bool>,
}

/// Colored latency text shared by the room page and the guest game header:
/// green under 60 ms, yellow under 120 ms, red beyond.
pub fn latency_text(ms: u32) -> egui::RichText {
    let color = if ms < 60 {
        theme::GREEN
    } else if ms < 120 {
        egui::Color32::from_rgb(222, 178, 88)
    } else {
        theme::RED_BRIGHT
    };
    egui::RichText::new(format!("延迟 {ms} ms"))
        .size(12.0)
        .color(color)
}

/// Applies network events to room state and returns application-level signals.
pub fn apply_events(state: &mut RoomState) -> RoomUpdates {
    let mut updates = RoomUpdates::default();
    for event in state.handle.poll_events() {
        match event {
            RoomEvent::PeerJoined { name } => {
                state.peer = Some(name);
                state.error = None;
            }
            RoomEvent::Connected { room_name } => {
                state.room_name = room_name;
                // A connected client necessarily has the host as its peer.
                state.peer = Some("主机".to_string());
            }
            RoomEvent::MySlot(slot) => {
                state.my_slot = slot;
                // Any authoritative slot broadcast settles the negotiation.
                state.swap_outgoing = false;
                state.swap_incoming = None;
            }
            RoomEvent::SwapRequested => {
                // A crossing local request was absorbed by the network task.
                state.swap_outgoing = false;
                state.swap_incoming = Some(Instant::now() + SWAP_PROMPT_TIMEOUT);
            }
            RoomEvent::SwapDeclined => {
                state.swap_outgoing = false;
                state.notice = Some("对方拒绝了交换席位的请求".to_string());
            }
            RoomEvent::Roster { player, spectators } => {
                state.roster_player = player;
                state.spectators = spectators;
            }
            RoomEvent::SeatRequested { spectator } => {
                state.seat_incoming = Some((spectator, Instant::now() + SWAP_PROMPT_TIMEOUT));
            }
            RoomEvent::SeatDeclined => {
                state.seat_outgoing = false;
                state.notice = Some("上场请求被拒绝".to_string());
            }
            RoomEvent::RoleChanged { is_spectator } => {
                state.is_spectator = is_spectator;
                state.seat_outgoing = false;
                state.seat_incoming = None;
                state.swap_outgoing = false;
                state.swap_incoming = None;
                state.notice = Some(if is_spectator {
                    "你已与观众互换位置，转为观战".to_string()
                } else {
                    "你已上场，成为玩家".to_string()
                });
                updates.role_changed = Some(is_spectator);
            }
            RoomEvent::GameStarted {
                rom_name,
                sample_rate,
                framebuffer,
                ring,
            } => {
                // Entering gameplay: stale messages and negotiation prompts
                // must not carry into (or back out of) the game page.
                state.error = None;
                state.notice = None;
                state.swap_outgoing = false;
                state.swap_incoming = None;
                state.seat_outgoing = false;
                state.seat_incoming = None;
                updates.start = Some(StartInfo {
                    rom_name,
                    sample_rate,
                    framebuffer,
                    ring,
                });
            }
            RoomEvent::GameEnded => updates.game_ended = true,
            RoomEvent::Latency { rtt_ms } => state.latency_ms = Some(rtt_ms),
            RoomEvent::Reconnecting { attempt } => {
                state.error = Some(format!("连接中断，正在自动重连…（第 {attempt} 次）"));
                // The link is down; the last measurement no longer applies.
                state.latency_ms = None;
                state.swap_outgoing = false;
                state.swap_incoming = None;
                state.seat_outgoing = false;
                state.seat_incoming = None;
            }
            RoomEvent::Reconnected => {
                state.error = None;
                state.peer = Some("主机".to_string());
                updates.reconnected = true;
            }
            RoomEvent::PeerLeft => {
                state.peer = None;
                state.error = Some(if state.is_host {
                    "对方已离开，等待新玩家加入…".to_string()
                } else {
                    "与主机断开连接".to_string()
                });
                state.swap_outgoing = false;
                state.swap_incoming = None;
                state.seat_outgoing = false;
                state.seat_incoming = None;
            }
            // `Failed` means the network task ended, so the application must return to the lobby.
            RoomEvent::Failed(reason) => updates.fatal = Some(reason),
        }
    }
    updates
}

/// Draws the room page.
pub fn show(ui: &mut egui::Ui, state: &mut RoomState) -> RoomAction {
    let mut action = RoomAction::None;
    egui::CentralPanel::default().show(ui, |ui| {
        if theme::page_header(ui, &state.room_name) {
            action = RoomAction::Leave;
        }

        // Host-only PIN badge and direct address.
        if state.is_host {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(" PIN {} ", state.pin))
                        .strong()
                        .color(egui::Color32::WHITE)
                        .background_color(theme::RED),
                );
                ui.label(
                    egui::RichText::new(&state.addr_display)
                        .size(12.0)
                        .color(theme::TEXT_WEAK),
                );
            });
            ui.add_space(6.0);
        }

        if let Some(err) = &state.error {
            theme::error_banner(ui, err);
            ui.add_space(6.0);
        }
        if let Some(notice) = &state.notice {
            ui.label(egui::RichText::new(notice).color(theme::GREEN));
            ui.add_space(6.0);
        }
        // Guest side: connection quality to the host at a glance.
        if let Some(ms) = state.latency_ms {
            ui.label(latency_text(ms));
            ui.add_space(6.0);
        }

        // Members and slots.
        theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            theme::section_title(ui, "玩家");
            ui.add_space(8.0);
            if state.is_spectator {
                // Spectators have no seat. `my_slot` mirrors the guest player
                // seat here, because SlotState always describes the non-host side.
                let host_seat = match state.my_slot {
                    Player::One => Player::Two,
                    Player::Two => Player::One,
                };
                slot_row(ui, "主机", Some(host_seat), false);
                match &state.roster_player {
                    Some(name) => slot_row(ui, name, Some(state.my_slot), false),
                    None => slot_row(ui, "等待玩家加入…", None, false),
                }
            } else {
                let me = std::env::var("USER").unwrap_or_else(|_| "我".to_string());
                slot_row(ui, &me, Some(state.my_slot), true);
                match &state.peer {
                    Some(peer) => {
                        let peer_slot = match state.my_slot {
                            Player::One => Player::Two,
                            Player::Two => Player::One,
                        };
                        slot_row(ui, peer, Some(peer_slot), false);
                    }
                    None => slot_row(ui, "等待玩家加入…", None, false),
                }
            }
            show_spectator_list(ui, state);
            ui.add_space(10.0);
            if state.is_spectator {
                show_seat_request_controls(ui, state);
            } else {
                show_seat_prompt(ui, state);
                show_swap_controls(ui, state);
            }
        });
        ui.add_space(10.0);

        if state.is_host {
            // Host game selection and start controls.
            theme::card().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                theme::section_title(ui, "选择游戏");
                ui.add_space(8.0);
                if state.games.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "roms 目录中没有找到游戏，请放入 .nes 文件后重新进入房间",
                        )
                        .color(theme::TEXT_WEAK),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            for (i, game) in state.games.iter().enumerate() {
                                let selected = state.selected == Some(i);
                                let text = egui::RichText::new(format!("🕹  {}", game.title));
                                if ui.selectable_label(selected, text).clicked() {
                                    state.selected = Some(i);
                                }
                            }
                        });
                }
                ui.add_space(10.0);
                let can_start = state.peer.is_some() && state.selected.is_some();
                if theme::primary_button(ui, "▶ 开始游戏", can_start).clicked()
                    && let Some(i) = state.selected
                    && let Some(game) = state.games.get(i)
                {
                    action = RoomAction::StartGame(game.path.clone(), game.title.clone());
                }
                if state.peer.is_none() {
                    ui.label(
                        egui::RichText::new("需要另一位玩家加入后才能开始")
                            .size(12.0)
                            .color(theme::TEXT_WEAK),
                    );
                }
            });
        } else {
            // Client waits until the host starts a game.
            let waiting = if state.is_spectator {
                "观战中，等待主机开始游戏…"
            } else {
                "等待主机选择游戏并开始…"
            };
            theme::card().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new(waiting).color(theme::TEXT_WEAK));
                });
            });
        }
    });
    action
}

/// Lists connected spectators under the seat rows.
fn show_spectator_list(ui: &mut egui::Ui, state: &RoomState) {
    if state.spectators.is_empty() {
        return;
    }
    ui.add_space(6.0);
    let me = std::env::var("USER").unwrap_or_else(|_| "我".to_string());
    let names: Vec<String> = state
        .spectators
        .iter()
        .map(|n| {
            if state.is_spectator && *n == me {
                format!("{n}（我）")
            } else {
                n.clone()
            }
        })
        .collect();
    ui.label(
        egui::RichText::new(format!("观众（{}）：{}", names.len(), names.join("、")))
            .size(12.0)
            .color(theme::TEXT_WEAK),
    );
}

/// Seat-change controls for a spectator: the request button or the waiting state.
fn show_seat_request_controls(ui: &mut egui::Ui, state: &mut RoomState) {
    if state.seat_outgoing {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(egui::RichText::new("等待对方同意上场…").color(theme::TEXT_WEAK));
        });
    } else if ui.button("🎮 申请上场").clicked() {
        state.handle.send(RoomCmd::RequestSeat);
        state.seat_outgoing = true;
    }
}

/// Prompt shown to the approver of a seat-change request, with an
/// auto-decline countdown. The seated player approves an exchange; the host
/// approves a promotion into the empty seat.
fn show_seat_prompt(ui: &mut egui::Ui, state: &mut RoomState) {
    let Some((name, deadline)) = state.seat_incoming.clone() else {
        return;
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        state.handle.send(RoomCmd::RespondSeat(false));
        state.seat_incoming = None;
        return;
    }
    let secs = remaining.as_secs() + 1;
    let text = if state.is_host {
        format!("观众 {name} 请求上场（{secs} 秒后自动拒绝）")
    } else {
        format!("观众 {name} 请求与你互换位置（{secs} 秒后自动拒绝）")
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(text).color(theme::RED_BRIGHT));
        if ui.button("同意").clicked() {
            state.handle.send(RoomCmd::RespondSeat(true));
            state.seat_incoming = None;
        }
        if ui.button("拒绝").clicked() {
            state.handle.send(RoomCmd::RespondSeat(false));
            state.seat_incoming = None;
        }
    });
    ui.add_space(6.0);
}

/// Swap controls in one of three states: the request button, waiting for the
/// peer's answer, or the peer's request with accept/decline and a countdown.
fn show_swap_controls(ui: &mut egui::Ui, state: &mut RoomState) {
    if let Some(deadline) = state.swap_incoming {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            // Unanswered prompts decline automatically at the deadline.
            state.handle.send(RoomCmd::RespondSwap(false));
            state.swap_incoming = None;
            return;
        }
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "对方请求交换 P1 / P2（{} 秒后自动拒绝）",
                    remaining.as_secs() + 1
                ))
                .color(theme::RED_BRIGHT),
            );
            if ui.button("同意").clicked() {
                state.handle.send(RoomCmd::RespondSwap(true));
                state.swap_incoming = None;
            }
            if ui.button("拒绝").clicked() {
                state.handle.send(RoomCmd::RespondSwap(false));
                state.swap_incoming = None;
            }
        });
    } else if state.swap_outgoing {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(egui::RichText::new("等待对方同意交换…").color(theme::TEXT_WEAK));
        });
    } else if ui
        .add_enabled(state.peer.is_some(), egui::Button::new("↔ 交换 P1 / P2"))
        .clicked()
    {
        state.handle.send(RoomCmd::RequestSwap);
        state.swap_outgoing = true;
    }
}

/// One player row containing a slot badge and name.
fn slot_row(ui: &mut egui::Ui, name: &str, slot: Option<Player>, is_me: bool) {
    ui.horizontal(|ui| {
        theme::slot_badge(ui, slot);
        let display = if is_me {
            format!("{name}（我）")
        } else {
            name.to_string()
        };
        let text = if slot.is_some() {
            egui::RichText::new(display)
        } else {
            egui::RichText::new(display).color(theme::TEXT_WEAK)
        };
        ui.label(text);
    });
}
