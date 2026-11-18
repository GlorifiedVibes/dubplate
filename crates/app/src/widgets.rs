use egui::{Color32, Pos2, Response, Sense, Stroke, Ui};
use std::f32::consts::PI;

use crate::app::palette;

// Indicator arc: 7:30 o'clock (min) → 4:30 o'clock (max), 270° clockwise travel.
// Screen angles: 0=right, π/2=down, π=left, 3π/2=up.
const ARC_START: f32 = 3.0 * PI / 4.0; // 135° = bottom-left = 7:30
const ARC_SWEEP: f32 = 3.0 * PI / 2.0; // 270° clockwise

/// Circular drag knob. `radius` controls knob size (18.0 for deck controls, 13.0 for mixer).
/// Drag up/down to change value; double-click resets to `default`.
pub fn knob(
    ui: &mut Ui,
    value: &mut f32,
    min: f32,
    max: f32,
    default: f32,
    label: &str,
    accent: Color32,
    radius: f32,
) -> Response {
    let size = egui::vec2(radius * 2.0 + 4.0, radius * 2.0 + 4.0 + crate::theme::sz(10.0));
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());

    if response.double_clicked() {
        if (*value - default).abs() > 1e-6 {
            *value = default;
            response.mark_changed();
        }
    } else if response.dragged() {
        let nv = (*value - response.drag_delta().y * (max - min) / 100.0).clamp(min, max);
        if (nv - *value).abs() > 1e-6 {
            *value = nv;
            response.mark_changed();
        }
    }

    if ui.is_rect_visible(rect) {
        let p = ui.painter_at(rect);
        let c = egui::pos2(rect.center().x, rect.top() + radius + 2.0);
        let t = (*value - min) / (max - min);

        p.circle_filled(c, radius, palette::raised());
        p.circle_stroke(c, radius, Stroke::new(1.5_f32, palette::outline()));

        // Full-range dim arc
        arc_stroke(&p, c, radius * 0.70, 0.0, 1.0, palette::track(), 2.0);

        // Value arc
        if t > 0.005 {
            let col = if response.hovered() || response.dragged() { accent } else { accent.linear_multiply(0.75) };
            arc_stroke(&p, c, radius * 0.70, 0.0, t, col, 2.0);
        }

        // Indicator line
        let a = ARC_START + t * ARC_SWEEP;
        let tip = egui::pos2(c.x + a.cos() * (radius - 3.5), c.y + a.sin() * (radius - 3.5));
        p.line_segment([c, tip], Stroke::new(2.0_f32, palette::text_bright()));

        // Label below, free to be wider than the knob
        ui.painter().text(
            egui::pos2(rect.center().x, rect.bottom()),
            egui::Align2::CENTER_BOTTOM,
            label,
            egui::FontId::monospace(crate::theme::sz(9.0)),
            palette::text_dim(),
        );
    }

    response
}

