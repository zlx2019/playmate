//! User configuration: `playmate.toml` loading, default bindings, and keyboard lookup.
//!
//! The configuration file is read from the current working directory when
//! present; otherwise the built-in layout is used. Parse errors are reported
//! instead of silently ignoring invalid user changes.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{Context, Result};
use playmate_core::{Button, Player};
use serde::{Deserialize, Serialize};
use winit::keyboard::KeyCode;

/// Configuration file name.
const CONFIG_FILE: &str = "playmate.toml";

/// Top-level configuration.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Key binding configuration.
    #[serde(default)]
    pub keys: KeysConfig,
    /// Game Genie cheat codes per game, keyed by ROM title.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cheats: BTreeMap<String, Vec<CheatEntry>>,
    /// Most recently played local ROM, offered as quick resume on the main menu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_game: Option<PathBuf>,
    /// Video output options.
    #[serde(default)]
    pub video: VideoConfig,
}

/// Video output options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoConfig {
    /// NTSC composite filter, the hardware-faithful softened look shown by a
    /// CRT. Disabled means raw crisp pixels.
    #[serde(default = "default_true")]
    pub ntsc_filter: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self { ntsc_filter: true }
    }
}

/// serde helper matching [`VideoConfig::default`].
fn default_true() -> bool {
    true
}

/// One Game Genie cheat entry for a game.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheatEntry {
    /// Normalized 6- or 8-letter Game Genie code.
    pub code: String,
    /// Whether the code is applied while the game runs.
    pub enabled: bool,
}

