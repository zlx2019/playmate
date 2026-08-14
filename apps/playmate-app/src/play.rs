//! Game session containing all runtime resources for one game:
//! emulation thread, audio output, video texture, and merged keyboard/gamepad input.
//!
//! A session starts immediately and stops emulation and audio when dropped.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Context;
use playmate_core::{ButtonState, NesCore, Player, SCREEN_HEIGHT, SCREEN_WIDTH, TetanesCore};
use winit::keyboard::KeyCode;

use crate::audio::{self, AudioRing};
use crate::config::InputMap;
use crate::emu::{self, NetSink, SharedState};
use crate::gamepad::GamepadInput;

/// A running game session.
pub struct PlaySession {
    /// Game title derived from the file name for display.
    pub rom_title: String,
    /// Audio output sample rate, sent to clients in `GameStart`.
    pub sample_rate: u32,
    /// Input and video state shared with the emulation thread.
    shared: Arc<SharedState>,
    /// Emulation thread handle, joined when the session stops.
    emu_handle: Option<JoinHandle<()>>,
    /// Audio stream handle retained to keep playback alive.
    _audio_stream: cpal::Stream,
    /// Game texture, created lazily on the first frame.
    texture: Option<egui::TextureHandle>,
    /// Per-player keyboard bitmaps, maintained separately from gamepad input.
    keyboard: [ButtonState; 2],
    /// Local player's slot in netplay. When set, local input writes only this
    /// slot and the network task writes the remote slot. `None` means local play.
    net_local_slot: Option<Player>,
}

impl PlaySession {
    /// Loads a ROM for local play and starts emulation and audio.
    pub fn start(rom_path: &Path) -> anyhow::Result<Self> {
        Self::start_with(rom_path, None)
    }

    /// Starts host-mode netplay, using `local_slot` locally and sending media through `sink`.
    pub fn start_networked(
        rom_path: &Path,
        local_slot: Player,
        sink: NetSink,
    ) -> anyhow::Result<Self> {
        Self::start_with(rom_path, Some((local_slot, sink)))
    }

    /// Shared startup path.
    fn start_with(rom_path: &Path, net: Option<(Player, NetSink)>) -> anyhow::Result<Self> {
        let rom_bytes = std::fs::read(rom_path)
            .with_context(|| format!("无法读取 ROM 文件: {}", rom_path.display()))?;
        let rom_title = rom_path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| rom_path.display().to_string());

        let mut core = TetanesCore::new();
        core.load_rom(&rom_title, &rom_bytes)
            .with_context(|| format!("加载 ROM 失败: {rom_title}"))?;

        let ring = Arc::new(AudioRing::new());
        let (audio_stream, sample_rate) = audio::start(Arc::clone(&ring), None)?;
        core.set_sample_rate(sample_rate as f32);

        let (net_local_slot, net_sink) = match net {
            Some((slot, sink)) => (Some(slot), Some(sink)),
            None => (None, None),
        };
        let shared = Arc::new(SharedState::new());
        let emu_shared = Arc::clone(&shared);
        let emu_handle =
            std::thread::spawn(move || emu::run_emulation(core, emu_shared, ring, net_sink));

        log::info!("game started: {rom_title}");
        Ok(Self {
            rom_title,
            sample_rate,
            shared,
            emu_handle: Some(emu_handle),
            _audio_stream: audio_stream,
            texture: None,
            keyboard: [ButtonState::empty(); 2],
            net_local_slot,
        })
    }

    /// Returns host-mode shared state so the network task can write remote input.
    pub fn shared_state(&self) -> Arc<SharedState> {
        Arc::clone(&self.shared)
    }

    /// Maps a player to its array index.
    fn index(player: Player) -> usize {
        match player {
            Player::One => 0,
            Player::Two => 1,
        }
    }

    /// Handles a gameplay keyboard event and returns whether a binding consumed it.
    ///
    /// Local play writes each binding to its configured player slot. In netplay,
    /// both P1 and P2 key layouts map to the local slot. Pressing the same logical
    /// button through both layouts and releasing one early is a minor edge case.
    pub fn on_key(&mut self, input_map: &InputMap, code: KeyCode, pressed: bool) -> bool {
        let Some((player, button)) = input_map.lookup(code) else {
            return false;
        };
        let slot = self.net_local_slot.unwrap_or(player);
        self.keyboard[Self::index(slot)].set(button, pressed);
        true
    }

    /// Merges keyboard and gamepad bitmaps and publishes them once per frame.
    /// Local play controls both slots; netplay writes only the local slot.
    pub fn sync_input(&self, gamepad: &GamepadInput) {
        match self.net_local_slot {
            None => {
                for player in [Player::One, Player::Two] {
                    let merged =
                        self.keyboard[Self::index(player)].bits() | gamepad.state(player).bits();
                    self.buttons_cell(player).store(merged, Ordering::Relaxed);
                }
            }
            Some(local) => {
                // In netplay, the first local gamepad controls the local slot.
                let merged =
                    self.keyboard[Self::index(local)].bits() | gamepad.state(Player::One).bits();
                self.buttons_cell(local).store(merged, Ordering::Relaxed);
            }
        }
    }

    /// Returns the shared input cell for a player slot.
    fn buttons_cell(&self, player: Player) -> &std::sync::atomic::AtomicU8 {
        match player {
            Player::One => &self.shared.p1_buttons,
            Player::Two => &self.shared.p2_buttons,
        }
    }

    /// Draws the game frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let Ok(fb) = self.shared.framebuffer.lock() else {
            return;
        };
        render_frame_texture(ui, &mut self.texture, &fb);
    }
}

