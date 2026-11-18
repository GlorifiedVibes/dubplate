use dubplate_engine::{BeatGrid, Cue, DeckHandle, DecodedTrack, LoopRegion, WAVEFORM_CHUNK_FRAMES};
use egui::{Color32, Painter, Rect, Stroke};

use crate::app::palette;
use crate::wave_draw;

const CHUNK: f64 = WAVEFORM_CHUNK_FRAMES as f64;

/// Cue / loop state to draw over a waveform (source-frame positions).
pub struct Marks<'a> {
    pub cue_point: f64,
    pub cues: &'a [Cue],
    pub loop_region: Option<LoopRegion>,
    pub loop_in: Option<f64>,
}

impl<'a> Marks<'a> {
    pub fn of(deck: &'a DeckHandle) -> Self {
        Marks { cue_point: deck.cue_point, cues: &deck.cues, loop_region: deck.loop_region, loop_in: deck.loop_in }
    }
}

fn cue_colour(c: &Cue) -> Color32 {
    Color32::from_rgb(c.color[0], c.color[1], c.color[2])
}

/// Loop region, pending loop-in, hot cue flags and the main cue over the scrolling waveform.
fn draw_markers(painter: &Painter, rect: Rect, marks: &Marks, progress: f64, n_chunks: f64, zoom: f32) {
    let half_w = rect.width() / 2.0;
    let head = progress * n_chunks;
    let x_of = |frame: f64| rect.left() + half_w + ((frame / WAVEFORM_CHUNK_FRAMES as f64) - head) as f32 * zoom;

    if let Some(r) = marks.loop_region {
        let (x0, x1) = (x_of(r.start), x_of(r.end));
        let body = Rect::from_min_max(
            egui::pos2(x0.max(rect.left()), rect.top()),
            egui::pos2(x1.min(rect.right()), rect.bottom()),
        );
        if body.width() > 0.0 {
            let (fill, edge) = if r.active {
                (palette::alpha(palette::loop_on(), 48), palette::loop_on())
            } else {
                (
                    Color32::from_rgba_unmultiplied(170, 170, 170, 22),
                    Color32::from_rgba_unmultiplied(170, 170, 170, 120),
                )
            };
            painter.rect_filled(body, 0.0, fill);
            for x in [x0, x1] {
                painter.line_segment(
                    [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                    Stroke::new(1.5_f32, edge),
                );
            }
            if r.active && x0 >= rect.left() {
                painter.text(
                    egui::pos2(x0 + 4.0, rect.bottom() - 3.0),
                    egui::Align2::LEFT_BOTTOM,
                    "LOOP",
                    egui::FontId::monospace(crate::theme::sz(9.0)),
                    palette::loop_on(),
                );
            }
        }
    }
    if let Some(a) = marks.loop_in {
        let x = x_of(a);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.5_f32, palette::loop_on()),
        );
        painter.text(
            egui::pos2(x + 3.0, rect.bottom() - 3.0),
            egui::Align2::LEFT_BOTTOM,
            "IN",
            egui::FontId::monospace(crate::theme::sz(9.0)),
            palette::loop_on(),
        );
    }

    // Main cue point
    let xc = x_of(marks.cue_point);
    painter.line_segment(
        [egui::pos2(xc, rect.top()), egui::pos2(xc, rect.bottom())],
        Stroke::new(1.0_f32, palette::cue().linear_multiply(0.8)),
    );
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(xc - 5.0, rect.bottom()),
            egui::pos2(xc + 5.0, rect.bottom()),
            egui::pos2(xc, rect.bottom() - 7.0),
        ],
        palette::cue(),
        Stroke::NONE,
    ));

    // Hot cues: coloured line + lettered flag
    for c in marks.cues {
        let Some(pad) = c.hot_cue else { continue };
        let x = x_of(c.pos_frames);
        let col = cue_colour(c);
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], Stroke::new(1.5_f32, col));
        let flag = Rect::from_min_size(egui::pos2(x, rect.top() + 14.0), egui::vec2(13.0, 12.0));
        painter.rect_filled(flag, 2.0, col);
        painter.text(
            flag.center(),
            egui::Align2::CENTER_CENTER,
            ((b'A' + pad - 1) as char).to_string(),
            egui::FontId::monospace(crate::theme::sz(9.0)),
            palette::background(),
        );
        if let Some(end) = c.out_frames {
            let xe = x_of(end);
            painter.line_segment(
                [egui::pos2(xe, rect.top()), egui::pos2(xe, rect.bottom())],
                Stroke::new(1.0_f32, col.linear_multiply(0.6)),
            );
        }
    }
}

