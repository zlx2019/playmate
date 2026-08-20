//! Game session containing all runtime resources for one game:
//! emulation thread, audio output, video texture, and merged keyboard/gamepad input.
//!
//! A session starts immediately and stops emulation and audio when dropped.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use playmate_core::{
    Button, ButtonState, NesCore, Player, SCREEN_HEIGHT, SCREEN_WIDTH, TetanesCore,
};
use winit::keyboard::KeyCode;

use crate::audio::{self, AudioRing};
use crate::config::{self, Config, InputConfig, InputMap};
use crate::emu::{
    self, CheatCmd, NetSink, STATE_SLOTS, SharedState, StatePaths, THUMB_HEIGHT, THUMB_WIDTH,
};
use crate::gamepad::GamepadInput;

/// How long a save/load result toast stays on screen.
const TOAST_DURATION: Duration = Duration::from_millis(2500);

/// Turbo cadence half-period: 33 ms pressed, 33 ms released, roughly 15
/// presses per second, which registers reliably with games polling input
/// once per frame.
const TURBO_HALF_PERIOD_MS: u128 = 33;

/// Whether turbo-held buttons fire at this instant of the session clock.
fn turbo_fire_on(since: Instant) -> bool {
    (since.elapsed().as_millis() / TURBO_HALF_PERIOD_MS).is_multiple_of(2)
}

/// Bitmap mask applied to plainly held buttons: a hold-turbo-enabled button
/// has its bit dropped during the off phase so holding it becomes a
/// turbo-cadence pulse train; every other button passes through.
const fn hold_turbo_mask(input: &InputConfig, fire: bool) -> u8 {
    if fire {
        return !0;
    }
    let mut mask = !0u8;
    if input.hold_turbo_a {
        mask &= !(1 << Button::A as u8);
    }
    if input.hold_turbo_b {
        mask &= !(1 << Button::B as u8);
    }
    mask
}

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
    /// Per-player keyboard turbo-held bitmaps, fired on the turbo cadence.
    turbo: [ButtonState; 2],
    /// Session clock driving the turbo cadence.
    started: Instant,
    /// Local player's slot in netplay. When set, local input writes only this
    /// slot and the network task writes the remote slot. `None` means local play.
    net_local_slot: Option<Player>,
    /// Manual state-slot files, kept for the pause menu's occupancy markers.
    state_slots: [std::path::PathBuf; STATE_SLOTS],
    /// Active save/load result toast with its display start time.
    toast: Option<(String, Instant)>,
}

impl PlaySession {
    /// Loads a ROM for local play and starts emulation and audio, applying
    /// the game's enabled cheats and the configured video filter.
    pub fn start(rom_path: &Path, cfg: &Config) -> anyhow::Result<Self> {
        Self::start_with(rom_path, None, cfg, false)
    }

    /// Like [`start`](Self::start), but restores the auto snapshot written
    /// when the previous session of this game ended. A missing or unreadable
    /// snapshot degrades to a fresh start.
    pub fn resume(rom_path: &Path, cfg: &Config) -> anyhow::Result<Self> {
        Self::start_with(rom_path, None, cfg, true)
    }

    /// Starts host-mode netplay, using `local_slot` locally and sending media through `sink`.
    pub fn start_networked(
        rom_path: &Path,
        local_slot: Player,
        sink: NetSink,
        cfg: &Config,
    ) -> anyhow::Result<Self> {
        Self::start_with(rom_path, Some((local_slot, sink)), cfg, false)
    }

    /// Shared startup path.
    fn start_with(
        rom_path: &Path,
        net: Option<(Player, NetSink)>,
        cfg: &Config,
        resume: bool,
    ) -> anyhow::Result<Self> {
        let rom_bytes = std::fs::read(rom_path)
            .with_context(|| format!("无法读取 ROM 文件: {}", rom_path.display()))?;
        let rom_title = rom_path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| rom_path.display().to_string());

        // Battery saves live in the saves directory; loading the ROM restores
        // its .sram file automatically when one exists.
        let saves_dir = config::saves_dir();
        let mut core = TetanesCore::with_sram_dir(Some(saves_dir.clone()));
        core.load_rom(&rom_title, &rom_bytes)
            .with_context(|| format!("加载 ROM 失败: {rom_title}"))?;