impl Config {
    /// Enabled cheat codes for a game, in stored order.
    pub fn enabled_cheats(&self, rom_title: &str) -> Vec<String> {
        self.cheats
            .get(rom_title)
            .map(|list| {
                list.iter()
                    .filter(|cheat| cheat.enabled)
                    .map(|cheat| cheat.code.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Key binding sections for both players.
///
/// Overrides apply per section. A missing player section uses the built-in
/// defaults, while a present section completely replaces them. Omitted keys
/// inside a present section are unbound.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeysConfig {
    /// P1 bindings; defaults to WASD, J/K, left Shift, and Enter.
    pub p1: Option<PlayerKeys>,
    /// P2 bindings; defaults to arrow keys, numpad 0/decimal, and numpad Enter.
    pub p2: Option<PlayerKeys>,
}

/// A bindable logical control: one of the eight console buttons or an
/// app-level turbo trigger that auto-fires A/B while held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindKey {
    /// Direct console button.
    Btn(Button),
    /// Turbo trigger for the A button.
    TurboA,
    /// Turbo trigger for the B button.
    TurboB,
}

impl BindKey {
    /// All bindable controls, used for iteration and conflict removal.
    pub const ALL: [BindKey; 10] = [
        BindKey::Btn(Button::Up),
        BindKey::Btn(Button::Down),
        BindKey::Btn(Button::Left),
        BindKey::Btn(Button::Right),
        BindKey::Btn(Button::A),
        BindKey::Btn(Button::B),
        BindKey::Btn(Button::Select),
        BindKey::Btn(Button::Start),
        BindKey::TurboA,
        BindKey::TurboB,
    ];

    /// The console button this control ultimately drives.
    pub const fn button(self) -> Button {
        match self {
            BindKey::Btn(button) => button,
            BindKey::TurboA => Button::A,
            BindKey::TurboB => Button::B,
        }
    }

    /// Whether this control is a turbo trigger rather than a direct button.
    pub const fn is_turbo(self) -> bool {
        matches!(self, BindKey::TurboA | BindKey::TurboB)
    }
}

/// Bindings for the eight NES/Famicom buttons plus turbo triggers; `None`
/// means unbound.
///
/// Key names use winit `KeyCode` variant names such as `"KeyW"`, `"ArrowUp"`,
/// `"Numpad0"`, `"NumpadDecimal"`, `"NumpadEnter"`, and `"ShiftLeft"`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerKeys {
    /// D-pad up.
    pub up: Option<KeyCode>,
    /// D-pad down.
    pub down: Option<KeyCode>,
    /// D-pad left.
    pub left: Option<KeyCode>,
    /// D-pad right.
    pub right: Option<KeyCode>,
    /// A button.
    pub a: Option<KeyCode>,
    /// B button.
    pub b: Option<KeyCode>,
    /// Select button.
    pub select: Option<KeyCode>,
    /// Start button.
    pub start: Option<KeyCode>,
    /// Turbo A trigger: auto-fires A while held.
    pub turbo_a: Option<KeyCode>,
    /// Turbo B trigger: auto-fires B while held.
    pub turbo_b: Option<KeyCode>,
}

impl PlayerKeys {
    /// Built-in P1 layout: WASD + J(B)/K(A), turbo on U(B)/I(A) above them,
    /// and left Shift(Select)/Enter(Start).
    fn default_p1() -> Self {
        Self {
            up: Some(KeyCode::KeyW),
            down: Some(KeyCode::KeyS),
            left: Some(KeyCode::KeyA),
            right: Some(KeyCode::KeyD),
            b: Some(KeyCode::KeyJ),
            a: Some(KeyCode::KeyK),
            select: Some(KeyCode::ShiftLeft),
            start: Some(KeyCode::Enter),
            turbo_b: Some(KeyCode::KeyU),
            turbo_a: Some(KeyCode::KeyI),
        }
    }

    /// Built-in P2 layout: arrows + numpad 0(B)/decimal(A), turbo on numpad
    /// 1(B)/2(A) above them, and numpad Enter(Start).
    /// Select is unbound by default, matching the original second controller.
    fn default_p2() -> Self {
        Self {
            up: Some(KeyCode::ArrowUp),
            down: Some(KeyCode::ArrowDown),
            left: Some(KeyCode::ArrowLeft),
            right: Some(KeyCode::ArrowRight),
            b: Some(KeyCode::Numpad0),
            a: Some(KeyCode::NumpadDecimal),
            select: None,
            start: Some(KeyCode::NumpadEnter),
            turbo_b: Some(KeyCode::Numpad1),
            turbo_a: Some(KeyCode::Numpad2),
        }
    }

    /// Returns the physical key bound to a control.
    pub fn get(&self, key: BindKey) -> Option<KeyCode> {
        match key {
            BindKey::Btn(Button::Up) => self.up,
            BindKey::Btn(Button::Down) => self.down,
            BindKey::Btn(Button::Left) => self.left,
            BindKey::Btn(Button::Right) => self.right,
            BindKey::Btn(Button::A) => self.a,
            BindKey::Btn(Button::B) => self.b,
            BindKey::Btn(Button::Select) => self.select,
            BindKey::Btn(Button::Start) => self.start,
            BindKey::TurboA => self.turbo_a,
            BindKey::TurboB => self.turbo_b,
        }
    }

    /// Sets or clears the physical key bound to a control.
    pub fn set(&mut self, key: BindKey, code: Option<KeyCode>) {
        let slot = match key {
            BindKey::Btn(Button::Up) => &mut self.up,
            BindKey::Btn(Button::Down) => &mut self.down,
            BindKey::Btn(Button::Left) => &mut self.left,
            BindKey::Btn(Button::Right) => &mut self.right,
            BindKey::Btn(Button::A) => &mut self.a,
            BindKey::Btn(Button::B) => &mut self.b,
            BindKey::Btn(Button::Select) => &mut self.select,
            BindKey::Btn(Button::Start) => &mut self.start,
            BindKey::TurboA => &mut self.turbo_a,
            BindKey::TurboB => &mut self.turbo_b,
        };
        *slot = code;
    }

    /// Iterates over `(physical key, control)` pairs, skipping unbound entries.
    fn bindings(&self) -> impl Iterator<Item = (KeyCode, BindKey)> {
        BindKey::ALL
            .into_iter()
            .filter_map(|key| self.get(key).map(|code| (code, key)))
    }
}

/// Keyboard lookup table: physical key -> player and bound control.
pub struct InputMap {
    /// Flattened bindings; P2 wins when both players bind the same physical key.
    map: HashMap<KeyCode, (Player, BindKey)>,
}

impl InputMap {
    /// Builds the lookup table, applying built-in defaults for missing player sections.
    pub fn from_config(cfg: &Config) -> Self {
        let mut map = HashMap::new();
        let p1 = cfg.keys.p1.clone().unwrap_or_else(PlayerKeys::default_p1);
        let p2 = cfg.keys.p2.clone().unwrap_or_else(PlayerKeys::default_p2);
        for (code, key) in p1.bindings() {
            map.insert(code, (Player::One, key));
        }
        for (code, key) in p2.bindings() {
            map.insert(code, (Player::Two, key));
        }
        Self { map }
    }

    /// Looks up the binding for a physical key.
    pub fn lookup(&self, code: KeyCode) -> Option<(Player, BindKey)> {
        self.map.get(&code).copied()
    }
}

impl KeysConfig {
    /// Returns a player's effective bindings, including defaults for a missing section.
    pub fn effective(&self, player: Player) -> PlayerKeys {
        let (slot, default_fn): (&Option<PlayerKeys>, fn() -> PlayerKeys) = match player {
            Player::One => (&self.p1, PlayerKeys::default_p1),
            Player::Two => (&self.p2, PlayerKeys::default_p2),
        };
        slot.clone().unwrap_or_else(default_fn)
    }

    /// Returns mutable effective bindings, materializing defaults when necessary.
    pub fn effective_mut(&mut self, player: Player) -> &mut PlayerKeys {
        let (slot, default_fn): (&mut Option<PlayerKeys>, fn() -> PlayerKeys) = match player {
            Player::One => (&mut self.p1, PlayerKeys::default_p1),
            Player::Two => (&mut self.p2, PlayerKeys::default_p2),
        };
        slot.get_or_insert_with(default_fn)
    }
}

/// Binds `code` to a player's control, removing any previous use by either player.
pub fn bind_key(cfg: &mut Config, player: Player, key: BindKey, code: KeyCode) {
    // Materialize defaults first, then remove every conflicting binding from both players.
    for p in [Player::One, Player::Two] {
        let keys = cfg.keys.effective_mut(p);
        for k in BindKey::ALL {
            if keys.get(k) == Some(code) {
                keys.set(k, None);
            }
        }
    }
    cfg.keys.effective_mut(player).set(key, Some(code));
}

/// Platform user-data directory for configuration and ROMs:
/// macOS `~/Library/Application Support/Playmate`, Windows `%APPDATA%\Playmate`,
/// and Linux `$XDG_CONFIG_HOME/playmate` or `~/.config/playmate`.
/// Returns `None` when required environment variables are unavailable.
pub fn data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Application Support/Playmate"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(appdata).join("Playmate"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(xdg).join("playmate"));
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".config/playmate"))
    }
}

