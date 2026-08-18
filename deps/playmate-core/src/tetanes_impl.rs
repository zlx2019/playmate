//! [`NesCore`] implementation backed by [tetanes-core].
//!
//! [tetanes-core]: https://crates.io/crates/tetanes-core

use std::io::Cursor;
use std::path::PathBuf;

use tetanes_core::{
    common::ResetKind,
    control_deck::{Clocked, Config, ControlDeck},
    input::{JoypadBtn, Player as TetanesPlayer},
};

use crate::{Button, ButtonState, CoreError, NesCore, Player};

/// tetanes-core wrapper with default NTSC configuration and randomized power-on RAM.
pub struct TetanesCore {
    /// tetanes host object containing the CPU, PPU, APU, and bus.
    deck: ControlDeck,
}

impl TetanesCore {
    /// Creates a core with battery saves disabled; the deck performs no
    /// filesystem access of its own. Suitable for tests and headless use.
    pub fn new() -> Self {
        Self::with_sram_dir(None)
    }

    /// Creates a core that keeps battery-backed SRAM files under `sram_dir`.
    /// Loading a ROM restores its `.sram` file automatically when present;
    /// writing back happens through [`NesCore::persist_sram`].
    pub fn with_sram_dir(sram_dir: Option<PathBuf>) -> Self {
        let cfg = Config {
            sram_dir,
            ..Config::default()
        };
        Self {
            deck: ControlDeck::with_config(cfg),
        }
    }

    /// Maps a Playmate player slot to a tetanes player slot.
    fn map_player(player: Player) -> TetanesPlayer {
        match player {
            Player::One => TetanesPlayer::One,
            Player::Two => TetanesPlayer::Two,
        }
    }

    /// Maps a Playmate button to a tetanes button.
    fn map_button(button: Button) -> JoypadBtn {
        match button {
            Button::A => JoypadBtn::A,
            Button::B => JoypadBtn::B,
            Button::Select => JoypadBtn::Select,
            Button::Start => JoypadBtn::Start,
            Button::Up => JoypadBtn::Up,
            Button::Down => JoypadBtn::Down,
            Button::Left => JoypadBtn::Left,
            Button::Right => JoypadBtn::Right,
        }
    }
}

impl Default for TetanesCore {
    fn default() -> Self {
        Self::new()
    }
}

impl NesCore for TetanesCore {
    fn load_rom(&mut self, name: &str, bytes: &[u8]) -> Result<(), CoreError> {
        self.deck
            .load_rom(name, &mut Cursor::new(bytes))
            .map(|rom| {
                log::info!(
                    "ROM loaded: {} (region: {:?}, battery-backed: {})",
                    rom.name,
                    rom.region,
                    rom.battery_backed
                );
            })
            .map_err(|e| CoreError::LoadRom(e.to_string()))
    }

    fn clock_frame(&mut self) -> Result<(), CoreError> {
        // At speeds above 1x the deck owes several NES frames per display
        // frame and hands them out one call at a time; drain them here so one
        // call always advances one full display frame.
        loop {
            match self.deck.clock_frame() {
                Ok(Clocked::Continue) => {}
                Ok(_) => return Ok(()),
                Err(e) => return Err(CoreError::Clock(e.to_string())),
            }
        }
    }

    fn set_player_input(&mut self, player: Player, state: ButtonState) {
        let joypad = self.deck.joypad_mut(Self::map_player(player));
        // Replace the full state by applying all eight bits from the bitmap.
        for button in Button::ALL {
            joypad.set_button(Self::map_button(button), state.pressed(button));
        }
    }

    fn frame_buffer(&mut self) -> &[u8] {
        self.deck.frame_buffer()
    }

    fn audio_samples(&self) -> &[f32] {
        self.deck.audio_samples()
    }

    fn clear_audio_samples(&mut self) {
        self.deck.clear_audio_samples();
    }

    fn set_sample_rate(&mut self, rate: f32) {
        self.deck.set_sample_rate(rate);
    }

    fn set_frame_speed(&mut self, speed: f32) {
        self.deck.set_frame_speed(speed);
    }

    fn reset(&mut self) {
        self.deck.reset(ResetKind::Soft);
    }

    fn battery_backed(&self) -> bool {
        self.deck.cart_battery_backed().unwrap_or(false)
    }

    fn persist_sram(&mut self) -> Result<(), CoreError> {
        // save_sram_path is already a no-op without a battery; resolving the
        // path first also makes a disabled sram_dir a clean no-op.
        let Some(name) = self.deck.loaded_rom().map(|rom| rom.name.clone()) else {
            return Ok(());
        };
        let Some(path) = self.deck.sram_path(&name) else {
            return Ok(());
        };
        self.deck
            .save_sram_path(path)
            .map_err(|e| CoreError::Sram(e.to_string()))
    }

