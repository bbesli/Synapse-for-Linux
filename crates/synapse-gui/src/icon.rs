//! Procedurally drawn icons (window icon and tray icon), so the binary
//! needs no image files at runtime.

/// RGBA color, 0..=255 per channel.
type Rgba = [u8; 4];

const GREEN: Rgba = [0x44, 0xD6, 0x2C, 0xFF];
const AMBER: Rgba = [0xFF, 0xB4, 0x30, 0xFF];
const RED: Rgba = [0xFF, 0x5A, 0x5A, 0xFF];
const GREY: Rgba = [0x9A, 0x9E, 0xA5, 0xFF];
const BACKGROUND: Rgba = [0x16, 0x18, 0x1B, 0xFF];
const TRACK: Rgba = [0x55, 0x5A, 0x62, 0xFF];

/// Signed distance to a rounded rectangle (negative inside).
fn sd_round_rect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - hw + r;
    let qy = (py - cy).abs() - hh + r;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// Signed distance to the upper half of a ring with round caps.
fn sd_arc(px: f32, py: f32, cx: f32, cy: f32, radius: f32, thickness: f32) -> f32 {
    if py <= cy {
        ((px - cx).hypot(py - cy) - radius).abs() - thickness / 2.0
    } else {
        let left = (px - (cx - radius)).hypot(py - cy);
        let right = (px - (cx + radius)).hypot(py - cy);
        left.min(right) - thickness / 2.0
    }
}

/// Distance field of the headset glyph in a unit square.
fn headset(px: f32, py: f32) -> f32 {
    let band = sd_arc(px, py, 0.5, 0.56, 0.31, 0.085);
    let left = sd_round_rect(px, py, 0.22, 0.66, 0.085, 0.155, 0.07);
    let right = sd_round_rect(px, py, 0.78, 0.66, 0.085, 0.155, 0.07);
    band.min(left).min(right)
}

fn blend(dst: &mut Rgba, src: Rgba, coverage: f32) {
    let a = f32::from(src[3]) / 255.0 * coverage.clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let da = f32::from(dst[3]) / 255.0;
    let out_a = a + da * (1.0 - a);
    for c in 0..3 {
        let s = f32::from(src[c]) / 255.0;
        let d = f32::from(dst[c]) / 255.0;
        let v = (s * a + d * da * (1.0 - a)) / out_a.max(1e-6);
        dst[c] = (v * 255.0).round() as u8;
    }
    dst[3] = (out_a * 255.0).round() as u8;
}

fn coverage(distance: f32, size: u32) -> f32 {
    // `distance` is in unit coordinates; convert to pixels for anti-aliasing.
    (0.5 - distance * size as f32).clamp(0.0, 1.0)
}

/// Application icon: green headset on a dark rounded square.
pub fn app_icon_rgba(size: u32) -> Vec<u8> {
    let mut pixels = vec![[0u8; 4]; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let px = (x as f32 + 0.5) / size as f32;
            let py = (y as f32 + 0.5) / size as f32;
            let p = &mut pixels[(y * size + x) as usize];
            blend(
                p,
                BACKGROUND,
                coverage(sd_round_rect(px, py, 0.5, 0.5, 0.47, 0.47, 0.2), size),
            );
            let glyph = headset((px - 0.5) / 0.78 + 0.5, (py - 0.5) / 0.78 + 0.5) * 0.78;
            blend(p, GREEN, coverage(glyph, size));
        }
    }
    pixels.concat()
}

/// Tray icon: headset glyph colored by state with a battery bar underneath.
/// Offline draws a grey glyph without the bar.
pub fn tray_icon_rgba(size: u32, online: bool, battery: Option<u8>) -> Vec<u8> {
    let battery = battery.filter(|_| online);
    let color = match (online, battery) {
        (false, _) => GREY,
        (true, Some(0..=15)) => RED,
        (true, Some(16..=35)) => AMBER,
        (true, _) => GREEN,
    };
    let mut pixels = vec![[0u8; 4]; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let px = (x as f32 + 0.5) / size as f32;
            let py = (y as f32 + 0.5) / size as f32;
            let p = &mut pixels[(y * size + x) as usize];
            // Glyph shrunk into the upper part.
            let scale = 0.86;
            let glyph = headset((px - 0.5) / scale + 0.5, (py - 0.43) / scale + 0.5) * scale;
            blend(p, color, coverage(glyph, size));
            if let Some(level) = battery {
                let bar_y = 0.9;
                let track = sd_round_rect(px, py, 0.5, bar_y, 0.42, 0.06, 0.05);
                blend(p, TRACK, coverage(track, size));
                let fill_w = 0.84 * f32::from(level.min(100)) / 100.0;
                if fill_w > 0.0 {
                    let fill = sd_round_rect(px, py, 0.08 + fill_w / 2.0, bar_y, fill_w / 2.0, 0.06, 0.05);
                    blend(p, color, coverage(fill.max(track), size));
                }
            }
        }
    }
    pixels.concat()
}

/// RGBA -> ARGB32 in network byte order (StatusNotifierItem pixmaps).
pub fn rgba_to_argb(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| [a, r, g, b])
        .collect()
}

pub fn window_icon() -> egui::IconData {
    let size = 128;
    egui::IconData {
        rgba: app_icon_rgba(size),
        width: size,
        height: size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_have_expected_size_and_content() {
        let icon = app_icon_rgba(64);
        assert_eq!(icon.len(), 64 * 64 * 4);
        // Corner is transparent, center of the left ear cup is green.
        assert_eq!(icon[3], 0);
        let at = |x: usize, y: usize| &icon[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        // Left ear cup center: (0.5 + (0.22 - 0.5) * 0.78, 0.5 + (0.66 - 0.5) * 0.78) * 64.
        let cup = at(18, 40);
        assert!(cup[1] > 150 && cup[3] == 255, "{cup:?}");

        let tray = tray_icon_rgba(32, true, Some(10));
        assert_eq!(tray.len(), 32 * 32 * 4);
        assert_eq!(rgba_to_argb(&[1, 2, 3, 4]), vec![4, 1, 2, 3]);
    }
}