/// Scrolling waveform centred on the playhead, from the track's waveform summary (see `wave_draw`).
/// `eq` = [lo, mid, hi] gains (0.0 = kill, 1.0 = flat). `filter_val` = 0.0 deep low-pass, 0.5 off, 1.0 deep high-pass.
/// `zoom` = pixels per 256 frames.
pub fn draw_waveform(
    painter: &Painter,
    rect: Rect,
    track: &DecodedTrack,
    grid: BeatGrid,
    marks: &Marks,
    progress: f64,
    accent: Color32,
    eq: [f32; 3],
    filter_val: f32,
    zoom: f32,
) {
    painter.rect_filled(rect, 4.0, palette::surface());
    if track.wave.is_empty() {
        return;
    }

    let head = progress * track.duration_frames as f64;
    wave_draw::scroll(painter, rect, &track.wave, head, zoom, wave_draw::Dims::of(eq, filter_val));

    draw_beat_grid_waveform(painter, rect, track, grid, progress, accent, zoom);
    draw_markers(painter, rect, marks, progress, track.duration_frames as f64 / CHUNK, zoom);

    // Playhead: a bright centre line with a soft glow.
    let (px, half_w) = (rect.center().x, rect.width() / 2.0);
    painter.line_segment(
        [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
        Stroke::new(1.5_f32, palette::text_bright()),
    );
    let glow = palette::alpha(palette::text_bright(), 34);
    for side in [-1.0f32, 1.0] {
        fade(
            painter,
            Rect::from_x_y_ranges(px.min(px + side * 4.0)..=px.max(px + side * 4.0), rect.y_range()),
            glow,
            side > 0.0,
        );
    }

    // Edges fade to black so the picture does not stop at a hard line.
    let edge = (half_w * 0.18).clamp(20.0, 60.0);
    let black = Color32::from_black_alpha(190);
    fade(painter, Rect::from_x_y_ranges(rect.left()..=rect.left() + edge, rect.y_range()), black, false);
    fade(painter, Rect::from_x_y_ranges(rect.right() - edge..=rect.right(), rect.y_range()), black, true);
}

/// A rectangle that fades out sideways in one draw: `colour` at the left edge and clear at the right, or the reverse.
fn fade(painter: &Painter, r: Rect, colour: Color32, clear_on_left: bool) {
    let (left, right) = if clear_on_left { (Color32::TRANSPARENT, colour) } else { (colour, Color32::TRANSPARENT) };
    let mut m = egui::Mesh::default();
    for (pos, c) in [(r.left_top(), left), (r.right_top(), right), (r.right_bottom(), right), (r.left_bottom(), left)] {
        m.vertices.push(egui::epaint::Vertex { pos, uv: egui::epaint::WHITE_UV, color: c });
    }
    m.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(m));
}

