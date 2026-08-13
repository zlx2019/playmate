//! Game-selection page listing `.nes` files from ROM directories.

use std::path::PathBuf;

use crate::theme;

/// A selectable game entry.
#[derive(Debug, Clone)]
pub struct GameEntry {
    /// Display title derived from the file name without its extension.
    pub title: String,
    /// Full ROM path.
    pub path: PathBuf,
}

/// Action triggered by the game-selection page.
pub enum GameSelectAction {
    /// No action.
    None,
    /// Return to the main menu.
    Back,
    /// Start the selected game.
    Play(PathBuf),
    /// Rescan ROM directories after files are added.
    Refresh,
}

/// Opens the preferred ROM directory in the system file manager, creating it first.
fn open_roms_dir() {
    let dir = crate::config::data_dir()
        .map(|d| d.join("roms"))
        .unwrap_or_else(|| PathBuf::from("roms"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("failed to create directory {dir:?}: {e}");
    }
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(target_os = "windows")]
    let opener = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let opener = "xdg-open";
    match std::process::Command::new(opener).arg(&dir).spawn() {
        // Reap the child in the background to avoid leaving a zombie process.
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::warn!("failed to open directory {dir:?}: {e}"),
    }
}

/// Scans and deduplicates ROM directories beside the current directory,
/// executable, macOS app bundle, and platform user-data directory.
pub fn scan_roms() -> Vec<GameEntry> {
    let mut dirs = vec![PathBuf::from("roms")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("roms"));
        }
        // Also scan beside the macOS app bundle for portable installations.
        if let Some(bundle) = exe
            .ancestors()
            .find(|p| p.extension().is_some_and(|ext| ext == "app"))
            && let Some(beside) = bundle.parent()
        {
            dirs.push(beside.join("roms"));
        }
    }
    // Standard user-data location for app bundles and installed packages.
    if let Some(data) = crate::config::data_dir() {
        dirs.push(data.join("roms"));
    }

    let mut games: Vec<GameEntry> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_nes = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("nes"));
            if !is_nes {
                continue;
            }
            let title = path
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Different search roots may resolve to the same location, so deduplicate by title.
            if !games.iter().any(|g| g.title == title) {
                games.push(GameEntry { title, path });
            }
        }
    }
    games.sort_by(|a, b| a.title.cmp(&b.title));
    games
}

/// Draws game selection with an optional error from the previous launch attempt.
pub fn show(ui: &mut egui::Ui, games: &[GameEntry], error: Option<&str>) -> GameSelectAction {
    let mut action = GameSelectAction::None;
    egui::CentralPanel::default().show(ui, |ui| {
        if theme::page_header(ui, "选择游戏") {
            action = GameSelectAction::Back;
        }

        if let Some(msg) = error {
            theme::error_banner(ui, msg);
            ui.add_space(6.0);
        }

        if games.is_empty() {
            // Empty state with centered guidance and quick actions.
            let top = (ui.available_height() * 0.18).max(30.0);
            ui.vertical_centered(|ui| {
                ui.add_space(top);
                ui.label(egui::RichText::new("🕹").size(56.0));
                ui.add_space(12.0);
                ui.label(egui::RichText::new("未找到任何游戏").size(18.0).strong());
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("将 .nes 游戏文件放入 ROM 文件夹后点击刷新")
                        .color(theme::TEXT_WEAK),
                );
                if let Some(data) = crate::config::data_dir() {
                    ui.label(
                        egui::RichText::new(format!("位置：{}", data.join("roms").display()))
                            .size(12.0)
                            .color(theme::TEXT_WEAK),
                    );
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    // Center the button group by reserving half of the remaining width.
                    let spacing = ui.spacing().item_spacing.x;
                    let total = 150.0 + 90.0 + spacing;
                    ui.add_space((ui.available_width() - total).max(0.0) / 2.0);
                    if ui
                        .add(egui::Button::new("📂 打开 ROM 文件夹").min_size([150.0, 32.0].into()))
                        .clicked()
                    {
                        open_roms_dir();
                    }
                    if ui
                        .add(egui::Button::new("↻ 刷新").min_size([90.0, 32.0].into()))
                        .clicked()
                    {
                        action = GameSelectAction::Refresh;
                    }
                });
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("提示：请使用合法持有的 ROM 或自由分发的 homebrew 游戏")
                        .size(12.0)
                        .color(theme::TEXT_WEAK),
                );
            });
            return;
        }

        ui.vertical_centered(|ui| {
            ui.set_max_width(640.0);
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("共 {} 款游戏", games.len()))
                            .size(12.0)
                            .color(theme::TEXT_WEAK),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("↻ 刷新").clicked() {
                            action = GameSelectAction::Refresh;
                        }
                        if ui.button("📂 打开 ROM 文件夹").clicked() {
                            open_roms_dir();
                        }
                    });
                });
                ui.add_space(2.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    for game in games {
                        if theme::card_row(ui, "🕹", &game.title, None, true).clicked() {
                            action = GameSelectAction::Play(game.path.clone());
                        }
                    }
                });
            });
        });
    });
    action
}
