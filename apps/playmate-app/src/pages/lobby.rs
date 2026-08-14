//! LAN lobby for creating PIN-protected rooms and browsing or joining nearby rooms.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use playmate_net::{JoinRole, Room, browse_rooms};

use crate::theme;

/// Background room-list refresher that browses mDNS on a worker thread.
pub struct LobbyDiscovery {
    /// Receives the latest room list.
    rx: std_mpsc::Receiver<Vec<Room>>,
    /// Stop flag set on drop; the thread exits after the current browse cycle.
    stop: Arc<AtomicBool>,
}

impl LobbyDiscovery {
    /// Starts the background refresher with a two-second browse cycle.
    pub fn start() -> Self {
        let (tx, rx) = std_mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                match browse_rooms(Duration::from_secs(2)) {
                    Ok(rooms) => {
                        if tx.send(rooms).is_err() {
                            return; // The UI dropped the receiver.
                        }
                    }
                    Err(e) => log::warn!("failed to browse LAN rooms: {e}"),
                }
            }
        });
        Self { rx, stop }
    }

    /// Returns the latest result, or `None` when no new cycle has completed.
    pub fn poll(&self) -> Option<Vec<Room>> {
        let mut latest = None;
        while let Ok(rooms) = self.rx.try_recv() {
            latest = Some(rooms);
        }
        latest
    }
}

impl Drop for LobbyDiscovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Lobby page state.
pub struct LobbyState {
    /// Room display name in the creation form.
    pub room_name: String,
    /// Four-digit creation PIN, generated automatically when empty.
    pub pin: String,
    /// Rooms found by the most recent browse cycle.
    pub rooms: Vec<Room>,
    /// Room currently being joined through the PIN dialog.
    pub joining: Option<JoinDialog>,
    /// User-facing error message.
    pub error: Option<String>,
    /// Background room discovery worker.
    pub discovery: LobbyDiscovery,
}

/// Join-room dialog state.
pub struct JoinDialog {
    /// Target room.
    pub room: Room,
    /// User-entered PIN.
    pub pin_input: String,
    /// Whether to join as a spectator instead of a player.
    pub as_spectator: bool,
}

impl LobbyState {
    /// Enters the lobby with a localized default room name and starts discovery.
    pub fn new() -> Self {
        let user = std::env::var("USER").unwrap_or_else(|_| "玩家".to_string());
        Self {
            room_name: format!("{user} 的房间"),
            pin: String::new(),
            rooms: Vec::new(),
            joining: None,
            error: None,
            discovery: LobbyDiscovery::start(),
        }
    }
}

/// Action triggered by the lobby page.
pub enum LobbyAction {
    /// No action.
    None,
    /// Return to the main menu.
    Back,
    /// Create a room with its name and PIN.
    Create(String, String),
    /// Join a target room with a PIN and role.
    Join(Room, String, JoinRole),
    /// Rejoin the previous room using address and PIN retained by the application.
    Rejoin,
}

/// Draws the lobby; `last_join` controls whether quick rejoin is shown.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut LobbyState,
    last_join: Option<&std::net::SocketAddr>,
) -> LobbyAction {
    let mut action = LobbyAction::None;

    // Consume the latest background discovery result first.
    if let Some(rooms) = state.discovery.poll() {
        state.rooms = rooms;
    }

    egui::CentralPanel::default().show(ui, |ui| {
        if theme::page_header(ui, "局域网联机") {
            action = LobbyAction::Back;
        }

        if let Some(err) = &state.error {
            theme::error_banner(ui, err);
            ui.add_space(6.0);
        }

        // Show quick rejoin only while the previous room is still advertised.
        // Its randomized port identifies the specific room session.
        if let Some(addr) = last_join {
            let still_alive = state.rooms.iter().any(|r| r.addr.port() == addr.port());
            if still_alive {
                if theme::card_row(ui, "↻", "重新加入上个房间", Some(&addr.to_string()), true)
                    .clicked()
                {
                    action = LobbyAction::Rejoin;
                }
                ui.add_space(6.0);
            }
        }

        ui.columns(2, |cols| {
            // Left column: create a room.
            theme::card().show(&mut cols[0], |ui| {
                ui.set_min_width(ui.available_width());
                theme::section_title(ui, "创建房间");
                ui.add_space(10.0);
                ui.label(egui::RichText::new("房间名").color(theme::TEXT_WEAK));
                ui.add(
                    egui::TextEdit::singleline(&mut state.room_name).desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("PIN 码（4 位数字，留空自动生成）").color(theme::TEXT_WEAK),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut state.pin)
                        .desired_width(f32::INFINITY)
                        .char_limit(4)
                        .font(egui::FontId::proportional(18.0))
                        .hint_text("自动生成"),
                );
                ui.add_space(14.0);
                if theme::primary_button(ui, "创建房间", true).clicked() {
                    let pin_valid = state.pin.is_empty()
                        || (state.pin.len() == 4 && state.pin.chars().all(|c| c.is_ascii_digit()));
                    if pin_valid {
                        state.error = None;
                        action = LobbyAction::Create(state.room_name.clone(), state.pin.clone());
                    } else {
                        state.error = Some("PIN 码必须是 4 位数字".to_string());
                    }
                }
            });

            // Right column: discovered rooms.
            theme::card().show(&mut cols[1], |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    theme::section_title(ui, "附近的房间");
                    ui.spinner();
                });
                ui.add_space(10.0);
                if state.rooms.is_empty() {
                    ui.add_space(18.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("正在搜索…").color(theme::TEXT_WEAK));
                        ui.label(
                            egui::RichText::new("确认对方已在同一局域网内创建房间")
                                .size(12.0)
                                .color(theme::TEXT_WEAK),
                        );
                    });
                } else {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        for room in &state.rooms {
                            let addr = room.addr.to_string();
                            if theme::card_row(ui, "🖥", &room.display_name, Some(&addr), true)
                                .clicked()
                            {
                                state.joining = Some(JoinDialog {
                                    room: room.clone(),
                                    pin_input: String::new(),
                                    as_spectator: false,
                                });
                            }
                        }
                    });
                }
            });
        });
    });

    // Join-room PIN dialog.
    let mut close_dialog = false;
    if let Some(dialog) = &mut state.joining {
        egui::Window::new(
            egui::RichText::new(format!("加入 {}", dialog.room.display_name)).strong(),
        )
        .collapsible(false)
        .resizable(false)
        .fixed_size([260.0, 0.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("输入房间 PIN 码").color(theme::TEXT_WEAK));
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::singleline(&mut dialog.pin_input)
                    .desired_width(f32::INFINITY)
                    .char_limit(4)
                    .font(egui::FontId::proportional(24.0))
                    .hint_text("····"),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("身份").color(theme::TEXT_WEAK));
                ui.selectable_value(&mut dialog.as_spectator, false, "🎮 玩家");
                ui.selectable_value(&mut dialog.as_spectator, true, "🖥 观战");
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, "加入", true).clicked() {
                    let role = if dialog.as_spectator {
                        JoinRole::Spectator
                    } else {
                        JoinRole::Player
                    };
                    action = LobbyAction::Join(dialog.room.clone(), dialog.pin_input.clone(), role);
                    close_dialog = true;
                }
                if ui.button("取消").clicked() {
                    close_dialog = true;
                }
            });
            ui.add_space(2.0);
        });
    }
    if close_dialog {
        state.joining = None;
    }

    action
}