        // Apply the game's enabled cheat codes before the thread takes the core.
        for code in cfg.enabled_cheats(&rom_title) {
            if let Err(e) = core.add_genie_code(&code) {
                log::warn!("skipping stored cheat {code}: {e}");
            }
        }

        let ring = Arc::new(AudioRing::new());
        let (audio_stream, sample_rate) = audio::start(Arc::clone(&ring), None)?;
        core.set_sample_rate(sample_rate as f32);

        let (net_local_slot, net_sink) = match net {
            Some((slot, sink)) => (Some(slot), Some(sink)),
            None => (None, None),
        };
        // Slot 1 keeps the pre-multi-slot file name for compatibility.
        let paths = StatePaths {
            slots: [
                saves_dir.join(format!("{rom_title}.state")),
                saves_dir.join(format!("{rom_title}.slot2.state")),
                saves_dir.join(format!("{rom_title}.slot3.state")),
            ],
            auto: saves_dir.join(format!("{rom_title}.auto.state")),
        };
        let state_slots = paths.slots.clone();

        // Quick resume: restore the snapshot from the previous session end.
        if resume {
            match std::fs::read(&paths.auto) {
                Ok(data) => match core.load_state(&data) {
                    Ok(()) => {
                        // The snapshot carries the previous session's APU
                        // sample rate; the current device may differ.
                        core.set_sample_rate(sample_rate as f32);
                        log::info!("resumed from auto state {:?}", paths.auto);
                    }
                    Err(e) => log::warn!("ignoring unreadable auto state: {e}"),
                },
                Err(e) => log::info!("no auto state ({e}); starting fresh"),
            }
        }

        let shared = Arc::new(SharedState::new());
        shared
            .ntsc_filter
            .store(cfg.video.ntsc_filter, Ordering::Relaxed);
        let emu_shared = Arc::clone(&shared);
        let emu_handle = std::thread::spawn(move || {
            emu::run_emulation(core, emu_shared, ring, net_sink, Some(paths));
        });

