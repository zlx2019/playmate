//! Application shell: manually integrated winit, egui-winit, and egui-wgpu rendering plus routing.
//!
//! Manual integration preserves control of the winit event loop, allowing exact
//! `KeyCode` events, including numpad distinctions, to reach gameplay and
//! rebinding capture before remaining events are passed to egui.

use std::num::NonZeroU32;
use std::sync::Arc;

use egui_wgpu::winit::Painter;
use egui_wgpu::{RendererOptions, WgpuConfiguration};
use playmate_core::Player;
use playmate_net::{Announcer, JoinRole, Room};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::config::{self, Config, InputMap};
use crate::emu::NetSink;
use crate::gamepad::GamepadInput;
use crate::netplay::{self, RoomCmd};
use crate::pages::game_menu::{self, GameMenu, GameMenuAction};
use crate::pages::game_select::{self, GameEntry, GameSelectAction};
use crate::pages::lobby::{self, LobbyAction, LobbyState};
use crate::pages::main_menu::{self, MenuAction};
use crate::pages::room::{self, RoomAction, RoomState};
use crate::pages::settings::{self, SettingsAction, SettingsState};
use crate::play::{GuestPlay, PlaySession};

/// Render clear color matching the dark egui theme.
// Matches theme background #141417 after converting sRGB to wgpu's linear color space.
const CLEAR_COLOR: [f32; 4] = [0.007, 0.007, 0.0085, 1.0];

/// Current page, with each variant carrying its private state.
pub enum Page {
    /// Main menu.
    MainMenu,
    /// Local-play game selection.
    GameSelect {
        /// Games found when entering the page.
        games: Vec<GameEntry>,
        /// Error from the most recent launch attempt.
        error: Option<String>,
    },
    /// Active local or host game.
    Playing {
        /// Runtime resources for the current game.
        session: PlaySession,
        /// Netplay context for a host game; `None` for local play.
        /// Dropping it disconnects the session and unregisters mDNS.
        net: Option<RoomState>,
        /// Pause/menu overlay state.
        menu: GameMenu,
    },
    /// LAN lobby.
    LanLobby {
        /// Lobby form and discovery state.
        state: LobbyState,
    },
    /// Active netplay room.
    Room {
        /// Room session, slot assignment, and game-selection state.
        state: RoomState,
    },
    /// Client gameplay using media streamed from the host.
    GuestPlaying {
        /// Client gameplay session.
        play: GuestPlay,
        /// Room network state; dropping the handle leaves the room.
        net: RoomState,
        /// Pause/menu overlay state.
        menu: GameMenu,
    },
    /// Key-binding settings.
    Settings {
        /// Rebinding capture and other page state.
        state: SettingsState,
    },
}

/// Deferred navigation command applied after UI traversal to avoid borrowing `self.page`.
enum Nav {
    /// Remain on the current page.
    Stay,
    /// Switch to a page.
    To(Box<Page>),
    /// Create a netplay room and enter it.
    CreateRoom {
        /// Room display name.
        name: String,
        /// PIN, generated automatically when empty.
        pin: String,
    },
    /// Join a netplay room.
    JoinRoom {
        /// Target room.
        room: Room,
        /// User-entered PIN.
        pin: String,
        /// Seat to request from the host.
        role: JoinRole,
    },
    /// Start a host game and transfer the room network handle.
    StartNetGame {
        /// Started game session.
        session: PlaySession,
        /// Game title sent to the client.
        title: String,
        /// Frame receiver transferred to the network task.
        frame_rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
        /// Audio receiver transferred to the network task.
        audio_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<f32>>,
        /// Remote player slot.
        remote_slot: Player,
    },
    /// Enter client gameplay and transfer the room network handle.
    StartGuestGame {
        /// Client gameplay session.
        play: GuestPlay,
    },
    /// End gameplay, retaining a netplay room or returning local play to the menu.
    BackToRoom,
}

