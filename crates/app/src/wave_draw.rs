//! Draws the waveform from a track's [`WaveSummary`].
//!
//! Each frame is one mesh, a couple of rectangles per pixel column, however wide the view is. Columns are tied to
//! positions in the track, not to the screen: a pixel column always covers the same stretch of audio at a given zoom,
//! so as the track scrolls the shapes slide along without changing, which is what keeps it from shimmering. All the
//! reading is from the pre-made summary (about one column per pixel), never from the audio.
//!
//! A column's height is the real envelope of the signal; its solid core is the RMS; its colour is how the loudness
//! splits between bass, mids and highs.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use dubplate_engine::{WaveLevelValues, WaveSummary};
use egui::{epaint::Vertex, Color32, Mesh, Painter, Rect, Shape};
use serde::{Deserialize, Serialize};

/// How the three bands are coloured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Palette {
    /// Red bass, green mids, blue highs, added together: the common three-colour look.
    #[default]
    Rgb,
    /// Blue bass, amber mids, white highs; the dominant band takes the column (the CDJ-3000's 3-band look).
    Club,
    /// One blue that gets brighter with the highs: the older single-colour look.
    Blue,
    /// The three bands as three shapes on top of each other, each as tall as its band is loud: blue bass at the
    /// back, amber mids, white highs in front. What is in a passage is read from the shapes, not from a mixed colour.
    Stacked,
    /// Only the bass band (the kick and the bass line) in colour, the rest of the sound a faint outline: for lining
    /// beats up by eye.
    Beats,
}

impl Palette {
    pub const ALL: [Palette; 5] = [Palette::Rgb, Palette::Club, Palette::Blue, Palette::Stacked, Palette::Beats];

    pub fn label(self) -> &'static str {
        match self {
            Palette::Rgb => "RGB",
            Palette::Club => "3-Band",
            Palette::Blue => "Blue",
            Palette::Stacked => "Stacked",
            Palette::Beats => "Beats",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Palette::Rgb => "Red bass, green mids, blue highs",
            Palette::Club => "Blue bass, amber mids, white highs",
            Palette::Blue => "One colour, brighter with the highs",
            Palette::Stacked => "Bass, mids and highs as three shapes, one on another",
            Palette::Beats => "Only the kick and bass, the rest a faint outline",
        }
    }

    fn from_u8(v: u8) -> Palette {
        Palette::ALL.get(v as usize).copied().unwrap_or_default()
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The palette every waveform is drawn in.
pub fn palette() -> Palette {
    Palette::from_u8(CURRENT.load(Ordering::Relaxed))
}
pub fn set_palette(p: Palette) {
    CURRENT.store(p as u8, Ordering::Relaxed)
}

/// What the user chose, remembered between runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub palette: Palette,
    /// Whether a deck shows its stem lanes as soon as it has stems.
    #[serde(default)]
    pub lanes: bool,
}

fn settings_path() -> std::path::PathBuf {
    dubplate_engine::paths::config_dir().join("waveform.toml")
}

static LANES: AtomicBool = AtomicBool::new(false);

/// Whether decks show their stem lanes by default (each deck can still be switched on its own).
pub fn lanes_default() -> bool {
    LANES.load(Ordering::Relaxed)
}
pub fn set_lanes_default(on: bool) {
    LANES.store(on, Ordering::Relaxed)
}

