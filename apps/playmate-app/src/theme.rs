//! Global Playmate theme with a retro NES/Famicom console style.
//!
//! The palette draws from the classic red-and-white console: near-black
//! background, dark-gray cards, red accents, and off-white text. Reusable
//! headers, cards, rows, and primary buttons keep pages visually consistent.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Response, RichText, Sense,
    Stroke, StrokeKind, TextStyle, Ui, vec2,
};

// ---------- Color constants ----------

/// Near-black window background.
pub const BG: Color32 = Color32::from_rgb(0x14, 0x14, 0x17);
/// Card background.
pub const CARD: Color32 = Color32::from_rgb(0x1E, 0x1E, 0x23);
/// Hovered card background.
pub const CARD_HOVER: Color32 = Color32::from_rgb(0x28, 0x28, 0x2F);
/// Recessed background for inputs and similar controls.
pub const SUNKEN: Color32 = Color32::from_rgb(0x0E, 0x0E, 0x11);
/// Console red used as the primary accent.
pub const RED: Color32 = Color32::from_rgb(0xE6, 0x00, 0x12);
/// Bright red for readable text and outlines on dark backgrounds.
pub const RED_BRIGHT: Color32 = Color32::from_rgb(0xFF, 0x45, 0x52);
/// Off-white body text.
pub const TEXT: Color32 = Color32::from_rgb(0xEC, 0xE6, 0xDA);
/// Muted text.
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x8F, 0x8D, 0x88);
/// Subtle border.
pub const OUTLINE: Color32 = Color32::from_rgb(0x2E, 0x2E, 0x36);
/// Softer red for the P1 badge and other larger areas.
pub const P1_RED: Color32 = Color32::from_rgb(0xD8, 0x3A, 0x45);
/// Blue for the P2 badge.
pub const P2_BLUE: Color32 = Color32::from_rgb(0x4A, 0x7B, 0xD8);
/// Online status green.
pub const GREEN: Color32 = Color32::from_rgb(0x53, 0xC2, 0x7E);

/// Card corner radius.
const RADIUS: u8 = 10;

// ---------- Global style ----------

/// Applies the global theme once at startup.
pub fn apply(ctx: &egui::Context) {
    // Always use the dark theme, independent of the system setting.
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();

    // Type scale.
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(24.0));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(15.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(15.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(12.0));

    // Roomier spacing and button padding.
    style.spacing.item_spacing = vec2(10.0, 10.0);
    style.spacing.button_padding = vec2(14.0, 7.0);
    style.spacing.interact_size.y = 30.0;

    let v = &mut style.visuals;
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, OUTLINE);
    v.window_corner_radius = CornerRadius::same(12);
    v.extreme_bg_color = SUNKEN;
    v.faint_bg_color = CARD_HOVER;
    v.hyperlink_color = RED_BRIGHT;
    // Use translucent red for selected labels and text selection.
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(0xE6, 0x00, 0x12, 70);
    v.selection.stroke = Stroke::new(1.0, RED_BRIGHT);

    // Widget states progress from dark gray to brighter hover with red outline, then solid red.
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = CARD;
    w.noninteractive.weak_bg_fill = CARD;
    w.noninteractive.bg_stroke = Stroke::new(1.0, OUTLINE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = CornerRadius::same(RADIUS);

    w.inactive.bg_fill = CARD;
    w.inactive.weak_bg_fill = CARD;
    w.inactive.bg_stroke = Stroke::new(1.0, OUTLINE);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = CornerRadius::same(RADIUS);

    w.hovered.bg_fill = CARD_HOVER;
    w.hovered.weak_bg_fill = CARD_HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, RED);
    w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.hovered.corner_radius = CornerRadius::same(RADIUS);
    w.hovered.expansion = 1.0;

    w.active.bg_fill = RED;
    w.active.weak_bg_fill = RED;
    w.active.bg_stroke = Stroke::new(1.0, RED_BRIGHT);
    w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.active.corner_radius = CornerRadius::same(RADIUS);

    w.open.bg_fill = CARD_HOVER;
    w.open.weak_bg_fill = CARD_HOVER;
    w.open.bg_stroke = Stroke::new(1.0, RED);
    w.open.fg_stroke = Stroke::new(1.0, TEXT);
    w.open.corner_radius = CornerRadius::same(RADIUS);

    ctx.set_style_of(egui::Theme::Dark, style);
}

// ---------- Reusable components ----------

/// Card container with a dark background, subtle border, rounded corners, and padding.
pub fn card() -> Frame {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, OUTLINE))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
}

/// Page header with a back button, title, and red underline; returns whether Back was clicked.
pub fn page_header(ui: &mut Ui, title: &str) -> bool {
    let mut back = false;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        back = ui
            .add(egui::Button::new(RichText::new("←").size(17.0)).min_size(vec2(38.0, 32.0)))
            .on_hover_cursor(CursorIcon::PointingHand)
            .clicked();
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.add_space(1.0);
            ui.label(RichText::new(title).size(22.0).strong());
            // Short red underline matching the primary heading.
            let (rect, _) = ui.allocate_exact_size(vec2(26.0, 3.0), Sense::hover());
            ui.painter()
                .rect(rect, 2, RED, Stroke::NONE, StrokeKind::Inside);
        });
    });
    ui.add_space(10.0);
    back
}

