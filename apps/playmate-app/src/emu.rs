//! Emulation thread: drives the NES core at NTSC speed and publishes video and audio.
//!
//! All communication with the main window/input thread uses [`SharedState`]:
//! - Input: two `AtomicU8` button bitmaps, written by the main or network thread
//!   and read by the emulation thread.
//! - Video: a `Mutex<Vec<u8>>` containing the latest RGBA8 frame, written by
//!   emulation and read by rendering.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use playmate_core::{ButtonState, FRAME_BYTES, NTSC_FPS, NesCore, Player};

use crate::audio::AudioRing;

/// Media output channels from emulation to the network sender in host mode.
pub struct NetSink {
    /// Raw RGBA frames; the small bounded queue may drop frames rather than block emulation.
    pub frame_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    /// Audio samples use an unbounded queue because dropping them causes audible artifacts.
    pub audio_tx: tokio::sync::mpsc::UnboundedSender<Vec<f32>>,
}

/// State shared between the main and emulation threads.
pub struct SharedState {
    /// Current P1 button bitmap as a raw [`ButtonState`] byte.
    pub p1_buttons: AtomicU8,
    /// Current P2 button bitmap.
    pub p2_buttons: AtomicU8,
    /// Global running flag; clearing it stops emulation at the next frame boundary.
    pub running: AtomicBool,
    /// Latest RGBA8 frame, always [`FRAME_BYTES`] bytes.
    pub framebuffer: Mutex<Vec<u8>>,
}

impl SharedState {
    /// Creates the initial running state with a black frame and no input.
    pub fn new() -> Self {
        Self {
            p1_buttons: AtomicU8::new(0),
            p2_buttons: AtomicU8::new(0),
            running: AtomicBool::new(true),
            framebuffer: Mutex::new(vec![0u8; FRAME_BYTES]),
        }
    }
}

/// Emulation loop: apply input, advance the core, publish media, and wait for the next frame.
///
/// An absolute deadline accumulator prevents sleep jitter from accumulating and
/// keeps the long-term frame rate locked to NTSC's 60.0988 fps.
pub fn run_emulation(
    mut core: impl NesCore,
    shared: Arc<SharedState>,
    ring: Arc<AudioRing>,
    net: Option<NetSink>,
) {
    let frame_dur = Duration::from_secs_f64(1.0 / NTSC_FPS);
    let mut next = Instant::now() + frame_dur;

    while shared.running.load(Ordering::Relaxed) {
        // 1. Replace both players' input with the latest state.
        let p1 = ButtonState::from_bits(shared.p1_buttons.load(Ordering::Relaxed));
        let p2 = ButtonState::from_bits(shared.p2_buttons.load(Ordering::Relaxed));
        core.set_player_input(Player::One, p1);
        core.set_player_input(Player::Two, p2);

        // 2. Advance emulation by one frame.
        if let Err(e) = core.clock_frame() {
            log::error!("failed to advance emulation; thread exiting: {e}");
            break;
        }

        // 3. Publish the latest frame with one short copy under the lock.
        if let Ok(mut fb) = shared.framebuffer.lock() {
            fb.copy_from_slice(core.frame_buffer());
        }

        // 3.5 In host mode, send frames and audio to the network task.
        // Frames use try_send and may be dropped on a slow network; audio must be delivered.
        if let Some(net) = &net {
            let _ = net.frame_tx.try_send(core.frame_buffer().to_vec());
            let _ = net.audio_tx.send(core.audio_samples().to_vec());
        }

        // 4. Push this frame's audio samples and clear the core buffer.
        ring.push(core.audio_samples());
        core.clear_audio_samples();

        // 5. Wait for the absolute next-frame deadline. Reset after a 250 ms lag,
        // such as system sleep, to avoid an unbounded catch-up loop.
        let now = Instant::now();
        if next > now {
            sleep_until(next);
        } else if now.duration_since(next) > Duration::from_millis(250) {
            next = now;
        }
        next += frame_dur;
    }
    log::info!("emulation thread exited");
}

/// Sleeps until `deadline` with sub-millisecond accuracy.
///
/// The Windows scheduler tick makes `thread::sleep` overshoot by up to ~15 ms,
/// which turns the frame cadence into visible judder. Sleep short of the
/// deadline there and spin the final stretch. macOS and Linux sleeps are
/// already sub-millisecond accurate, so a plain sleep avoids burning CPU.
#[cfg(target_os = "windows")]
fn sleep_until(deadline: Instant) {
    const SPIN_MARGIN: Duration = Duration::from_millis(3);
    if let Some(remaining) = deadline.checked_duration_since(Instant::now())
        && remaining > SPIN_MARGIN
    {
        thread::sleep(remaining - SPIN_MARGIN);
    }
    while Instant::now() < deadline {
        std::hint::spin_loop();
    }
}

#[cfg(not(target_os = "windows"))]
fn sleep_until(deadline: Instant) {
    if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        thread::sleep(remaining);
    }
}