pub fn load_settings() {
    let saved: Saved =
        std::fs::read_to_string(settings_path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
    set_palette(saved.palette);
    set_lanes_default(saved.lanes);
}

pub fn save_settings() {
    let _ = std::fs::create_dir_all(dubplate_engine::paths::config_dir());
    if let Ok(s) = toml::to_string_pretty(&Saved { palette: palette(), lanes: lanes_default() }) {
        let _ = std::fs::write(settings_path(), s);
    }
}

/// What the music's own tilt does to the bands: the mids and highs of a mix are much quieter than its bass, so
/// without this every column would come out red. The ratios are about what rekordbox's own 3-band data shows next to
/// our measurements of the same tracks.
const BAND_GAIN: [f32; 3] = [1.0, 2.4, 3.2];

/// Looks up the colour of a column. `bands` are its three band levels after any EQ dimming.
fn colour_of(bands: [f32; 3], brightness: f32) -> Color32 {
    let e = [bands[0] * BAND_GAIN[0], bands[1] * BAND_GAIN[1], bands[2] * BAND_GAIN[2]];
    let top = e[0].max(e[1]).max(e[2]).max(1e-6);
    let n = [e[0] / top, e[1] / top, e[2] / top];
    let (r, g, b) = match palette() {
        Palette::Rgb | Palette::Beats => {
            const C: [[f32; 3]; 3] = [[255.0, 56.0, 36.0], [120.0, 236.0, 70.0], [50.0, 140.0, 255.0]];
            let w = [n[0].powf(1.6), n[1].powf(1.6), n[2].powf(1.6)];
            let sum = (w[0] + w[1] + w[2]).max(1e-6);
            let mut c = [0.0f32; 3];
            for k in 0..3 {
                for j in 0..3 {
                    c[j] += C[k][j] * w[k] / sum;
                }
            }
            (c[0], c[1], c[2])
        }
        Palette::Club | Palette::Stacked => {
            const C: [[f32; 3]; 3] = [[26.0, 86.0, 255.0], [255.0, 172.0, 64.0], [244.0, 247.0, 255.0]];
            let w = [n[0] * n[0], n[1] * n[1], n[2] * n[2]];
            let sum = (w[0] + w[1] + w[2]).max(1e-6);
            let mut c = [0.0f32; 3];
            for k in 0..3 {
                for j in 0..3 {
                    c[j] += C[k][j] * w[k] / sum;
                }
            }
            (c[0], c[1], c[2])
        }
        Palette::Blue => {
            let t = (e[2] + e[1] * 0.4) / (e[0] + e[1] + e[2]).max(1e-6);
            let t = t.clamp(0.0, 1.0).sqrt();
            (24.0 + 176.0 * t, 60.0 + 170.0 * t, 190.0 + 65.0 * t)
        }
    };
    let k = brightness;
    Color32::from_rgb((r * k).min(255.0) as u8, (g * k).min(255.0) as u8, (b * k).min(255.0) as u8)
}

/// What the deck's EQ and filter leave of each band (1.0 = untouched, 0.0 = gone).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dims(pub [f32; 3]);

impl Dims {
    pub const NONE: Dims = Dims([1.0; 3]);

    /// The EQ gains and the filter knob (0 low-pass, 0.5 off, 1 high-pass) as one dimming per band.
    pub fn of(eq: [f32; 3], filter: f32) -> Dims {
        const DEAD: f32 = 0.025;
        let (f_lo, f_mid, f_hi) = if filter < 0.5 - DEAD {
            let depth = 1.0 - filter / (0.5 - DEAD);
            (1.0, (1.0 - depth * 0.4).max(0.0), (1.0 - depth).max(0.0))
        } else if filter > 0.5 + DEAD {
            let depth = (filter - 0.5 - DEAD) / (0.5 - DEAD);
            ((1.0 - depth).max(0.0), (1.0 - depth * 0.4).max(0.0), 1.0)
        } else {
            (1.0, 1.0, 1.0)
        };
        let e = |v: f32| v.clamp(0.0, 1.0);
        Dims([e(eq[0]) * f_lo, e(eq[1]) * f_mid, e(eq[2]) * f_hi])
    }

    fn is_none(&self) -> bool {
        self.0 == Self::NONE.0
    }

    /// The bands of `l` after dimming, and what is left of its height (the dimmed energy over the whole).
    fn apply(&self, l: &WaveLevelValues) -> ([f32; 3], f32) {
        if self.is_none() {
            return (l.bands, 1.0);
        }
        let b = [l.bands[0] * self.0[0], l.bands[1] * self.0[1], l.bands[2] * self.0[2]];
        let before: f32 = l.bands.iter().map(|v| v * v).sum();
        let after: f32 = b.iter().map(|v| v * v).sum();
        (b, if before > 1e-9 { (after / before).sqrt() } else { 1.0 })
    }
}

/// How tall a level of 1.0 is drawn, relative to the half height: a slight lift, so quiet passages stay readable.
fn lift(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(0.85)
}

/// A column's shape in pixels above and below the centre, for the whole envelope and for the solid core.
struct Shapes {
    top: f32,
    bottom: f32,
    core_top: f32,
    core_bottom: f32,
    bands: [f32; 3],
}

impl Shapes {
    /// The three slots' shapes blended 1:2:1, so a single loud column does not stand alone as a spike. A slot's
    /// neighbours are fixed in the track, so this never changes with the playhead.
    fn blend(prev: &Shapes, cur: &Shapes, next: &Shapes) -> Shapes {
        let m = |a: f32, b: f32, c: f32| a * 0.25 + b * 0.5 + c * 0.25;
        Shapes {
            top: m(prev.top, cur.top, next.top),
            bottom: m(prev.bottom, cur.bottom, next.bottom),
            core_top: m(prev.core_top, cur.core_top, next.core_top),
            core_bottom: m(prev.core_bottom, cur.core_bottom, next.core_bottom),
            bands: cur.bands,
        }
    }
}

/// A column of the overview: how much energy there is, not how high the peaks reach, so quiet and loud parts of the
/// track show up as different heights.
fn energy_shape(l: &WaveLevelValues, gain: f32, half: f32) -> Shapes {
    let h = lift(l.rms * 3.4 * gain) * half;
    Shapes { top: h, bottom: h, core_top: h * 0.6, core_bottom: h * 0.6, bands: l.bands }
}

fn shapes(l: &WaveLevelValues, dims: &Dims, gain: f32, half: f32) -> Shapes {
    let (bands, kept) = dims.apply(l);
    let g = gain * kept;
    let (top, bottom) = (lift(l.max * g) * half, lift(-l.min * g) * half);
    let peak = l.peak().max(1e-6);
    // The RMS of a loud signal is about 0.3 of its peak; the core is drawn at twice the RMS so it reads as the body.
    let k = (l.rms * 2.2 / peak).clamp(0.0, 1.0);
    Shapes { top, bottom, core_top: top * k, core_bottom: bottom * k, bands }
}

/// Scales the picture so a quiet track is not drawn as a thin line: up to 2.5x, never down.
pub fn display_gain(s: &WaveSummary) -> f32 {
    (0.95 / s.ref_peak.max(0.05)).clamp(1.0, 2.5)
}

fn push_rect(mesh: &mut Mesh, x0: f32, x1: f32, y0: f32, y1: f32, c: Color32) {
    if y1 - y0 < 0.4 {
        return;
    }
    let i = mesh.vertices.len() as u32;
    for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
        mesh.vertices.push(Vertex { pos: egui::pos2(x, y), uv: egui::epaint::WHITE_UV, color: c });
    }
    mesh.indices.extend_from_slice(&[i, i + 1, i + 2, i, i + 2, i + 3]);
}

