//! Settings page for viewing and editing P1/P2 key bindings.
//!
//! Clicking a binding enters capture mode. The application intercepts the next
//! exact physical `KeyCode`, including numpad distinctions, updates the
//! configuration, and saves `playmate.toml` immediately.

use playmate_core::{Button, Player};
use winit::keyboard::KeyCode;

use crate::config::{BindKey, Config};
use crate::theme;

/// Transient settings-page state.
#[derive(Default)]
pub struct SettingsState {
    /// Binding awaiting a new physical key; the application intercepts input while set.
    pub capturing: Option<(Player, BindKey)>,
    /// User-facing result message for save, cancel, and reset operations.
    pub hint: Option<String>,
}

/// Action triggered by the settings page.
pub enum SettingsAction {
    /// No action.
    None,
    /// Return to the main menu.
    Back,
    /// Restore default bindings.
    RestoreDefaults,
    /// Switch the NTSC composite filter on or off; the application persists
    /// the choice and applies it to a running session.
    SetNtsc(bool),
    /// Switch hold-turbo per button: a plainly held A and/or B auto-fires on
    /// the turbo cadence. Carries the new `(hold A, hold B)` pair; persisted
    /// by the application and applied immediately.
    SetHoldTurbo(bool, bool),
}

/// Localized labels for bindable controls.
fn bind_label(key: BindKey) -> &'static str {
    match key {
        BindKey::Btn(Button::Up) => "上",
        BindKey::Btn(Button::Down) => "下",
        BindKey::Btn(Button::Left) => "左",
        BindKey::Btn(Button::Right) => "右",
        BindKey::Btn(Button::A) => "A",
        BindKey::Btn(Button::B) => "B",
        BindKey::Btn(Button::Select) => "选择 (Select)",
        BindKey::Btn(Button::Start) => "开始 (Start)",
        BindKey::TurboA => "连发 A",
        BindKey::TurboB => "连发 B",
    }
}

/// Localized display name for common physical keys, with `KeyCode` fallback.
pub fn key_label(code: Option<KeyCode>) -> String {
    let Some(code) = code else {
        return "（未绑定）".to_string();
    };
    let text = match code {
        KeyCode::ArrowUp => "↑",
        KeyCode::ArrowDown => "↓",
        KeyCode::ArrowLeft => "←",
        KeyCode::ArrowRight => "→",
        KeyCode::Enter => "回车",
        KeyCode::NumpadEnter => "小键盘回车",
        KeyCode::NumpadDecimal => "小键盘 .",
        KeyCode::ShiftLeft => "左 Shift",
        KeyCode::ShiftRight => "右 Shift",
        KeyCode::ControlLeft => "左 Ctrl",
        KeyCode::ControlRight => "右 Ctrl",
        KeyCode::Space => "空格",
        KeyCode::Tab => "Tab",
        KeyCode::Backspace => "退格",
        other => {
            // Simplify common `Key`, `Digit`, and `Numpad` debug names for display.
            let name = format!("{other:?}");
            return name
                .strip_prefix("Key")
                .map(str::to_string)
                .or_else(|| name.strip_prefix("Digit").map(str::to_string))
                .or_else(|| name.strip_prefix("Numpad").map(|n| format!("小键盘 {n}")))
                .unwrap_or(name);
        }
    };
    text.to_string()
}

/// Draws the standalone settings page inside its own panel.
pub fn show(ui: &mut egui::Ui, cfg: &Config, state: &mut SettingsState) -> SettingsAction {
    let mut action = SettingsAction::None;
    egui::CentralPanel::default().show(ui, |ui| {
        action = body(ui, cfg, state);
    });
    action
}

/// Draws the settings content without a page container, for embedding in
/// the in-game menu window, whose height adapts to this content.
pub fn show_embedded(ui: &mut egui::Ui, cfg: &Config, state: &mut SettingsState) -> SettingsAction {
    body(ui, cfg, state)
}