    fn save_state(&mut self) -> Result<Vec<u8>, CoreError> {
        let mut buf = Vec::new();
        self.deck
            .save_state(&mut buf)
            .map_err(|e| CoreError::State(e.to_string()))?;
        Ok(buf)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), CoreError> {
        self.deck
            .load_state(&mut Cursor::new(data))
            .map_err(|e| CoreError::State(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::FRAME_BYTES;

    /// Builds a minimal valid iNES ROM (NROM-128: 16 KiB PRG + 8 KiB CHR).
    /// The program loops forever at $8000, and all three interrupt vectors point there.
    fn synthetic_rom() -> Vec<u8> {
        // iNES header: "NES\x1A", one 16 KiB PRG bank, one 8 KiB CHR bank, all other flags zero.
        let mut rom = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

        let mut prg = vec![0u8; 16 * 1024];
        // $8000: JMP $8000 (0x4C, low byte, high byte), an infinite loop.
        prg[0] = 0x4C;
        prg[1] = 0x00;
        prg[2] = 0x80;
        // The NMI, RESET, and IRQ vectors at $FFFA-$FFFF all point to $8000.
        let vectors = prg.len() - 6;
        for i in 0..3 {
            prg[vectors + i * 2] = 0x00;
            prg[vectors + i * 2 + 1] = 0x80;
        }
        rom.extend_from_slice(&prg);
        rom.extend_from_slice(&[0u8; 8 * 1024]); // CHR ROM with blank tiles.
        rom
    }

    /// P1 and P2 inputs remain isolated in both directions.
    #[test]
    fn player_inputs_are_isolated() {
        let mut core = TetanesCore::new();
        core.load_rom("synthetic", &synthetic_rom()).unwrap();

        // Apply Up+A only to P2 and leave P1 with no input.
        let mut p2 = ButtonState::empty();
        p2.set(Button::Up, true);
        p2.set(Button::A, true);
        core.set_player_input(Player::One, ButtonState::empty());
        core.set_player_input(Player::Two, p2);

        // Inspect the actual state of both tetanes controllers.
        let p1_pad = core.deck.joypad(TetanesPlayer::One);
        assert!(
            !p1_pad.button(JoypadBtn::Up.into()),
            "P2 directional input leaked into P1"
        );
        assert!(
            !p1_pad.button(JoypadBtn::A.into()),
            "P2 A input leaked into P1"
        );
        let p2_pad = core.deck.joypad(TetanesPlayer::Two);
        assert!(p2_pad.button(JoypadBtn::Up.into()));
        assert!(p2_pad.button(JoypadBtn::A.into()));

        // Reverse direction: apply input only to P1; P2 must remain clear.
        let mut p1 = ButtonState::empty();
        p1.set(Button::Left, true);
        core.set_player_input(Player::One, p1);
        core.set_player_input(Player::Two, ButtonState::empty());
        assert!(
            core.deck
                .joypad(TetanesPlayer::One)
                .button(JoypadBtn::Left.into())
        );
        assert!(
            !core
                .deck
                .joypad(TetanesPlayer::Two)
                .button(JoypadBtn::Left.into())
        );
    }

    /// An instant state saved mid-game restores after further emulation,
    /// and corrupted state data is rejected without touching the console.
    #[test]
    fn save_and_load_state_roundtrip() {
        let mut core = TetanesCore::new();
        core.load_rom("synthetic", &synthetic_rom()).unwrap();
        for _ in 0..2 {
            core.clock_frame().unwrap();
        }

        let state = core.save_state().unwrap();
        assert!(!state.is_empty());
        for _ in 0..3 {
            core.clock_frame().unwrap();
        }
        core.load_state(&state).unwrap();
        // The restored console keeps running.
        core.clock_frame().unwrap();

        assert!(core.load_state(&[0u8; 16]).is_err());
    }

    /// The synthetic ROM has no battery, so SRAM persistence is a no-op.
    #[test]
    fn sram_noop_without_battery() {
        let mut core = TetanesCore::new();
        core.load_rom("synthetic", &synthetic_rom()).unwrap();
        assert!(!core.battery_backed());
        core.persist_sram().unwrap();
    }

    /// Smoke test: load a synthetic ROM, apply input, advance frames, and validate output.
    #[test]
    fn load_and_clock_synthetic_rom() {
        let mut core = TetanesCore::new();
        core.load_rom("synthetic", &synthetic_rom()).unwrap();
        core.set_sample_rate(48_000.0);

        let mut input = ButtonState::empty();
        input.set(Button::A, true);
        core.set_player_input(Player::One, input);
        core.set_player_input(Player::Two, ButtonState::empty());

        for _ in 0..5 {
            core.clock_frame().unwrap();
        }

        // The frame buffer must have the fixed RGBA8 size.
        assert_eq!(core.frame_buffer().len(), FRAME_BYTES);
        // Five emulated frames should produce audio samples (about 48000/60*5).
        assert!(!core.audio_samples().is_empty());
        core.clear_audio_samples();
        assert!(core.audio_samples().is_empty());
    }
}
