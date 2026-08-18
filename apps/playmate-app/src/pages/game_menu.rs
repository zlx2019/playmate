//! In-game pause overlay with resume, settings, and exit actions.
//!
//! The overlay is a floating window above a dimmed game frame. Local play
//! truly pauses emulation while it is open; netplay keeps running because a
//! peer's game cannot be paused, so only local input is suppressed.

use crate::config::Config;
use crate::pages::settings::{self, SettingsAction, SettingsState};

/// Overlay state stored inside the active game page.
#[derive(Default)]
pub struct GameMenu {
    /// Whether the overlay is visible.
    pub open: bool,
    /// Embedded settings view replacing the button list while `Some`.
    pub settings: Option<SettingsState>,
}

/// Action triggered by the overlay.
pub enum GameMenuAction {
    /// Nothing happened.
    None,
    /// Close the overlay and resume gameplay.
    Resume,
    /// Save an instant state; only offered while running the emulator locally.
    SaveState,
    /// Restore the instant state; only offered while running the emulator locally.
    LoadState,
    /// End the game session.
    Exit,
    /// Restore default key bindings; handled by the application.
    RestoreDefaults,
}

/// Draws the dimmed backdrop and the centered overlay. Call only while open.
/// `paused` selects the title: local play pauses, netplay only shows the menu.
/// `can_save` offers instant save/load, available only where the emulator
/// runs locally (single-player or netplay host, never a guest).
pub fn show(
    ui: &mut egui::Ui,
    cfg: &Config,
    menu: &mut GameMenu,
    paused: bool,
    can_save: bool,
) -> GameMenuAction {
    let mut action = GameMenuAction::None;
    // Dim the game frame; the floating window renders above this paint.
    ui.painter().rect_filled(
        ui.ctx().content_rect(),
        0.0,
        egui::Color32::from_black_alpha(140),
    );

    let title = if paused { "已暂停" } else { "菜单" };
    let size = if menu.settings.is_some() {
        // Tall enough for the ten-row binding grid including turbo keys.
        [620.0, 560.0]
    } else {
        [240.0, 0.0]
    };
    egui::Window::new(egui::RichText::new(title).strong())
        .collapsible(false)
        .resizable(false)
        .fixed_size(size)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| match &mut menu.settings {
            Some(state) => match settings::show(ui, cfg, state) {
                SettingsAction::None => {}
                SettingsAction::Back => menu.settings = None,
                SettingsAction::RestoreDefaults => action = GameMenuAction::RestoreDefaults,
            },
            None => {
                ui.add_space(4.0);
                if menu_button(ui, "▶ 继续游戏").clicked() {
                    action = GameMenuAction::Resume;
                }
                if can_save {
                    ui.add_space(4.0);
                    if menu_button(ui, "存档 (F5)").clicked() {
                        action = GameMenuAction::SaveState;
                    }
                    ui.add_space(4.0);
                    if menu_button(ui, "读档 (F9)").clicked() {
                        action = GameMenuAction::LoadState;
                    }
                }
                ui.add_space(4.0);
                if menu_button(ui, "⚙ 设置").clicked() {
                    menu.settings = Some(SettingsState::default());
                }
                ui.add_space(4.0);
                if menu_button(ui, "退出游戏").clicked() {
                    action = GameMenuAction::Exit;
                }
                ui.add_space(2.0);
            }
        });
    action
}

/// Full-width overlay button.
fn menu_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized([ui.available_width(), 34.0], egui::Button::new(text))
}