/// Application state containing the egui pipeline and page router.
pub struct PlaymateApp {
    /// tokio runtime for netplay tasks and asynchronous wgpu initialization.
    rt: tokio::runtime::Runtime,
    /// User configuration, including key bindings.
    cfg: Config,
    /// Keyboard lookup table rebuilt after settings are saved.
    input_map: InputMap,
    /// Gamepad input manager.
    gamepad: GamepadInput,
    /// Current page.
    page: Page,
    /// Most recently joined address, PIN, and role for quick rejoin.
    last_join: Option<(std::net::SocketAddr, String, JoinRole)>,
    /// egui context, created before the window and configured with CJK fonts.
    egui_ctx: egui::Context,
    /// Main window, created after `resumed`.
    window: Option<Arc<Window>>,
    /// Bridge from winit events to egui input.
    egui_state: Option<egui_winit::State>,
    /// egui-to-wgpu renderer and surface manager.
    painter: Option<Painter>,
}

impl PlaymateApp {
    /// Creates the application, runtime, configuration, egui context, and CJK fonts.
    pub fn new() -> anyhow::Result<Self> {
        let rt = tokio::runtime::Runtime::new()?;
        config::ensure_data_dirs();
        let cfg = config::load()?;
        let input_map = InputMap::from_config(&cfg);
        let egui_ctx = egui::Context::default();
        install_cjk_fonts(&egui_ctx);
        crate::theme::apply(&egui_ctx);
        Ok(Self {
            rt,
            cfg,
            input_map,
            gamepad: GamepadInput::new(),
            page: Page::MainMenu,
            last_join: None,
            egui_ctx,
            window: None,
            egui_state: None,
            painter: None,
        })
    }

