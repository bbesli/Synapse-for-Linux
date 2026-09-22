//! Interactive 10-band equalizer curve editor.

use egui::{Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};

use synapse_core::eq::{self, BAND_COUNT, Bands, MAX_GAIN_DB};

use crate::theme::{ACCENT, BG, CARD_BORDER, TEXT, TEXT_DIM, TEXT_FAINT};

/// View range (a little wider than the hardware range for breathing room).
const VIEW_MIN: f32 = -10.5;
const VIEW_MAX: f32 = 7.5;
/// Pixels of wheel scrolling per 1 dB step.
const SCROLL_STEP: f32 = 40.0;

pub struct EqGraphOutput {
    /// `bands` was modified this frame.
    pub changed: bool,
    /// A drag or click finished: a good moment to apply.
    pub committed: bool,
    /// Changed with the mouse wheel (apply after a short pause).
    pub scrolled: bool,
}

/// Draw the curve. `ghost` is an optional second curve (e.g. what the
/// headset currently uses while the user is editing).
#[allow(clippy::needless_range_loop)] // band index drives position, id and value
pub fn eq_graph(ui: &mut Ui, id_salt: &str, bands: &mut Bands, ghost: Option<&Bands>, editable: bool) -> EqGraphOutput {
    let id = Id::new(("eq_graph", id_salt));
    let width = ui.available_width().max(360.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 270.0), Sense::hover());
    let plot = Rect::from_min_max(rect.min + Vec2::new(46.0, 22.0), rect.max - Vec2::new(18.0, 30.0));

    let x_of = |i: usize| plot.left() + (i as f32 + 0.5) * plot.width() / BAND_COUNT as f32;
    let y_of = |db: f32| plot.bottom() - (db - VIEW_MIN) / (VIEW_MAX - VIEW_MIN) * plot.height();
    let db_at = |y: f32| {
        let db = VIEW_MIN + (plot.bottom() - y) / plot.height() * (VIEW_MAX - VIEW_MIN);
        eq::clamp_gain(db.round() as i32)
    };

    let mut out = EqGraphOutput {
        changed: false,
        committed: false,
        scrolled: false,
    };
    let painter = ui.painter_at(rect);
    let dim = |c: Color32| if editable { c } else { c.gamma_multiply(0.5) };

    // Background and grid.
    painter.rect_filled(plot.expand2(Vec2::new(8.0, 10.0)), CornerRadius::same(10), BG);
    for db in [6, 3, 0, -3, -6, -9] {
        let y = y_of(db as f32);
        let (color, width) = if db == 0 { (TEXT_FAINT, 1.2) } else { (CARD_BORDER, 1.0) };
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(width, color),
        );
        let label = if db > 0 { format!("+{db}") } else { db.to_string() };
        painter.text(
            Pos2::new(plot.left() - 14.0, y),
            Align2::RIGHT_CENTER,
            label,
            FontId::proportional(11.0),
            TEXT_FAINT,
        );
    }
    painter.text(
        Pos2::new(plot.left() - 14.0, plot.top() - 14.0),
        Align2::RIGHT_CENTER,
        "dB",
        FontId::proportional(10.5),
        TEXT_FAINT,
    );
    for i in 0..BAND_COUNT {
        let x = x_of(i);
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.0, CARD_BORDER.gamma_multiply(0.6)),
        );
        painter.text(
            Pos2::new(x, plot.bottom() + 16.0),
            Align2::CENTER_CENTER,
            eq::band_label(i),
            FontId::proportional(11.5),
            TEXT_DIM,
        );
    }
    painter.text(
        Pos2::new(plot.right() + 6.0, plot.bottom() + 16.0),
        Align2::LEFT_CENTER,
        "Hz",
        FontId::proportional(10.5),
        TEXT_FAINT,
    );

    // Interaction: one column per band.
    let column_w = plot.width() / BAND_COUNT as f32;
    let mut active_band = None;
    for i in 0..BAND_COUNT {
        let column = Rect::from_center_size(
            Pos2::new(x_of(i), plot.center().y),
            Vec2::new(column_w, plot.height() + 24.0),
        );
        let sense = if editable {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        };
        let response = ui.interact(column, id.with(i), sense);
        if !editable {
            continue;
        }
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            active_band = Some(i);
        }
        if (response.dragged() || response.clicked())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let db = db_at(pos.y);
            if db != bands[i] {
                bands[i] = db;
                out.changed = true;
            }
        }
        if response.drag_stopped() || response.clicked() {
            out.committed = true;
        }
        if response.hovered() {
            let delta: f32 = ui.ctx().input(|input| {
                input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::MouseWheel { unit, delta, .. } => Some(match unit {
                            egui::MouseWheelUnit::Line => delta.y * SCROLL_STEP,
                            egui::MouseWheelUnit::Point => delta.y,
                            egui::MouseWheelUnit::Page => delta.y * SCROLL_STEP * 5.0,
                        }),
                        _ => None,
                    })
                    .sum()
            });
            if delta != 0.0 {
                let acc_id = id.with("scroll");
                let mut acc: f32 = ui.data(|d| d.get_temp(acc_id)).unwrap_or(0.0) + delta;
                while acc.abs() >= SCROLL_STEP {
                    let step = if acc > 0.0 { 1 } else { -1 };
                    let db = eq::clamp_gain(i32::from(bands[i]) + step);
                    if db != bands[i] {
                        bands[i] = db;
                        out.changed = true;
                        out.scrolled = true;
                    }
                    acc -= SCROLL_STEP * acc.signum();
                }
                ui.data_mut(|d| d.insert_temp(acc_id, acc));
            }
            // Keep the page from scrolling while adjusting a band.
            ui.ctx().input_mut(|input| input.smooth_scroll_delta = Vec2::ZERO);
        }
    }

    // Ghost curve (applied state while editing).
    if let Some(ghost) = ghost
        && ghost != bands
    {
        let points = smooth_curve(&ghost.map(|b| Pos2::new(0.0, y_of(f32::from(b)))), &x_of, plot);
        painter.add(Shape::dashed_line(
            &points,
            Stroke::new(1.2, TEXT_DIM.gamma_multiply(0.7)),
            6.0,
            4.0,
        ));
    }

    // Filled area between the curve and 0 dB, then the curve itself.
    let points = smooth_curve(&bands.map(|b| Pos2::new(0.0, y_of(f32::from(b)))), &x_of, plot);
    let zero_y = y_of(0.0);
    let mut mesh = Mesh::default();
    let fill = dim(ACCENT).gamma_multiply(0.16);
    for pair in points.windows(2) {
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(pair[0], fill);
        mesh.colored_vertex(pair[1], fill);
        mesh.colored_vertex(Pos2::new(pair[1].x, zero_y), fill);
        mesh.colored_vertex(Pos2::new(pair[0].x, zero_y), fill);
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    painter.add(Shape::mesh(mesh));
    painter.line(points, Stroke::new(2.5, dim(ACCENT)));

    // Handles and values.
    for i in 0..BAND_COUNT {
        let center = Pos2::new(x_of(i), y_of(f32::from(bands[i])));
        let hot = active_band == Some(i);
        let radius = if hot { 8.0 } else { 6.0 };
        painter.circle_filled(center, radius + 2.0, BG);
        painter.circle_filled(center, radius, dim(ACCENT));
        if hot {
            painter.circle_stroke(center, radius + 3.5, Stroke::new(1.5, ACCENT.gamma_multiply(0.6)));
        }
        let label = if bands[i] > 0 {
            format!("+{}", bands[i])
        } else {
            bands[i].to_string()
        };
        let above = f32::from(bands[i]) < f32::from(MAX_GAIN_DB) - 1.0;
        let offset = if above { -18.0 } else { 18.0 };
        painter.text(
            Pos2::new(center.x, center.y + offset),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(if hot { 13.0 } else { 11.5 }),
            if hot { TEXT } else { dim(TEXT_DIM) },
        );
    }
    out
}