/// Uploads and draws an RGBA frame, scaling proportionally within the available
/// area at no less than 1x, centering it, and using nearest-neighbor sampling.
/// Shared by host and client gameplay views.
fn render_frame_texture(ui: &mut egui::Ui, texture: &mut Option<egui::TextureHandle>, fb: &[u8]) {
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [SCREEN_WIDTH as usize, SCREEN_HEIGHT as usize],
        fb,
    );
    let texture = match texture {
        Some(t) => {
            t.set(image, egui::TextureOptions::NEAREST);
            t
        }
        None => texture.insert(ui.ctx().load_texture(
            "nes-frame",
            image,
            egui::TextureOptions::NEAREST,
        )),
    };

    let avail = ui.available_size();
    let scale = (avail.x / SCREEN_WIDTH as f32)
        .min(avail.y / SCREEN_HEIGHT as f32)
        .max(1.0);
    let size = egui::vec2(SCREEN_WIDTH as f32 * scale, SCREEN_HEIGHT as f32 * scale);
    ui.centered_and_justified(|ui| {
        ui.add(egui::Image::new(&*texture).fit_to_exact_size(size));
    });
}

/// Client netplay session that renders host video, plays audio, and sends local input.
/// Emulation runs on the host, so this session does not own an NES core.
pub struct GuestPlay {
    /// Game title for display.
    pub rom_title: String,
    /// Latest frame continuously updated by the network task.
    framebuffer: Arc<Mutex<Vec<u8>>>,
    /// Audio stream handle retained to keep playback alive.
    _audio_stream: cpal::Stream,
    /// Lazily created frame texture.
    texture: Option<egui::TextureHandle>,
    /// Local keyboard bitmap; both configured layouts map to the local character.
    keyboard: ButtonState,
    /// Last merged bitmap sent, used to avoid sending unchanged input every frame.
    last_sent: u8,
    /// Spectators watch the stream and never produce input.
    spectator: bool,
}

impl GuestPlay {
    /// Creates a client session and opens audio at the host's sample rate.
    pub fn start(
        rom_title: String,
        my_slot: Player,
        framebuffer: Arc<Mutex<Vec<u8>>>,
        ring: Arc<AudioRing>,
        sample_rate: u32,
        spectator: bool,
    ) -> anyhow::Result<Self> {
        // `audio::start` resamples internally if the device rejects the host
        // rate, so the ring is always consumed at `sample_rate`.
        let (audio_stream, _) = audio::start(ring, Some(sample_rate))?;
        if spectator {
            log::info!("netplay spectating started: {rom_title}");
        } else {
            log::info!("netplay started: {rom_title} (local slot: {my_slot:?})");
        }
        Ok(Self {
            rom_title,
            framebuffer,
            _audio_stream: audio_stream,
            texture: None,
            keyboard: ButtonState::empty(),
            last_sent: 0,
            spectator,
        })
    }

    /// Whether this session is watch-only.
    pub fn is_spectator(&self) -> bool {
        self.spectator
    }

    /// Handles a keyboard event and returns whether it was consumed.
    /// In netplay, both configured keyboard layouts control the local player.
    pub fn on_key(&mut self, input_map: &InputMap, code: KeyCode, pressed: bool) -> bool {
        if self.spectator {
            return false;
        }
        let Some((_player, button)) = input_map.lookup(code) else {
            return false;
        };
        self.keyboard.set(button, pressed);
        true
    }

    /// Merges keyboard and gamepad input, returning changes for the network task.
    /// Spectators never produce outgoing input.
    pub fn poll_outgoing(&mut self, gamepad: &GamepadInput) -> Option<u8> {
        if self.spectator {
            return None;
        }
        let merged = self.keyboard.bits() | gamepad.state(Player::One).bits();
        if merged != self.last_sent {
            self.last_sent = merged;
            Some(merged)
        } else {
            None
        }
    }

    /// Draws the frame streamed by the host.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let Ok(fb) = self.framebuffer.lock() else {
            return;
        };
        render_frame_texture(ui, &mut self.texture, &fb);
    }
}

impl Drop for PlaySession {
    fn drop(&mut self) {
        // Stop and join emulation so no background thread retains the audio buffer.
        self.shared.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.emu_handle.take()
            && handle.join().is_err()
        {
            log::error!("emulation thread terminated unexpectedly");
        }
        log::info!("game session ended: {}", self.rom_title);
    }
}