/// Shared settings body: key bindings, video and input options, and hints.
fn body(ui: &mut egui::Ui, cfg: &Config, state: &mut SettingsState) -> SettingsAction {
    let mut action = SettingsAction::None;
    if theme::page_header(ui, "设置") {
        action = SettingsAction::Back;
    }

    ui.columns(2, |cols| {
        player_keys_panel(&mut cols[0], cfg, state, Player::One, "P1 键位");
        player_keys_panel(&mut cols[1], cfg, state, Player::Two, "P2 键位");
    });

    ui.add_space(12.0);
    // Video and input options; changes apply immediately, including mid-game.
    ui.horizontal(|ui| {
        let mut ntsc = cfg.video.ntsc_filter;
        if ui
            .checkbox(&mut ntsc, "NTSC 柔化滤镜（CRT 显像管观感，关闭为锐利点阵）")
            .changed()
        {
            action = SettingsAction::SetNtsc(ntsc);
        }
    });
    ui.horizontal(|ui| {
        let mut hold_a = cfg.input.hold_turbo_a;
        let mut hold_b = cfg.input.hold_turbo_b;
        ui.label("长按连发：");
        let changed =
            ui.checkbox(&mut hold_a, "A 键").changed() | ui.checkbox(&mut hold_b, "B 键").changed();
        if changed {
            action = SettingsAction::SetHoldTurbo(hold_a, hold_b);
        }
        ui.label(
            egui::RichText::new("（需要长按的键请勿开启，如马力欧 B 跑步、魂斗罗 A 跳跃）")
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
    });

    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui.button("↺ 恢复默认键位").clicked() {
            action = SettingsAction::RestoreDefaults;
        }
        if let Some(hint) = &state.hint {
            ui.label(egui::RichText::new(hint).color(theme::GREEN));
        }
    });
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "提示：改动即时保存；同一物理键被重复绑定时会自动解除旧绑定；\
                 Esc 为保留键不可绑定。连发键按住时约每秒 15 连；\
                 手柄无需配置，即插即用（X/Y 键为连发 B/A，Mode 键呼出菜单）。",
        )
        .size(12.0)
        .color(theme::TEXT_WEAK),
    );
    action
}

/// Key-binding table for one player.
fn player_keys_panel(
    ui: &mut egui::Ui,
    cfg: &Config,
    state: &mut SettingsState,
    player: Player,
    title: &str,
) {
    theme::card().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        theme::section_title(ui, title);
        ui.add_space(8.0);
        let keys = cfg.keys.effective(player);
        // Display directions first, then A/B with their turbo triggers,
        // followed by function buttons.
        const DISPLAY_ORDER: [BindKey; 10] = [
            BindKey::Btn(Button::Up),
            BindKey::Btn(Button::Down),
            BindKey::Btn(Button::Left),
            BindKey::Btn(Button::Right),
            BindKey::Btn(Button::A),
            BindKey::Btn(Button::B),
            BindKey::TurboA,
            BindKey::TurboB,
            BindKey::Btn(Button::Select),
            BindKey::Btn(Button::Start),
        ];
        egui::Grid::new(title)
            .num_columns(2)
            .spacing([24.0, 8.0])
            .show(ui, |ui| {
                for key in DISPLAY_ORDER {
                    ui.label(bind_label(key));
                    if keycap(ui, &keys, state, player, key) {
                        state.capturing = Some((player, key));
                        state.hint = None;
                    }
                    ui.end_row();
                }
            });
    });
}

/// Keycap-style binding button that reports whether it was clicked.
/// The normal state is recessed with a subtle border; capture uses a red outline.
fn keycap(
    ui: &mut egui::Ui,
    keys: &crate::config::PlayerKeys,
    state: &SettingsState,
    player: Player,
    key: BindKey,
) -> bool {
    let capturing_this = state.capturing == Some((player, key));
    let bound = keys.get(key).is_some();
    let text = if capturing_this {
        egui::RichText::new("按下新键…（Esc 取消）").color(theme::RED_BRIGHT)
    } else if bound {
        egui::RichText::new(key_label(keys.get(key))).strong()
    } else {
        egui::RichText::new(key_label(None)).color(theme::TEXT_WEAK)
    };
    let stroke = if capturing_this {
        egui::Stroke::new(1.5, theme::RED)
    } else {
        egui::Stroke::new(1.0, theme::OUTLINE)
    };
    let widget = egui::Button::new(text)
        .fill(theme::SUNKEN)
        .stroke(stroke)
        .min_size(egui::vec2(170.0, 30.0));
    ui.add(widget).clicked()
}
