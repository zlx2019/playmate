//! User configuration: `playmate.toml` loading, default bindings, and keyboard lookup.
//!
//! The configuration file is read from the current working directory when
//! present; otherwise the built-in layout is used. Parse errors are reported
//! instead of silently ignoring invalid user changes.

use std::collections::HashMap;
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

/// Bindings for the eight NES/Famicom buttons; `None` means unbound.
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
}

impl PlayerKeys {
    /// Built-in P1 layout: WASD + J(B)/K(A) + left Shift(Select)/Enter(Start).
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
        }
    }

    /// Built-in P2 layout: arrows + numpad 0(B)/decimal(A) + numpad Enter(Start).
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
        }
    }

    /// Returns the physical key bound to an NES/Famicom button.
    pub fn get(&self, button: Button) -> Option<KeyCode> {
        match button {
            Button::Up => self.up,
            Button::Down => self.down,
            Button::Left => self.left,
            Button::Right => self.right,
            Button::A => self.a,
            Button::B => self.b,
            Button::Select => self.select,
            Button::Start => self.start,
        }
    }

    /// Sets or clears the physical key bound to an NES/Famicom button.
    pub fn set(&mut self, button: Button, code: Option<KeyCode>) {
        let slot = match button {
            Button::Up => &mut self.up,
            Button::Down => &mut self.down,
            Button::Left => &mut self.left,
            Button::Right => &mut self.right,
            Button::A => &mut self.a,
            Button::B => &mut self.b,
            Button::Select => &mut self.select,
            Button::Start => &mut self.start,
        };
        *slot = code;
    }

    /// Iterates over `(physical key, console button)` pairs, skipping unbound entries.
    fn bindings(&self) -> impl Iterator<Item = (KeyCode, Button)> {
        [
            (self.up, Button::Up),
            (self.down, Button::Down),
            (self.left, Button::Left),
            (self.right, Button::Right),
            (self.a, Button::A),
            (self.b, Button::B),
            (self.select, Button::Select),
            (self.start, Button::Start),
        ]
        .into_iter()
        .filter_map(|(code, button)| code.map(|c| (c, button)))
    }
}

/// Keyboard lookup table: physical key -> player and console button.
pub struct InputMap {
    /// Flattened bindings; P2 wins when both players bind the same physical key.
    map: HashMap<KeyCode, (Player, Button)>,
}

impl InputMap {
    /// Builds the lookup table, applying built-in defaults for missing player sections.
    pub fn from_config(cfg: &Config) -> Self {
        let mut map = HashMap::new();
        let p1 = cfg.keys.p1.clone().unwrap_or_else(PlayerKeys::default_p1);
        let p2 = cfg.keys.p2.clone().unwrap_or_else(PlayerKeys::default_p2);
        for (code, button) in p1.bindings() {
            map.insert(code, (Player::One, button));
        }
        for (code, button) in p2.bindings() {
            map.insert(code, (Player::Two, button));
        }
        Self { map }
    }

    /// Looks up the binding for a physical key.
    pub fn lookup(&self, code: KeyCode) -> Option<(Player, Button)> {
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

/// Binds `code` to a player's button, removing any previous use by either player.
pub fn bind_key(cfg: &mut Config, player: Player, button: Button, code: KeyCode) {
    // Materialize defaults first, then remove every conflicting binding from both players.
    for p in [Player::One, Player::Two] {
        let keys = cfg.keys.effective_mut(p);
        for b in Button::ALL {
            if keys.get(b) == Some(code) {
                keys.set(b, None);
            }
        }
    }
    cfg.keys.effective_mut(player).set(button, Some(code));
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

    /// Default layout uses WASD for P1 and arrows plus numpad for P2.
    #[test]
    fn default_layout_lookup() {
        let map = InputMap::from_config(&Config::default());
        assert_eq!(map.lookup(KeyCode::KeyW), Some((Player::One, Button::Up)));
        assert_eq!(map.lookup(KeyCode::KeyJ), Some((Player::One, Button::B)));
        assert_eq!(
            map.lookup(KeyCode::ShiftLeft),
            Some((Player::One, Button::Select))
        );
        assert_eq!(
            map.lookup(KeyCode::ArrowUp),
            Some((Player::Two, Button::Up))
        );
        assert_eq!(map.lookup(KeyCode::Numpad0), Some((Player::Two, Button::B)));
        assert_eq!(
            map.lookup(KeyCode::NumpadDecimal),
            Some((Player::Two, Button::A))
        );
        assert_eq!(
            map.lookup(KeyCode::NumpadEnter),
            Some((Player::Two, Button::Start))
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
        assert_eq!(map.lookup(KeyCode::KeyI), Some((Player::One, Button::Up)));
        assert_eq!(map.lookup(KeyCode::KeyL), Some((Player::One, Button::A)));
        // Omitted keys are unbound, so the default W binding no longer applies.
        assert_eq!(map.lookup(KeyCode::KeyW), None);
        // P2 retains defaults because its section is absent.
        assert_eq!(
            map.lookup(KeyCode::ArrowLeft),
            Some((Player::Two, Button::Left))
        );
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
