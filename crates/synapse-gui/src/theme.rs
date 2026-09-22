//! Dark theme with the Razer green accent, plus font setup.

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Shadow, Stroke, TextStyle, Theme, Visuals,
};

pub const ACCENT: Color32 = Color32::from_rgb(0x44, 0xD6, 0x2C);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x2B, 0x7F, 0x1E);
pub const ACCENT_BG: Color32 = Color32::from_rgb(0x1B, 0x33, 0x17);
pub const BG: Color32 = Color32::from_rgb(0x0D, 0x0E, 0x10);
pub const PANEL: Color32 = Color32::from_rgb(0x14, 0x15, 0x18);
pub const CARD: Color32 = Color32::from_rgb(0x1B, 0x1D, 0x21);
pub const CARD_BORDER: Color32 = Color32::from_rgb(0x28, 0x2B, 0x30);
pub const CONTROL: Color32 = Color32::from_rgb(0x25, 0x28, 0x2D);
pub const CONTROL_HOVER: Color32 = Color32::from_rgb(0x30, 0x34, 0x3A);
pub const TEXT: Color32 = Color32::from_rgb(0xE9, 0xEA, 0xEC);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x96, 0x9B, 0xA3);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5E, 0x63, 0x6B);
pub const DANGER: Color32 = Color32::from_rgb(0xFF, 0x5A, 0x5A);
pub const WARNING: Color32 = Color32::from_rgb(0xFF, 0xB4, 0x30);

/// Font family used for headings.
pub fn heading_family() -> FontFamily {
    FontFamily::Name("heading".into())
}

pub fn heading(size: f32) -> FontId {
    FontId::new(size, heading_family())
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(Theme::Dark);

    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, CARD_BORDER);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    v.extreme_bg_color = BG;
    v.faint_bg_color = CARD;
    v.code_bg_color = BG;
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = WARNING;
    v.error_fg_color = DANGER;
    v.override_text_color = None;
    v.slider_trailing_fill = true;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.5, ACCENT);

    let radius = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = CARD;
    w.noninteractive.weak_bg_fill = CARD;
    w.noninteractive.bg_stroke = Stroke::new(1.0, CARD_BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = CONTROL;
    w.inactive.weak_bg_fill = CONTROL;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = radius;

    w.hovered.bg_fill = CONTROL_HOVER;
    w.hovered.weak_bg_fill = CONTROL_HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    w.hovered.fg_stroke = Stroke::new(1.5, TEXT);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = ACCENT_DIM;
    w.active.weak_bg_fill = ACCENT_DIM;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.bg_fill = CONTROL_HOVER;
    w.open.weak_bg_fill = CONTROL_HOVER;
    w.open.corner_radius = radius;

    ctx.set_visuals_of(Theme::Dark, v);
    ctx.style_mut_of(Theme::Dark, |style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.slider_width = 220.0;
        style.spacing.combo_width = 160.0;
        style.animation_time = 0.12;
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
            (TextStyle::Heading, heading(20.0)),
        ]
        .into();
    });
}

/// Prefer the desktop's Noto Sans (native look, full Turkish coverage);
/// fall back to egui's bundled fonts.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let regular = find_font(&["NotoSans-Regular.ttf"], "Noto Sans:style=Regular");
    let bold = find_font(&["NotoSans-SemiBold.ttf", "NotoSans-Bold.ttf"], "Noto Sans:style=Bold");

    if let Some(data) = regular {
        fonts
            .font_data
            .insert("ui-regular".into(), Arc::new(FontData::from_owned(data)));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "ui-regular".into());
    }
    let mut heading_fonts: Vec<String> = Vec::new();
    if let Some(data) = bold {
        fonts
            .font_data
            .insert("ui-bold".into(), Arc::new(FontData::from_owned(data)));
        heading_fonts.push("ui-bold".into());
    }
    // Fall back to the proportional stack (includes emoji/symbol fonts).
    heading_fonts.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts.families.insert(heading_family(), heading_fonts);

    ctx.set_fonts(fonts);
}

fn find_font(file_names: &[&str], fc_pattern: &str) -> Option<Vec<u8>> {
    const DIRS: &[&str] = &[
        "/usr/share/fonts/noto",
        "/usr/share/fonts/truetype/noto",
        "/usr/share/fonts/google-noto",
        "/usr/share/fonts/noto-fonts",
        "/usr/local/share/fonts",
    ];
    for dir in DIRS {
        for name in file_names {
            if let Ok(data) = std::fs::read(std::path::Path::new(dir).join(name)) {
                return Some(data);
            }
        }
    }
    // Ask fontconfig, but only accept a real Noto Sans TrueType file.
    let output = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", fc_pattern])
        .output()
        .ok()?;
    let path = String::from_utf8(output.stdout).ok()?;
    let lower = path.to_lowercase();
    if lower.contains("notosans-") && lower.ends_with(".ttf") {
        std::fs::read(path.trim()).ok()
    } else {
        None
    }
}