/// Draws beat / bar / phrase grid lines over the scrolling waveform.
fn draw_beat_grid_waveform(
    painter: &Painter,
    rect: Rect,
    track: &DecodedTrack,
    grid: BeatGrid,
    progress: f64,
    accent: Color32,
    zoom: f32,
) {
    if !grid.is_valid() {
        return;
    }

    let beat_period = grid.period_chunks(track.sample_rate);
    let beat_offset = grid.offset;

    let n = (track.duration_frames as f64 / CHUNK).ceil() as usize;
    let head_col = progress * track.duration_frames as f64 / CHUNK;
    let half_w = rect.width() / 2.0;
    let h = rect.height();
    let cy = rect.center().y;

    let mut phrase_labels: Vec<(f32, String)> = Vec::new();
    let mut prev_beat = i64::MIN;

    let left_chunk = head_col - (half_w / zoom) as f64;
    let right_chunk = head_col + (half_w / zoom) as f64;

    if beat_period < 1.0 {
        return;
    }

    // Phrase lines (every 16 beats) sit where the track's sections start when they agree on a lattice, else on the
    // first downbeat.
    let structure = crate::sections::of(track, grid);
    let phrase_phase = structure.as_ref().and_then(|s| s.phrase).map_or(0, |(_, r)| r.rem_euclid(16));

    let first_beat = ((left_chunk - beat_offset) / beat_period).floor() as i64 - 1;
    let last_beat = ((right_chunk - beat_offset) / beat_period).ceil() as i64 + 1;

    for beat_num in first_beat..=last_beat {
        let beat_chunk = beat_offset + beat_num as f64 * beat_period;
        if beat_chunk < 0.0 || beat_chunk >= n as f64 {
            continue;
        }

        let x = rect.left() + half_w + (beat_chunk - head_col) as f32 * zoom;
        if x < rect.left() || x > rect.right() {
            continue;
        }
        if beat_num == prev_beat {
            continue;
        }
        prev_beat = beat_num;

        let beat_in_bar = beat_num.rem_euclid(4);
        let beat_in_phrase = (beat_num - phrase_phase).rem_euclid(16);

        if beat_in_phrase == 0 {
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                Stroke::new(1.5_f32, accent.linear_multiply(0.9)),
            );
            let bar_num = (beat_num / 4 + 1).max(1);
            phrase_labels.push((x, format!("{bar_num}")));
        } else if beat_in_bar == 0 {
            let hh = h * 0.75;
            painter.line_segment(
                [egui::pos2(x, cy - hh * 0.5), egui::pos2(x, cy + hh * 0.5)],
                Stroke::new(1.0_f32, palette::alpha(palette::text_bright(), 150)),
            );
        } else {
            let hh = h * 0.30;
            painter.line_segment(
                [egui::pos2(x, cy - hh * 0.5), egui::pos2(x, cy + hh * 0.5)],
                Stroke::new(0.75_f32, palette::alpha(palette::text_bright(), 55)),
            );
        }
    }

    for (x, label) in phrase_labels {
        painter.text(
            egui::pos2(x + 2.0, rect.top() + 2.0),
            egui::Align2::LEFT_TOP,
            label,
            egui::FontId::monospace(crate::theme::sz(8.0)),
            accent.linear_multiply(0.90),
        );
    }

    if let Some(s) = &structure {
        let sr = track.sample_rate.max(1) as f64;
        crate::sections::draw_flags(
            painter,
            rect,
            s,
            |secs| rect.left() + half_w + (secs * sr / CHUNK - head_col) as f32 * zoom,
            |secs| crate::sections::bar_and_beat(secs, grid, track.sample_rate),
        );
    }
}