/// Card section heading with a red vertical bar and bold text.
pub fn section_title(ui: &mut Ui, title: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(4.0, 17.0), Sense::hover());
        ui.painter()
            .rect(rect, 2, RED, Stroke::NONE, StrokeKind::Inside);
        ui.label(RichText::new(title).size(16.0).strong());
    });
}

/// Reusable list row with icon, title, optional subtitle, and trailing arrow.
///
/// Hover brightens the background, fades the border to red, and shifts the
/// arrow right with a 120 ms transition.
pub fn card_row(
    ui: &mut Ui,
    icon: &str,
    title: &str,
    subtitle: Option<&str>,
    enabled: bool,
) -> Response {
    let width = ui.available_width();
    let height = if subtitle.is_some() { 58.0 } else { 48.0 };
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), sense);
    if !ui.is_rect_visible(rect) {
        return resp;
    }

    let hovered = enabled && resp.hovered();
    // A 0-to-1 hover transition drives the background, border, and arrow animation.
    let t = ui.ctx().animate_bool_with_time(resp.id, hovered, 0.12);
    let bg = CARD.lerp_to_gamma(CARD_HOVER, t);
    let stroke = OUTLINE.lerp_to_gamma(RED, t);
    let p = ui.painter();
    p.rect(
        rect,
        RADIUS,
        bg,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );

    let (title_color, icon_color) = if enabled {
        (TEXT, TEXT)
    } else {
        (TEXT_WEAK, TEXT_WEAK)
    };
    p.text(
        rect.left_center() + vec2(18.0, 0.0),
        Align2::LEFT_CENTER,
        icon,
        FontId::proportional(20.0),
        icon_color,
    );
    let text_x = 56.0;
    match subtitle {
        Some(sub) => {
            p.text(
                rect.left_top() + vec2(text_x, 12.0),
                Align2::LEFT_TOP,
                title,
                FontId::proportional(16.0),
                title_color,
            );
            p.text(
                rect.left_bottom() + vec2(text_x, -11.0),
                Align2::LEFT_BOTTOM,
                sub,
                FontId::proportional(12.0),
                TEXT_WEAK,
            );
        }
        None => {
            p.text(
                rect.left_center() + vec2(text_x, 0.0),
                Align2::LEFT_CENTER,
                title,
                FontId::proportional(16.0),
                title_color,
            );
        }
    }
    if enabled {
        let arrow_color = TEXT_WEAK.lerp_to_gamma(RED_BRIGHT, t);
        // Use U+203A because both the fallback and CJK fonts include it.
        p.text(
            rect.right_center() + vec2(-18.0 + t * 4.0, 0.0),
            Align2::CENTER_CENTER,
            "›",
            FontId::proportional(20.0),
            arrow_color,
        );
    }

    if hovered {
        resp.clone().on_hover_cursor(CursorIcon::PointingHand);
    }
    resp
}

/// Full-width red primary action button with centered text.
pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    // A justified layout fills the row and keeps the button label centered.
    ui.with_layout(
        egui::Layout::top_down_justified(egui::Align::Center),
        |ui| {
            // Use a white hover border because red is invisible against the red fill.
            let w = &mut ui.style_mut().visuals.widgets;
            w.hovered.bg_stroke = Stroke::new(1.5, Color32::WHITE);
            w.hovered.expansion = 1.5;
            let button = egui::Button::new(
                RichText::new(text)
                    .size(16.0)
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(RED)
            .min_size(vec2(160.0, 40.0));
            ui.add_enabled(enabled, button)
                .on_hover_cursor(CursorIcon::PointingHand)
        },
    )
    .inner
}

/// Error or status banner with a translucent red background and bright red text.
pub fn error_banner(ui: &mut Ui, msg: &str) {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(0xE6, 0x00, 0x12, 24))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(0xE6, 0x00, 0x12, 90),
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("⚠").color(RED_BRIGHT));
                ui.label(RichText::new(msg).color(RED_BRIGHT));
            });
        });
}

/// P1/P2 slot badge with a colored background and bold white text.
pub fn slot_badge(ui: &mut Ui, slot: Option<playmate_core::Player>) {
    let (label, color) = match slot {
        Some(playmate_core::Player::One) => ("P1", P1_RED),
        Some(playmate_core::Player::Two) => ("P2", P2_BLUE),
        None => ("--", OUTLINE),
    };
    let (rect, _) = ui.allocate_exact_size(vec2(40.0, 28.0), Sense::hover());
    let p = ui.painter();
    p.rect(rect, 6, color, Stroke::NONE, StrokeKind::Inside);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(15.0),
        Color32::WHITE,
    );
}