        log::info!("game started: {rom_title}");
        Ok(Self {
            rom_title,
            sample_rate,
            shared,
            emu_handle: Some(emu_handle),
            _audio_stream: audio_stream,
            texture: None,
            keyboard: [ButtonState::empty(); 2],
            turbo: [ButtonState::empty(); 2],
            started: Instant::now(),
            net_local_slot,
            state_slots,
            toast: None,
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
        let Some((player, key)) = input_map.lookup(code) else {
            return false;
        };
        let slot = Self::index(self.net_local_slot.unwrap_or(player));
        if key.is_turbo() {
            self.turbo[slot].set(key.button(), pressed);
        } else {
            self.keyboard[slot].set(key.button(), pressed);
        }
        true
    }

    /// Merges keyboard and gamepad bitmaps and publishes them once per frame,
    /// overlaying turbo-held buttons during the on phase of the turbo cadence.
    /// `input` selects which plainly held buttons also pulse on that cadence.
    /// Local play controls both slots; netplay writes only the local slot.
    /// `blocked` publishes released buttons instead, used while the in-game
    /// menu is open; a remote player's slot is never touched.
    pub fn sync_input(&self, gamepad: &GamepadInput, blocked: bool, input: &InputConfig) {
        let fire = turbo_fire_on(self.started);
        let mask = hold_turbo_mask(input, fire);
        match self.net_local_slot {
            None => {
                for player in [Player::One, Player::Two] {
                    let i = Self::index(player);
                    let mut merged =
                        (self.keyboard[i].bits() | gamepad.state(player).bits()) & mask;
                    if fire {
                        merged |= self.turbo[i].bits() | gamepad.turbo(player).bits();
                    }
                    let value = if blocked { 0 } else { merged };
                    self.buttons_cell(player).store(value, Ordering::Relaxed);
                }
            }
            Some(local) => {
                // In netplay, the first local gamepad controls the local slot.
                let i = Self::index(local);
                let mut merged =
                    (self.keyboard[i].bits() | gamepad.state(Player::One).bits()) & mask;
                if fire {
                    merged |= self.turbo[i].bits() | gamepad.turbo(Player::One).bits();
                }
                let value = if blocked { 0 } else { merged };
                self.buttons_cell(local).store(value, Ordering::Relaxed);
            }
        }
    }

    /// Releases all locally held keyboard buttons, used when the in-game menu
    /// opens so keys held at that moment do not stay latched.
    pub fn clear_input(&mut self) {
        self.keyboard = [ButtonState::empty(); 2];
        self.turbo = [ButtonState::empty(); 2];
    }

    /// Pauses or resumes the emulation thread. Only local play pauses; a
    /// netplay host keeps running because peers cannot be paused.
    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    /// Sets the emulation speed multiplier; 1 restores normal speed.
    pub fn set_speed(&self, multiplier: u8) {
        self.shared
            .speed
            .store(multiplier.max(1), Ordering::Relaxed);
    }

    /// Queues applying a validated Game Genie code on the emulation thread.
    pub fn add_cheat(&self, code: String) {
        if let Ok(mut cmds) = self.shared.cheat_cmds.lock() {
            cmds.push(CheatCmd::Add(code));
        }
    }

    /// Queues removing an applied Game Genie code.
    pub fn remove_cheat(&self, code: String) {
        if let Ok(mut cmds) = self.shared.cheat_cmds.lock() {
            cmds.push(CheatCmd::Remove(code));
        }
    }

    /// Whether fast-forward is currently engaged.
    pub fn is_fast_forward(&self) -> bool {
        self.shared.speed.load(Ordering::Relaxed) > 1
    }

    /// Applies the video filter choice to the running session.
    pub fn set_ntsc_filter(&self, enabled: bool) {
        self.shared.ntsc_filter.store(enabled, Ordering::Relaxed);
    }

    /// Engages or releases hold-to-rewind; the emulation thread rolls back
    /// through recent snapshots while set. Local play only.
    pub fn set_rewinding(&self, on: bool) {
        self.shared.rewinding.store(on, Ordering::Relaxed);
    }

    /// Whether rewind is currently engaged.
    pub fn is_rewinding(&self) -> bool {
        self.shared.rewinding.load(Ordering::Relaxed)
    }

    /// Requests an instant state save into a one-based slot,
    /// served by the emulation thread.
    pub fn request_save_state(&self, slot: u8) {
        self.shared
            .save_state_req
            .store(slot.clamp(1, STATE_SLOTS as u8), Ordering::Relaxed);
    }

    /// Requests restoring a one-based slot, served by the emulation thread.
    pub fn request_load_state(&self, slot: u8) {
        self.shared
            .load_state_req
            .store(slot.clamp(1, STATE_SLOTS as u8), Ordering::Relaxed);
    }

    /// Returns a manual slot's state file path for a one-based slot number.
    fn slot_file(&self, slot: u8) -> Option<&std::path::PathBuf> {
        usize::from(slot)
            .checked_sub(1)
            .and_then(|i| self.state_slots.get(i))
    }

    /// Modification time of a slot's state file; `None` means the slot is empty.
    pub fn slot_mtime(&self, slot: u8) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.slot_file(slot)?)
            .ok()?
            .modified()
            .ok()
    }

    /// Raw RGBA thumbnail for a slot, when present and well-formed.
    /// Saves made before thumbnails existed simply have no preview.
    pub fn slot_thumb(&self, slot: u8) -> Option<Vec<u8>> {
        let data = std::fs::read(self.slot_file(slot)?.with_extension("thumb")).ok()?;
        (data.len() == THUMB_WIDTH * THUMB_HEIGHT * 4).then_some(data)
    }

    /// Deletes a manual slot's saved state and reports the outcome as a toast.
    /// Runs on the UI thread; the emulation thread only touches these files
    /// while serving an explicit save/load request.
    pub fn delete_state(&self, slot: u8) {
        let Some(path) = self.slot_file(slot) else {
            return;
        };
        // The thumbnail goes with the save; ignore a missing one.
        let _ = std::fs::remove_file(path.with_extension("thumb"));
        let msg = match std::fs::remove_file(path) {
            Ok(()) => format!("已删除槽 {slot} 存档"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                format!("槽 {slot} 暂无存档")
            }
            Err(e) => format!("删除失败: {e}"),
        };
        if let Ok(mut status) = self.shared.status.lock() {
            *status = Some(msg);
        }
    }

    /// Returns the shared input cell for a player slot.
    fn buttons_cell(&self, player: Player) -> &std::sync::atomic::AtomicU8 {
        match player {
            Player::One => &self.shared.p1_buttons,
            Player::Two => &self.shared.p2_buttons,
        }
    }

    /// Draws the game frame and any transient save/load result toast.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if let Ok(fb) = self.shared.framebuffer.lock() {
            render_frame_texture(ui, &mut self.texture, &fb);
        }
        self.show_toast(ui.ctx());
    }

    /// Picks up the latest emulation status message and draws it above the
    /// frame for a short time.
    fn show_toast(&mut self, ctx: &egui::Context) {
        if let Ok(mut status) = self.shared.status.lock()
            && let Some(msg) = status.take()
        {
            self.toast = Some((msg, Instant::now()));
        }
        let Some((msg, since)) = &self.toast else {
            return;
        };
        if since.elapsed() >= TOAST_DURATION {
            self.toast = None;
            return;
        }
        egui::Area::new(egui::Id::new("emu-status-toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -48.0])
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new(msg).strong());
                });
            });
        // Keep repainting while the toast is visible so it expires even when
        // the game is paused and nothing else triggers a redraw.
        ctx.request_repaint_after(Duration::from_millis(100));
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
    /// Keyboard turbo-held bitmap, fired on the turbo cadence.
    turbo: ButtonState,
    /// Session clock driving the turbo cadence.
    started: Instant,
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
            turbo: ButtonState::empty(),
            started: Instant::now(),
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
        let Some((_player, key)) = input_map.lookup(code) else {
            return false;
        };
        if key.is_turbo() {
            self.turbo.set(key.button(), pressed);
        } else {
            self.keyboard.set(key.button(), pressed);
        }
        true
    }

    /// Releases all locally held keyboard buttons, used when the in-game menu
    /// opens so keys held at that moment do not stay latched.
    pub fn clear_input(&mut self) {
        self.keyboard = ButtonState::empty();
        self.turbo = ButtonState::empty();
    }

    /// Merges keyboard and gamepad input, overlaying turbo-held buttons on the
    /// turbo cadence, and returns changes for the network task.
    /// `input` selects which plainly held buttons also pulse on that cadence.
    /// Spectators never produce outgoing input. `blocked` reports released
    /// buttons instead, used while the in-game menu is open.
    pub fn poll_outgoing(
        &mut self,
        gamepad: &GamepadInput,
        blocked: bool,
        input: &InputConfig,
    ) -> Option<u8> {
        if self.spectator {
            return None;
        }
        let merged = if blocked {
            0
        } else {
            let fire = turbo_fire_on(self.started);
            let mut bits = (self.keyboard.bits() | gamepad.state(Player::One).bits())
                & hold_turbo_mask(input, fire);
            if fire {
                bits |= self.turbo.bits() | gamepad.turbo(Player::One).bits();
            }
            bits
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the config with the given per-button hold-turbo switches.
    fn input(hold_a: bool, hold_b: bool) -> InputConfig {
        InputConfig {
            hold_turbo_a: hold_a,
            hold_turbo_b: hold_b,
        }
    }

    /// Hold-turbo masks exactly the enabled buttons during the off phase
    /// and nothing otherwise.
    #[test]
    fn hold_turbo_mask_pulses_only_enabled_buttons() {
        // Disabled or on-phase: every button passes through.
        assert_eq!(hold_turbo_mask(&input(false, false), false), !0);
        assert_eq!(hold_turbo_mask(&input(false, false), true), !0);
        assert_eq!(hold_turbo_mask(&input(true, true), true), !0);

        // Off phase: only the enabled button is dropped, the rest pass.
        let mut held = ButtonState::empty();
        held.set(Button::A, true);
        held.set(Button::B, true);
        held.set(Button::Right, true);
        held.set(Button::Start, true);
        let mut without_a = held;
        without_a.set(Button::A, false);
        let mut without_b = held;
        without_b.set(Button::B, false);
        let mut without_ab = without_a;
        without_ab.set(Button::B, false);

        assert_eq!(
            held.bits() & hold_turbo_mask(&input(true, false), false),
            without_a.bits()
        );
        assert_eq!(
            held.bits() & hold_turbo_mask(&input(false, true), false),
            without_b.bits()
        );
        assert_eq!(
            held.bits() & hold_turbo_mask(&input(true, true), false),
            without_ab.bits()
        );
    }
}
