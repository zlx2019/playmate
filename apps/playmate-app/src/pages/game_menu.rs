//! In-game pause overlay with resume, save/load, cheats, settings, and exit.
//!
//! The overlay is a floating window above a dimmed game frame. Local play
//! truly pauses emulation while it is open; netplay keeps running because a
//! peer's game cannot be paused, so only local input is suppressed.

use std::time::SystemTime;

use crate::config::Config;
use crate::emu::{THUMB_HEIGHT, THUMB_WIDTH};
use crate::pages::settings::{self, SettingsAction, SettingsState};
use crate::play::PlaySession;
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
    /// Embedded save-slot manager replacing the button list while `Some`.
    pub slots: Option<SlotsState>,
}

/// Save-slot manager view state.
#[derive(Default)]
pub struct SlotsState {
    /// Cached slot thumbnails keyed by the state file's modification time.
    thumbs: [Option<(egui::TextureHandle, SystemTime)>; 3],
    /// Slot with an armed delete confirmation.
    confirm_delete: Option<u8>,
}

/// Cheat editor view state.
#[derive(Default)]
pub struct CheatsState {
    /// Code currently being typed.
    pub input: String,
    /// Result hint below the input row and whether it reports success.
    pub hint: Option<(String, bool)>,
}

/// Action triggered by the overlay.
pub enum GameMenuAction {
    /// Nothing happened.
    None,
    /// Close the overlay and resume gameplay.
    Resume,
    /// Save an instant state into this one-based slot; only offered while
    /// running the emulator locally.
    SaveState(u8),
    /// Restore the instant state of this one-based slot; only offered while
    /// running the emulator locally.
    LoadState(u8),
    /// Delete the saved state of this one-based slot after confirmation.
    DeleteState(u8),
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
    /// Switch the NTSC filter; persisted and applied by the application.
    SetNtsc(bool),
    /// Switch per-button hold-turbo as `(hold A, hold B)`; persisted and
    /// applied by the application.
    SetHoldTurbo(bool, bool),
}