/// Compact overview strip — spectral colour, full track in one view.
pub fn draw_overview(
    painter: &Painter,
    rect: Rect,
    track: &DecodedTrack,
    grid: BeatGrid,
    marks: &Marks,
    progress: f64,
    accent: Color32,
    stems: Option<(&dubplate_engine::StemWaves, [f32; 4])>,
) {
    painter.rect_filled(rect, 2.0, palette::background());

    if track.wave.is_empty() {
        return;
    }
    let n = (track.duration_frames as usize).div_ceil(WAVEFORM_CHUNK_FRAMES).max(1);
    let w = rect.width();
    let h = rect.height();
    let cy = rect.center().y;
    match stems {
        // the section blocks take the top few points, so the four rows sit below them
        Some((waves, levels)) => wave_draw::overview_lanes(
            painter,
            Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 7.0), rect.max),
            &waves.0,
            &palette::stems(),
            levels,
            progress,
        ),
        None => wave_draw::overview(painter, rect, &track.wave, progress),
    }
    if let Some(s) = crate::sections::of(track, grid) {
        crate::sections::draw_strip(painter, rect, &s, track.duration_secs);
    }

    // ── Bar / phrase markers ─────────────────────────────────────────────────
    if grid.is_valid() {
        let beat_period = grid.period_chunks(track.sample_rate);
        let bar_period = beat_period * 4.0;

        let mut chunk = grid.offset.rem_euclid(bar_period * 4.0);
        // grid.offset is a downbeat; stepping back whole phrases keeps bar/phrase phase.
        let mut bar = ((grid.offset - chunk) / bar_period).round() as i64;
        while chunk < n as f64 {
            let x = rect.left() + (chunk / n as f64 * w as f64) as f32;
            let is_phrase = bar % 4 == 0;
            let (color, height) = if is_phrase {
                (accent.linear_multiply(0.70), h * 0.75)
            } else {
                (palette::alpha(palette::text_bright(), 70), h * 0.35)
            };
            painter.line_segment(
                [egui::pos2(x, cy - height / 2.0), egui::pos2(x, cy + height / 2.0)],
                Stroke::new(if is_phrase { 1.0_f32 } else { 0.75_f32 }, color),
            );
            chunk += bar_period;
            bar += 1;
        }
    }

    // ── Loop, main cue and hot cue markers ───────────────────────────────────
    {
        let x_of = |frame: f64| rect.left() + (frame / WAVEFORM_CHUNK_FRAMES as f64 / n as f64 * w as f64) as f32;
        if let Some(r) = marks.loop_region {
            let (x0, x1) = (x_of(r.start), x_of(r.end).max(x_of(r.start) + 2.0));
            let (fill, edge) = if r.active {
                (palette::alpha(palette::loop_on(), 70), palette::loop_on())
            } else {
                (Color32::from_rgba_unmultiplied(170, 170, 170, 30), Color32::from_rgb(120, 120, 120))
            };
            painter.rect_filled(
                Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
                0.0,
                fill,
            );
            painter
                .line_segment([egui::pos2(x0, rect.top()), egui::pos2(x0, rect.bottom())], Stroke::new(1.0_f32, edge));
        }
        let xc = x_of(marks.cue_point);
        painter.line_segment(
            [egui::pos2(xc, rect.bottom() - 6.0), egui::pos2(xc, rect.bottom())],
            Stroke::new(2.0_f32, palette::cue()),
        );
        for c in marks.cues {
            if c.hot_cue.is_none() {
                continue;
            }
            let x = x_of(c.pos_frames);
            painter.add(egui::Shape::convex_polygon(
                vec![egui::pos2(x - 3.0, rect.top()), egui::pos2(x + 3.0, rect.top()), egui::pos2(x, rect.top() + 6.0)],
                cue_colour(c),
                Stroke::NONE,
            ));
        }
    }

    // ── Hot cue / memory cue triangle markers ────────────────────────────────
    for cue in &track.cues {
        let t = cue.time_ms as f64 / (track.duration_secs * 1000.0);
        let x = rect.left() + (t.clamp(0.0, 1.0) as f32 * w);
        let col = Color32::from_rgb(cue.color[0], cue.color[1], cue.color[2]);
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x, rect.top()),
                egui::pos2(x - 4.0, rect.top() + 7.0),
                egui::pos2(x + 4.0, rect.top() + 7.0),
            ],
            col,
            Stroke::NONE,
        ));
    }

    // ── Playhead ─────────────────────────────────────────────────────────────
    let px = rect.left() + (progress as f32 * w);
    painter.line_segment(
        [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
        Stroke::new(2.0_f32, palette::text_bright()),
    );
}