/// The colours of the bass, mids and highs when each is drawn as its own shape.
const STACK: [Color32; 3] =
    [Color32::from_rgb(26, 86, 255), Color32::from_rgb(255, 172, 64), Color32::from_rgb(244, 247, 255)];
/// The bass band's colour in the beats-only view.
const BEAT: Color32 = Color32::from_rgb(255, 96, 56);

/// Each band's share of the column's loudest band, after the tilt that makes mids and highs comparable to bass.
fn band_shares(bands: [f32; 3]) -> [f32; 3] {
    let e = [bands[0] * BAND_GAIN[0], bands[1] * BAND_GAIN[1], bands[2] * BAND_GAIN[2]];
    let top = e[0].max(e[1]).max(e[2]).max(1e-6);
    [e[0] / top, e[1] / top, e[2] / top]
}

/// One column: the envelope in a darker tone, the core on top in the full colour. In the stacked view it is three
/// shapes, the tallest at the back; in the beats view the bass band's part of the envelope over a faint outline.
fn push_column(mesh: &mut Mesh, x0: f32, x1: f32, cy: f32, s: &Shapes, brightness: f32) {
    match palette() {
        Palette::Stacked => {
            let n = band_shares(s.bands);
            let mut order = [0usize, 1, 2];
            order.sort_by(|&a, &b| n[b].total_cmp(&n[a]));
            for k in order {
                // a little lift, so a band at a quarter of the loudest is still a visible sliver
                let h = n[k].powf(0.7);
                push_rect(
                    mesh,
                    x0,
                    x1,
                    cy - (s.top * h).max(0.5),
                    cy + (s.bottom * h).max(0.5),
                    scale(STACK[k], brightness),
                );
            }
            return;
        }
        Palette::Beats => {
            let energy = s.bands.iter().map(|b| b * b).sum::<f32>().max(1e-12);
            let low = (s.bands[0] * s.bands[0] / energy).clamp(0.0, 1.0);
            push_rect(
                mesh,
                x0,
                x1,
                cy - s.top.max(1.0),
                cy + s.bottom.max(1.0),
                scale(Color32::from_gray(70), brightness),
            );
            push_rect(mesh, x0, x1, cy - s.top * low, cy + s.bottom * low, scale(BEAT, brightness));
            return;
        }
        _ => {}
    }
    let col = colour_of(s.bands, 1.0);
    let body = scale(col, 0.52 * brightness);
    let core = scale(col, brightness);
    push_rect(mesh, x0, x1, cy - s.top.max(1.0), cy + s.bottom.max(1.0), body);
    push_rect(mesh, x0, x1, cy - s.core_top.max(0.5), cy + s.core_bottom.max(0.5), core);
}