/// Draws the dimmed backdrop and the centered overlay. Call only while open.
/// `paused` selects the title: local play pauses, netplay only shows the menu.
/// `session` is the locally running emulator (single-player or netplay host);
/// save/load and cheats are only offered when it is present, never to guests.
/// `rom_title` keys the cheat list inside `cfg`.
pub fn show(
    ui: &mut egui::Ui,
    cfg: &Config,
    menu: &mut GameMenu,
    paused: bool,
    rom_title: &str,
    session: Option<&PlaySession>,
) -> GameMenuAction {
    let mut action = GameMenuAction::None;
    // Dim the game frame; the floating window renders above this paint.
    ui.painter().rect_filled(
        ui.ctx().content_rect(),
        0.0,
        egui::Color32::from_black_alpha(140),
    );

    let title = if paused { "已暂停" } else { "菜单" };
    // Heights are 0 so the window always adapts to its content.
    let size = if menu.settings.is_some() {
        [620.0, 0.0]
    } else if menu.cheats.is_some() || menu.slots.is_some() {
        [430.0, 0.0]
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
                // The embedded body has no page panel, so the window height
                // tracks the content instead of clipping it.
                match settings::show_embedded(ui, cfg, state) {
                    SettingsAction::None => {}
                    SettingsAction::Back => menu.settings = None,
                    SettingsAction::RestoreDefaults => action = GameMenuAction::RestoreDefaults,
                    SettingsAction::SetNtsc(on) => action = GameMenuAction::SetNtsc(on),
                    SettingsAction::SetHoldTurbo(a, b) => {
                        action = GameMenuAction::SetHoldTurbo(a, b);
                    }
                }
            } else if menu.cheats.is_some() {
                action = cheats_view(ui, cfg, menu, rom_title);
            } else if menu.slots.is_some() {
                match session {
                    Some(session) => action = slots_view(ui, menu, session),
                    None => menu.slots = None,
                }
            } else {
                ui.add_space(4.0);
                if menu_button(ui, "▶ 继续游戏").clicked() {
                    action = GameMenuAction::Resume;
                }
                if session.is_some() {
                    ui.add_space(4.0);
                    if menu_button(ui, "存档 / 读档").clicked() {
                        menu.slots = Some(SlotsState::default());
                    }
                    ui.add_space(4.0);
                    if menu_button(ui, "作弊码").clicked() {
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

/// Cheat editor: stored codes as sunken chip rows with enable and delete
/// controls, plus a framed input row. Mutations are reported as actions; the
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
        ui.add_space(4.0);
        ui.label(egui::RichText::new("作弊码").strong().size(16.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(rom_title)
                    .size(12.0)
                    .color(theme::TEXT_WEAK),
            );
        });
    });
    let Some(state) = &mut menu.cheats else {
        return action;
    };
    ui.add_space(10.0);

    let entries = cfg.cheats.get(rom_title).map(Vec::as_slice).unwrap_or(&[]);
    if entries.is_empty() {
        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("尚无作弊码").color(theme::TEXT_WEAK));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("输入 6 或 8 位 Game Genie 码，如 SXIOPO（超马无限命）")
                    .size(12.0)
                    .color(theme::TEXT_WEAK),
            );
        });
        ui.add_space(14.0);
    } else {
        egui::ScrollArea::vertical()
            .max_height(240.0)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for (i, entry) in entries.iter().enumerate() {
                    cheat_chip(ui, i, entry, &mut action);
                }
            });
    }

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let edit = egui::TextEdit::singleline(&mut state.input)
            .desired_width(ui.available_width() - 88.0)
            .char_limit(8)
            .font(egui::FontId::monospace(18.0))
            .hint_text("输入作弊码");
        let submitted = ui.add(edit).lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let filled = !state.input.trim().is_empty();
        // A compact inline primary button; theme::primary_button is
        // full-width and would overflow this row.
        let add = egui::Button::new(
            egui::RichText::new("添加")
                .size(14.0)
                .strong()
                .color(egui::Color32::WHITE),
        )
        .fill(theme::RED)
        .min_size(egui::vec2(72.0, 30.0));
        if (ui.add_enabled(filled, add).clicked() || submitted) && filled {
            action = GameMenuAction::AddCheat(state.input.trim().to_string());
        }
    });
    if let Some((hint, ok)) = &state.hint {
        ui.add_space(6.0);
        let color = if *ok { theme::GREEN } else { theme::RED_BRIGHT };
        ui.label(egui::RichText::new(hint).size(12.0).color(color));
    }
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("码与 ROM 版本相关（美版/日版不通用），无效果时请核对版本")
            .size(11.0)
            .color(theme::TEXT_WEAK),
    );
    action
}

/// One stored cheat as a full-width sunken chip: enable checkbox, the code
/// in monospace, and a right-aligned delete button.
fn cheat_chip(
    ui: &mut egui::Ui,
    index: usize,
    entry: &crate::config::CheatEntry,
    action: &mut GameMenuAction,
) {
    egui::Frame::new()
        .fill(theme::SUNKEN)
        .stroke(egui::Stroke::new(1.0, theme::OUTLINE))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let mut enabled = entry.enabled;
                if ui.checkbox(&mut enabled, "").changed() {
                    *action = GameMenuAction::ToggleCheat(index);
                }
                let code = egui::RichText::new(&entry.code).monospace().size(16.0);
                let code = if entry.enabled {
                    code.strong().color(theme::GREEN)
                } else {
                    code.color(theme::TEXT_WEAK)
                };
                ui.label(code);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(
                            egui::RichText::new("删除")
                                .size(12.0)
                                .color(theme::RED_BRIGHT),
                        )
                        .clicked()
                    {
                        *action = GameMenuAction::RemoveCheat(index);
                    }
                });
            });
        });
}

/// Full-width overlay button.
fn menu_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized([ui.available_width(), 34.0], egui::Button::new(text))
}

