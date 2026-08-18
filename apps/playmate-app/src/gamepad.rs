//! Gamepad support: gilrs polling, player-slot assignment, and button mapping.
//!
//! Mapping follows common emulator conventions using an Xbox-style layout:
//! - D-pad / left stick -> NES/Famicom directions
//! - South button (A) -> B, east button (B) -> A, preserving the console's B-left/A-right layout
//! - West button (X) -> turbo B, north button (Y) -> turbo A, mirroring B/A one row up
//! - Select/Back -> Select, Start/Menu -> Start
//! - Mode (Xbox guide / PS button) -> toggles the in-game pause menu
//!
//! Gamepads are assigned to P1 and P2 in first-input order and released on disconnect.

use gilrs::{Axis, Button as PadButton, Event, EventType, GamepadId, Gilrs};
use playmate_core::{Button, ButtonState, Player};

/// Axis threshold used to convert analog stick movement into digital directions.
const STICK_THRESHOLD: f32 = 0.5;

/// Gamepad input manager.
pub struct GamepadInput {
    /// gilrs context; `None` after initialization failure, while keyboard input remains available.
    gilrs: Option<Gilrs>,
    /// Gamepad IDs currently assigned to the P1/P2 slots.
    slots: [Option<GamepadId>; 2],
    /// Gamepad button bitmap for each player.
    states: [ButtonState; 2],
    /// Turbo-held bitmap for each player; the caller applies the fire cadence.
    turbo: [ButtonState; 2],
    /// Edge flag set by a Mode (guide) press, taken by the caller to toggle the menu.
    menu_press: bool,
}

impl GamepadInput {
    /// Initializes gamepad support; failure degrades gracefully to keyboard-only input.
    pub fn new() -> Self {
        let gilrs = match Gilrs::new() {
            Ok(g) => Some(g),
            Err(e) => {
                log::warn!("failed to initialize gamepad support; keyboard remains available: {e}");
                None
            }
        };
        Self {
            gilrs,
            slots: [None; 2],
            states: [ButtonState::empty(); 2],
            turbo: [ButtonState::empty(); 2],
            menu_press: false,
        }
    }

    /// Polls all pending gamepad events and reports whether any player state changed.
    pub fn poll(&mut self) -> bool {
        let Some(gilrs) = self.gilrs.as_mut() else {
            return false;
        };
        let mut changed = false;
        while let Some(event) = gilrs.next_event() {
            changed |= Self::on_event(
                &mut self.slots,
                &mut self.states,
                &mut self.turbo,
                &mut self.menu_press,
                gilrs,
                event,
            );
        }
        changed
    }

    /// Returns and clears the pending Mode-press flag; the application
    /// toggles the in-game menu on it, mirroring the Esc key.
    pub fn take_menu_press(&mut self) -> bool {
        std::mem::take(&mut self.menu_press)
    }

    /// Returns the current gamepad bitmap for a player.
    pub fn state(&self, player: Player) -> ButtonState {
        self.states[Self::index(player)]
    }

    /// Returns the turbo-held bitmap for a player; buttons in it fire only
    /// while the caller's turbo cadence is in its on phase.
    pub fn turbo(&self, player: Player) -> ButtonState {
        self.turbo[Self::index(player)]
    }

    /// Maps a player to its slot index.
    fn index(player: Player) -> usize {
        match player {
            Player::One => 0,
            Player::Two => 1,
        }
    }

    /// Finds a gamepad's slot, assigning the first available slot with P1 preferred.
    fn slot_of(slots: &mut [Option<GamepadId>; 2], gilrs: &Gilrs, id: GamepadId) -> Option<usize> {
        if let Some(i) = slots.iter().position(|s| *s == Some(id)) {
            return Some(i);
        }
        let free = slots.iter().position(Option::is_none)?;
        slots[free] = Some(id);
        log::info!(
            "gamepad [{}] connected and assigned to P{}",
            gilrs.gamepad(id).name(),
            free + 1
        );
        Some(free)
    }

    /// Handles one gilrs event and reports whether a state bitmap changed.
    fn on_event(
        slots: &mut [Option<GamepadId>; 2],
        states: &mut [ButtonState; 2],
        turbo: &mut [ButtonState; 2],
        menu_press: &mut bool,
        gilrs: &Gilrs,
        event: Event,
    ) -> bool {
        match event.event {
            // Mode toggles the menu regardless of slot assignment.
            EventType::ButtonPressed(PadButton::Mode, _) => {
                *menu_press = true;
                false
            }
            EventType::ButtonPressed(button, _) => {
                Self::apply_button(slots, states, turbo, gilrs, event.id, button, true)
            }
            EventType::ButtonReleased(button, _) => {
                Self::apply_button(slots, states, turbo, gilrs, event.id, button, false)
            }
            EventType::AxisChanged(axis, value, _) => {
                Self::apply_axis(slots, states, gilrs, event.id, axis, value)
            }
            EventType::Disconnected => {
                // Release the slot and clear its state so buttons cannot remain stuck.
                if let Some(i) = slots.iter().position(|s| *s == Some(event.id)) {
                    slots[i] = None;
                    states[i] = ButtonState::empty();
                    turbo[i] = ButtonState::empty();
                    log::info!("P{} gamepad disconnected", i + 1);
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Updates the NES/Famicom bitmap from a physical gamepad button.
    fn apply_button(
        slots: &mut [Option<GamepadId>; 2],
        states: &mut [ButtonState; 2],
        turbo: &mut [ButtonState; 2],
        gilrs: &Gilrs,
        id: GamepadId,
        button: PadButton,
        pressed: bool,
    ) -> bool {
        let (target, is_turbo) = match button {
            PadButton::South => (Button::B, false),
            PadButton::East => (Button::A, false),
            PadButton::West => (Button::B, true),
            PadButton::North => (Button::A, true),
            PadButton::DPadUp => (Button::Up, false),
            PadButton::DPadDown => (Button::Down, false),
            PadButton::DPadLeft => (Button::Left, false),
            PadButton::DPadRight => (Button::Right, false),
            PadButton::Select => (Button::Select, false),
            PadButton::Start => (Button::Start, false),
            _ => return false,
        };
        let Some(i) = Self::slot_of(slots, gilrs, id) else {
            return false;
        };
        if is_turbo {
            turbo[i].set(target, pressed);
        } else {
            states[i].set(target, pressed);
        }
        true
    }

    /// Converts a stick or D-pad axis into digital directions.
    /// One axis controls two opposing directions, pressing one side past the
    /// threshold while releasing the other.
    fn apply_axis(
        slots: &mut [Option<GamepadId>; 2],
        states: &mut [ButtonState; 2],
        gilrs: &Gilrs,
        id: GamepadId,
        axis: Axis,
        value: f32,
    ) -> bool {
        // gilrs uses positive X for right and positive Y for up.
        let (positive, negative) = match axis {
            Axis::LeftStickX | Axis::DPadX => (Button::Right, Button::Left),
            Axis::LeftStickY | Axis::DPadY => (Button::Up, Button::Down),
            _ => return false,
        };
        let Some(i) = Self::slot_of(slots, gilrs, id) else {
            return false;
        };
        states[i].set(positive, value > STICK_THRESHOLD);
        states[i].set(negative, value < -STICK_THRESHOLD);
        true
    }
}
