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
    /// User-facing error or status message.
    pub error: Option<String>,
    /// Waiting for the peer to answer the local swap request.
    pub swap_outgoing: bool,
    /// Auto-decline deadline of the peer's pending swap request.
    pub swap_incoming: Option<Instant>,
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
                state.error = Some("对方拒绝了交换席位的请求".to_string());
            }
            RoomEvent::GameStarted {
                rom_name,
                sample_rate,
                framebuffer,
                ring,
            } => {
                updates.start = Some(StartInfo {
                    rom_name,
                    sample_rate,
                    framebuffer,
                    ring,
                });
            }
            RoomEvent::GameEnded => updates.game_ended = true,
            RoomEvent::Reconnecting { attempt } => {
                state.error = Some(format!("连接中断，正在自动重连…（第 {attempt} 次）"));
                state.swap_outgoing = false;
                state.swap_incoming = None;
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

        // Members and slots.
        theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            theme::section_title(ui, "玩家");
            ui.add_space(8.0);
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
            ui.add_space(10.0);
            show_swap_controls(ui, state);
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
            theme::card().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("等待主机选择游戏并开始…").color(theme::TEXT_WEAK),
                    );
                });
            });
        }
    });
    action
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
