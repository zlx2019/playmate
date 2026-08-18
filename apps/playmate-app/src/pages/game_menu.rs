//! In-game pause overlay with resume, save/load, cheats, settings, and exit.
//!
//! The overlay is a floating window above a dimmed game frame. Local play
//! truly pauses emulation while it is open; netplay keeps running because a
//! peer's game cannot be paused, so only local input is suppressed.

use crate::config::Config;
use crate::pages::settings::{self, SettingsAction, SettingsState};
use crate::theme;

/// Overlay state stored inside the active game page.
#[derive(Default)]
pub struct GameMenu {
    /// Whether the overlay is visible.
    pub open: bool,
    /// Embedded settings view replacing the button list while `Some`.
    pub settings: Option<SettingsState>,
    /// Embedded cheat editor replacing the button list while `Some`.
    pub cheats: Option<CheatsState>,
}

/// Cheat editor view state.
#[derive(Default)]
pub struct CheatsState {
    /// Code currently being typed.
    pub input: String,
    /// Validation or result hint below the input row.
    pub hint: Option<String>,
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
    /// Add the typed Game Genie code; the application validates and persists it.
    AddCheat(String),
    /// Flip the enabled flag of the cheat at this index.
    ToggleCheat(usize),
    /// Delete the cheat at this index.
    RemoveCheat(usize),
    /// End the game session.
    Exit,
    /// Restore default key bindings; handled by the application.
    RestoreDefaults,
}

/// Draws the dimmed backdrop and the centered overlay. Call only while open.
/// `paused` selects the title: local play pauses, netplay only shows the menu.
/// `can_save` offers instant save/load and cheats, available only where the
/// emulator runs locally (single-player or netplay host, never a guest).
/// `rom_title` keys the cheat list inside `cfg`.
pub fn show(
    ui: &mut egui::Ui,
    cfg: &Config,
    menu: &mut GameMenu,
    paused: bool,
    can_save: bool,
    rom_title: &str,
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
    } else if menu.cheats.is_some() {
        [420.0, 0.0]
    } else {
        [240.0, 0.0]
    };
    egui::Window::new(egui::RichText::new(title).strong())
        .collapsible(false)
        .resizable(false)
        .fixed_size(size)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            if let Some(state) = &mut menu.settings {
                match settings::show(ui, cfg, state) {
                    SettingsAction::None => {}
                    SettingsAction::Back => menu.settings = None,
                    SettingsAction::RestoreDefaults => action = GameMenuAction::RestoreDefaults,
                }
            } else if menu.cheats.is_some() {
                action = cheats_view(ui, cfg, menu, rom_title);
            } else {
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
                    ui.add_space(4.0);
                    if menu_button(ui, "金手指").clicked() {
                        menu.cheats = Some(CheatsState::default());
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

/// Cheat editor: list of stored codes with enable/delete controls plus an
/// input row for new codes. Mutations are reported as actions; the
/// application owns validation, persistence, and applying to the console.
fn cheats_view(
    ui: &mut egui::Ui,
    cfg: &Config,
    menu: &mut GameMenu,
    rom_title: &str,
) -> GameMenuAction {
    let mut action = GameMenuAction::None;
    ui.horizontal(|ui| {
        if ui.button("‹ 返回").clicked() {
            menu.cheats = None;
        }
        ui.label(egui::RichText::new("金手指").strong());
        ui.label(
            egui::RichText::new(rom_title)
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
    });
    let Some(state) = &mut menu.cheats else {
        return action;
    };
    ui.add_space(8.0);

    let entries = cfg.cheats.get(rom_title).map(Vec::as_slice).unwrap_or(&[]);
    if entries.is_empty() {
        ui.label(egui::RichText::new("尚无金手指，输入 6 或 8 位码添加").color(theme::TEXT_WEAK));
    } else {
        for (i, entry) in entries.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut enabled = entry.enabled;
                if ui.checkbox(&mut enabled, "").changed() {
                    action = GameMenuAction::ToggleCheat(i);
                }
                ui.label(
                    egui::RichText::new(&entry.code)
                        .monospace()
                        .strong()
                        .size(16.0),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("删除").clicked() {
                        action = GameMenuAction::RemoveCheat(i);
                    }
                });
            });
        }
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let edit = egui::TextEdit::singleline(&mut state.input)
            .desired_width(160.0)
            .char_limit(8)
            .font(egui::FontId::monospace(16.0))
            .hint_text("SXIOPO");
        let submitted = ui.add(edit).lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("添加").clicked() || submitted) && !state.input.trim().is_empty() {
            action = GameMenuAction::AddCheat(state.input.trim().to_string());
        }
    });
    if let Some(hint) = &state.hint {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(hint).size(12.0).color(theme::GREEN));
    }
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("码与 ROM 版本相关（美版/日版不通用），无效果时请核对版本")
            .size(12.0)
            .color(theme::TEXT_WEAK),
    );
    action
}

/// Full-width overlay button.
fn menu_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized([ui.available_width(), 34.0], egui::Button::new(text))
}