/// Ensures the user-data directory and its `roms/` subdirectory exist.
/// Failure is logged at startup without preventing the application from opening.
pub fn ensure_data_dirs() {
    let Some(dir) = data_dir() else {
        return;
    };
    let roms = dir.join("roms");
    match std::fs::create_dir_all(&roms) {
        Ok(()) => log::debug!("data directory ready: {roms:?}"),
        Err(e) => log::warn!("failed to create data directory {roms:?}: {e}"),
    }
}

/// Directory for battery saves and instant save states, following the same
/// portable-versus-user-directory rule as the configuration file: a local
/// `playmate.toml` keeps saves next to it, otherwise they go to the user
/// data directory.
pub fn saves_dir() -> PathBuf {
    if PathBuf::from(CONFIG_FILE).exists() {
        return PathBuf::from("saves");
    }
    match data_dir() {
        Some(dir) => dir.join("saves"),
        None => PathBuf::from("saves"),
    }
}

/// Configuration path: use an existing local `playmate.toml` for portable or
/// development setups; otherwise use the platform user-data directory.
fn config_path() -> PathBuf {
    let local = PathBuf::from(CONFIG_FILE);
    if local.exists() {
        return local;
    }
    match data_dir() {
        Some(dir) => dir.join(CONFIG_FILE),
        None => local,
    }
}

