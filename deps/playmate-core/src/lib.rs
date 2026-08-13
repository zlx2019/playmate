//! playmate-core: abstraction layer for the NES emulation core.
//!
//! The [`NesCore`] trait isolates the concrete emulation core implementation
//! (currently tetanes-core). Higher layers (rendering, audio, and networking)
//! depend only on this crate's types and interfaces, so the core can be
//! replaced with an in-house implementation later.

mod tetanes_impl;
mod types;

pub use tetanes_impl::TetanesCore;
pub use types::{Button, ButtonState, FRAME_BYTES, NTSC_FPS, Player, SCREEN_HEIGHT, SCREEN_WIDTH};

/// Common error type for the emulation core.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// ROM loading failed due to an invalid file, unsupported mapper, or similar issue.
    #[error("加载 ROM 失败: {0}")]
    LoadRom(String),
    /// Advancing the emulation failed.
    #[error("模拟执行出错: {0}")]
    Clock(String),
}

/// Common interface for an NES emulation core.
///
/// Typical per-frame sequence:
/// 1. Call [`set_player_input`](NesCore::set_player_input) with each player's current input.
/// 2. Call [`clock_frame`](NesCore::clock_frame) to advance the emulation by one frame.
/// 3. Read [`frame_buffer`](NesCore::frame_buffer) for rendering.
/// 4. Send [`audio_samples`](NesCore::audio_samples) to the audio output, then clear them
///    with [`clear_audio_samples`](NesCore::clear_audio_samples).
pub trait NesCore {
    /// Loads an iNES ROM from memory; `name` is used only for logs and save-file naming.
    fn load_rom(&mut self, name: &str, bytes: &[u8]) -> Result<(), CoreError>;

    /// Advances the emulation by one full frame (1/60.0988 seconds).
    fn clock_frame(&mut self) -> Result<(), CoreError>;

    /// Replaces the specified player's current controller state.
    fn set_player_input(&mut self, player: Player, state: ButtonState);

    /// Returns the current RGBA8 frame buffer, always [`FRAME_BYTES`] bytes long.
    fn frame_buffer(&mut self) -> &[u8];

    /// Returns the mono f32 audio samples accumulated since the last clear.
    fn audio_samples(&self) -> &[f32];

    /// Clears the audio sample buffer after the current frame has been consumed.
    fn clear_audio_samples(&mut self);

    /// Sets the audio output sample rate in Hz; it must match the actual audio device.
    fn set_sample_rate(&mut self, rate: f32);

    /// Performs a soft reset, equivalent to pressing Reset on the console.
    fn reset(&mut self);
}
