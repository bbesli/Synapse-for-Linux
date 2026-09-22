//! Small custom widgets: cards, switches, chips, battery gauge.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2, WidgetInfo, WidgetType,
};

use crate::theme::{
    self, ACCENT, ACCENT_DIM, BG, CARD, CARD_BORDER, CONTROL, CONTROL_HOVER, DANGER, TEXT, TEXT_DIM, WARNING,
};

/// A rounded panel that groups related settings.
pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, CARD_BORDER))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

pub fn card_title(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    ui.label(RichText::new(title).font(theme::heading(16.5)).color(TEXT));
    if let Some(subtitle) = subtitle {
        ui.add(Label::new(RichText::new(subtitle).color(TEXT_DIM).size(12.5)).wrap());
    }
    ui.add_space(4.0);
}

pub fn section_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).color(TEXT_DIM).size(11.0).strong());
}

/// Title + optional description on the left, a control on the right.
pub fn setting_row<R>(
    ui: &mut Ui,
    title: &str,
    description: Option<&str>,
    control_width: f32,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    let total = ui.available_width();
    ui.horizontal_top(|ui| {
        let text_width = (total - control_width - 16.0).max(140.0);
        ui.allocate_ui_with_layout(Vec2::new(text_width, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_max_width(text_width);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(RichText::new(title).color(TEXT).size(14.5));
            if let Some(description) = description {
                ui.add(Label::new(RichText::new(description).color(TEXT_DIM).size(12.0)).wrap());
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Min), control).inner
    })
    .inner
}

/// Lay `add` out left to right in a box of `width`; for multi-part controls
/// (slider + value) inside right-aligned rows.
pub fn ltr_box<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(Vec2::new(width, 24.0), Layout::left_to_right(Align::Center), add)
        .inner
}

/// iOS style on/off switch.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let size = Vec2::new(42.0, 24.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *on, ""));

    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let off_color = if response.hovered() { CONTROL_HOVER } else { CONTROL };
        let mut track = lerp_color(off_color, ACCENT_DIM, t);
        let mut knob = lerp_color(TEXT_DIM, ACCENT, t);
        if !enabled {
            track = track.gamma_multiply(0.45);
            knob = knob.gamma_multiply(0.45);
        }
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), track);
        let radius = rect.height() / 2.0 - 4.0;
        let x = egui::lerp((rect.left() + radius + 4.0)..=(rect.right() - radius - 4.0), t);
        painter.circle_filled(egui::pos2(x, rect.center().y), radius, knob);
    }
    response
}

/// Pill shaped selectable button.
pub fn chip(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let text_color = if selected { BG } else { TEXT };
    let fill = if selected { ACCENT } else { CONTROL };
    let button = egui::Button::new(RichText::new(text).color(text_color).size(13.5))
        .fill(fill)
        .stroke(if selected {
            Stroke::NONE
        } else {
            Stroke::new(1.0, CARD_BORDER)
        })
        .corner_radius(CornerRadius::same(15))
        .min_size(Vec2::new(0.0, 30.0));
    ui.add(button)
}

pub fn secondary_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(TEXT))
            .fill(CONTROL)
            .stroke(Stroke::new(1.0, CARD_BORDER))
            .corner_radius(CornerRadius::same(6))
            .min_size(Vec2::new(0.0, 30.0)),
    )
}

pub fn battery_color(level: u8) -> Color32 {
    match level {
        0..=15 => DANGER,
        16..=35 => WARNING,
        _ => ACCENT,
    }
}

/// Battery outline with a level fill and a bolt when charging.
pub fn battery_gauge(ui: &mut Ui, level: Option<u8>, charging: bool, size: Vec2) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let nub_w = (size.x * 0.07).max(2.0);
    let body = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - nub_w - 1.0, rect.max.y));
    let radius = CornerRadius::same((size.y * 0.18) as u8);
    let stroke_w = (size.y * 0.09).clamp(1.2, 3.0);
    painter.rect_stroke(body, radius, Stroke::new(stroke_w, TEXT_DIM), StrokeKind::Inside);
    let nub_h = size.y * 0.4;
    let nub = egui::Rect::from_min_size(
        egui::pos2(body.max.x + 1.0, body.center().y - nub_h / 2.0),
        Vec2::new(nub_w, nub_h),
    );
    painter.rect_filled(nub, CornerRadius::same(1), TEXT_DIM);

    if let Some(level) = level {
        let inner = body.shrink(stroke_w + (size.y * 0.06).max(1.0));
        let width = inner.width() * f32::from(level.min(100)) / 100.0;
        if width > 0.5 {
            let fill = egui::Rect::from_min_size(inner.min, Vec2::new(width, inner.height()));
            painter.rect_filled(fill, CornerRadius::same((size.y * 0.1) as u8), battery_color(level));
        }
    }
    if charging {
        painter.text(
            body.center(),
            Align2::CENTER_CENTER,
            "⚡",
            FontId::proportional(size.y * 0.8),
            Color32::WHITE,
        );
    }
    response
}

