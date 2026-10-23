//! The CrStudio mark and wordmark, drawn in code (original work, no image files): a rounded
//! crimson tile with a white "C" stroke that opens around a play triangle — a camera aperture and
//! a play button in one shape.

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

/// Brand crimson.
pub const CRIMSON: Color32 = Color32::from_rgb(225, 29, 72);
const CRIMSON_DARK: Color32 = Color32::from_rgb(159, 18, 57);

/// Paint the square mark in `rect`.
pub fn paint_logo_mark(p: &Painter, rect: Rect) {
    let s = rect.width().min(rect.height());
    let r = Rect::from_center_size(rect.center(), vec2(s, s));
    let radius = (s * 0.24).round().clamp(2.0, 120.0) as u8;
    p.rect_filled(r, CornerRadius::same(radius), CRIMSON_DARK);
    p.rect_filled(r.shrink(s * 0.04), CornerRadius::same(radius.saturating_sub(1)), CRIMSON);
    // "C": an arc of ~290° that opens to the right.
    let c = r.center();
    let rr = s * 0.30;
    let w = (s * 0.085).max(1.2);
    let n = 40;
    let (a0, a1) = (0.62_f32, std::f32::consts::TAU - 0.62);
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            pos2(c.x + rr * a.cos(), c.y + rr * a.sin())
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(w, Color32::WHITE)));
    // Play triangle inside the C.
    let tri = vec![pos2(c.x - s * 0.08, c.y - s * 0.13), pos2(c.x + s * 0.15, c.y), pos2(c.x - s * 0.08, c.y + s * 0.13)];
    p.add(Shape::convex_polygon(tri, Color32::WHITE, Stroke::NONE));
}

/// Draw the wordmark (mark + "CrStudio") `height` points tall with its left edge at
/// `left_center` (vertically centred). Returns the drawn rect.
pub fn paint_wordmark(ui: &egui::Ui, left_center: Pos2, height: f32, dark: bool) -> Option<Rect> {
    let mark = Rect::from_min_size(pos2(left_center.x, left_center.y - height * 0.75), vec2(height * 1.5, height * 1.5));
    paint_logo_mark(ui.painter(), mark);
    let ink = if dark { Color32::WHITE } else { Color32::from_rgb(17, 24, 39) };
    let font = egui::FontId::new(height * 1.05, egui::FontFamily::Proportional);
    let g = ui.painter().layout_no_wrap("CrStudio".into(), font, ink);
    let text_pos = pos2(mark.max.x + height * 0.45, left_center.y - g.size().y / 2.0);
    let tr = Rect::from_min_size(text_pos, g.size());
    ui.painter().galley(text_pos, g, ink);
    Some(mark.union(tr))
}

/// The app icon as RGBA pixels (`size`×`size`), rendered from the same geometry: used for the
/// window icon so the Dock/taskbar shows CrStudio.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let n = size.clamp(16, 1024) as usize;
    let mut px = vec![0u8; n * n * 4];
    let s = n as f32;
    let radius = s * 0.24;
    let c = s / 2.0;
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // rounded square coverage
            let dx = (fx - c).abs() - (c - radius);
            let dy = (fy - c).abs() - (c - radius);
            let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() - radius;
            let cov = (0.5 - outside).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let mut col = [225.0, 29.0, 72.0];
            // C ring
            let d = ((fx - c).powi(2) + (fy - c).powi(2)).sqrt();
            let ang = (fy - c).atan2(fx - c).abs();
            let ring = ((d - s * 0.30).abs() < s * 0.045) && ang > 0.62;
            // triangle
            let (tx, ty) = (fx - c, fy - c);
            let tri = tx > -s * 0.08 && tx < s * 0.15 && ty.abs() < (s * 0.13) * (1.0 - (tx + s * 0.08) / (s * 0.23));
            if ring || tri {
                col = [255.0, 255.0, 255.0];
            }
            let i = (y * n + x) * 4;
            if let Some(p) = px.get_mut(i..i + 4) {
                p[0] = col[0] as u8;
                p[1] = col[1] as u8;
                p[2] = col[2] as u8;
                p[3] = (cov * 255.0) as u8;
            }
        }
    }
    px
}
