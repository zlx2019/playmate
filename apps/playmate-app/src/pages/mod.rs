//! UI rendering modules for each page.
//!
//! Each page exposes a `show` function that accepts egui state and returns the
//! action triggered during the frame for [`crate::app::PlaymateApp`] to route.

pub mod game_menu;
pub mod game_select;
pub mod lobby;
pub mod main_menu;
pub mod room;
pub mod settings;
