//! Emulation thread: drives the NES core at NTSC speed and publishes video and audio.
//!
//! All communication with the main window/input thread uses [`SharedState`]:
//! - Input: two `AtomicU8` button bitmaps, written by the main or network thread
//!   and read by the emulation thread.
//! - Video: a `Mutex<Vec<u8>>` containing the latest RGBA8 frame, written by
//!   emulation and read by rendering.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use playmate_core::{ButtonState, FRAME_BYTES, NTSC_FPS, NesCore, Player};

use crate::audio::AudioRing;

/// Polling interval while paused, balancing resume latency and idle CPU use.
const PAUSE_POLL: Duration = Duration::from_millis(25);

/// Interval between periodic battery-SRAM writes, bounding progress lost to a
/// crash; a final write also happens when the session ends.
const SRAM_AUTOSAVE: Duration = Duration::from_secs(60);

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
    /// Pause flag; the emulation thread idles without stepping while set.
    pub paused: AtomicBool,
    /// One-shot request to write an instant save state.
    pub save_state_req: AtomicBool,
    /// One-shot request to restore the instant save state.
    pub load_state_req: AtomicBool,
    /// Latest save/load result message, taken by the UI for a transient toast.
    pub status: Mutex<Option<String>>,
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
            paused: AtomicBool::new(false),
            save_state_req: AtomicBool::new(false),
            load_state_req: AtomicBool::new(false),
            status: Mutex::new(None),
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
    state_path: Option<PathBuf>,
) {
    let frame_dur = Duration::from_secs_f64(1.0 / NTSC_FPS);
    let mut next = Instant::now() + frame_dur;
    let mut last_sram_save = Instant::now();

    while shared.running.load(Ordering::Relaxed) {
        // 0. Serve instant save/load requests before the pause check, so the
        // pause-menu actions work while local play is paused.
        handle_state_requests(&mut core, &shared, state_path.as_deref());

        // 1. Paused (local play): idle without stepping and hold the frame
        // clock so resuming does not fast-forward the missed frames.
        if shared.paused.load(Ordering::Relaxed) {
            thread::sleep(PAUSE_POLL);
            next = Instant::now() + frame_dur;
            continue;
        }

        // 2. Replace both players' input with the latest state.
        let p1 = ButtonState::from_bits(shared.p1_buttons.load(Ordering::Relaxed));
        let p2 = ButtonState::from_bits(shared.p2_buttons.load(Ordering::Relaxed));
        core.set_player_input(Player::One, p1);
        core.set_player_input(Player::Two, p2);

        // 3. Advance emulation by one frame.
        if let Err(e) = core.clock_frame() {
            log::error!("failed to advance emulation; thread exiting: {e}");
            break;
        }

        // 4. Publish the latest frame with one short copy under the lock.
        if let Ok(mut fb) = shared.framebuffer.lock() {
            fb.copy_from_slice(core.frame_buffer());
        }

        // 4.5 In host mode, send frames and audio to the network task.
        // Frames use try_send and may be dropped on a slow network; audio must be delivered.
        if let Some(net) = &net {
            let _ = net.frame_tx.try_send(core.frame_buffer().to_vec());
            let _ = net.audio_tx.send(core.audio_samples().to_vec());
        }

        // 5. Push this frame's audio samples and clear the core buffer.
        ring.push(core.audio_samples());
        core.clear_audio_samples();

        // 6. Periodically flush battery SRAM; a no-op without a battery.
        if last_sram_save.elapsed() >= SRAM_AUTOSAVE {
            last_sram_save = Instant::now();
            if let Err(e) = core.persist_sram() {
                log::warn!("periodic battery SRAM save failed: {e}");
            }
        }

        // 7. Wait for the absolute next-frame deadline. Reset after a 250 ms lag,
        // such as system sleep, to avoid an unbounded catch-up loop.
        let now = Instant::now();
        if next > now {
            sleep_until(next);
        } else if now.duration_since(next) > Duration::from_millis(250) {
            next = now;
        }
        next += frame_dur;
    }

    // Final SRAM write so battery-backed progress survives closing the game.
    if let Err(e) = core.persist_sram() {
        log::error!("failed to save battery SRAM on exit: {e}");
    }
    log::info!("emulation thread exited");
}

/// Serves one-shot instant save/load requests from the UI thread and reports
/// the outcome through [`SharedState::status`].
fn handle_state_requests(core: &mut impl NesCore, shared: &SharedState, state_path: Option<&Path>) {
    let Some(path) = state_path else {
        return;
    };
    if shared.save_state_req.swap(false, Ordering::Relaxed) {
        let msg = match save_state_file(core, path) {
            Ok(()) => "已存档".to_string(),
            Err(e) => e,
        };
        publish_status(shared, msg);
    }
    if shared.load_state_req.swap(false, Ordering::Relaxed) {
        let msg = match load_state_file(core, shared, path) {
            Ok(()) => "已读档".to_string(),
            Err(e) => e,
        };
        publish_status(shared, msg);
    }
}

/// Serializes the console state into the state file, creating its directory.
fn save_state_file(core: &mut impl NesCore, path: &Path) -> Result<(), String> {
    let data = core.save_state().map_err(|e| format!("存档失败: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("存档失败: 创建目录出错 {e}"))?;
    }
    std::fs::write(path, data).map_err(|e| format!("存档失败: 写入文件出错 {e}"))?;
    log::info!("instant state saved to {path:?}");
    Ok(())
}

/// Restores the console from the state file and republishes the frame so the
/// restored picture shows immediately, even while paused.
fn load_state_file(
    core: &mut impl NesCore,
    shared: &SharedState,
    path: &Path,
) -> Result<(), String> {
    if !path.is_file() {
        return Err("暂无存档".to_string());
    }
    let data = std::fs::read(path).map_err(|e| format!("读档失败: 读取文件出错 {e}"))?;
    core.load_state(&data)
        .map_err(|e| format!("读档失败: {e}"))?;
    if let Ok(mut fb) = shared.framebuffer.lock() {
        fb.copy_from_slice(core.frame_buffer());
    }
    log::info!("instant state restored from {path:?}");
    Ok(())
}

/// Replaces the pending status message; only the latest result matters.
fn publish_status(shared: &SharedState, msg: String) {
    if let Ok(mut status) = shared.status.lock() {
        *status = Some(msg);
    }
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