/// Saves configuration with both player sections materialized for direct editing.
pub fn save(cfg: &mut Config) -> Result<()> {
    // Materialize both sections so the written file contains every binding.
    cfg.keys.effective_mut(Player::One);
    cfg.keys.effective_mut(Player::Two);
    let text = toml::to_string_pretty(cfg).context("序列化配置失败")?;
    let content = format!(
        "# Playmate configuration (generated by Settings; may be edited manually)\n# See playmate.example.toml for key names\n\n{text}"
    );
    let path = config_path();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("创建目录 {parent:?} 失败"))?;
    }
    std::fs::write(&path, content).with_context(|| format!("写入 {path:?} 失败"))?;
    log::info!("configuration saved to {path:?}");
    Ok(())
}

/// Loads `playmate.toml` from the selected path or uses built-in defaults when absent.
pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        log::info!("{CONFIG_FILE} not found; using default bindings (see playmate.example.toml)");
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("读取 {path:?} 失败"))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("解析 {path:?} 失败，请检查格式"))?;
    log::info!("configuration loaded from {path:?}");
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Default layout uses WASD for P1 and arrows plus numpad for P2,
    /// with turbo triggers above each player's B/A keys.
    #[test]
    fn default_layout_lookup() {
        let map = InputMap::from_config(&Config::default());
        assert_eq!(
            map.lookup(KeyCode::KeyW),
            Some((Player::One, BindKey::Btn(Button::Up)))
        );
        assert_eq!(
            map.lookup(KeyCode::KeyJ),
            Some((Player::One, BindKey::Btn(Button::B)))
        );
        assert_eq!(
            map.lookup(KeyCode::ShiftLeft),
            Some((Player::One, BindKey::Btn(Button::Select)))
        );
        assert_eq!(
            map.lookup(KeyCode::KeyU),
            Some((Player::One, BindKey::TurboB))
        );
        assert_eq!(
            map.lookup(KeyCode::KeyI),
            Some((Player::One, BindKey::TurboA))
        );
        assert_eq!(
            map.lookup(KeyCode::ArrowUp),
            Some((Player::Two, BindKey::Btn(Button::Up)))
        );
        assert_eq!(
            map.lookup(KeyCode::Numpad0),
            Some((Player::Two, BindKey::Btn(Button::B)))
        );
        assert_eq!(
            map.lookup(KeyCode::NumpadDecimal),
            Some((Player::Two, BindKey::Btn(Button::A)))
        );
        assert_eq!(
            map.lookup(KeyCode::NumpadEnter),
            Some((Player::Two, BindKey::Btn(Button::Start)))
        );
        assert_eq!(
            map.lookup(KeyCode::Numpad1),
            Some((Player::Two, BindKey::TurboB))
        );
        // P2 Select is unbound by default, and Escape is reserved.
        assert_eq!(map.lookup(KeyCode::Escape), None);
    }

    /// A `[keys.p1]` section completely replaces the P1 defaults.
    #[test]
    fn config_section_overrides_default() {
        let cfg: Config = toml::from_str(
            r#"
            [keys.p1]
            up = "KeyI"
            a = "KeyL"
            "#,
        )
        .unwrap();
        let map = InputMap::from_config(&cfg);
        // Keys declared in the section are active.
        assert_eq!(
            map.lookup(KeyCode::KeyI),
            Some((Player::One, BindKey::Btn(Button::Up)))
        );
        assert_eq!(
            map.lookup(KeyCode::KeyL),
            Some((Player::One, BindKey::Btn(Button::A)))
        );
        // Omitted keys are unbound, so the default W binding no longer applies.
        assert_eq!(map.lookup(KeyCode::KeyW), None);
        // P2 retains defaults because its section is absent.
        assert_eq!(
            map.lookup(KeyCode::ArrowLeft),
            Some((Player::Two, BindKey::Btn(Button::Left)))
        );
    }

    /// Turbo triggers resolve to their underlying console button.
    #[test]
    fn bind_key_turbo_maps_to_button() {
        assert_eq!(BindKey::TurboA.button(), Button::A);
        assert_eq!(BindKey::TurboB.button(), Button::B);
        assert!(BindKey::TurboA.is_turbo());
        assert!(!BindKey::Btn(Button::A).is_turbo());
    }

    /// Misspelled field names are rejected instead of silently ignored.
    #[test]
    fn unknown_field_is_rejected() {
        let result: std::result::Result<Config, _> = toml::from_str(
            r#"
            [keys.p1]
            upp = "KeyI"
            "#,
        );
        assert!(result.is_err());
    }
}