/// Small colored dot, e.g. connection state.
pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// A simple two-column key/value grid row.
pub fn info_row(ui: &mut Ui, key: &str, value: &str) {
    ui.label(RichText::new(key).color(TEXT_DIM));
    ui.add(Label::new(RichText::new(value).color(TEXT)).selectable(true));
    ui.end_row();
}

/// Line icons drawn with the painter (consistent look, no font dependency).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Headphones,
    Microphone,
    MicrophoneMuted,
    Battery,
    Info,
    Gear,
}

/// Paint `glyph` centered at `center`, `size` points wide (20 unit design grid).
pub fn paint_glyph(painter: &egui::Painter, center: egui::Pos2, size: f32, glyph: Glyph, color: Color32) {
    use std::f32::consts::PI;
    let s = size / 20.0;
    let p = |x: f32, y: f32| center + Vec2::new((x - 10.0) * s, (y - 10.0) * s);
    let stroke = Stroke::new(1.8 * s, color);
    let radius = |r: f32| CornerRadius::same((r * s).round() as u8);
    let arc = |cx: f32, cy: f32, r: f32, from: f32, to: f32| -> Vec<egui::Pos2> {
        (0..=20)
            .map(|i| from + (to - from) * i as f32 / 20.0)
            .map(|a| p(cx + r * a.cos(), cy + r * a.sin()))
            .collect()
    };
    match glyph {
        Glyph::Headphones => {
            painter.add(egui::Shape::line(arc(10.0, 11.0, 7.2, PI, 2.0 * PI), stroke));
            painter.rect_filled(egui::Rect::from_min_max(p(2.0, 10.5), p(6.2, 17.8)), radius(1.6), color);
            painter.rect_filled(
                egui::Rect::from_min_max(p(13.8, 10.5), p(18.0, 17.8)),
                radius(1.6),
                color,
            );
        }
        Glyph::Microphone | Glyph::MicrophoneMuted => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(7.0, 1.8), p(13.0, 12.2)),
                radius(3.0),
                stroke,
                StrokeKind::Middle,
            );
            painter.add(egui::Shape::line(arc(10.0, 9.2, 6.0, 0.0, PI), stroke));
            painter.line_segment([p(10.0, 15.2), p(10.0, 18.4)], stroke);
            painter.line_segment([p(6.8, 18.4), p(13.2, 18.4)], stroke);
            if glyph == Glyph::MicrophoneMuted {
                painter.line_segment([p(3.0, 2.5), p(17.0, 17.5)], Stroke::new(2.0 * s, color));
            }
        }
        Glyph::Battery => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(1.8, 5.8), p(16.4, 14.2)),
                radius(2.0),
                stroke,
                StrokeKind::Middle,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(p(17.2, 8.2), p(18.9, 11.8)),
                radius(0.8),
                color,
            );
            painter.rect_filled(egui::Rect::from_min_max(p(4.1, 8.1), p(11.2, 11.9)), radius(0.8), color);
        }
        Glyph::Info => {
            painter.circle_stroke(p(10.0, 10.0), 8.0 * s, stroke);
            painter.circle_filled(p(10.0, 6.2), 1.25 * s, color);
            painter.line_segment([p(10.0, 9.0), p(10.0, 14.6)], Stroke::new(2.0 * s, color));
        }
        Glyph::Gear => {
            for k in 0..8 {
                let a = k as f32 * PI / 4.0;
                painter.line_segment(
                    [
                        p(10.0 + 5.6 * a.cos(), 10.0 + 5.6 * a.sin()),
                        p(10.0 + 8.6 * a.cos(), 10.0 + 8.6 * a.sin()),
                    ],
                    Stroke::new(2.8 * s, color),
                );
            }
            painter.circle_stroke(p(10.0, 10.0), 5.6 * s, Stroke::new(2.2 * s, color));
            painter.circle_stroke(p(10.0, 10.0), 2.0 * s, stroke);
        }
    }
}

/// Allocate space for a glyph in the layout and paint it.
pub fn glyph(ui: &mut Ui, glyph: Glyph, size: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_glyph(ui.painter(), rect.center(), size, glyph, color);
    response
}