fn scale(c: Color32, k: f32) -> Color32 {
    Color32::from_rgb((c.r() as f32 * k) as u8, (c.g() as f32 * k) as u8, (c.b() as f32 * k) as u8)
}

/// The frames per pixel at a zoom of `zoom` pixels per 256 frames (the unit the beat grid is measured in).
pub fn frames_per_pixel(zoom: f32) -> f64 {
    256.0 / zoom.max(0.01) as f64
}

/// The scrolling waveform: `head` is the frame under the centre line. Played audio is drawn dimmer.
pub fn scroll(painter: &Painter, rect: Rect, wave: &WaveSummary, head: f64, zoom: f32, dims: Dims) {
    let gain = display_gain(wave);
    let half_h = rect.height() * 0.47;
    scroll_columns(painter, rect, wave, head, zoom, 1.0, |l| shapes(l, &dims, gain, half_h), push_column);
}

/// The scrolling view's one mesh (a column `px` points wide), for any way of turning a summary column into a shape (`shape`) and a shape into
/// rectangles (`push`): the main waveform and the stem lanes share it, so they slide along identically.
fn scroll_columns(
    painter: &Painter,
    rect: Rect,
    wave: &WaveSummary,
    head: f64,
    zoom: f32,
    px: f32,
    shape: impl Fn(&WaveLevelValues) -> Shapes,
    push: impl Fn(&mut Mesh, f32, f32, f32, &Shapes, f32),
) {
    let fpp = frames_per_pixel(zoom) * px as f64;
    let Some(level) = wave.level_for(fpp) else { return };
    let (half_w, cy) = (rect.width() / 2.0, rect.center().y);

    // Pixel columns are slots of `fpp` frames fixed in the track: slot k covers k*fpp .. (k+1)*fpp.
    let centre_slot = head / fpp;
    let reach = (half_w / px) as f64;
    let first = (centre_slot - reach).floor() as i64 - 1;
    let last = (centre_slot + reach).ceil() as i64 + 1;
    let total = wave.total_frames as f64;
    let slots: Vec<Option<Shapes>> = (first - 1..=last + 1)
        .map(|k| {
            let (from, to) = (k as f64 * fpp, (k + 1) as f64 * fpp);
            (to > 0.0 && from < total).then(|| shape(&level.span(from.max(0.0), to)))
        })
        .collect();
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(slots.len() * 8);
    mesh.reserve_triangles(slots.len() * 4);
    for (i, k) in (first..=last).enumerate() {
        let Some(cur) = &slots[i + 1] else { continue };
        let blended = match (&slots[i], &slots[i + 2]) {
            (Some(prev), Some(next)) => Shapes::blend(prev, cur, next),
            _ => Shapes::blend(cur, cur, cur),
        };
        // One function for both edges, so neighbours share their exact edge and no hairline shows between them.
        let edge = |k: i64| rect.left() + half_w + (k as f64 - centre_slot) as f32 * px;
        let played = if k as f64 * fpp + fpp * 0.5 < head { 0.4 } else { 1.0 };
        push(&mut mesh, edge(k), edge(k + 1), cy, &blended, played);
    }
    painter.add(Shape::mesh(mesh));
}

