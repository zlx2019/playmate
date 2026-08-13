//! Playmate desktop entry point; the GUI opens on the main menu.
//!
//! UI flow: Main Menu -> Local Play (game selection) / LAN Play (lobby and room) /
//! Settings (key bindings).
//! Threading model:
//! - Main thread: winit event loop and egui rendering
//! - Emulation thread: drives the NES core at 60.0988 fps during gameplay
//! - Audio callback thread: cpal pulls samples from the ring buffer
//! - tokio runtime: netplay tasks

mod app;
mod audio;
mod config;
mod emu;
mod gamepad;
mod netplay;
mod pages;
mod play;
mod theme;

use anyhow::Context as _;
use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> anyhow::Result<()> {
    // Default to info-level logging; RUST_LOG can override it.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let event_loop = EventLoop::new().context("failed to create event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = app::PlaymateApp::new().context("failed to initialize application")?;
    event_loop
        .run_app(&mut app)
        .context("event loop exited unexpectedly")?;
    log::info!("Playmate exited");
    Ok(())
}