/// Monotone cubic (Fritsch–Carlson) curve through the band points, flat
/// out to the plot edges. Only `y` of `points` is used; `x` comes from `x_of`.
fn smooth_curve(points: &[Pos2; BAND_COUNT], x_of: &dyn Fn(usize) -> f32, plot: Rect) -> Vec<Pos2> {
    let xs: Vec<f32> = (0..BAND_COUNT).map(x_of).collect();
    let ys: Vec<f32> = points.iter().map(|p| p.y).collect();
    let h = xs[1] - xs[0];
    let n = BAND_COUNT;

    let d: Vec<f32> = (0..n - 1).map(|k| (ys[k + 1] - ys[k]) / h).collect();
    let mut m = vec![0.0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for k in 1..n - 1 {
        m[k] = if d[k - 1] * d[k] > 0.0 {
            (d[k - 1] + d[k]) / 2.0
        } else {
            0.0
        };
    }
    for k in 0..n - 1 {
        if d[k] == 0.0 {
            m[k] = 0.0;
            m[k + 1] = 0.0;
            continue;
        }
        let a = m[k] / d[k];
        let b = m[k + 1] / d[k];
        let s = a * a + b * b;
        if s > 9.0 {
            let tau = 3.0 / s.sqrt();
            m[k] = tau * a * d[k];
            m[k + 1] = tau * b * d[k];
        }
    }

    let mut out = Vec::with_capacity(n * 12 + 2);
    out.push(Pos2::new(plot.left(), ys[0]));
    const STEPS: usize = 12;
    for k in 0..n - 1 {
        for s in 0..STEPS {
            let t = s as f32 / STEPS as f32;
            let (t2, t3) = (t * t, t * t * t);
            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h10 = t3 - 2.0 * t2 + t;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;
            let y = h00 * ys[k] + h10 * h * m[k] + h01 * ys[k + 1] + h11 * h * m[k + 1];
            out.push(Pos2::new(xs[k] + t * h, y));
        }
    }
    out.push(Pos2::new(xs[n - 1], ys[n - 1]));
    out.push(Pos2::new(plot.right(), ys[n - 1]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_passes_through_points_and_never_overshoots() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1000.0, 200.0));
        let x_of = |i: usize| (i as f32 + 0.5) * 100.0;
        let ys = [100.0, 20.0, 180.0, 180.0, 50.0, 60.0, 70.0, 190.0, 10.0, 100.0];
        let points = ys.map(|y| Pos2::new(0.0, y));
        let curve = smooth_curve(&points, &x_of, plot);
        for (i, y) in ys.iter().enumerate() {
            let p = curve.iter().find(|p| (p.x - x_of(i)).abs() < 0.01).unwrap();
            assert!((p.y - y).abs() < 0.01);
        }
        // Monotone interpolation stays inside the range of its neighbours.
        for w in curve.windows(1) {
            assert!(w[0].y >= 10.0 - 0.01 && w[0].y <= 190.0 + 0.01);
        }
        assert_eq!(curve.first().unwrap().x, 0.0);
        assert_eq!(curve.last().unwrap().x, 1000.0);
    }
}