/// One stem's lane in the scrolling view: its own envelope in `colour`, scaled so a quiet stem still shows.
/// `level` is how much of the stem is let through (1.0 on, 0.0 muted): a muted lane keeps its shape but is drawn dim,
/// so you can still see where that stem is.
pub fn lane(painter: &Painter, rect: Rect, wave: &WaveSummary, head: f64, zoom: f32, colour: Color32, level: f32) {
    let gain = lane_gain(wave);
    let half_h = rect.height() * 0.46;
    let live = 0.3 + 0.7 * level.clamp(0.0, 1.0);
    scroll_columns(
        painter,
        rect,
        wave,
        head,
        zoom,
        LANE_PX,
        |l| lane_shape(l, gain, half_h),
        |mesh, x0, x1, cy, s, played| push_lane_column(mesh, x0, x1, cy, s, colour, played * live),
    );
}

/// A lane's columns are this many points wide: half the horizontal detail of the main waveform, for half the cost.
const LANE_PX: f32 = 2.0;

/// A stem is quieter than the mix it came from, so it is lifted more than the main waveform is: up to 6 times.
fn lane_gain(s: &WaveSummary) -> f32 {
    (0.95 / s.ref_peak.max(0.05)).clamp(1.0, 6.0)
}

fn lane_shape(l: &WaveLevelValues, gain: f32, half: f32) -> Shapes {
    let (top, bottom) = (lift(l.max * gain) * half, lift(-l.min * gain) * half);
    let k = (l.rms * 2.2 / l.peak().max(1e-6)).clamp(0.0, 1.0);
    Shapes { top, bottom, core_top: top * k, core_bottom: bottom * k, bands: l.bands }
}

fn push_lane_column(mesh: &mut Mesh, x0: f32, x1: f32, cy: f32, s: &Shapes, colour: Color32, brightness: f32) {
    // a silent stretch is left empty, not drawn as a line, so a stem that is not there is plainly not there
    if s.top + s.bottom < 0.6 {
        return;
    }
    push_rect(mesh, x0, x1, cy - s.top, cy + s.bottom, scale(colour, 0.52 * brightness));
    push_rect(mesh, x0, x1, cy - s.core_top.max(0.4), cy + s.core_bottom.max(0.4), scale(colour, brightness));
}

/// The stems of the whole track as four rows in one strip, drums at the top: the part played is dimmer, a muted stem
/// is dim. `levels` are the stems' gains.
pub fn overview_lanes(
    painter: &Painter,
    rect: Rect,
    waves: &[WaveSummary; 4],
    colours: &[Color32; 4],
    levels: [f32; 4],
    progress: f64,
) {
    let row_h = rect.height() / 4.0;
    let cols = rect.width().floor().max(1.0) as usize;
    for (row, wave) in waves.iter().enumerate() {
        if wave.is_empty() {
            continue;
        }
        let fpp = wave.total_frames as f64 / cols as f64;
        let Some(level) = wave.level_for(fpp) else { continue };
        let cy = rect.top() + row_h * (row as f32 + 0.5);
        let gain = lane_gain(wave);
        let live = 0.3 + 0.7 * levels[row].clamp(0.0, 1.0);
        let mut mesh = Mesh::default();
        for i in 0..cols {
            let l = level.span(i as f64 * fpp, (i + 1) as f64 * fpp);
            let h = lift(l.rms * 3.4 * gain) * row_h * 0.48;
            let s = Shapes { top: h, bottom: h, core_top: h * 0.6, core_bottom: h * 0.6, bands: l.bands };
            let played = if ((i as f64 + 0.5) / cols as f64) < progress { 0.55 } else { 0.9 };
            push_lane_column(
                &mut mesh,
                rect.left() + i as f32,
                rect.left() + i as f32 + 1.0,
                cy,
                &s,
                colours[row],
                played * live,
            );
        }
        painter.add(Shape::mesh(mesh));
    }
}