/// Save-slot manager: one card per slot with a thumbnail, save time, and
/// explicit actions, following the state galleries of OpenEmu-style frontends.
fn slots_view(ui: &mut egui::Ui, menu: &mut GameMenu, session: &PlaySession) -> GameMenuAction {
    let mut action = GameMenuAction::None;
    ui.horizontal(|ui| {
        if ui.button("‹ 返回").clicked() {
            menu.slots = None;
        }
        ui.label(egui::RichText::new("存档 / 读档").strong());
        ui.label(
            egui::RichText::new(&session.rom_title)
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
    });
    let Some(state) = &mut menu.slots else {
        return action;
    };
    ui.add_space(8.0);

    let thumb_size = egui::vec2(96.0, 90.0);
    for slot in 1..=3u8 {
        let mtime = session.slot_mtime(slot);
        ui.horizontal(|ui| {
            match slot_texture(ui.ctx(), state, session, slot, mtime) {
                Some(texture) => {
                    ui.add(egui::Image::new(&texture).fit_to_exact_size(thumb_size));
                }
                None => {
                    // Placeholder: an empty slot, or a save without a preview.
                    let (rect, _) = ui.allocate_exact_size(thumb_size, egui::Sense::hover());
                    ui.painter().rect_filled(rect, 4.0, theme::SUNKEN);
                    let text = if mtime.is_some() { "无预览" } else { "空" };
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        text,
                        egui::FontId::proportional(12.0),
                        theme::TEXT_WEAK,
                    );
                }
            }
            ui.vertical(|ui| {
                ui.add_space(6.0);
                match mtime {
                    Some(mtime) => {
                        ui.label(
                            egui::RichText::new(format!("槽 {slot} · {}", age_label(mtime)))
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("读取").clicked() {
                                action = GameMenuAction::LoadState(slot);
                            }
                            if ui.button("覆盖").clicked() {
                                action = GameMenuAction::SaveState(slot);
                            }
                            let armed = state.confirm_delete == Some(slot);
                            let label = if armed { "确认删除" } else { "删除" };
                            let text = egui::RichText::new(label).color(theme::RED_BRIGHT);
                            if ui.button(text).clicked() {
                                if armed {
                                    state.confirm_delete = None;
                                    action = GameMenuAction::DeleteState(slot);
                                } else {
                                    state.confirm_delete = Some(slot);
                                }
                            }
                        });
                    }
                    None => {
                        ui.label(
                            egui::RichText::new(format!("槽 {slot} · 空")).color(theme::TEXT_WEAK),
                        );
                        ui.add_space(4.0);
                        if ui.button("存入").clicked() {
                            action = GameMenuAction::SaveState(slot);
                        }
                    }
                }
            });
        });
        ui.add_space(6.0);
    }
    ui.label(
        egui::RichText::new("F5 快存 / F9 快读（槽 1）")
            .size(12.0)
            .color(theme::TEXT_WEAK),
    );
    action
}

/// Returns the cached thumbnail texture for a slot, reloading it when the
/// state file's modification time changes.
fn slot_texture(
    ctx: &egui::Context,
    state: &mut SlotsState,
    session: &PlaySession,
    slot: u8,
    mtime: Option<SystemTime>,
) -> Option<egui::TextureHandle> {
    let idx = usize::from(slot) - 1;
    let Some(mtime) = mtime else {
        state.thumbs[idx] = None;
        return None;
    };
    if let Some((texture, cached)) = &state.thumbs[idx]
        && *cached == mtime
    {
        return Some(texture.clone());
    }
    let rgba = session.slot_thumb(slot)?;
    let image = egui::ColorImage::from_rgba_unmultiplied([THUMB_WIDTH, THUMB_HEIGHT], &rgba);
    let texture = ctx.load_texture(
        format!("state-thumb-{slot}"),
        image,
        egui::TextureOptions::NEAREST,
    );
    state.thumbs[idx] = Some((texture.clone(), mtime));
    Some(texture)
}

/// Human-readable age of a save, relative to now.
fn age_label(mtime: SystemTime) -> String {
    let secs = mtime.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..=59 => "刚刚".to_string(),
        60..=3599 => format!("{} 分钟前", secs / 60),
        3600..=86399 => format!("{} 小时前", secs / 3600),
        _ => format!("{} 天前", secs / 86400),
    }
}