    /// Draws the routed UI for one frame and applies deferred navigation.
    fn ui(&mut self, ui: &mut egui::Ui) {
        let mut nav = Nav::Stay;
        match &mut self.page {
            Page::MainMenu => match main_menu::show(ui) {
                MenuAction::None => {}
                MenuAction::SinglePlayer => {
                    nav = Nav::To(Box::new(Page::GameSelect {
                        games: game_select::scan_roms(),
                        error: None,
                    }));
                }
                MenuAction::LanPlay => {
                    nav = Nav::To(Box::new(Page::LanLobby {
                        state: LobbyState::new(),
                    }));
                }
                MenuAction::Settings => {
                    nav = Nav::To(Box::new(Page::Settings {
                        state: SettingsState::default(),
                    }));
                }
            },
            Page::GameSelect { games, error } => {
                match game_select::show(ui, games, error.as_deref()) {
                    GameSelectAction::None => {}
                    GameSelectAction::Back => nav = Nav::To(Box::new(Page::MainMenu)),
                    GameSelectAction::Refresh => *games = game_select::scan_roms(),
                    GameSelectAction::Play(path) => match PlaySession::start(&path) {
                        Ok(session) => {
                            nav = Nav::To(Box::new(Page::Playing {
                                session,
                                net: None,
                                menu: GameMenu::default(),
                            }));
                        }
                        Err(e) => *error = Some(format!("启动失败: {e:#}")),
                    },
                }
            }
            Page::Playing { session, net, menu } => {
                // Host: consume room events, including peer-disconnect status.
                if let Some(net_state) = net {
                    let updates = room::apply_events(net_state);
                    // If the network task ends, local gameplay continues with a status message.
                    if let Some(reason) = updates.fatal {
                        net_state.error = Some(reason);
                    }
                }
                let mut open_menu = false;
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("菜单 (Esc)").clicked() {
                            open_menu = true;
                        }
                        ui.label(egui::RichText::new(&session.rom_title).strong());
                        if let Some(net_state) = net {
                            match &net_state.error {
                                Some(err) => {
                                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                                }
                                None => {
                                    ui.label(
                                        egui::RichText::new("● 联机中")
                                            .color(egui::Color32::from_rgb(110, 200, 110)),
                                    );
                                }
                            }
                        }
                    });
                    ui.separator();
                    session.ui(ui);
                });
                if open_menu && !menu.open {
                    menu.open = true;
                    session.clear_input();
                    // Only local play truly pauses; a host game keeps running for peers.
                    session.set_paused(net.is_none());
                }
                if menu.open {
                    match game_menu::show(ui, &self.cfg, menu, net.is_none(), true) {
                        GameMenuAction::None => {}
                        GameMenuAction::Resume => {
                            menu.open = false;
                            menu.settings = None;
                            session.set_paused(false);
                        }
                        // Save/load close the menu so the result toast and the
                        // (possibly restored) frame are immediately visible.
                        GameMenuAction::SaveState => {
                            session.request_save_state();
                            menu.open = false;
                            menu.settings = None;
                            session.set_paused(false);
                        }
                        GameMenuAction::LoadState => {
                            session.request_load_state();
                            menu.open = false;
                            menu.settings = None;
                            session.set_paused(false);
                        }
                        // Netplay returns to the room for another game; local
                        // play drops the session and returns to the menu.
                        GameMenuAction::Exit => nav = Nav::BackToRoom,
                        GameMenuAction::RestoreDefaults => restore_default_keys(
                            &mut self.cfg,
                            &mut self.input_map,
                            menu.settings.as_mut(),
                        ),
                    }
                }
            }
            Page::GuestPlaying { play, net, menu } => {
                let updates = room::apply_events(net);
                let had_start = updates.start.is_some();
                // Rebuild gameplay with new shared buffers after reconnecting to an active game.
                if let Some(start) = updates.start {
                    match GuestPlay::start(
                        start.rom_name,
                        net.my_slot,
                        start.framebuffer,
                        start.ring,
                        start.sample_rate,
                        net.is_spectator,
                    ) {
                        Ok(new_play) => *play = new_play,
                        Err(e) => net.error = Some(format!("恢复联机画面失败: {e:#}")),
                    }
                }
                let mut leave = false;
                let mut open_menu = false;
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("菜单 (Esc)").clicked() {
                            open_menu = true;
                        }
                        ui.label(egui::RichText::new(&play.rom_title).strong());
                        match &net.error {
                            Some(err) => {
                                ui.colored_label(egui::Color32::LIGHT_RED, err);
                            }
                            None => {
                                let status = if play.is_spectator() {
                                    "● 观战中"
                                } else {
                                    "● 联机中"
                                };
                                ui.label(
                                    egui::RichText::new(status)
                                        .color(egui::Color32::from_rgb(110, 200, 110)),
                                );
                                if let Some(ms) = net.latency_ms {
                                    ui.label(room::latency_text(ms));
                                }
                            }
                        }
                    });
                    ui.separator();
                    play.ui(ui);
                });
                if open_menu && !menu.open {
                    menu.open = true;
                    play.clear_input();
                }
                if menu.open {
                    // Clients cannot pause the host's game; the menu only overlays it.
                    match game_menu::show(ui, &self.cfg, menu, false, false) {
                        GameMenuAction::None => {}
                        GameMenuAction::Resume => {
                            menu.open = false;
                            menu.settings = None;
                        }
                        // Guests never see save/load; the emulator is not local.
                        GameMenuAction::SaveState | GameMenuAction::LoadState => {}
                        GameMenuAction::Exit => leave = true,
                        GameMenuAction::RestoreDefaults => restore_default_keys(
                            &mut self.cfg,
                            &mut self.input_map,
                            menu.settings.as_mut(),
                        ),
                    }
                }
                if let Some(reason) = updates.fatal {
                    // A terminal session failure returns to the lobby and clears stale quick rejoin.
                    self.last_join = None;
                    nav = Nav::To(Box::new(Page::LanLobby {
                        state: lobby_with_error(reason),
                    }));
                } else if updates.game_ended || (updates.reconnected && !had_start) {
                    // Return to the room after game end or reconnecting to an idle host.
                    nav = Nav::BackToRoom;
                } else if leave {
                    // Leaving manually disconnects the session and returns to the lobby.
                    nav = Nav::To(Box::new(Page::LanLobby {
                        state: LobbyState::new(),
                    }));
                }
            }
            Page::LanLobby { state } => {
                let last_addr = self.last_join.as_ref().map(|(addr, _, _)| addr);
                match lobby::show(ui, state, last_addr) {
                    LobbyAction::None => {}
                    LobbyAction::Back => nav = Nav::To(Box::new(Page::MainMenu)),
                    LobbyAction::Create(name, pin) => nav = Nav::CreateRoom { name, pin },
                    LobbyAction::Join(room_info, pin, role) => {
                        nav = Nav::JoinRoom {
                            room: room_info,
                            pin,
                            role,
                        };
                    }
                    LobbyAction::Rejoin => {
                        if let Some((addr, pin, role)) = self.last_join.clone() {
                            nav = Nav::JoinRoom {
                                room: Room {
                                    name: String::new(),
                                    display_name: "上个房间".to_string(),
                                    addr,
                                },
                                pin,
                                role,
                            };
                        }
                    }
                }
            }
            Page::Room { state } => {
                // Client: create gameplay when the host starts a game.
                let updates = room::apply_events(state);
                // An approved seat change updates the quick-rejoin role.
                if let Some(is_spectator) = updates.role_changed
                    && let Some((_, _, role)) = self.last_join.as_mut()
                {
                    *role = if is_spectator {
                        JoinRole::Spectator
                    } else {
                        JoinRole::Player
                    };
                }
                // Terminal failures return to the lobby and clear the now-invalid quick-rejoin entry.
                if let Some(reason) = updates.fatal {
                    self.last_join = None;
                    nav = Nav::To(Box::new(Page::LanLobby {
                        state: lobby_with_error(reason),
                    }));
                } else if let Some(start) = updates.start {
                    match GuestPlay::start(
                        start.rom_name,
                        state.my_slot,
                        start.framebuffer,
                        start.ring,
                        start.sample_rate,
                        state.is_spectator,
                    ) {
                        Ok(play) => nav = Nav::StartGuestGame { play },
                        Err(e) => state.error = Some(format!("建立联机画面失败: {e:#}")),
                    }
                }
                if matches!(nav, Nav::Stay) {
                    match room::show(ui, state) {
                        RoomAction::None => {}
                        RoomAction::Leave => {
                            nav = Nav::To(Box::new(Page::LanLobby {
                                state: LobbyState::new(),
                            }));
                        }
                        RoomAction::StartGame(path, title) => {
                            // Host startup: connect emulation media channels to the network task.
                            let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(2);
                            let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel();
                            let sink = NetSink { frame_tx, audio_tx };
                            let remote_slot = match state.my_slot {
                                Player::One => Player::Two,
                                Player::Two => Player::One,
                            };
                            match PlaySession::start_networked(&path, state.my_slot, sink) {
                                Ok(session) => {
                                    nav = Nav::StartNetGame {
                                        session,
                                        title,
                                        frame_rx,
                                        audio_rx,
                                        remote_slot,
                                    };
                                }
                                Err(e) => state.error = Some(format!("启动失败: {e:#}")),
                            }
                        }
                    }
                }
            }
            Page::Settings { state } => match settings::show(ui, &self.cfg, state) {
                SettingsAction::None => {}
                SettingsAction::Back => nav = Nav::To(Box::new(Page::MainMenu)),
                SettingsAction::RestoreDefaults => {
                    restore_default_keys(&mut self.cfg, &mut self.input_map, Some(state));
                }
            },
        }
        self.apply_nav(nav);
    }

    /// Applies navigation, room creation/joining, and netplay startup commands.
    fn apply_nav(&mut self, nav: Nav) {
        match nav {
            Nav::Stay => {}
            Nav::To(page) => self.page = *page,
            Nav::CreateRoom { name, pin } => match self.create_room(name, pin) {
                Ok(page) => self.page = page,
                Err(e) => {
                    if let Page::LanLobby { state } = &mut self.page {
                        state.error = Some(e);
                    }
                }
            },
            Nav::JoinRoom { room, pin, role } => {
                let my_name = std::env::var("USER").unwrap_or_else(|_| "玩家".to_string());
                // Retain join details for the lobby's quick-rejoin action.
                self.last_join = Some((room.addr, pin.clone(), role));
                let handle = netplay::spawn_guest(&self.rt, room.addr, pin, my_name, role);
                self.page = Page::Room {
                    state: RoomState {
                        handle,
                        is_host: false,
                        room_name: room.display_name,
                        pin: String::new(),
                        addr_display: String::new(),
                        peer: None,
                        my_slot: Player::Two,
                        games: Vec::new(),
                        selected: None,
                        error: None,
                        notice: None,
                        latency_ms: None,
                        swap_outgoing: false,
                        swap_incoming: None,
                        is_spectator: role == JoinRole::Spectator,
                        seat_outgoing: false,
                        seat_incoming: None,
                        roster_player: None,
                        spectators: Vec::new(),
                        _announcer: None,
                    },
                };
            }
            Nav::StartNetGame {
                session,
                title,
                frame_rx,
                audio_rx,
                remote_slot,
            } => {
                // Transfer room network state, media channels, and shared state to the network task.
                let old = std::mem::replace(&mut self.page, Page::MainMenu);
                if let Page::Room { state } = old {
                    state.handle.send(RoomCmd::StartGame {
                        title,
                        sample_rate: session.sample_rate,
                        frame_rx,
                        audio_rx,
                        shared: session.shared_state(),
                        remote_slot,
                    });
                    self.page = Page::Playing {
                        session,
                        net: Some(state),
                        menu: GameMenu::default(),
                    };
                }
            }
            Nav::StartGuestGame { play } => {
                let old = std::mem::replace(&mut self.page, Page::MainMenu);
                if let Page::Room { state } = old {
                    self.page = Page::GuestPlaying {
                        play,
                        net: state,
                        menu: GameMenu::default(),
                    };
                }
            }
            Nav::BackToRoom => {
                let old = std::mem::replace(&mut self.page, Page::MainMenu);
                match old {
                    // A host ending a game retains the room for another selection.
                    Page::Playing {
                        net: Some(state), ..
                    } => self.page = Page::Room { state },
                    // A client returns to the room after the host ends the game.
                    Page::GuestPlaying { net, .. } => self.page = Page::Room { state: net },
                    // Local play drops the session and remains on the main menu.
                    _ => {}
                }
            }
        }
    }

    /// Creates a room by binding a port, advertising mDNS, and starting the host task.
    fn create_room(&mut self, name: String, pin: String) -> Result<Page, String> {
        let pin = if pin.is_empty() {
            netplay::gen_pin()
        } else {
            pin
        };
        let listener = self
            .rt
            .block_on(tokio::net::TcpListener::bind(("0.0.0.0", 0)))
            .map_err(|e| format!("创建监听端口失败: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("获取端口失败: {e}"))?
            .port();
        let instance = format!("playmate-{}", std::process::id());
        let announcer =
            Announcer::start(&instance, port, &name).map_err(|e| format!("mDNS 广播失败: {e}"))?;
        let handle = netplay::spawn_host(&self.rt, listener, pin.clone(), name.clone());
        log::info!("room created: {name} (PIN {pin}, port {port})");
        Ok(Page::Room {
            state: RoomState {
                handle,
                is_host: true,
                room_name: name,
                pin,
                addr_display: format!("{}:{port}", netplay::local_ip_display()),
                peer: None,
                my_slot: Player::One,
                games: game_select::scan_roms(),
                selected: None,
                error: None,
                notice: None,
                latency_ms: None,
                swap_outgoing: false,
                swap_incoming: None,
                is_spectator: false,
                seat_outgoing: false,
                seat_incoming: None,
                roster_player: None,
                spectators: Vec::new(),
                _announcer: Some(announcer),
            },
        })
    }

    /// Returns the settings state currently accepting input, either the
    /// settings page or the settings view embedded in the in-game menu.
    fn settings_state_mut(&mut self) -> Option<&mut SettingsState> {
        match &mut self.page {
            Page::Settings { state } => Some(state),
            Page::Playing { menu, .. } | Page::GuestPlaying { menu, .. } => menu.settings.as_mut(),
            _ => None,
        }
    }

    /// Captures a physical key for the pending setting and saves it immediately.
    /// Returns whether the keyboard event was consumed.
    fn handle_rebind_capture(&mut self, code: KeyCode) -> bool {
        let Some((player, key)) = self.settings_state_mut().and_then(|s| s.capturing.take()) else {
            return false;
        };
        if code == KeyCode::Escape {
            if let Some(state) = self.settings_state_mut() {
                state.hint = Some("已取消".to_string());
            }
            return true;
        }
        config::bind_key(&mut self.cfg, player, key, code);
        self.input_map = InputMap::from_config(&self.cfg);
        let hint = match config::save(&mut self.cfg) {
            Ok(()) => format!("已保存：{}", settings::key_label(Some(code))),
            Err(e) => format!("保存失败: {e:#}"),
        };
        if let Some(state) = self.settings_state_mut() {
            state.hint = Some(hint);
        }
        true
    }

    /// Toggles the in-game overlay: opening releases held input and pauses
    /// local play; closing resumes and discards the embedded settings view.
    fn toggle_game_menu(&mut self) {
        match &mut self.page {
            Page::Playing { session, net, menu } => {
                menu.open = !menu.open;
                if menu.open {
                    session.clear_input();
                } else {
                    menu.settings = None;
                }
                // Only local play truly pauses; a host game keeps running for peers.
                session.set_paused(menu.open && net.is_none());
            }
            Page::GuestPlaying { play, menu, .. } => {
                menu.open = !menu.open;
                if menu.open {
                    play.clear_input();
                } else {
                    menu.settings = None;
                }
            }
            _ => {}
        }
    }

    /// Runs one egui frame and submits it to wgpu.
    fn redraw(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let raw_input = match &mut self.egui_state {
            Some(state) => state.take_egui_input(&window),
            None => return,
        };

        // Cloning the Arc-backed context avoids borrow conflicts between the closure and `self`.
        let egui_ctx = self.egui_ctx.clone();
        let mut output = egui_ctx.run_ui(raw_input, |ui| self.ui(ui));

        if let Some(state) = &mut self.egui_state {
            state.handle_platform_output(&window, output.platform_output);
        }
        let primitives = egui_ctx.tessellate(output.shapes, output.pixels_per_point);
        if let Some(painter) = &mut self.painter {
            painter.paint_and_update_textures(
                egui::ViewportId::ROOT,
                output.pixels_per_point,
                CLEAR_COLOR,
                &primitives,
                &mut output.textures_delta,
                Vec::new(),
                &window,
            );
        }
    }
}

impl ApplicationHandler for PlaymateApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Desktop platforms normally resume once; guard against duplicate window creation.
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Playmate")
            .with_window_icon(load_window_icon())
            .with_inner_size(LogicalSize::new(900.0, 700.0))
            .with_min_inner_size(LogicalSize::new(640.0, 520.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        // winit-to-egui event bridge.
        let state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );

        // Initialize the asynchronous wgpu renderer through the tokio runtime.
        let mut painter = self.rt.block_on(Painter::new(
            self.egui_ctx.clone(),
            WgpuConfiguration::default(),
            false,
            RendererOptions::default(),
        ));
        if let Err(e) = self
            .rt
            .block_on(painter.set_window(egui::ViewportId::ROOT, Some(Arc::clone(&window))))
        {
            log::error!("failed to initialize GPU rendering surface: {e}");
            event_loop.exit();
            return;
        }

        self.window = Some(window);
        self.egui_state = Some(state);
        self.painter = Some(painter);
        log::info!("window and rendering pipeline ready");
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };

        // Intercept exact KeyCode events before egui for gameplay and rebinding.
        // This preserves numpad distinctions that egui's key model does not expose.
        let mut consumed_by_game = false;
        let mut toggle_menu = false;
        if let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = &event
            && let PhysicalKey::Code(code) = key_event.physical_key
        {
            let pressed = key_event.state == ElementState::Pressed;
            // Holding Esc must not rapidly toggle the menu through key repeats.
            let toggle = pressed && !key_event.repeat;
            let menu_open = matches!(
                &self.page,
                Page::Playing { menu, .. } | Page::GuestPlaying { menu, .. } if menu.open
            );
            if menu_open {
                // The overlay owns the keyboard: rebinding capture first for
                // the embedded settings view, then Esc closes the menu.
                if pressed && self.handle_rebind_capture(code) {
                    consumed_by_game = true;
                } else if code == KeyCode::Escape {
                    toggle_menu = toggle;
                    consumed_by_game = true;
                }
            } else {
                match &mut self.page {
                    Page::Playing { session, .. } => {
                        if code == KeyCode::Escape {
                            toggle_menu = toggle;
                            consumed_by_game = true;
                        } else if code == KeyCode::F5 {
                            // Instant save/load hotkeys, host-side only.
                            if toggle {
                                session.request_save_state();
                            }
                            consumed_by_game = true;
                        } else if code == KeyCode::F9 {
                            if toggle {
                                session.request_load_state();
                            }
                            consumed_by_game = true;
                        } else {
                            consumed_by_game = session.on_key(&self.input_map, code, pressed);
                        }
                    }
                    Page::GuestPlaying { play, .. } => {
                        if code == KeyCode::Escape {
                            toggle_menu = toggle;
                            consumed_by_game = true;
                        } else {
                            consumed_by_game = play.on_key(&self.input_map, code, pressed);
                        }
                    }
                    Page::Settings { .. } if pressed => {
                        consumed_by_game = self.handle_rebind_capture(code);
                    }
                    _ => {}
                }
            }
        }
        if toggle_menu {
            self.toggle_game_menu();
        }

        if !consumed_by_game && let Some(state) = &mut self.egui_state {
            let response = state.on_window_event(&window, &event);
            if response.repaint {
                window.request_redraw();
            }
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let (Some(painter), Some(w), Some(h)) = (
                    self.painter.as_mut(),
                    NonZeroU32::new(size.width),
                    NonZeroU32::new(size.height),
                ) {
                    painter.on_window_resized(egui::ViewportId::ROOT, w, h);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Poll gamepads and publish or send input once per gameplay frame.
        self.gamepad.poll();
        match &mut self.page {
            // Local play and hosts publish merged input directly to emulation.
            Page::Playing { session, menu, .. } => session.sync_input(&self.gamepad, menu.open),
            // Clients send changed merged input to the host through the network task.
            Page::GuestPlaying { play, net, menu } => {
                if let Some(buttons) = play.poll_outgoing(&self.gamepad, menu.open) {
                    net.handle.send(RoomCmd::Input(buttons));
                }
            }
            _ => {}
        }
        // Poll continuously for 60 fps gameplay; menu-frame cost is negligible.
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

/// Restores default key bindings, rebuilds the lookup table, saves the
/// configuration, and reports the result on the given settings view.
fn restore_default_keys(
    cfg: &mut Config,
    input_map: &mut InputMap,
    state: Option<&mut SettingsState>,
) {
    cfg.keys = Default::default();
    *input_map = InputMap::from_config(cfg);
    let hint = match config::save(cfg) {
        Ok(()) => "已恢复默认键位".to_string(),
        Err(e) => format!("保存失败: {e:#}"),
    };
    if let Some(state) = state {
        state.hint = Some(hint);
    }
}

/// Creates lobby state containing the reason a netplay session ended.
fn lobby_with_error(reason: String) -> LobbyState {
    let mut state = LobbyState::new();
    state.error = Some(reason);
    state
}

/// Decodes the embedded raw RGBA window icon for the title bar and taskbar.
///
/// Windows and X11 use it at runtime; macOS ignores window icons and shows the
/// bundle .icns instead. The blob is exported by `scripts/gen-icons.sh`.
fn load_window_icon() -> Option<winit::window::Icon> {
    const RGBA: &[u8] = include_bytes!("../../../assets/icon/window-48.rgba");
    winit::window::Icon::from_rgba(RGBA.to_vec(), 48, 48)
        .inspect_err(|e| log::warn!("failed to load window icon: {e}"))
        .ok()
}

/// Installs a system CJK font because egui's default fonts do not cover Chinese.
/// Tries common platform paths and falls back with a warning when none are available.
fn install_cjk_fonts(ctx: &egui::Context) {
    let mut candidates: Vec<std::path::PathBuf> = vec![
        // macOS
        "/System/Library/Fonts/PingFang.ttc".into(),
        "/System/Library/Fonts/Hiragino Sans GB.ttc".into(),
    ];
    // Windows fonts live under %WINDIR%\Fonts, which is not always on drive C.
    if let Some(windir) = std::env::var_os("WINDIR") {
        let fonts_dir = std::path::Path::new(&windir).join("Fonts");
        for name in ["msyh.ttc", "msyh.ttf", "simhei.ttf"] {
            candidates.push(fonts_dir.join(name));
        }
    }
    // Common Noto CJK paths on Linux distributions.
    candidates.push("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc".into());
    candidates.push("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into());
    for path in &candidates {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
        // Put the CJK font first for proportional text to align mixed-script
        // glyphs, but use it only as a monospace fallback for CJK coverage.
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            list.insert(0, "cjk".to_owned());
        }
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            list.push("cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        log::info!("loaded CJK font: {}", path.display());
        return;
    }
    log::warn!("no system CJK font found; Chinese UI text may not render correctly");
}