/// Filter sweep knob.
/// `mode`: 0 = LP+HP sweep (centre=bypass), 1 = LP only (0=bypass,1=deep),
///          2 = HP only (0=bypass,1=deep).
/// Double-click resets to bypass (0.5 for mode 0, 0.0 for modes 1/2).
pub fn filter_knob(ui: &mut Ui, value: &mut f32, mode: u8, radius: f32) -> Response {
    let size = egui::vec2(radius * 2.0 + 4.0, radius * 2.0 + 4.0 + crate::theme::sz(10.0));
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());

    let default_val = if mode == 0 { 0.5_f32 } else { 0.0 };
    if response.double_clicked() {
        *value = default_val;
        response.mark_changed();
    } else if response.dragged() {
        let nv = (*value - response.drag_delta().y / 100.0).clamp(0.0, 1.0);
        if (nv - *value).abs() > 1e-4 {
            *value = nv;
            response.mark_changed();
        }
    }

    if ui.is_rect_visible(rect) {
        let p = ui.painter_at(rect);
        let c = egui::pos2(rect.center().x, rect.top() + radius + 2.0);
        let t = *value;

        p.circle_filled(c, radius, palette::raised());
        p.circle_stroke(c, radius, Stroke::new(1.5_f32, palette::outline()));

        // Dim full-range arc
        arc_stroke(&p, c, radius * 0.70, 0.0, 1.0, palette::track(), 2.0);

        // Active arc — colour and position depend on mode
        let active = match mode {
            1 => {
                // LP only: full range orange
                if t > 0.005 {
                    Some((palette::filter_low(), 0.0, t))
                } else {
                    None
                }
            }
            2 => {
                // HP only: full range blue
                if t > 0.005 {
                    Some((palette::filter_high(), 0.0, t))
                } else {
                    None
                }
            }
            _ => {
                // LP+HP sweep
                const DEAD: f32 = 0.025;
                if (t - 0.5).abs() > DEAD {
                    if t < 0.5 {
                        Some((palette::filter_low(), t, 0.5))
                    } else {
                        Some((palette::filter_high(), 0.5, t))
                    }
                } else {
                    None
                }
            }
        };
        if let Some((col, t0, t1)) = active {
            let col = if response.hovered() || response.dragged() { col } else { col.linear_multiply(0.75) };
            arc_stroke(&p, c, radius * 0.70, t0, t1, col, 2.5);
        }

        // Bypass notch (12 o'clock for mode 0, min position for modes 1/2)
        let notch_t = if mode == 0 { 0.5_f32 } else { 0.0 };
        let notch_a = ARC_START + notch_t * ARC_SWEEP;
        let outer = egui::pos2(c.x + notch_a.cos() * (radius + 1.0), c.y + notch_a.sin() * (radius + 1.0));
        let inner = egui::pos2(c.x + notch_a.cos() * (radius * 0.80), c.y + notch_a.sin() * (radius * 0.80));
        p.line_segment([outer, inner], Stroke::new(1.5_f32, palette::tick()));

        // Indicator line
        let a = ARC_START + t * ARC_SWEEP;
        let tip = egui::pos2(c.x + a.cos() * (radius - 3.5), c.y + a.sin() * (radius - 3.5));
        p.line_segment([c, tip], Stroke::new(2.0_f32, palette::text_bright()));

        // Label, free to be wider than the knob
        ui.painter().text(
            egui::pos2(rect.center().x, rect.bottom()),
            egui::Align2::CENTER_BOTTOM,
            "FILTER",
            egui::FontId::monospace(crate::theme::sz(9.0)),
            palette::text_dim(),
        );
    }

    response
}

fn arc_stroke(p: &egui::Painter, c: Pos2, r: f32, t0: f32, t1: f32, col: Color32, w: f32) {
    let segs = 24usize;
    let pts: Vec<Pos2> = (0..=segs)
        .map(|i| {
            let frac = i as f32 / segs as f32;
            let t = t0 + (t1 - t0) * frac;
            let a = ARC_START + t * ARC_SWEEP;
            egui::pos2(c.x + a.cos() * r, c.y + a.sin() * r)
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(w, col)));
}

/// Vertical DJ-style fader with relative drag behaviour.
///
/// Unlike `egui::Slider`, this widget does NOT jump to the clicked position.
/// It only responds to drag — value changes by the relative distance you move
/// the handle, exactly like a hardware DJ fader.
///
/// Returns a `Response` where `.changed()` is set when the value was moved.
pub fn vertical_fader(
    ui: &mut Ui,
    value: &mut f32,
    min: f32,
    max: f32,
    height: f32,
    label: &str,
    accent: Color32,
    default: Option<f32>,
) -> Response {
    let handle_w = 32.0;
    let handle_h = 14.0;

    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(handle_w, height), Sense::click_and_drag());

    if let (true, Some(d)) = (response.double_clicked(), default) {
        *value = d;
        response.mark_changed();
    } else if response.dragged() {
        let usable_h = height - handle_h;
        if usable_h > 0.0 {
            let prev = *value;
            // Drag up → negative dy → value increases (faster)
            // Drag down → positive dy → value decreases (slower)
            *value = (*value - response.drag_delta().y * (max - min) / usable_h).clamp(min, max);
            if (*value - prev).abs() > 1e-6 {
                response.mark_changed();
            }
        }
    }

    if ui.is_rect_visible(rect) {
        let p = ui.painter_at(rect);
        let cx = rect.center().x;
        let usable_h = height - handle_h;

        // Track
        let track_rect = egui::Rect::from_center_size(
            egui::pos2(cx, rect.top() + handle_h / 2.0 + usable_h / 2.0),
            egui::vec2(5.0, usable_h),
        );
        p.rect_filled(track_rect, 2.5, palette::inset());
        p.rect_stroke(track_rect, 2.5, Stroke::new(1.0_f32, palette::outline()));

        // Centre notch at 0% (only when range spans zero)
        if min < 0.0 && max > 0.0 {
            let zero_t = -min / (max - min);
            let notch_y = rect.top() + handle_h / 2.0 + zero_t * usable_h;
            p.line_segment(
                [egui::pos2(cx - 9.0, notch_y), egui::pos2(cx + 9.0, notch_y)],
                Stroke::new(1.0_f32, palette::tick()),
            );
        }

        // Handle — t=1.0 (max) is at top, t=0.0 (min) is at bottom
        let t = (*value - min) / (max - min);
        let handle_cy = rect.top() + handle_h / 2.0 + (1.0 - t) * usable_h;
        let handle_rect = egui::Rect::from_center_size(egui::pos2(cx, handle_cy), egui::vec2(handle_w, handle_h));

        let is_active = response.hovered() || response.dragged();
        p.rect_filled(handle_rect, 3.0, if is_active { palette::handle_active() } else { palette::handle() });
        p.rect_stroke(
            handle_rect,
            3.0,
            Stroke::new(if is_active { 1.5_f32 } else { 1.0_f32 }, if is_active { accent } else { palette::tick() }),
        );
        // Grip line
        p.line_segment(
            [egui::pos2(cx - 9.0, handle_cy), egui::pos2(cx + 9.0, handle_cy)],
            Stroke::new(1.5_f32, palette::handle_line()),
        );

        // Label below the fader
        p.text(
            egui::pos2(cx, rect.bottom() + 2.0),
            egui::Align2::CENTER_TOP,
            label,
            egui::FontId::monospace(crate::theme::sz(9.0)),
            palette::text_dim(),
        );
    }

    response
}