pub fn format_time(secs: f64) -> String {
    let s = secs as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// MM:SS.t format (tenths of a second) for live countdown display.
pub fn format_time_tenths(secs: f64) -> String {
    let s = secs.abs();
    let m = s as u64 / 60;
    let sec = s as u64 % 60;
    let tenth = ((s - s.floor()) * 10.0) as u32 % 10;
    format!("{:02}:{:02}.{}", m, sec, tenth)
}

/// Current beat-in-bar (0–3) and bar number, given a track and frame position.
pub fn beat_info(track: &DecodedTrack, grid: BeatGrid, frame: u64) -> Option<(usize, i64)> {
    if !grid.is_valid() {
        return None;
    }
    let beat = grid.beat_at_frame(track.sample_rate, frame);
    let beat_num = beat.floor() as i64;
    let beat_in_bar = beat_num.rem_euclid(4) as usize;
    let bar_num = (beat_num.div_euclid(4) + 1).max(1);
    Some((beat_in_bar, bar_num))
}

/// Draws the waveform with a plain software rasteriser into an image file, so what it looks like can be checked
/// without a window: `DUBPLATE_WAVE_FILE=song.ogg DUBPLATE_WAVE_OUT=/tmp/wave cargo test --release -p dubplate wave_snapshot -- --ignored`
/// writes `/tmp/wave-<palette>.ppm`. Only flat and sideways-fading rectangles are drawn; text is left out.
#[cfg(test)]
mod snapshot {
    use super::*;

    fn paint(buf: &mut [f32], w: usize, h: usize, x0: f32, x1: f32, y0: f32, y1: f32, c0: Color32, c1: Color32) {
        for y in (y0.round().max(0.0) as usize)..(y1.round().min(h as f32) as usize) {
            for x in (x0.round().max(0.0) as usize)..(x1.round().min(w as f32) as usize) {
                let t = if x1 > x0 { ((x as f32 + 0.5 - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { 0.0 };
                let mix = |a: u8, b: u8| a as f32 + (b as f32 - a as f32) * t;
                let a = mix(c0.a(), c1.a()) / 255.0;
                let px = &mut buf[(y * w + x) * 3..(y * w + x) * 3 + 3];
                px[0] += (mix(c0.r(), c1.r()) - px[0]) * a;
                px[1] += (mix(c0.g(), c1.g()) - px[1]) * a;
                px[2] += (mix(c0.b(), c1.b()) - px[2]) * a;
            }
        }
    }

    /// Draws what egui painted with the software rasteriser: a P6 image.
    fn to_ppm(ctx: &egui::Context, shapes: Vec<egui::epaint::ClippedShape>, w: usize, h: usize) -> Vec<u8> {
        let mut buf = vec![14.0f32; w * h * 3];
        for clipped in ctx.tessellate(shapes, 1.0) {
            let egui::epaint::Primitive::Mesh(m) = clipped.primitive else { continue };
            for q in m.indices.chunks_exact(6) {
                let v: Vec<_> = q.iter().map(|&i| m.vertices[i as usize]).collect();
                if v.iter().any(|v| v.uv != egui::epaint::WHITE_UV) {
                    continue;
                }
                let xs = v.iter().map(|v| v.pos.x);
                let ys = v.iter().map(|v| v.pos.y);
                let (x0, x1) = (xs.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max));
                let (y0, y1) = (ys.clone().fold(f32::MAX, f32::min), ys.fold(f32::MIN, f32::max));
                let left = v.iter().find(|v| v.pos.x == x0).unwrap().color;
                let right = v.iter().find(|v| v.pos.x == x1).unwrap().color;
                let (cl, cr) = (
                    Color32::from_rgba_premultiplied(left.r(), left.g(), left.b(), left.a()),
                    Color32::from_rgba_premultiplied(right.r(), right.g(), right.b(), right.a()),
                );
                let c = clipped.clip_rect;
                paint(&mut buf, w, h, x0.max(c.left()), x1.min(c.right()), y0.max(c.top()), y1.min(c.bottom()), cl, cr);
            }
        }
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        ppm.extend(buf.iter().map(|v| v.round().clamp(0.0, 255.0) as u8));
        ppm
    }

    /// Stems made by splitting the track into bands (not real stems: a stand-in for looking at the lanes without the
    /// separation model): drums are the highs, bass the lows, vocals the upper mids, other what is left.
    fn fake_stems(t: &DecodedTrack) -> dubplate_engine::TrackStems {
        let sr = t.sample_rate as f32;
        let lp = |hz: f32| (-std::f32::consts::TAU * hz / sr).exp();
        let split = |lo: f32, hi: f32| -> Vec<f32> {
            let (al, ah) = (lp(lo), lp(hi));
            let (mut zl, mut zh) = ([0.0f32; 2], [0.0f32; 2]);
            t.samples
                .chunks_exact(2)
                .flat_map(|f| {
                    let mut out = [0.0f32; 2];
                    for c in 0..2 {
                        zl[c] = al * zl[c] + (1.0 - al) * f[c];
                        zh[c] = ah * zh[c] + (1.0 - ah) * f[c];
                        out[c] = zh[c] - zl[c];
                    }
                    out
                })
                .collect()
        };
        let all_pass = |x: &[f32], y: &[f32]| -> Vec<f32> { x.iter().zip(y).map(|(a, b)| a - b).collect() };
        let bass = split(0.0001, 200.0);
        let vocals = split(300.0, 3400.0);
        let drums: Vec<f32> = all_pass(&t.samples, &split(0.0001, 3000.0));
        let other = all_pass(&all_pass(&t.samples, &bass), &vocals).iter().zip(&drums).map(|(a, d)| a - d).collect();
        dubplate_engine::TrackStems { drums, bass, other, vocals, sample_rate: t.sample_rate }
    }

    /// The deck's waveform with stem lanes under it and the lane overview, from stand-in stems:
    /// `DUBPLATE_WAVE_FILE=song.ogg DUBPLATE_WAVE_OUT=/tmp/lanes cargo test --release -p dubplate lane_snapshot -- --ignored`.
    #[test]
    #[ignore]
    fn lane_snapshot() {
        let (Ok(file), Ok(out)) = (std::env::var("DUBPLATE_WAVE_FILE"), std::env::var("DUBPLATE_WAVE_OUT")) else {
            return;
        };
        let track = dubplate_engine::decode_file(std::path::Path::new(&file)).expect("decode");
        let head_secs: f64 = std::env::var("DUBPLATE_WAVE_AT").ok().and_then(|s| s.parse().ok()).unwrap_or(60.0);
        let progress = head_secs / track.duration_secs;
        let grid = BeatGrid { bpm: track.bpm, offset: track.beat_offset };
        let clock = std::time::Instant::now();
        // real stems when the file has been split (the cache is keyed on the path), else the stand-ins
        let waves = match dubplate_engine::stems::load_stems_cache(std::path::Path::new(&file)) {
            Some(real) => {
                eprintln!("using the cached stems");
                dubplate_engine::StemWaves::build(&real)
            }
            None => dubplate_engine::StemWaves::build(&fake_stems(&track)),
        };
        eprintln!(
            "summaries of four stems: {:.0} ms (with the stand-in stems)",
            clock.elapsed().as_secs_f64() * 1000.0
        );
        let (w, h) = (1500usize, 330usize);
        let ctx = egui::Context::default();
        let marks = Marks { cue_point: 0.0, cues: &[], loop_region: None, loop_in: None };
        let gains = [1.0, 1.0, 0.0, 1.0]; // "other" muted, to see a dimmed lane
        let full = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w as f32, h as f32))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    let main = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w as f32, 130.0));
                    draw_waveform(
                        &ui.painter_at(main),
                        main,
                        &track,
                        grid,
                        &marks,
                        progress,
                        palette::deck_a(),
                        [1.0; 3],
                        0.5,
                        1.0,
                    );
                    let lanes = Rect::from_min_size(egui::pos2(0.0, 134.0), egui::vec2(w as f32, 68.0));
                    let head = progress * track.duration_frames as f64;
                    let p = ui.painter_at(lanes);
                    p.rect_filled(lanes, 0.0, palette::background());
                    for i in 0..4 {
                        let row = Rect::from_min_size(
                            egui::pos2(0.0, lanes.top() + 17.0 * i as f32),
                            egui::vec2(w as f32, 17.0),
                        );
                        wave_draw::lane(&p, row, &waves.0[i], head, 1.0, palette::stems()[i], gains[i]);
                    }
                    p.line_segment(
                        [egui::pos2(w as f32 / 2.0, lanes.top()), egui::pos2(w as f32 / 2.0, lanes.bottom())],
                        Stroke::new(1.5_f32, Color32::WHITE),
                    );
                    let ov = Rect::from_min_size(egui::pos2(0.0, 210.0), egui::vec2(w as f32, 40.0));
                    draw_overview(
                        &ui.painter_at(ov),
                        ov,
                        &track,
                        grid,
                        &marks,
                        progress,
                        palette::deck_a(),
                        Some((&waves, gains)),
                    );
                    let ov2 = Rect::from_min_size(egui::pos2(0.0, 260.0), egui::vec2(w as f32, 26.0));
                    draw_overview(&ui.painter_at(ov2), ov2, &track, grid, &marks, progress, palette::deck_a(), None);
                });
            },
        );
        std::fs::write(format!("{out}.ppm"), to_ppm(&ctx, full.shapes, w, h)).unwrap();
    }

    #[test]
    #[ignore]
    fn wave_snapshot() {
        let (Ok(file), Ok(out)) = (std::env::var("DUBPLATE_WAVE_FILE"), std::env::var("DUBPLATE_WAVE_OUT")) else {
            return;
        };
        let track = dubplate_engine::decode_file(std::path::Path::new(&file)).expect("decode");
        let head_secs: f64 = std::env::var("DUBPLATE_WAVE_AT").ok().and_then(|s| s.parse().ok()).unwrap_or(60.0);
        let progress = head_secs / track.duration_secs;
        let grid = BeatGrid { bpm: track.bpm, offset: track.beat_offset };
        let (w, h) = (1500usize, 380usize);
        for p in wave_draw::Palette::ALL {
            wave_draw::set_palette(p);
            let ctx = egui::Context::default();
            let marks = Marks { cue_point: 0.0, cues: &[], loop_region: None, loop_in: None };
            let full = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w as f32, h as f32))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                        let r = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w as f32, 220.0));
                        draw_waveform(
                            &ui.painter_at(r),
                            r,
                            &track,
                            grid,
                            &marks,
                            progress,
                            palette::deck_a(),
                            [1.0; 3],
                            0.5,
                            1.0,
                        );
                        let o = Rect::from_min_size(egui::pos2(0.0, 230.0), egui::vec2(w as f32, 44.0));
                        draw_overview(&ui.painter_at(o), o, &track, grid, &marks, progress, palette::deck_a(), None);
                        let z = Rect::from_min_size(egui::pos2(0.0, 282.0), egui::vec2(w as f32, 44.0));
                        draw_waveform(
                            &ui.painter_at(z),
                            z,
                            &track,
                            grid,
                            &marks,
                            progress,
                            palette::deck_a(),
                            [0.0, 1.0, 1.0],
                            0.5,
                            0.25,
                        );
                        // the small waveform of a library row
                        let m = Rect::from_min_size(egui::pos2(0.0, 334.0), egui::vec2(320.0, 34.0));
                        ui.painter().rect_filled(m, 2.0, Color32::from_rgb(8, 8, 13));
                        wave_draw::mini(&ui.painter_at(m), m, &track.wave.preview(160));
                    });
                },
            );
            let ppm = to_ppm(&ctx, full.shapes, w, h);
            std::fs::write(format!("{out}-{}.ppm", p.label()), ppm).unwrap();
        }
        wave_draw::set_palette(wave_draw::Palette::default());
    }
}
