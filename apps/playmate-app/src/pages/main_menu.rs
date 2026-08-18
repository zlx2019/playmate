//! Main menu for local play, LAN play, future internet play, and settings.

use egui::{RichText, Sense, Stroke, StrokeKind, vec2};

use crate::theme;

/// Action triggered from the main menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// No action.
    None,
    /// Resume the most recently played local game from its auto snapshot.
    Continue,
    /// Enter local game selection.
    SinglePlayer,
    /// Enter the LAN lobby.
    LanPlay,
    /// Enter settings.
    Settings,
}

/// Width of the menu content column.
const MENU_WIDTH: f32 = 360.0;

/// Draws the main menu and returns the current frame's action.
/// `resume_title` shows a quick-resume row for the last played game.
pub fn show(ui: &mut egui::Ui, resume_title: Option<&str>) -> MenuAction {
    let mut action = MenuAction::None;
    egui::CentralPanel::default().show(ui, |ui| {
        let top = (ui.available_height() * 0.14).max(24.0);
        ui.vertical_centered(|ui| {
            ui.add_space(top);

            // Title, red accent line, and subtitle.
            ui.label(
                RichText::new("Playmate")
                    .size(52.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(6.0);
            let (line, _) = ui.allocate_exact_size(vec2(64.0, 4.0), Sense::hover());
            ui.painter()
                .rect(line, 2, theme::RED, Stroke::NONE, StrokeKind::Inside);
            ui.add_space(10.0);
            ui.label(
                RichText::new("局域网双人 FC 模拟器")
                    .size(15.0)
                    .color(theme::TEXT_WEAK),
            );
            ui.add_space(36.0);

            // Centered menu rows with a fixed maximum width.
            ui.scope(|ui| {
                ui.set_max_width(MENU_WIDTH);
                ui.spacing_mut().item_spacing.y = 12.0;
                if let Some(title) = resume_title
                    && theme::card_row(ui, "▶", "继续游戏", Some(title), true).clicked()
                {
                    action = MenuAction::Continue;
                }
                if theme::card_row(ui, "🎮", "单机游戏", None, true).clicked() {
                    action = MenuAction::SinglePlayer;
                }
                if theme::card_row(ui, "🌐", "局域网联机", None, true).clicked() {
                    action = MenuAction::LanPlay;
                }
                theme::card_row(ui, "🌍", "互联网联机", Some("暂未开放"), false);
                if theme::card_row(ui, "⚙", "设置", None, true).clicked() {
                    action = MenuAction::Settings;
                }
            });

            // Keep the version near the bottom.
            let rest = ui.available_height() - 30.0;
            if rest > 0.0 {
                ui.add_space(rest);
            }
            ui.label(
                RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                    .size(12.0)
                    .color(theme::TEXT_WEAK),
            );
        });
    });
    action
}