/// Horizontal DJ fader (crossfader). Occupies exactly `width` x 24, so a centring
/// parent layout really does centre it. Drag moves relatively; double-click recentres.
pub fn horizontal_fader(ui: &mut Ui, value: &mut f32, width: f32, accent: Color32) -> Response {
    let handle_w = 16.0;
    let handle_h = 24.0;
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(width, handle_h), Sense::click_and_drag());

    let usable_w = width - handle_w;
    if response.double_clicked() {
        *value = 0.5;
        response.mark_changed();
    } else if response.dragged() && usable_w > 0.0 {
        let nv = (*value + response.drag_delta().x / usable_w).clamp(0.0, 1.0);
        if (nv - *value).abs() > 1e-6 {
            *value = nv;
            response.mark_changed();
        }
    }

    if ui.is_rect_visible(rect) {
        let p = ui.painter_at(rect);
        let cy = rect.center().y;
        let track = egui::Rect::from_center_size(rect.center(), egui::vec2(usable_w, 5.0));
        p.rect_filled(track, 2.5, palette::inset());
        p.rect_stroke(track, 2.5, Stroke::new(1.0_f32, palette::outline()));

        let cx = rect.center().x;
        p.line_segment([egui::pos2(cx, cy - 9.0), egui::pos2(cx, cy + 9.0)], Stroke::new(1.0_f32, palette::tick()));

        let hx = rect.left() + handle_w / 2.0 + *value * usable_w;
        let handle = egui::Rect::from_center_size(egui::pos2(hx, cy), egui::vec2(handle_w, handle_h));
        let active = response.hovered() || response.dragged();
        p.rect_filled(handle, 3.0, if active { palette::handle_active() } else { palette::handle() });
        p.rect_stroke(
            handle,
            3.0,
            Stroke::new(if active { 1.5_f32 } else { 1.0_f32 }, if active { accent } else { palette::tick() }),
        );
        p.line_segment(
            [egui::pos2(hx, cy - 8.0), egui::pos2(hx, cy + 8.0)],
            Stroke::new(1.5_f32, palette::handle_line()),
        );
    }
    response
}

/// Styled transport / action button. Active state lights the border and text in `active_color`.
pub fn transport_btn(
    ui: &mut Ui,
    label: &str,
    active: bool,
    enabled: bool,
    size: egui::Vec2,
    active_color: Color32,
) -> Response {
    // Disabled buttons must not register clicks — otherwise commands fire
    // before the track is loaded and the subsequent Load resets is_playing.
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);

    if ui.is_rect_visible(rect) {
        let bg = if !enabled {
            palette::inset()
        } else if active {
            active_color.linear_multiply(0.18)
        } else if response.hovered() {
            palette::hover()
        } else {
            palette::raised()
        };
        let border_col = if active {
            active_color
        } else if response.hovered() && enabled {
            palette::tick()
        } else {
            palette::pressed()
        };
        let border_w = if active { 1.5_f32 } else { 1.0 };

        ui.painter_at(rect).rect(rect, egui::Rounding::same(4.0), bg, Stroke::new(border_w, border_col));

        let text_col = if !enabled {
            palette::outline()
        } else if active {
            active_color
        } else {
            palette::text()
        };

        ui.painter_at(rect).text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(crate::theme::sz(if size.y >= 40.0 { 13.0 } else { 10.5 })),
            text_col,
        );
    }

    response
}