/// The whole track in one strip: the part played is dimmer. `progress` is 0..1.
pub fn overview(painter: &Painter, rect: Rect, wave: &WaveSummary, progress: f64) {
    if wave.is_empty() {
        return;
    }
    let cols = rect.width().floor().max(1.0) as usize;
    let fpp = wave.total_frames as f64 / cols as f64;
    let Some(level) = wave.level_for(fpp) else { return };
    let (cy, half_h) = (rect.center().y, rect.height() * 0.46);
    let gain = display_gain(wave);
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(cols * 8);
    mesh.reserve_triangles(cols * 4);
    for i in 0..cols {
        let (from, to) = (i as f64 * fpp, (i + 1) as f64 * fpp);
        let s = energy_shape(&level.span(from, to), gain, half_h);
        let played = if (i as f64 + 0.5) / (cols as f64) < progress { 0.55 } else { 0.9 };
        let edge = |i: usize| rect.left() + i as f32;
        push_column(&mut mesh, edge(i), edge(i + 1), cy, &s, played);
    }
    painter.add(Shape::mesh(mesh));
}

/// A small waveform for a list row: `cols` are the bass, mid and high loudness over the track (see
/// `WaveSummary::preview`). Each preview is scaled to fill its box, since it is for telling tracks apart by shape.
pub fn mini(painter: &Painter, rect: Rect, cols: &[[u8; 3]]) {
    if cols.is_empty() {
        return;
    }
    let level = |b: u8| (b as f32 / 255.0).powi(2);
    let decoded: Vec<[f32; 3]> = cols.iter().map(|c| [level(c[0]), level(c[1]), level(c[2])]).collect();
    let energy = |b: &[f32; 3]| (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt();
    let mut sorted: Vec<f32> = decoded.iter().map(energy).collect();
    sorted.sort_by(f32::total_cmp);
    let loud = sorted[((sorted.len() as f32 * 0.98) as usize).min(sorted.len() - 1)].max(0.02);
    let gain = 0.9 / loud;
    let (cy, half) = (rect.center().y, rect.height() / 2.0);
    let w = rect.width() / cols.len() as f32;
    let edge = |i: usize| rect.left() + i as f32 * w;
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(cols.len() * 8);
    mesh.reserve_triangles(cols.len() * 4);
    for (i, b) in decoded.iter().enumerate() {
        let h = (lift(energy(b) * gain) * half).max(0.6);
        let s = Shapes { top: h, bottom: h, core_top: h * 0.6, core_bottom: h * 0.6, bands: *b };
        push_column(&mut mesh, edge(i), edge(i + 1), cy, &s, 0.9);
    }
    painter.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    #[test]
    fn the_stacked_and_beats_views_draw_what_they_say() {
        let column = |p: Palette, bands: [f32; 3]| {
            set_palette(p);
            let mut mesh = Mesh::default();
            let s = Shapes { top: 40.0, bottom: 40.0, core_top: 20.0, core_bottom: 20.0, bands };
            push_column(&mut mesh, 0.0, 1.0, 50.0, &s, 1.0);
            set_palette(Palette::Rgb);
            // (top of each rectangle, its colour), in drawing order
            mesh.vertices.chunks(4).map(|v| (v[0].pos.y, v[0].color)).collect::<Vec<_>>()
        };
        // bass-heavy: three shapes, the bass full height at the back and the others shorter in front of it
        let stacked = column(Palette::Stacked, [0.5, 0.05, 0.02]);
        assert_eq!(stacked.len(), 3);
        assert_eq!(stacked[0], (10.0, STACK[0]));
        assert!(stacked[1].0 > stacked[0].0 && stacked[2].0 > stacked[1].0, "{stacked:?}");
        // beats: a kick fills the outline, a hat leaves only the outline
        let kick = column(Palette::Beats, [0.5, 0.02, 0.01]);
        assert_eq!(kick.last().unwrap().1, BEAT);
        assert!((kick.last().unwrap().0 - 10.0).abs() < 1.0, "{kick:?}");
        let hat = column(Palette::Beats, [0.005, 0.02, 0.3]);
        assert_eq!(hat.len(), 1, "no bass, so only the outline: {hat:?}");
    }

    const SR: u32 = 44_100;

    fn song(secs: f32) -> WaveSummary {
        let n = (SR as f32 * secs) as usize;
        let s: Vec<f32> = (0..n)
            .flat_map(|i| {
                let t = i as f32 / SR as f32;
                let kick = (TAU * 55.0 * t).sin() * (-((t * 2.0).fract()) * 9.0).exp() * 0.8;
                let hat = ((i * 7919 % 1000) as f32 / 500.0 - 1.0) * 0.1 * (t * 4.0).fract();
                [kick + hat, kick + hat]
            })
            .collect();
        WaveSummary::build(&s, 2, SR)
    }

    /// Runs `f` inside an egui frame, as the app draws.
    fn draw(mut f: impl FnMut(&Painter, Rect)) {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 120.0));
                f(&ui.painter_at(rect), rect);
            });
        });
    }

    #[test]
    fn a_small_waveform_draws_whatever_it_is_given() {
        draw(|p, r| {
            mini(p, r, &[]);
            mini(p, r, &[[0; 3]; 160]);
            mini(p, r, &[[255; 3]; 3]);
            mini(p, r, &song(4.0).preview(160));
        });
    }

    #[test]
    fn dims_follow_the_eq_and_filter() {
        assert_eq!(Dims::of([1.0; 3], 0.5), Dims::NONE);
        assert_eq!(Dims::of([0.0, 1.0, 1.0], 0.5).0[0], 0.0);
        assert_eq!(Dims::of([1.0; 3], 0.0).0[2], 0.0, "a deep low-pass takes the highs");
        assert_eq!(Dims::of([1.0; 3], 1.0).0[0], 0.0, "a deep high-pass takes the bass");
        assert_eq!(Dims::of([2.0, 1.0, 1.0], 0.5).0[0], 1.0, "a boost is not drawn taller");
    }

    #[test]
    fn a_dimmed_band_loses_its_share_of_the_height() {
        let l = WaveLevelValues { min: -0.8, max: 0.8, rms: 0.3, bands: [0.5, 0.01, 0.01] };
        let (_, kept) = Dims::of([0.0, 1.0, 1.0], 0.5).apply(&l);
        assert!(kept < 0.1, "{kept}");
        let (_, whole) = Dims::NONE.apply(&l);
        assert_eq!(whole, 1.0);
    }

    #[test]
    fn colours_follow_the_dominant_band() {
        for (p, bass, hat) in
            [(Palette::Rgb, (255, 62, 44), (58, 146, 255)), (Palette::Club, (26, 86, 255), (244, 247, 255))]
        {
            set_palette(p);
            let b = colour_of([0.5, 0.01, 0.001], 1.0);
            let h = colour_of([0.001, 0.01, 0.2], 1.0);
            let near = |c: Color32, t: (i32, i32, i32)| {
                (c.r() as i32 - t.0).abs() + (c.g() as i32 - t.1).abs() + (c.b() as i32 - t.2).abs() < 120
            };
            assert!(near(b, bass), "{p:?} bass {b:?}");
            assert!(near(h, hat), "{p:?} highs {h:?}");
        }
        set_palette(Palette::default());
    }

    #[test]
    fn silence_and_odd_values_draw_without_trouble() {
        let silent = WaveSummary::build(&vec![0.0; 100_000], 2, SR);
        let loud = WaveSummary::build(&vec![1.0; 100_000], 2, SR);
        for w in [&silent, &loud, &WaveSummary::default()] {
            draw(|p, r| {
                scroll(p, r, w, 20_000.0, 1.0, Dims::NONE);
                overview(p, r, w, 0.5);
            });
        }
    }

    #[test]
    fn a_slot_shows_the_same_audio_wherever_the_playhead_is() {
        // The shimmer fix: how a slot is read depends on the slot and the zoom, never on the head position.
        let w = song(6.0);
        let level = w.level_for(frames_per_pixel(1.0)).unwrap();
        let fpp = frames_per_pixel(1.0);
        let a = level.span(40.0 * fpp, 41.0 * fpp);
        let b = level.span(40.0 * fpp, 41.0 * fpp);
        assert_eq!(a, b);
        // and a mesh drawn with the head a pixel apart has the same shapes, one slot over
        let (m1, m2) = (cols_at(&w, 100_000.0), cols_at(&w, 100_000.0 + fpp));
        assert_eq!(&m1[1..], &m2[..m2.len() - 1]);
    }

    /// The (top, bottom) of each slot's body as `scroll` would draw it, from the head's slot outwards.
    fn cols_at(w: &WaveSummary, head: f64) -> Vec<(i32, i32)> {
        let fpp = frames_per_pixel(1.0);
        let level = w.level_for(fpp).unwrap();
        let centre = (head / fpp).round() as i64;
        (centre - 20..centre + 20)
            .map(|k| {
                let s = shapes(&level.span(k as f64 * fpp, (k + 1) as f64 * fpp), &Dims::NONE, display_gain(w), 50.0);
                ((s.top * 10.0) as i32, (s.bottom * 10.0) as i32)
            })
            .collect()
    }

    /// Cost of drawing the waveforms, as the app does on a busy frame:
    /// `cargo test --release -p dubplate wave_draw::tests::frame_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn frame_cost() {
        let w = song(240.0);
        let ctx = egui::Context::default();
        let frames = 120;
        let (mut build, mut tess) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        for f in 0..frames {
            let t0 = std::time::Instant::now();
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1900.0, 400.0))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        for d in 0..2 {
                            let r = Rect::from_min_size(egui::pos2(0.0, d as f32 * 140.0), egui::vec2(1900.0, 130.0));
                            scroll(&ui.painter_at(r), r, &w, 3_000_000.0 + f as f64 * 700.0, 1.0, Dims::NONE);
                            let o =
                                Rect::from_min_size(egui::pos2(0.0, 290.0 + d as f32 * 50.0), egui::vec2(1900.0, 40.0));
                            overview(&ui.painter_at(o), o, &w, 0.3);
                        }
                    });
                },
            );
            build += t0.elapsed();
            let t1 = std::time::Instant::now();
            let _ = ctx.tessellate(out.shapes, 1.0);
            tess += t1.elapsed();
        }
        println!(
            "two waveforms and two overviews per frame: build {:?}, tessellate {:?}",
            build / frames,
            tess / frames
        );

        // the same with a lane per stem under each waveform and the lane overviews
        let stems = dubplate_engine::StemWaves(std::array::from_fn(|_| song(240.0)));
        let (mut build, mut tess) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        for f in 0..frames {
            let t0 = std::time::Instant::now();
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1900.0, 700.0))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        for d in 0..2 {
                            let r = Rect::from_min_size(egui::pos2(0.0, d as f32 * 200.0), egui::vec2(1900.0, 130.0));
                            let head = 3_000_000.0 + f as f64 * 700.0;
                            scroll(&ui.painter_at(r), r, &w, head, 1.0, Dims::NONE);
                            for i in 0..4 {
                                let row = Rect::from_min_size(
                                    egui::pos2(0.0, d as f32 * 200.0 + 132.0 + 17.0 * i as f32),
                                    egui::vec2(1900.0, 17.0),
                                );
                                lane(&ui.painter_at(row), row, &stems.0[i], head, 1.0, Color32::RED, 1.0);
                            }
                            let o =
                                Rect::from_min_size(egui::pos2(0.0, 420.0 + d as f32 * 50.0), egui::vec2(1900.0, 40.0));
                            overview_lanes(&ui.painter_at(o), o, &stems.0, &[Color32::RED; 4], [1.0; 4], 0.3);
                        }
                    });
                },
            );
            build += t0.elapsed();
            let t1 = std::time::Instant::now();
            let _ = ctx.tessellate(out.shapes, 1.0);
            tess += t1.elapsed();
        }
        println!(
            "with four lanes per deck and the lane overviews: build {:?}, tessellate {:?}",
            build / frames,
            tess / frames
        );
    }
}
