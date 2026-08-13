//! Basic types: screen constants, player slots, controller buttons, and input bitmaps.

/// NES screen width in pixels.
pub const SCREEN_WIDTH: u32 = 256;

/// NES screen height in pixels.
pub const SCREEN_HEIGHT: u32 = 240;

/// Number of bytes in one RGBA8 frame buffer (256 x 240 x 4).
pub const FRAME_BYTES: usize = (SCREEN_WIDTH * SCREEN_HEIGHT * 4) as usize;

/// Actual NTSC console frame rate (approximately 60.0988 fps, not exactly 60).
pub const NTSC_FPS: f64 = 60.0988;

/// Player slot; this project supports two players.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Player {
    /// Player 1, the host's local player.
    One,
    /// Player 2, either a second local player or the remote player in netplay.
    Two,
}

/// The eight buttons on a standard NES/Famicom controller.
///
/// Each discriminant is the button's bit position in [`ButtonState`].
/// The order matches the console's $4016 read order: A, B, Select, Start,
/// Up, Down, Left, and Right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Button {
    /// A button.
    A = 0,
    /// B button.
    B = 1,
    /// Select button.
    Select = 2,
    /// Start button.
    Start = 3,
    /// D-pad up.
    Up = 4,
    /// D-pad down.
    Down = 5,
    /// D-pad left.
    Left = 6,
    /// D-pad right.
    Right = 7,
}

impl Button {
    /// All buttons in bitmap order.
    pub const ALL: [Button; 8] = [
        Button::A,
        Button::B,
        Button::Select,
        Button::Start,
        Button::Up,
        Button::Down,
        Button::Left,
        Button::Right,
    ];
}

/// Complete one-byte input bitmap for a controller.
///
/// Each button occupies one bit. This byte is sent directly as the input
/// payload in the network protocol. Because each update replaces the previous
/// state, a dropped packet does not need to be retransmitted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ButtonState(u8);

impl ButtonState {
    /// Empty state with no buttons pressed.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Creates a state from a raw bitmap byte, typically during network decoding.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns the raw bitmap byte, typically for network encoding.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Sets whether a button is pressed.
    pub fn set(&mut self, button: Button, pressed: bool) {
        let mask = 1u8 << (button as u8);
        if pressed {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
    }

    /// Returns whether a button is currently pressed.
    pub const fn pressed(self, button: Button) -> bool {
        self.0 & (1 << (button as u8)) != 0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// `set`, `pressed`, and `bits` preserve the same bitmap state.
    #[test]
    fn button_state_bitmap_roundtrip() {
        let mut state = ButtonState::empty();
        state.set(Button::A, true);
        state.set(Button::Right, true);

        assert!(state.pressed(Button::A));
        assert!(state.pressed(Button::Right));
        assert!(!state.pressed(Button::B));

        // bit0 = A, bit7 = Right
        assert_eq!(state.bits(), 0b1000_0001);
        // Reconstructing the state from its serialized bitmap preserves it.
        assert_eq!(ButtonState::from_bits(state.bits()), state);

        state.set(Button::A, false);
        assert!(!state.pressed(Button::A));
        assert_eq!(state.bits(), 0b1000_0000);
    }
}
