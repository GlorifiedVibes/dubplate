//! Prepare: a screen for checking and fixing one track's beat grid, separate from the decks.
//!
//! It has its own player (so it never touches a deck, always plays at normal speed, and is heard on the main
//! output only while no deck plays), a big waveform with the grid on it, a click track to check the grid by ear,
//! tap tempo, and the usual grid tools. The grid maths lives in plain functions at the top so it can be tested.

use std::path::PathBuf;
use std::sync::Arc;

use dubplate_engine::tap::{fit_taps, snap_to_onsets, TapError, TapFit};
use dubplate_engine::{
    BeatGrid, ClickControls, Cue, DeckHandle, DecodedTrack, LoopRegion, LOOP_SIZES, WAVEFORM_CHUNK_FRAMES,
};
use egui::{Color32, Rect, RichText, Sense};

use crate::app::cue_loop::PAD_COLORS;
use crate::app::palette;
use crate::waveform::{beat_info, draw_overview, draw_waveform, format_time_tenths, Marks};

const CHUNK: f64 = WAVEFORM_CHUNK_FRAMES as f64;
/// How far snapping to the audio may move a grid: a bit more than a person's tap delay.
const SNAP_MAX_SECS: f64 = 0.080;

// ── Grid maths ────────────────────────────────────────────────────────────────

pub fn secs_to_chunks(secs: f64, sample_rate: u32) -> f64 {
    secs * sample_rate as f64 / CHUNK
}
pub fn chunks_to_secs(chunks: f64, sample_rate: u32) -> f64 {
    chunks * CHUNK / sample_rate.max(1) as f64
}
/// Time of the grid's downbeat in seconds.
pub fn downbeat_secs(g: BeatGrid, sample_rate: u32) -> f64 {
    chunks_to_secs(g.offset, sample_rate)
}

/// Keeps the downbeat at or after the start of the track by whole bars, so bar numbering is unchanged.
fn normalised(mut g: BeatGrid, sample_rate: u32) -> BeatGrid {
    let bar = 4.0 * g.period_chunks(sample_rate);
    if bar > 0.0 && g.offset < 0.0 {
        g.offset += (-g.offset / bar).ceil() * bar;
    }
    g
}

/// The grid moved by `ms` (positive = later).
pub fn shifted(g: BeatGrid, ms: f64, sample_rate: u32) -> BeatGrid {
    normalised(BeatGrid { offset: g.offset + secs_to_chunks(ms / 1000.0, sample_rate), ..g }, sample_rate)
}

/// The tempo multiplied by `f` (2.0 and 0.5 for the double and half-time fixes), the downbeat kept.
pub fn scaled(g: BeatGrid, f: f32) -> BeatGrid {
    BeatGrid { bpm: (g.bpm * f).clamp(40.0, 250.0), ..g }
}

/// The beat nearest `t` becomes the downbeat. The beats do not move.
pub fn bar_one_here(g: BeatGrid, t: f64, sample_rate: u32) -> BeatGrid {
    let period = 60.0 / g.bpm as f64;
    let k = ((t - downbeat_secs(g, sample_rate)) / period).round();
    let down = downbeat_secs(g, sample_rate) + k * period;
    normalised(BeatGrid { offset: secs_to_chunks(down, sample_rate), ..g }, sample_rate)
}

/// A beat (and the downbeat) is put at `t`: the whole grid moves so a beat lands exactly there.
pub fn beat_here(g: BeatGrid, t: f64, sample_rate: u32) -> BeatGrid {
    normalised(BeatGrid { offset: secs_to_chunks(t, sample_rate), ..g }, sample_rate)
}

/// The grid a tap fit describes. The downbeat is the beat of the new grid nearest the old downbeat, so a small
/// correction keeps every bar line on the same beat it was on.
pub fn from_fit(fit: &TapFit, old: BeatGrid, sample_rate: u32) -> BeatGrid {
    let period = fit.period_secs();
    let old_down = if old.is_valid() { downbeat_secs(old, sample_rate) } else { fit.beat_secs };
    let k = ((old_down - fit.beat_secs) / period).round() as i64;
    let down = fit.beat_time(k);
    normalised(BeatGrid { bpm: fit.bpm as f32, offset: secs_to_chunks(down, sample_rate) }, sample_rate)
}

/// The grid moved onto the strongest onsets of the audio, within [`SNAP_MAX_SECS`]. Unchanged if nothing is
/// clearly better.
pub fn snapped(g: BeatGrid, onsets: &[f32], frame_secs: f64, sample_rate: u32) -> BeatGrid {
    let fit = TapFit::from_grid(g.bpm as f64, downbeat_secs(g, sample_rate));
    let s = snap_to_onsets(&fit, onsets, frame_secs, SNAP_MAX_SECS);
    normalised(BeatGrid { offset: secs_to_chunks(s.beat_secs, sample_rate), ..g }, sample_rate)
}

/// The beat nearest `frame` on the grid (source frames), kept inside the track. Unchanged without a grid.
pub fn snap_frame(g: BeatGrid, sample_rate: u32, duration_frames: u64, frame: f64) -> f64 {
    if !g.is_valid() {
        return frame;
    }
    let fpb = 60.0 / g.bpm as f64 * sample_rate as f64;
    let anchor = g.offset * CHUNK;
    (anchor + ((frame - anchor) / fpb).round() * fpb).clamp(0.0, (duration_frames as f64 - 1.0).max(0.0))
}

/// Frames in `beats` beats of the grid.
pub fn beats_to_frames(g: BeatGrid, sample_rate: u32, beats: f32) -> f64 {
    if g.is_valid() {
        60.0 / g.bpm as f64 * sample_rate as f64 * beats as f64
    } else {
        0.0
    }
}

/// What makes two sets of hot cues the same: pad, start, end, name and colour.
type CueKey = (u8, i64, Option<i64>, String, [u8; 3]);
fn cue_key(cues: &[Cue]) -> Vec<CueKey> {
    let mut k: Vec<_> = cues
        .iter()
        .filter_map(|c| {
            let at = (c.pos_frames.round() as i64, c.out_frames.map(|e| e.round() as i64));
            c.hot_cue.map(|p| (p, at.0, at.1, c.label.clone(), c.color))
        })
        .collect();
    k.sort();
    k
}

/// `c` at a new place, keeping its pad, name and colour.
fn recue(c: &Cue, pos: f64, out: Option<f64>, sample_rate: u32) -> Cue {
    let mut n = Cue::new(c.hot_cue, pos, out, sample_rate, c.color);
    n.label = c.label.clone();
    n
}

/// The frame one step of `beats` beats back or on from `head`: the grid line before or after it, so a playhead part
/// of the way through a beat first goes to that beat's start. A playhead within a fiftieth of a step of a line counts
/// as on it. Without a grid a beat is taken as half a second.
pub fn step_frame(g: BeatGrid, sample_rate: u32, head: f64, beats: f64, forward: bool) -> f64 {
    let (unit, anchor) = if g.is_valid() {
        (60.0 / g.bpm as f64 * sample_rate as f64 * beats, g.offset * CHUNK)
    } else {
        (0.5 * sample_rate as f64 * beats, 0.0)
    };
    let pos = (head - anchor) / unit;
    let k = if forward { (pos + 1.02).floor() } else { (pos - 1.02).ceil() };
    anchor + k * unit
}

/// The grid at another tempo with the beat nearest `pivot_secs` left where it is, and the bars on the same beats: the
/// way to fit a tempo by hand is to line up one beat, go far along the track and change the tempo until that fits too.
pub fn retempo(g: BeatGrid, bpm: f32, pivot_secs: f64, sample_rate: u32) -> BeatGrid {
    let bpm = bpm.clamp(40.0, 250.0);
    if !g.is_valid() {
        return BeatGrid { bpm, ..g };
    }
    let down = downbeat_secs(g, sample_rate);
    let k = ((pivot_secs - down) * g.bpm as f64 / 60.0).round();
    let pivot = down + k * 60.0 / g.bpm as f64;
    let new_down = pivot - k * 60.0 / bpm as f64;
    normalised(BeatGrid { bpm, offset: secs_to_chunks(new_down, sample_rate) }, sample_rate)
}

/// The two jobs of the screen; the tab also decides what dragging on the waveform may move.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Cues,
    Grid,
}

/// Something on the waveform that can be dragged.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mark {
    Cue(u8),
    /// The out point of a hot loop.
    CueEnd(u8),
    MainCue,
    LoopStart,
    LoopEnd,
    /// A drop, by its place in the list of drops.
    Drop(usize),
}

enum Drag {
    Scrub,
    Grid,
    Mark(Mark),
}

/// Where the scrolling waveform puts a frame: the playhead is in the middle, `zoom` pixels a chunk.
#[derive(Clone, Copy)]
struct View {
    left: f32,
    half_w: f32,
    head_chunks: f64,
    zoom: f32,
}

impl View {
    fn x_of(&self, frame: f64) -> f32 {
        self.left + self.half_w + ((frame / CHUNK) - self.head_chunks) as f32 * self.zoom
    }
    fn frame_at(&self, x: f32) -> f64 {
        (self.head_chunks + ((x - self.left - self.half_w) / self.zoom) as f64) * CHUNK
    }
}

// ── State ─────────────────────────────────────────────────────────────────────

/// One step of undo: the grid and the cues together.
struct Snap {
    grid: BeatGrid,
    source: Source,
    cue_point: f64,
    cues: Vec<Cue>,
    drops: Option<Vec<f64>>,
}

/// A drop put within this many beats of another takes its place; "Remove drop" reaches this far too.
const DROP_REACH_BEATS: f64 = 16.0;

/// `drops` with one at `t`, in order; a drop within `reach` seconds of it is the one being moved, so it goes.
pub fn with_drop_at(drops: &[f64], t: f64, reach: f64) -> Vec<f64> {
    let mut d: Vec<f64> = drops.iter().copied().filter(|x| (x - t).abs() > reach).collect();
    d.push(t);
    d.sort_by(f64::total_cmp);
    d
}

/// `drops` without the one the playhead at `t` is in (the last one at or before it) or, failing that, the nearest one
/// within `reach` seconds. `None` when there is no such drop.
pub fn without_drop_at(drops: &[f64], t: f64, reach: f64, in_a_drop: bool) -> Option<Vec<f64>> {
    let inside = if in_a_drop { drops.iter().rposition(|d| *d <= t + 1e-6) } else { None };
    let i = inside.or_else(|| {
        (0..drops.len())
            .filter(|&i| (drops[i] - t).abs() <= reach)
            .min_by(|&a, &b| (drops[a] - t).abs().total_cmp(&(drops[b] - t).abs()))
    })?;
    let mut d = drops.to_vec();
    d.remove(i);
    Some(d)
}

struct TempoNotice {
    text: String,
    can_switch: bool,
}

/// " (a 3:2 difference)" and the like, when two tempos are a simple ratio apart: the usual signature of an analysis
/// that locked onto the wrong beat.
fn ratio_hint(old: f32, fresh: f32) -> String {
    let r = (fresh / old) as f64;
    for (num, den, name) in [
        (3.0, 2.0, "3:2"),
        (2.0, 3.0, "2:3"),
        (2.0, 1.0, "double"),
        (1.0, 2.0, "half"),
        (4.0, 3.0, "4:3"),
        (3.0, 4.0, "3:4"),
    ] {
        if (r - num / den).abs() < 0.02 * num / den {
            return format!(" (a {name} difference: the earlier one had the wrong beat)");
        }
    }
    String::new()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Detected,
    Saved,
    Tapped,
    Edited,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Detected => "Detected by Dubplate",
            Source::Saved => "Saved by you",
            Source::Tapped => "From your taps",
            Source::Edited => "Edited",
        }
    }
}

/// What the screen asks the app to do.
#[derive(Default)]
pub struct PrepEvents {
    pub close: bool,
    pub save: bool,
    pub toggle_play: bool,
    /// Start / stop the player (hold-to-audition: press plays, release stops and returns).
    pub play: bool,
    pub pause: bool,
    pub restart: bool,
    /// Seek to this fraction of the track.
    pub seek: Option<f64>,
    /// Jump to this source frame.
    pub jump: Option<f64>,
    /// Run (Some(Some)) or stop (Some(None)) a loop on the prepare player.
    pub set_loop: Option<Option<(f64, f64)>>,
}

/// What the app tells the screen about the output.
pub struct PrepStatus {
    /// Where the player is being heard, or why it is not.
    pub note: String,
    pub warn: bool,
    /// Another deck holds this track and is playing, so a save reaches it only after it is reloaded.
    pub deck_playing_it: bool,
}

pub struct Prepare {
    pub path: PathBuf,
    pub track: Arc<DecodedTrack>,
    pub grid: BeatGrid,
    /// What detection found.
    pub analysed: BeatGrid,
    /// What is stored for the track now (the saved grid, or the detected one).
    baseline: BeatGrid,
    source: Source,
    undo: Vec<Snap>,
    /// Main cue point, source frames.
    pub cue_point: f64,
    /// Hot cues and hot loops on pads 1-8.
    pub cues: Vec<Cue>,
    cue_baseline: (f64, Vec<CueKey>),
    /// Drops put in by hand (seconds); `None` while the ones analysis found stand.
    pub drops: Option<Vec<f64>>,
    drops_baseline: Option<Vec<f64>>,
    /// Loop-in set, out not yet.
    loop_in: Option<f64>,
    loop_region: Option<LoopRegion>,
    loop_beats: f32,
    quantize: bool,
    taps: Vec<f64>,
    fit: Option<Result<TapFit, TapError>>,
    pub tap_delay_ms: f32,
    snap_taps: bool,
    /// The kick onset curve (for snapping) and the broadband one (for judging a grid), with their frame lengths. They
    /// take a seventh of a second each to work out for a five-minute track, so a thread does it while the screen
    /// opens: done on the spot, the first frame of Prepare would freeze the waveforms of a deck that is playing.
    onsets: Option<(Vec<f32>, f64)>,
    full_onsets: Option<(Vec<f32>, f64)>,
    curves: Option<std::sync::mpsc::Receiver<Curves>>,
    quality: Option<((u32, i64), dubplate_engine::bpm::GridQuality)>,
    bars: Option<((u32, i64), Option<dubplate_engine::downbeat::Opinion>)>,
    zoom: f32,
    pub click_on: bool,
    click_level: f32,
    playhead: dubplate_engine::playhead::Playhead,
    head_frames: f64,
    pub note: String,
    /// Set when the tempo stored for the track disagrees with what analysis finds now.
    notice: Option<TempoNotice>,
    /// The cue, pad or loop button being held to hear it.
    hold: Option<Hold>,
    tab: Tab,
    /// Whether the click was on before the Grid tab turned it on.
    click_before: Option<bool>,
    /// The pad whose fine controls are shown.
    selected: Option<u8>,
    /// What a drag on the waveform is moving.
    drag: Option<Drag>,
    /// What the waveform's right-click menu is about: the mark under it, and the frame.
    menu: Option<(Option<Mark>, f64)>,
    /// Close was asked for with changes not saved.
    confirm_close: bool,
}

/// The two onset curves of a track, each with its frame length in seconds.
type Curves = ((Vec<f32>, f64), (Vec<f32>, f64));

/// A button held down to audition what it sets: plays from there while held.
struct Hold {
    key: u32,
    since: std::time::Instant,
    /// The player was already running, so releasing leaves it running.
    was_playing: bool,
    /// Where the playhead goes back to on release.
    return_to: f64,
    /// A loop set by this hold, which ends with it when the hold was a real one (not a tap).
    loop_started: bool,
}

/// A press shorter than this is a tap: it parks the playhead on the cue (and keeps a loop set) instead of auditioning.
const TAP_SECS: f64 = 0.25;

/// Whether the primary mouse button went down on `r` (now or earlier, still held), and whether it is down at all.
fn pressed_on(ui: &egui::Ui, r: &egui::Response) -> (bool, bool) {
    let (pressed, down, secondary) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.secondary_down()));
    ((r.is_pointer_button_down_on() || (pressed && r.hovered())) && !secondary, down)
}

impl Prepare {
    pub fn new(
        path: PathBuf,
        track: Arc<DecodedTrack>,
        saved: Option<BeatGrid>,
        cues: Vec<Cue>,
        main_cue: Option<f64>,
        drops: Option<Vec<f64>>,
    ) -> Self {
        crate::sections::set_drops(&track, drops.as_deref());
        let (curves_tx, curves) = std::sync::mpsc::channel();
        let t = Arc::clone(&track);
        std::thread::spawn(move || {
            let kicks = dubplate_engine::bpm::onset_curve(&t.samples, t.channels, t.sample_rate);
            let all = dubplate_engine::bpm::onset_curve_full(&t.samples, t.channels, t.sample_rate);
            let _ = curves_tx.send((kicks, all));
        });
        let analysed = BeatGrid { bpm: track.bpm, offset: track.beat_offset };
        let (grid, source) = match saved {
            Some(g) if g != analysed => (g, Source::Saved),
            _ => (analysed, Source::Detected),
        };
        let cues: Vec<Cue> = cues.into_iter().filter(|c| matches!(c.hot_cue, Some(1..=8))).collect();
        let cue_point = main_cue.unwrap_or_else(|| {
            let f = grid.offset * CHUNK;
            if grid.is_valid() && f > 0.0 && f < track.duration_frames as f64 {
                f
            } else {
                0.0
            }
        });
        Prepare {
            cue_baseline: (cue_point, cue_key(&cues)),
            cue_point,
            cues,
            drops_baseline: drops.clone(),
            drops,
            loop_in: None,
            loop_region: None,
            loop_beats: 4.0,
            quantize: true,
            path,
            track,
            grid,
            analysed,
            baseline: grid,
            source,
            undo: Vec::new(),
            taps: Vec::new(),
            fit: None,
            tap_delay_ms: 0.0,
            snap_taps: true,
            onsets: None,
            full_onsets: None,
            curves: Some(curves),
            quality: None,
            bars: None,
            zoom: 1.0,
            click_on: false,
            click_level: 0.5,
            playhead: dubplate_engine::playhead::Playhead::new(),
            head_frames: 0.0,
            note: String::new(),
            notice: None,
            hold: None,
            tab: Tab::Cues,
            click_before: None,
            selected: None,
            drag: None,
            menu: None,
            confirm_close: false,
        }
    }

    /// Compares the tempo stored for the track with what analysis finds now (the analysis improves, and older ones
    /// could be out by a dotted or triplet ratio). Call once after opening, with the library's stored tempo.
    pub fn check_tempo(&mut self, library_bpm: Option<f32>) {
        let fresh = self.analysed.bpm;
        if fresh <= 0.0 {
            return;
        }
        let differs = |a: f32| (a - fresh).abs() / fresh > 0.015;
        self.notice = if self.source == Source::Saved && differs(self.grid.bpm) {
            Some(TempoNotice {
                text: format!(
                    "The grid you saved is {:.2} BPM; analysis now finds {:.2} BPM{}.",
                    self.grid.bpm,
                    fresh,
                    ratio_hint(self.grid.bpm, fresh)
                ),
                can_switch: true,
            })
        } else if self.source != Source::Saved && library_bpm.is_some_and(differs) {
            let old = library_bpm.unwrap_or(0.0);
            Some(TempoNotice { text: format!("The library had {:.2} BPM for this track; analysis now finds {:.2} BPM{}, and the grid is aligned to it.", old, fresh, ratio_hint(old, fresh)), can_switch: false })
        } else {
            None
        };
    }

    pub fn grid_dirty(&self) -> bool {
        self.grid != self.baseline
    }
    pub fn cues_dirty(&self) -> bool {
        self.cue_point.round() != self.cue_baseline.0.round() || cue_key(&self.cues) != self.cue_baseline.1
    }
    pub fn dirty(&self) -> bool {
        self.grid_dirty() || self.cues_dirty() || self.drops != self.drops_baseline
    }

    /// Tells the waveforms where the drops are now.
    fn show_drops(&self) {
        crate::sections::set_drops(&self.track, self.drops.as_deref());
    }

    /// Puts the drops back as they were last saved (closing without saving).
    pub fn forget_unsaved_drops(&self) {
        crate::sections::set_drops(&self.track, self.drops_baseline.as_deref());
    }

    /// The drops as they stand: the hand-set ones, else the ones found.
    fn current_drops(&self) -> Vec<f64> {
        match &self.drops {
            Some(d) => d.clone(),
            None => crate::sections::of(&self.track, self.grid).map(|s| s.drops()).unwrap_or_default(),
        }
    }

    /// The bar line nearest `secs`.
    fn bar_line(&self, secs: f64) -> f64 {
        let bar = 4.0 * 60.0 / self.grid.bpm as f64;
        let down = downbeat_secs(self.grid, self.track.sample_rate);
        down + ((secs - down) / bar).round() * bar
    }

    /// A drop on the bar line nearest the playhead. One within four bars is the one being moved.
    fn drop_here(&mut self, head_secs: f64) {
        if !self.grid.is_valid() {
            return;
        }
        let t = self.bar_line(head_secs).max(0.0);
        let reach = DROP_REACH_BEATS * 60.0 / self.grid.bpm as f64;
        let d = with_drop_at(&self.current_drops(), t, reach);
        self.remember();
        self.drops = Some(d);
        self.show_drops();
        let (bar, _) = crate::sections::bar_and_beat(t, self.grid, self.track.sample_rate);
        self.note = format!("Drop at bar {bar}");
    }

    /// Takes away the drop the playhead is in, or the nearest one within four bars.
    fn remove_drop(&mut self, head_secs: f64) {
        if !self.grid.is_valid() {
            return;
        }
        let reach = DROP_REACH_BEATS * 60.0 / self.grid.bpm as f64;
        let in_a_drop = crate::sections::of(&self.track, self.grid).is_some_and(|s| {
            s.sections.iter().any(|x| {
                x.kind == dubplate_engine::SectionKind::Drop && head_secs >= x.start_secs && head_secs < x.end_secs
            })
        });
        match without_drop_at(&self.current_drops(), head_secs, reach, in_a_drop) {
            Some(d) => {
                self.remember();
                self.drops = Some(d);
                self.show_drops();
                self.note = "Drop removed".into();
            }
            None => self.note = "No drop here to remove".into(),
        }
    }

    /// Back to the drops analysis finds.
    fn found_drops(&mut self) {
        if self.drops.is_some() {
            self.remember();
            self.drops = None;
            self.show_drops();
            self.note = "Drops as found".into();
        }
    }

    pub fn mark_saved(&mut self) {
        self.drops_baseline = self.drops.clone();
        self.cue_baseline = (self.cue_point, cue_key(&self.cues));
        self.baseline = self.grid;
        self.source = if self.grid == self.analysed { Source::Detected } else { Source::Saved };
    }

    /// Pushes the grid and the click settings to the audio thread.
    pub fn sync_click(&self, click: &ClickControls) {
        let sr = self.track.sample_rate;
        click.source_rate.store(sr, std::sync::atomic::Ordering::Relaxed);
        click.on.store(self.click_on, std::sync::atomic::Ordering::Relaxed);
        click.set_level(self.click_level);
        if self.grid.is_valid() {
            click.set_grid(self.grid.bpm as f64, downbeat_secs(self.grid, sr));
        } else {
            click.set_grid(0.0, 0.0);
        }
    }

    fn remember(&mut self) {
        self.undo.push(Snap {
            grid: self.grid,
            source: self.source,
            cue_point: self.cue_point,
            cues: self.cues.clone(),
            drops: self.drops.clone(),
        });
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
    }

    fn change(&mut self, g: BeatGrid, source: Source) {
        if g != self.grid {
            self.remember();
            self.grid = g;
            self.source = source;
        }
    }

    fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.grid = s.grid;
            self.source = s.source;
            self.cue_point = s.cue_point;
            self.cues = s.cues;
            self.drops = s.drops;
            self.show_drops();
        }
    }

    /// A position from the playhead, on the nearest beat when Quantize is on.
    fn place(&self, frame: f64) -> f64 {
        if self.quantize {
            snap_frame(self.grid, self.track.sample_rate, self.track.duration_frames, frame)
        } else {
            frame
        }
    }

    fn pad_cue(&self, pad: u8) -> Option<&Cue> {
        self.cues.iter().find(|c| c.hot_cue == Some(pad))
    }

    /// Puts a cue (or the running loop) on a pad.
    fn set_pad(&mut self, pad: u8, head: f64) {
        self.remember();
        let sr = self.track.sample_rate;
        let colour = PAD_COLORS[(pad - 1) as usize];
        let cue = match self.loop_region {
            Some(r) if r.active => Cue::new(Some(pad), r.start, Some(r.end), sr, colour),
            _ => Cue::new(Some(pad), self.place(head), None, sr, colour),
        };
        self.cues.retain(|c| c.hot_cue != Some(pad));
        self.cues.push(cue);
    }

    /// Puts the usual cues (start, build, drop, break, second drop, outro) on the pads that are empty, from the track's
    /// sections. Returns how many it set; pads already holding a cue are left alone.
    fn suggest_cues(&mut self) -> usize {
        let Some(structure) = crate::sections::of(&self.track, self.grid) else { return 0 };
        let sr = self.track.sample_rate;
        let roles = dubplate_engine::cue_roles::suggest(&structure, downbeat_secs(self.grid, sr));
        let free: Vec<_> = roles.into_iter().filter(|r| self.pad_cue(r.pad).is_none()).collect();
        if free.is_empty() {
            return 0;
        }
        self.remember();
        for r in &free {
            let mut cue = Cue::new(Some(r.pad), r.secs * sr as f64, None, sr, PAD_COLORS[(r.pad - 1) as usize]);
            cue.label = r.name.to_string();
            self.cues.push(cue);
        }
        free.len()
    }

    /// Hold-to-audition for a button. `key` names the button; `pressed_here` is whether the pointer went down on it
    /// (this frame or before); `mouse_down` is whether the button is still down anywhere. On the press the player goes
    /// to `start()`'s position (and loop, if any) and plays; the hold then lasts exactly as long as the mouse button
    /// does, wherever the pointer wanders (a widget's own "pressed" state ends if the mouse drifts a few pixels). On
    /// release it stops and returns there, unless it was already playing. A hold of a real length also ends a loop the
    /// press started; a tap leaves it set.
    fn hold_step(
        &mut self,
        key: u32,
        pressed_here: bool,
        mouse_down: bool,
        playing: bool,
        ev: &mut PrepEvents,
        start: impl FnOnce(&mut Self, &mut PrepEvents) -> Option<(f64, bool)>,
    ) {
        match &self.hold {
            None if pressed_here && mouse_down => {
                if let Some((at, loop_started)) = start(self, ev) {
                    ev.play = !playing;
                    self.hold = Some(Hold {
                        key,
                        since: std::time::Instant::now(),
                        was_playing: playing,
                        return_to: at,
                        loop_started,
                    });
                }
            }
            Some(h) if h.key == key && !mouse_down => self.release_hold(ev),
            _ => {}
        }
    }

    /// Ends the hold, if there is one: stop and go back, unless the player was running before.
    fn release_hold(&mut self, ev: &mut PrepEvents) {
        let Some(h) = self.hold.take() else { return };
        if !h.was_playing {
            ev.pause = true;
            ev.jump = Some(h.return_to);
        }
        if h.loop_started && h.since.elapsed().as_secs_f64() >= TAP_SECS {
            if let Some(r) = self.loop_region.as_mut() {
                r.active = false;
            }
            ev.set_loop = Some(None);
        }
    }

    fn delete_pad(&mut self, pad: u8) {
        self.remember();
        self.cues.retain(|c| c.hot_cue != Some(pad));
    }

    /// Starts a loop of `beats` beats at the playhead.
    fn auto_loop(&mut self, head: f64, beats: f32) -> Option<(f64, f64)> {
        let len = beats_to_frames(self.grid, self.track.sample_rate, beats);
        let start = self.place(head);
        let end = (start + len).min(self.track.duration_frames as f64 - 1.0);
        if len <= 0.0 || end - start < 64.0 {
            return None;
        }
        self.loop_in = None;
        self.loop_region = Some(LoopRegion { start, end, active: true });
        Some((start, end))
    }

    /// Takes the onset curves from the thread that works them out, once they are there.
    fn poll_curves(&mut self) {
        let Some(rx) = &self.curves else { return };
        match rx.try_recv() {
            Ok((kicks, all)) => {
                self.onsets = Some(kicks);
                self.full_onsets = Some(all);
                self.curves = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.curves = None,
        }
    }

    /// The kick onset curve, or a note saying it is not ready yet (the first moment after the screen opens).
    fn onsets(&mut self) -> Option<(Vec<f32>, f64)> {
        self.poll_curves();
        if self.onsets.is_none() {
            self.note = "Still reading the track: try again in a moment".into();
        }
        self.onsets.clone()
    }

    /// How well the current grid sits on the music; recomputed only when the grid changes.
    fn grid_quality(&mut self) -> Option<dubplate_engine::bpm::GridQuality> {
        let key = (self.grid.bpm.to_bits(), (self.grid.offset * 1000.0).round() as i64);
        if let Some((k, q)) = self.quality {
            if k == key {
                return Some(q);
            }
        }
        let (curve, frame) = self.full_onsets.as_ref()?;
        let q = dubplate_engine::bpm::grid_quality(
            curve,
            *frame,
            self.grid.bpm,
            self.grid.offset * WAVEFORM_CHUNK_FRAMES as f64 / self.track.sample_rate.max(1) as f64 / frame,
        );
        self.quality = Some((key, q));
        Some(q)
    }

    /// What the track's sections say about where bar 1 is, for the current grid; recomputed only when it changes.
    fn bar_opinion(&mut self) -> Option<dubplate_engine::downbeat::Opinion> {
        let key = (self.grid.bpm.to_bits(), (self.grid.offset * 1000.0).round() as i64);
        if let Some((k, o)) = self.bars {
            if k == key {
                return o;
            }
        }
        let sr = self.track.sample_rate;
        let o = self
            .grid
            .is_valid()
            .then(|| {
                dubplate_engine::downbeat::from_sections(
                    &self.track.wave,
                    60.0 / self.grid.bpm as f64,
                    downbeat_secs(self.grid, sr),
                )
            })
            .flatten();
        self.bars = Some((key, o));
        o
    }

    /// Do the bars start where the track's sections do? A second opinion on bar 1, with a button to take it.
    fn bar_badge(&mut self, ui: &mut egui::Ui) {
        // Under this margin the signs do not agree well enough to contradict the grid.
        const SURE: f32 = 2.0;
        let sr = self.track.sample_rate;
        let Some(o) = self.bar_opinion() else { return };
        if o.shift == 0 || o.margin < SURE {
            badge(ui, "bar 1 fits", palette::ok()).on_hover_text(
                "The bass and the changes in the track (a drop, a break) start on the bars of this grid.",
            );
            return;
        }
        let beats = if o.shift == 3 { -1 } else { o.shift };
        badge(ui, "bar 1?", palette::warn())
            .on_hover_text(format!("The bass and the changes in the track start {} from this grid's bar 1. Listen to a drop with the click on; if the drop lands on the 2, 3 or 4, move bar 1.", if beats == -1 { "a beat before".to_string() } else { format!("{beats} beats after") }));
        let g = self.grid;
        let period = 60.0 / g.bpm as f64;
        if ui
            .small_button(if beats == -1 {
                "move bar 1 a beat earlier".to_string()
            } else {
                format!("move bar 1 {beats} later")
            })
            .on_hover_text("Make that beat the first beat of a bar. The beats do not move.")
            .clicked()
        {
            let ng = bar_one_here(g, downbeat_secs(g, sr) + beats as f64 * period, sr);
            self.change(ng, Source::Edited);
        }
    }

    /// A traffic light for the grid: do the kicks sit on it, all the way through the track?
    fn quality_badge(&mut self, ui: &mut egui::Ui) {
        use dubplate_engine::bpm::GridVerdict::*;
        let Some(q) = self.grid_quality() else {
            badge(ui, "checking grid", palette::text_mid())
                .on_hover_text("Reading the track to see whether the kicks sit on the grid.");
            return;
        };
        let (text, col, what) = match q.verdict {
            Good => ("grid fits", palette::ok(), "The kicks sit on the grid from the start to the end of the track."),
            Check => ("check grid", palette::warn(), "Close, but worth a look: the kicks are loose, early or late by a few ms, or the tempo drifts a little."),
            Off => ("grid is off", palette::bad(), "The kicks are clearly away from the grid, or the grid drifts off them. Try Snap to the kicks, or tap the tempo."),
            Unknown => ("can't tell", palette::text_mid(), "Too few clear kicks to judge (ambient, very loose, or too short). Check it by ear with the click."),
        };
        let detail = if q.hits == 0 {
            String::new()
        } else {
            format!("\n\nkicks found on {:.0}% of beats\nkicks are {:+.0} ms from the grid (negative: before it)\nscatter {:.1} ms, drift over the track {:+.1} ms\n\nThe offset is judged loosely: soft kicks start earlier than their loudest edge. Scatter and drift are exact.", q.hit_rate * 100.0, q.bias_ms, q.spread_ms, q.drift_ms)
        };
        badge(ui, text, col).on_hover_text(format!("{what}{detail}"));
    }

    fn snap(&mut self) {
        let sr = self.track.sample_rate;
        let g = self.grid;
        let Some((curve, frame)) = self.onsets() else { return };
        let s = snapped(g, &curve, frame, sr);
        if s == g {
            self.note = "Already on the kicks".into();
        } else {
            let ms = (downbeat_secs(s, sr) - downbeat_secs(g, sr)) * 1000.0;
            self.note = format!("Moved {ms:+.1} ms onto the kicks");
            self.change(s, Source::Edited);
        }
    }

    fn tap(&mut self, at_secs: f64) {
        self.taps.push(at_secs);
        self.refit();
    }

    fn refit(&mut self) {
        self.fit =
            if self.taps.len() < 2 { None } else { Some(fit_taps(&self.taps, self.tap_delay_ms as f64 / 1000.0)) };
    }

    fn apply_taps(&mut self) {
        let Some(Ok(fit)) = self.fit else { return };
        let sr = self.track.sample_rate;
        let mut g = from_fit(&fit, self.grid, sr);
        let mut note = format!("{:.2} BPM from {} taps", fit.bpm, fit.taps_used);
        if self.snap_taps {
            let Some((curve, frame)) = self.onsets() else { return };
            let s = snapped(g, &curve, frame, sr);
            if s != g {
                note += &format!(
                    ", moved {:+.1} ms onto the kicks",
                    (downbeat_secs(s, sr) - downbeat_secs(g, sr)) * 1000.0
                );
                g = s;
            }
        }
        self.change(g, Source::Tapped);
        self.note = note;
        self.taps.clear();
        self.fit = None;
    }

    // ── Small actions the buttons, the keys and the waveform share ────────────

    fn set_tab(&mut self, tab: Tab) {
        if tab == self.tab {
            return;
        }
        // the click is how a grid is checked, so it is on while the Grid tools are open
        match tab {
            Tab::Grid => {
                self.click_before = Some(self.click_on);
                self.click_on = true;
            }
            Tab::Cues => {
                if let Some(was) = self.click_before.take() {
                    self.click_on = was;
                }
            }
        }
        self.tab = tab;
    }

    /// A plain cue on `pad` at `frame` (on the beat when Snap is on).
    fn put_cue(&mut self, pad: u8, frame: f64) {
        self.remember();
        let sr = self.track.sample_rate;
        let cue = Cue::new(Some(pad), self.place(frame), None, sr, PAD_COLORS[(pad - 1) as usize]);
        self.cues.retain(|c| c.hot_cue != Some(pad));
        self.cues.push(cue);
        self.selected = Some(pad);
    }

    fn free_pad(&self) -> Option<u8> {
        (1..=8u8).find(|p| self.pad_cue(*p).is_none())
    }

    fn min_loop_frames(&self) -> f64 {
        beats_to_frames(self.grid, self.track.sample_rate, 0.25).max(64.0)
    }

    fn last_frame(&self) -> f64 {
        (self.track.duration_frames as f64 - 1.0).max(0.0)
    }

    /// Puts what is being dragged at `frame`: on the beat (a drop: on the bar line), or exactly there when `free`.
    fn move_mark(&mut self, mark: Mark, frame: f64, free: bool, ev: &mut PrepEvents) {
        let sr = self.track.sample_rate;
        let last = self.last_frame();
        let min = self.min_loop_frames();
        let at =
            if free { frame } else { snap_frame(self.grid, sr, self.track.duration_frames, frame) }.clamp(0.0, last);
        match mark {
            Mark::Cue(pad) => {
                if let Some(c) = self.cues.iter_mut().find(|c| c.hot_cue == Some(pad)) {
                    let len = c.out_frames.map(|e| e - c.pos_frames);
                    *c = recue(c, at, len.map(|l| (at + l).min(last)), sr);
                }
            }
            Mark::CueEnd(pad) => {
                if let Some(c) = self.cues.iter_mut().find(|c| c.hot_cue == Some(pad)) {
                    *c = recue(c, c.pos_frames, Some(at.max(c.pos_frames + min).min(last)), sr);
                }
            }
            Mark::MainCue => self.cue_point = at,
            Mark::LoopStart | Mark::LoopEnd => {
                if let Some(r) = self.loop_region.as_mut() {
                    if mark == Mark::LoopStart {
                        r.start = at.min(r.end - min).max(0.0);
                    } else {
                        r.end = at.max(r.start + min).min(last);
                    }
                    if r.active {
                        ev.set_loop = Some(Some((r.start, r.end)));
                    }
                }
            }
            Mark::Drop(i) => {
                let t = self.bar_line(frame / sr as f64).max(0.0);
                if let Some(d) = self.drops.as_mut().and_then(|d| d.get_mut(i)) {
                    *d = t;
                }
                self.show_drops();
            }
        }
    }

    /// The cue, loop edge or drop under `x` on the waveform.
    fn mark_at(&self, v: View, x: f32) -> Option<Mark> {
        const REACH: f32 = 5.0;
        let near = |frame: f64| (v.x_of(frame) - x).abs() <= REACH;
        for c in &self.cues {
            let Some(pad) = c.hot_cue else { continue };
            if near(c.pos_frames) {
                return Some(Mark::Cue(pad));
            }
            if c.out_frames.is_some_and(near) {
                return Some(Mark::CueEnd(pad));
            }
        }
        if let Some(r) = self.loop_region {
            if near(r.end) {
                return Some(Mark::LoopEnd);
            }
            if near(r.start) {
                return Some(Mark::LoopStart);
            }
        }
        let sr = self.track.sample_rate as f64;
        if let Some(i) = self.current_drops().iter().position(|d| near(d * sr)) {
            return Some(Mark::Drop(i));
        }
        near(self.cue_point).then_some(Mark::MainCue)
    }

    /// Where the previous or next section starts, in frames.
    fn section_step(&self, head: f64, forward: bool) -> Option<f64> {
        let sr = self.track.sample_rate as f64;
        let s = crate::sections::of(&self.track, self.grid)?;
        let t = head / sr;
        let mut starts = s.sections.iter().map(|x| x.start_secs);
        let to = if forward { starts.find(|x| *x > t + 0.05) } else { starts.rfind(|x| *x < t - 0.25) };
        to.map(|x| x * sr)
    }

    fn step(&self, beats: f64, forward: bool) -> f64 {
        step_frame(self.grid, self.track.sample_rate, self.head_frames, beats, forward).clamp(0.0, self.last_frame())
    }

    fn loop_in_here(&mut self) {
        self.loop_in = Some(self.place(self.head_frames));
    }

    fn loop_out_here(&mut self, ev: &mut PrepEvents) {
        if let Some(start) = self.loop_in.take() {
            let min = beats_to_frames(self.grid, self.track.sample_rate, 1.0).max(64.0);
            let end = self.place(self.head_frames).max(start + min).min(self.last_frame());
            if end - start >= 64.0 {
                self.loop_region = Some(LoopRegion { start, end, active: true });
                ev.set_loop = Some(Some((start, end)));
            }
        }
    }

    /// Loop on or off: leaves a running loop, starts the last one again, or makes one of the chosen size here.
    fn toggle_loop(&mut self, ev: &mut PrepEvents) {
        match self.loop_region.as_mut() {
            Some(r) if r.active => {
                r.active = false;
                ev.set_loop = Some(None);
            }
            Some(r) => {
                r.active = true;
                ev.set_loop = Some(Some((r.start, r.end)));
            }
            None => {
                if let Some(l) = self.auto_loop(self.head_frames, self.loop_beats) {
                    ev.set_loop = Some(Some(l));
                }
            }
        }
    }

    /// The loop made `f` times as long, from the same start.
    fn scale_loop(&mut self, f: f64, ev: &mut PrepEvents) {
        let (min, last) = (self.min_loop_frames(), self.last_frame());
        if let Some(r) = self.loop_region.as_mut() {
            let len = ((r.end - r.start) * f).max(min);
            if r.start + len <= last {
                r.end = r.start + len;
                if r.active {
                    ev.set_loop = Some(Some((r.start, r.end)));
                }
            }
        }
    }

    /// The loop moved by `beats` beats.
    fn move_loop(&mut self, beats: f32, ev: &mut PrepEvents) {
        let d = beats_to_frames(self.grid, self.track.sample_rate, beats);
        let last = self.last_frame();
        if let Some(r) = self.loop_region.as_mut() {
            if r.start + d >= 0.0 && r.end + d <= last {
                r.start += d;
                r.end += d;
                if r.active {
                    ev.set_loop = Some(Some((r.start, r.end)));
                }
            }
        }
    }

    /// The cue on `pad` moved by `frames` (a hot loop keeps its length).
    fn shift_pad(&mut self, pad: u8, frames: f64) {
        let sr = self.track.sample_rate;
        let last = self.last_frame();
        if self.pad_cue(pad).is_none() {
            return;
        }
        self.remember();
        if let Some(c) = self.cues.iter_mut().find(|c| c.hot_cue == Some(pad)) {
            let pos = (c.pos_frames + frames).clamp(0.0, last);
            let out = c.out_frames.map(|e| (e + pos - c.pos_frames).min(last));
            *c = recue(c, pos, out, sr);
        }
    }

    fn bar_text(&self, frame: f64) -> String {
        if !self.grid.is_valid() {
            return String::new();
        }
        let sr = self.track.sample_rate;
        let (bar, beat) = crate::sections::bar_and_beat(frame / sr as f64, self.grid, sr);
        format!("{bar}.{beat}")
    }

    /// Puts the screen in a state worth a picture (see `Shot` in the app): the Grid tools, or cues, a selected pad
    /// and a loop.
    pub fn set_up_for_a_picture(&mut self, grid_tab: bool, demo: bool) {
        if grid_tab {
            self.set_tab(Tab::Grid);
        }
        if demo {
            self.suggest_cues();
            self.selected = self.cues.iter().filter_map(|c| c.hot_cue).max();
            self.auto_loop(self.head_frames, 8.0);
        }
    }

    // ── Keys ──────────────────────────────────────────────────────────────────

    fn keys(&mut self, ui: &egui::Ui, ev: &mut PrepEvents, playing: bool) {
        use egui::Key;
        if ui.ctx().wants_keyboard_input() {
            return;
        }
        let (cmd, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        let hit = |k: Key| ui.input(|i| i.key_pressed(k));
        let head = self.head_frames;
        if cmd {
            if hit(Key::Z) {
                self.undo();
            }
            if hit(Key::S) && self.dirty() {
                ev.save = true;
            }
            for (k, forward) in [(Key::ArrowLeft, false), (Key::ArrowRight, true)] {
                if hit(k) {
                    ev.jump = self.section_step(head, forward);
                }
            }
            return;
        }
        if hit(Key::Space) {
            ev.toggle_play = true;
        }
        if hit(Key::Escape) {
            if self.confirm_close {
                self.confirm_close = false;
            } else if self.dirty() {
                self.confirm_close = true;
            } else {
                ev.close = true;
            }
        }
        for (k, forward) in [(Key::ArrowLeft, false), (Key::ArrowRight, true)] {
            if hit(k) {
                ev.jump = Some(self.step(if shift { 4.0 } else { 1.0 }, forward));
            }
        }
        if hit(Key::Home) {
            ev.restart = true;
        }
        for (n, k) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8]
            .into_iter()
            .enumerate()
        {
            if !hit(k) {
                continue;
            }
            let pad = n as u8 + 1;
            match self.pad_cue(pad).cloned() {
                Some(_) if shift => self.delete_pad(pad),
                Some(c) => {
                    self.selected = Some(pad);
                    ev.jump = Some(c.pos_frames);
                }
                None if !shift => {
                    self.set_pad(pad, head);
                    self.selected = Some(pad);
                }
                None => {}
            }
        }
        if hit(Key::C) {
            self.remember();
            self.cue_point = self.place(head);
        }
        if hit(Key::I) {
            self.loop_in_here();
        }
        if hit(Key::O) {
            self.loop_out_here(ev);
        }
        if hit(Key::L) {
            self.toggle_loop(ev);
        }
        if hit(Key::OpenBracket) {
            self.scale_loop(0.5, ev);
        }
        if hit(Key::CloseBracket) {
            self.scale_loop(2.0, ev);
        }
        if hit(Key::Plus) || hit(Key::Equals) {
            self.zoom = (self.zoom * 1.4).clamp(ZOOM_MIN, ZOOM_MAX);
        }
        if hit(Key::Minus) {
            self.zoom = (self.zoom / 1.4).clamp(ZOOM_MIN, ZOOM_MAX);
        }
        if hit(Key::G) {
            self.set_tab(if self.tab == Tab::Cues { Tab::Grid } else { Tab::Cues });
        }
        if hit(Key::M) {
            self.click_on = !self.click_on;
        }
        if hit(Key::T) && playing {
            self.tap(head / self.track.sample_rate as f64);
        }
    }

    // ── The screen ────────────────────────────────────────────────────────────

    fn header(&mut self, ui: &mut egui::Ui, ev: &mut PrepEvents) {
        let sr = self.track.sample_rate;
        let dirty = self.dirty();
        ui.horizontal(|ui| {
            ui.set_height(26.0);
            let (chip, _) = ui.allocate_exact_size(egui::vec2(62.0, 18.0), Sense::hover());
            ui.painter().rect_filled(chip, 3.0, palette::deck_a());
            ui.painter().text(
                chip.center(),
                egui::Align2::CENTER_CENTER,
                "PREPARE",
                egui::FontId::proportional(crate::theme::sz(10.5)),
                palette::background(),
            );
            let title: String = self.track.title.chars().take(60).collect();
            ui.label(RichText::new(title).strong().size(crate::theme::sz(15.0)).color(palette::text()));
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "{:.2} BPM  ·  bar 1 at {:.3} s  ·  {}",
                    self.grid.bpm,
                    downbeat_secs(self.grid, sr),
                    self.source.label(),
                ))
                .monospace()
                .size(crate::theme::sz(11.0))
                .color(palette::text_dim()),
            );
            ui.add_space(6.0);
            self.quality_badge(ui);
            self.bar_badge(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.confirm_close {
                    if ui.button("Cancel").clicked() {
                        self.confirm_close = false;
                    }
                    if ui.button("Discard").on_hover_text("Close and lose the changes").clicked() {
                        ev.close = true;
                    }
                    if ui.add(primary("Save and close")).clicked() {
                        ev.save = true;
                        ev.close = true;
                    }
                    ui.label(
                        RichText::new("Changes are not saved:").size(crate::theme::sz(12.0)).color(palette::warn()),
                    );
                    return;
                }
                if ui.button("Close").on_hover_text("Esc").clicked() {
                    if dirty {
                        self.confirm_close = true;
                    } else {
                        ev.close = true;
                    }
                }
                let save = if dirty { primary("Save") } else { egui::Button::new("Save") };
                if ui
                    .add_enabled(dirty, save)
                    .on_hover_text("Saves the grid, the cue point, the hot cues and the drops for this track (Ctrl+S)")
                    .clicked()
                {
                    ev.save = true;
                }
                if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo")).on_hover_text("Ctrl+Z").clicked() {
                    self.undo();
                }
                if dirty {
                    ui.label(RichText::new("not saved").size(crate::theme::sz(11.0)).color(palette::warn()));
                }
            });
        });
    }

    /// Notes under the header: what the last action did, where the player is heard, a tempo warning.
    fn notes(&mut self, ui: &mut egui::Ui, status: &PrepStatus) {
        ui.horizontal(|ui| {
            ui.set_height(16.0);
            if !self.note.is_empty() {
                ui.label(RichText::new(&self.note).size(crate::theme::sz(11.0)).color(palette::text()));
                ui.label(RichText::new("·").size(crate::theme::sz(11.0)).color(palette::text_dim()));
            }
            ui.label(RichText::new(&status.note).size(crate::theme::sz(11.0)).color(if status.warn {
                palette::bad()
            } else {
                palette::text_dim()
            }));
            if status.deck_playing_it {
                ui.label(
                    RichText::new("·  A deck that is playing this track takes the saved grid when it is loaded again.")
                        .size(crate::theme::sz(11.0))
                        .color(palette::text_dim()),
                );
            }
        });
        let (mut switch, mut dismiss) = (false, false);
        if let Some(n) = &self.notice {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&n.text).size(crate::theme::sz(12.0)).color(palette::warn()));
                if n.can_switch && ui.button(format!("Use the new analysis ({:.2} BPM)", self.analysed.bpm)).clicked() {
                    switch = true;
                }
                if ui.small_button("OK").clicked() {
                    dismiss = true;
                }
            });
        }
        if switch {
            let g = self.analysed;
            self.change(g, Source::Detected);
            self.notice = None;
            self.note = "Using the new analysis; save to keep it".into();
        }
        if dismiss {
            self.notice = None;
        }
    }

    /// The bar ruler and the scrolling waveform, and everything the mouse does on them.
    fn waveform(&mut self, ui: &mut egui::Ui, ev: &mut PrepEvents, progress: f64, wave_h: f32) {
        let sr = self.track.sample_rate;
        let w = ui.available_width();
        let (all, resp) = ui.allocate_exact_size(egui::vec2(w, RULER_H + wave_h), Sense::click_and_drag());
        let ruler = Rect::from_min_size(all.min, egui::vec2(w, RULER_H));
        let wf = Rect::from_min_max(egui::pos2(all.left(), all.top() + RULER_H), all.max);
        let view = View {
            left: wf.left(),
            half_w: wf.width() / 2.0,
            head_chunks: progress * self.track.duration_frames as f64 / CHUNK,
            zoom: self.zoom,
        };
        let (shift, alt) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt));
        let pointer = resp.hover_pos().or_else(|| resp.interact_pointer_pos());

        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.zoom = (self.zoom * (scroll * 0.004).exp()).clamp(ZOOM_MIN, ZOOM_MAX);
            }
        }

        // what is under the pointer: only the Cues tab lets marks be moved, so a grid edit cannot shift a cue by accident
        let under = match (self.tab, pointer) {
            (Tab::Cues, Some(p)) => self.mark_at(view, p.x),
            _ => None,
        };

        if resp.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin()).map(|p| p.x);
            let mark = match (self.tab, origin) {
                (Tab::Cues, Some(x)) => self.mark_at(view, x),
                _ => None,
            };
            self.drag = Some(match mark {
                Some(m) => {
                    self.remember();
                    if matches!(m, Mark::Drop(_)) && self.drops.is_none() {
                        self.drops = Some(self.current_drops());
                    }
                    if let Mark::Cue(p) | Mark::CueEnd(p) = m {
                        self.selected = Some(p);
                    }
                    Drag::Mark(m)
                }
                None if self.tab == Tab::Grid && shift && self.grid.is_valid() => {
                    self.remember();
                    Drag::Grid
                }
                None => Drag::Scrub,
            });
        }
        if resp.dragged() {
            let dx = resp.drag_delta().x;
            match self.drag {
                Some(Drag::Mark(m)) => {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.move_mark(m, view.frame_at(p.x), alt, ev);
                    }
                }
                Some(Drag::Grid) => {
                    let ms = (dx / self.zoom) as f64 * CHUNK / sr as f64 * 1000.0;
                    self.grid = shifted(self.grid, ms, sr);
                    self.source = Source::Edited;
                }
                _ => {
                    let frames = self.track.duration_frames.max(1) as f64;
                    let target = (self.head_frames - (dx / self.zoom) as f64 * CHUNK).clamp(0.0, frames - 1.0);
                    ev.seek = Some(target / frames);
                }
            }
        }
        if resp.drag_stopped() {
            if let Some(Drag::Mark(Mark::Drop(_))) = self.drag {
                if let Some(d) = self.drops.as_mut() {
                    d.sort_by(f64::total_cmp);
                    d.dedup_by(|a, b| (*a - *b).abs() < 0.05);
                }
                self.show_drops();
            }
            self.drag = None;
        }

        if resp.double_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let frame = view.frame_at(p.x);
                match self.tab {
                    Tab::Cues => match self.free_pad() {
                        Some(pad) if under.is_none() => self.put_cue(pad, frame),
                        Some(_) => {}
                        None => self.note = "All eight pads are in use".into(),
                    },
                    Tab::Grid => {
                        let g = bar_one_here(self.grid, frame / sr as f64, sr);
                        self.change(g, Source::Edited);
                    }
                }
            }
        } else if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                match under {
                    Some(Mark::Cue(pad)) | Some(Mark::CueEnd(pad)) => {
                        self.selected = Some(pad);
                        ev.jump = self.pad_cue(pad).map(|c| c.pos_frames);
                    }
                    _ => ev.jump = Some(self.place(view.frame_at(p.x)).clamp(0.0, self.last_frame())),
                }
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.menu = Some((under, view.frame_at(p.x)));
            }
        }
        if let Some((mark, frame)) = self.menu {
            resp.context_menu(|ui| self.wave_menu(ui, ev, mark, frame));
        }

        let icon = match (&self.drag, under) {
            (Some(Drag::Mark(_)), _) | (None, Some(_)) => Some(egui::CursorIcon::ResizeHorizontal),
            (Some(Drag::Scrub), _) | (Some(Drag::Grid), _) => Some(egui::CursorIcon::Grabbing),
            _ => None,
        };
        if let (Some(icon), true) = (icon, resp.hovered() || resp.dragged()) {
            ui.ctx().set_cursor_icon(icon);
        }

        // ── drawing ──
        let marks =
            Marks { cue_point: self.cue_point, cues: &self.cues, loop_region: self.loop_region, loop_in: self.loop_in };
        let painter = ui.painter_at(all);
        draw_waveform(
            &painter,
            wf,
            &self.track,
            self.grid,
            &marks,
            progress,
            palette::deck_a(),
            [1.0; 3],
            0.5,
            self.zoom,
        );
        self.draw_ruler(&painter, ruler, view);
        // hot cues get a name tab: the letter and what it marks
        for c in &self.cues {
            let Some(pad) = c.hot_cue else { continue };
            let x = view.x_of(c.pos_frames);
            if x < wf.left() - 80.0 || x > wf.right() {
                continue;
            }
            let col = Color32::from_rgb(c.color[0], c.color[1], c.color[2]);
            let letter = (b'A' + pad - 1) as char;
            let text = if c.label.is_empty() { letter.to_string() } else { format!("{letter} {}", c.label) };
            let galley =
                painter.layout_no_wrap(text, egui::FontId::proportional(crate::theme::sz(10.5)), palette::background());
            let tab = Rect::from_min_size(egui::pos2(x, wf.top() + 13.0), galley.size() + egui::vec2(9.0, 3.0));
            painter.rect_filled(tab, 2.0, col);
            if self.selected == Some(pad) {
                painter.rect_stroke(tab.expand(1.0), 3.0, egui::Stroke::new(1.5_f32, palette::text_bright()));
            }
            painter.galley(tab.min + egui::vec2(4.5, 1.5), galley, palette::background());
        }
        if self.tab == Tab::Grid {
            painter.text(
                egui::pos2(wf.right() - 70.0, wf.bottom() - 6.0),
                egui::Align2::RIGHT_BOTTOM,
                "Shift+drag slides the grid  ·  double-click a beat for bar 1",
                egui::FontId::proportional(crate::theme::sz(10.5)),
                palette::text_dim(),
            );
        }
    }

    /// Bar numbers along the top, as many as fit.
    fn draw_ruler(&self, painter: &egui::Painter, rect: Rect, v: View) {
        painter.rect_filled(rect, 0.0, palette::inset());
        if !self.grid.is_valid() {
            return;
        }
        let sr = self.track.sample_rate;
        let bar_frames = beats_to_frames(self.grid, sr, 4.0);
        let px_per_bar = (bar_frames / CHUNK) as f32 * v.zoom;
        let every = [1i64, 2, 4, 8, 16, 32, 64].into_iter().find(|n| px_per_bar * *n as f32 >= 46.0).unwrap_or(64);
        let anchor = self.grid.offset * CHUNK;
        let first = ((v.frame_at(rect.left()) - anchor) / bar_frames).floor() as i64;
        let last = ((v.frame_at(rect.right()) - anchor) / bar_frames).ceil() as i64;
        for k in first.max(0)..=last {
            let frame = anchor + k as f64 * bar_frames;
            if frame > self.track.duration_frames as f64 {
                break;
            }
            let x = v.x_of(frame);
            let numbered = k % every == 0;
            let top = if numbered { rect.top() + 2.0 } else { rect.bottom() - 4.0 };
            painter.line_segment(
                [egui::pos2(x, top), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(1.0_f32, palette::tick()),
            );
            if numbered {
                painter.text(
                    egui::pos2(x + 4.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{}", k + 1),
                    egui::FontId::monospace(crate::theme::sz(10.0)),
                    palette::handle_line(),
                );
            }
        }
    }

    /// Right-click on the waveform: what can be done with the mark there, or at that place.
    fn wave_menu(&mut self, ui: &mut egui::Ui, ev: &mut PrepEvents, mark: Option<Mark>, frame: f64) {
        let sr = self.track.sample_rate;
        ui.set_min_width(170.0);
        let mut done = false;
        match mark {
            Some(Mark::Cue(pad)) | Some(Mark::CueEnd(pad)) => {
                let letter = (b'A' + pad - 1) as char;
                ui.label(
                    RichText::new(format!("Hot cue {letter}")).size(crate::theme::sz(11.0)).color(palette::text_dim()),
                );
                if ui.button("Go to it").clicked() {
                    ev.jump = self.pad_cue(pad).map(|c| c.pos_frames);
                    self.selected = Some(pad);
                    done = true;
                }
                if ui.button("Move to the playhead").clicked() {
                    self.remember();
                    let head = self.head_frames;
                    self.move_mark(Mark::Cue(pad), head, !self.quantize, ev);
                    done = true;
                }
                if ui.button("Delete").clicked() {
                    self.delete_pad(pad);
                    done = true;
                }
            }
            Some(Mark::Drop(i)) => {
                ui.label(RichText::new("Drop").size(crate::theme::sz(11.0)).color(palette::text_dim()));
                if ui.button("Remove this drop").clicked() {
                    let mut d = self.current_drops();
                    if i < d.len() {
                        self.remember();
                        d.remove(i);
                        self.drops = Some(d);
                        self.show_drops();
                    }
                    done = true;
                }
            }
            _ => {
                ui.label(
                    RichText::new(format!("At bar {}", self.bar_text(self.place(frame))))
                        .size(crate::theme::sz(11.0))
                        .color(palette::text_dim()),
                );
                if self.tab == Tab::Cues {
                    if let Some(pad) = self.free_pad() {
                        if ui.button(format!("Hot cue {} here", (b'A' + pad - 1) as char)).clicked() {
                            self.put_cue(pad, frame);
                            done = true;
                        }
                    }
                    if ui.button("Main cue here").clicked() {
                        self.remember();
                        self.cue_point = self.place(frame);
                        done = true;
                    }
                    if ui.button(format!("Loop {} from here", beats_text(self.loop_beats))).clicked() {
                        if let Some(l) = self.auto_loop(frame, self.loop_beats) {
                            ev.set_loop = Some(Some(l));
                            ev.jump = Some(l.0);
                        }
                        done = true;
                    }
                    if ui.button("Drop here").clicked() {
                        self.drop_here(frame / sr as f64);
                        done = true;
                    }
                } else {
                    if ui.button("Bar 1 here").clicked() {
                        let g = bar_one_here(self.grid, frame / sr as f64, sr);
                        self.change(g, Source::Edited);
                        done = true;
                    }
                    if ui.button("A beat exactly here").clicked() {
                        let g = beat_here(self.grid, frame / sr as f64, sr);
                        self.change(g, Source::Edited);
                        done = true;
                    }
                }
            }
        }
        if done {
            self.menu = None;
            ui.close_menu();
        }
    }

    /// Transport: stepping, play, where the playhead is, click, snap and zoom.
    fn transport(&mut self, ui: &mut egui::Ui, ev: &mut PrepEvents, playing: bool) {
        let sr = self.track.sample_rate;
        let head_secs = self.head_frames / sr as f64;
        ui.horizontal(|ui| {
            ui.set_height(32.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            let small = egui::vec2(32.0, 30.0);
            if icon_button(ui, small, Icon::Start, false, "To the start (Home)").clicked() {
                ev.restart = true;
            }
            if icon_button(ui, small, Icon::BackBar, false, "Back a bar (Shift+Left). Ctrl+Left: the section before").clicked() {
                ev.jump = Some(self.step(4.0, false));
            }
            if icon_button(ui, small, Icon::BackBeat, false, "Back a beat (Left)").clicked() {
                ev.jump = Some(self.step(1.0, false));
            }
            let play = if playing { Icon::Pause } else { Icon::Play };
            if icon_button(ui, egui::vec2(58.0, 30.0), play, true, "Play or pause (Space)").clicked() {
                ev.toggle_play = true;
            }
            if icon_button(ui, small, Icon::FwdBeat, false, "On a beat (Right)").clicked() {
                ev.jump = Some(self.step(1.0, true));
            }
            if icon_button(ui, small, Icon::FwdBar, false, "On a bar (Shift+Right). Ctrl+Right: the next section").clicked() {
                ev.jump = Some(self.step(4.0, true));
            }
            ui.add_space(14.0);
            ui.label(RichText::new(format_time_tenths(head_secs)).monospace().size(crate::theme::sz(19.0)).color(palette::text()));
            ui.label(
                RichText::new(format!("/ {}", format_time_tenths(self.track.duration_secs)))
                    .monospace()
                    .size(crate::theme::sz(12.0))
                    .color(palette::text_dim()),
            );
            ui.add_space(12.0);
            if let Some((b, bar)) = beat_info(&self.track, self.grid, self.head_frames as u64) {
                ui.label(RichText::new(format!("bar {bar}")).monospace().size(crate::theme::sz(15.0)).color(palette::deck_a()));
                ui.label(RichText::new(format!("beat {}", b + 1)).monospace().size(crate::theme::sz(12.0)).color(palette::text_dim()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("keys").size(crate::theme::sz(11.0)).color(palette::text_dim()).underline()).on_hover_text(KEYS_HELP);
                ui.add_space(8.0);
                if ui.button("+").on_hover_text("Zoom in (+, or the mouse wheel over the waveform)").clicked() {
                    self.zoom = (self.zoom * 1.4).clamp(ZOOM_MIN, ZOOM_MAX);
                }
                ui.spacing_mut().slider_width = 90.0;
                ui.add(egui::Slider::new(&mut self.zoom, ZOOM_MIN..=ZOOM_MAX).logarithmic(true).show_value(false));
                if ui.button("−").on_hover_text("Zoom out (−)").clicked() {
                    self.zoom = (self.zoom / 1.4).clamp(ZOOM_MIN, ZOOM_MAX);
                }
                ui.label(RichText::new("Zoom").size(crate::theme::sz(11.0)).color(palette::text_dim()));
                ui.add_space(10.0);
                ui.toggle_value(&mut self.quantize, "Snap")
                    .on_hover_text("Cues, loops and clicks on the waveform land on the nearest beat. Hold Alt while dragging a cue to place it freely.");
                ui.add_space(10.0);
                ui.spacing_mut().slider_width = 60.0;
                ui.add_enabled(self.click_on, egui::Slider::new(&mut self.click_level, 0.1..=1.0).show_value(false))
                    .on_hover_text("Click volume");
                ui.toggle_value(&mut self.click_on, "Click")
                    .on_hover_text("A tick on every beat, higher on bar 1, to check the grid by ear (M)");
            });
        });
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (tab, name, tip) in [
                (Tab::Cues, "CUES & LOOPS", "Hot cues, the main cue, loops and drops. Flags on the waveform can be dragged."),
                (Tab::Grid, "GRID", "Fix the beat grid: where the beats are, which beat is bar 1, the tempo. The click is on while this is open."),
            ] {
                let on = self.tab == tab;
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(132.0, 24.0), Sense::click());
                let bg = if on {
                    palette::surface()
                } else if resp.hovered() {
                    palette::raised()
                } else {
                    palette::background()
                };
                let p = ui.painter();
                p.rect_filled(rect, egui::Rounding { nw: 5.0, ne: 5.0, sw: 0.0, se: 0.0 }, bg);
                if on {
                    p.rect_filled(Rect::from_min_size(rect.min, egui::vec2(rect.width(), 2.0)), 0.0, palette::deck_a());
                }
                p.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    name,
                    egui::FontId::proportional(crate::theme::sz(11.5)),
                    if on { palette::text() } else { palette::text_dim() },
                );
                if resp.on_hover_text(format!("{tip} (G switches)")).clicked() {
                    self.set_tab(tab);
                }
            }
        });
    }

    // ── Cues & Loops ──────────────────────────────────────────────────────────

    fn cues_tab(&mut self, ui: &mut egui::Ui, ev: &mut PrepEvents, playing: bool) {
        let w = ui.available_width();
        let (cue_w, loop_w, drop_w, gap) = (138.0, 366.0, 150.0, 8.0);
        let pads_w = (w - cue_w - loop_w - drop_w - 3.0 * gap).max(520.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            card(ui, cue_w, PANEL_H, "MAIN CUE", |ui| self.main_cue_card(ui, ev, playing));
            card(ui, pads_w, PANEL_H, "", |ui| self.pads_card(ui, ev, playing));
            card(ui, loop_w, PANEL_H, "LOOP", |ui| self.loop_card(ui, ev, playing));
            card(ui, drop_w, PANEL_H, "DROPS", |ui| self.drops_card(ui));
        });
    }

    fn main_cue_card(&mut self, ui: &mut egui::Ui, ev: &mut PrepEvents, playing: bool) {
        let sr = self.track.sample_rate;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), Sense::click());
        let held = self.hold.as_ref().is_some_and(|h| h.key == 1000);
        let p = ui.painter();
        p.rect_filled(
            rect,
            5.0,
            if held {
                palette::cue()
            } else {
                mix(palette::surface(), palette::cue(), if resp.hovered() { 0.3 } else { 0.18 })
            },
        );
        p.rect_stroke(rect, 5.0, egui::Stroke::new(1.5_f32, palette::cue()));
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "CUE",
            egui::FontId::proportional(crate::theme::sz(15.0)),
            if held { palette::background() } else { palette::cue() },
        );
        let at = self.cue_point;
        let (here, mouse) = pressed_on(ui, &resp);
        self.hold_step(1000, here, mouse, playing, ev, |_, ev| {
            ev.jump = Some(at);
            Some((at, false))
        });
        resp.on_hover_text(
            "Hold to play from the main cue; let go and it stops and comes back. A tap just goes there.",
        );
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format_time_tenths(at / sr as f64))
                    .monospace()
                    .size(crate::theme::sz(12.0))
                    .color(palette::text()),
            );
            ui.label(
                RichText::new(format!("bar {}", self.bar_text(at)))
                    .monospace()
                    .size(crate::theme::sz(11.0))
                    .color(palette::text_dim()),
            );
        });
        if ui
            .add_sized([ui.available_width(), 22.0], egui::Button::new("Set at playhead"))
            .on_hover_text("The main cue point: where CUE returns to and where the track loads on a deck (C)")
            .clicked()
        {
            self.remember();
            self.cue_point = self.place(self.head_frames);
        }
    }

    fn pads_card(&mut self, ui: &mut egui::Ui, ev: &mut PrepEvents, playing: bool) {
        let sr = self.track.sample_rate;
        let head = self.head_frames;
        let gap = 6.0;
        let roomy = ui.available_width() >= 700.0;
        let pad_w = ((ui.available_width() - 7.0 * gap) / 8.0).floor();
        ui.horizontal(|ui| {
            ui.set_height(18.0);
            ui.label(RichText::new("HOT CUES").size(crate::theme::sz(10.0)).strong().color(palette::text_dim()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Suggest cues").on_hover_text(SUGGEST_HELP).clicked() {
                    self.suggest_cues();
                }
            });
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for i in 0..8u8 {
                let pad = i + 1;
                let cue = self.pad_cue(pad).cloned();
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(pad_w, PAD_H), Sense::click());
                let col = cue.as_ref().map(|c| Color32::from_rgb(c.color[0], c.color[1], c.color[2]));
                let held = self.hold.as_ref().is_some_and(|h| h.key == pad as u32);
                let selected = self.selected == Some(pad) && cue.is_some();
                let bg = match col {
                    Some(c) => mix(palette::surface(), c, if held { 0.75 } else if resp.hovered() { 0.42 } else { 0.28 }),
                    None if resp.hovered() => palette::hover(),
                    None => palette::raised(),
                };
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 5.0, bg);
                if let Some(c) = col {
                    p.rect_filled(
                        Rect::from_min_size(rect.min, egui::vec2(rect.width(), 4.0)),
                        egui::Rounding { nw: 5.0, ne: 5.0, sw: 0.0, se: 0.0 },
                        c,
                    );
                }
                let edge = match (selected, col) {
                    (true, _) => egui::Stroke::new(1.5_f32, palette::text_bright()),
                    (false, Some(c)) => egui::Stroke::new(1.0_f32, c.linear_multiply(0.6)),
                    (false, None) => egui::Stroke::new(1.0_f32, palette::outline()),
                };
                p.rect_stroke(rect, 5.0, edge);
                let letter = ((b'A' + i) as char).to_string();
                let lx = rect.left() + 9.0;
                p.text(
                    egui::pos2(lx, rect.top() + 20.0),
                    egui::Align2::LEFT_CENTER,
                    &letter,
                    egui::FontId::proportional(crate::theme::sz(17.0)),
                    col.map_or(palette::text_dim(), |c| mix(c, palette::text_bright(), 0.45)),
                );
                match &cue {
                    Some(c) => {
                        if !c.label.is_empty() && c.label != letter {
                            p.text(
                                egui::pos2(lx + 20.0, rect.top() + 20.0),
                                egui::Align2::LEFT_CENTER,
                                &c.label,
                                egui::FontId::proportional(crate::theme::sz(12.5)),
                                palette::text(),
                            );
                        }
                        // a narrow pad has room for the bar only
                        let mut line = if pad_w >= 96.0 {
                            format!("{}  {}", self.bar_text(c.pos_frames), format_time_tenths(c.pos_frames / sr as f64))
                        } else {
                            self.bar_text(c.pos_frames)
                        };
                        if let Some(end) = c.out_frames {
                            let beats = (end - c.pos_frames) / beats_to_frames(self.grid, sr, 1.0).max(1.0);
                            line = if pad_w >= 96.0 {
                                format!("{}  loop {}", self.bar_text(c.pos_frames), beats_text(beats as f32))
                            } else {
                                format!("loop {}", beats_text(beats as f32))
                            };
                        }
                        p.text(
                            egui::pos2(lx, rect.top() + 39.0),
                            egui::Align2::LEFT_CENTER,
                            line,
                            egui::FontId::monospace(crate::theme::sz(10.0)),
                            palette::text_mid(),
                        );
                    }
                    None => {
                        p.text(
                            egui::pos2(lx, rect.top() + 39.0),
                            egui::Align2::LEFT_CENTER,
                            "+ set",
                            egui::FontId::proportional(crate::theme::sz(10.5)),
                            palette::text_dim(),
                        );
                    }
                }
                let shift = ui.input(|i| i.modifiers.shift);
                if resp.clicked() && shift && cue.is_some() {
                    self.delete_pad(pad);
                } else if cue.is_none() {
                    if resp.clicked() {
                        self.set_pad(pad, head);
                        self.selected = Some(pad);
                    }
                } else if let Some(c) = cue.clone() {
                    let (here, mouse) = pressed_on(ui, &resp);
                    if here && mouse && !shift {
                        self.selected = Some(pad);
                    }
                    // a set pad plays from its cue (or its loop) while held; release stops and returns
                    self.hold_step(pad as u32, here && !shift, mouse, playing, ev, |me, ev| {
                        ev.jump = Some(c.pos_frames);
                        if let Some(end) = c.out_frames {
                            me.loop_in = None;
                            me.loop_region = Some(LoopRegion { start: c.pos_frames, end, active: true });
                            ev.set_loop = Some(Some((c.pos_frames, end)));
                        }
                        Some((c.pos_frames, c.out_frames.is_some()))
                    });
                }
                if cue.is_some() {
                    resp.context_menu(|ui| {
                        ui.set_min_width(150.0);
                        if ui.button("Move to the playhead").clicked() {
                            self.remember();
                            self.move_mark(Mark::Cue(pad), head, !self.quantize, ev);
                            ui.close_menu();
                        }
                        if ui.button("Delete").clicked() {
                            self.delete_pad(pad);
                            ui.close_menu();
                        }
                    });
                }
                resp.on_hover_text(match &cue {
                    Some(c) => format!(
                        "Hot cue {letter}{}: tap to go there, hold to hear it{}. Right-click for more, Shift+click deletes. Key {pad}.",
                        if c.label.is_empty() { String::new() } else { format!(" ({})", c.label) },
                        if c.is_loop() { " looping" } else { "" }
                    ),
                    None => format!("Hot cue {letter} is empty: click to put it at the playhead, or to keep the running loop here. Key {pad}."),
                });
            }
        });
        ui.add_space(2.0);
        // the selected pad's fine controls, or how the pads work
        let selected = self.selected.filter(|p| self.pad_cue(*p).is_some());
        ui.horizontal(|ui| {
            ui.set_height(24.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            let Some(pad) = selected else {
                ui.label(
                    RichText::new("Click an empty pad to put a cue at the playhead. Tap a pad to go to it, hold to hear it; drag its flag on the waveform to move it.")
                        .size(crate::theme::sz(11.0))
                        .color(palette::text_dim()),
                );
                return;
            };
            let letter = (b'A' + pad - 1) as char;
            let cue = self.pad_cue(pad).cloned().unwrap();
            let col = Color32::from_rgb(cue.color[0], cue.color[1], cue.color[2]);
            ui.label(RichText::new(letter.to_string()).strong().size(crate::theme::sz(13.0)).color(mix(col, palette::text_bright(), 0.4)));
            let mut label = cue.label.clone();
            let edit = ui.add(egui::TextEdit::singleline(&mut label).desired_width(92.0).hint_text("name").char_limit(14));
            if edit.gained_focus() {
                self.remember();
            }
            if edit.changed() {
                if let Some(c) = self.cues.iter_mut().find(|c| c.hot_cue == Some(pad)) {
                    c.label = label;
                }
            }
            ui.add_space(6.0);
            let beat = beats_to_frames(self.grid, sr, 1.0);
            let ms10 = sr as f64 * 0.010;
            for (text, by, tip, fine) in [
                ("−beat", -beat, "A beat earlier", false),
                ("−10 ms", -ms10, "10 ms earlier", true),
                ("+10 ms", ms10, "10 ms later", true),
                ("+beat", beat, "A beat later", false),
            ] {
                if fine && !roomy {
                    continue;
                }
                if ui.add_enabled(by != 0.0, egui::Button::new(text)).on_hover_text(tip).clicked() {
                    self.shift_pad(pad, by);
                }
            }
            if ui.button("To playhead").on_hover_text("Move this cue to where the playhead is").clicked() {
                self.remember();
                self.move_mark(Mark::Cue(pad), head, !self.quantize, ev);
            }
            ui.add_space(6.0);
            ui.spacing_mut().item_spacing.x = 3.0;
            for rgb in PAD_COLORS {
                let (r, resp) = ui.allocate_exact_size(egui::vec2(15.0, 15.0), Sense::click());
                let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                ui.painter().rect_filled(r, 3.0, c);
                if rgb == cue.color {
                    ui.painter().rect_stroke(r.expand(1.5), 4.0, egui::Stroke::new(1.5_f32, palette::text_bright()));
                }
                if resp.on_hover_text("Colour").clicked() && rgb != cue.color {
                    self.remember();
                    if let Some(c) = self.cues.iter_mut().find(|c| c.hot_cue == Some(pad)) {
                        c.color = rgb;
                    }
                }
            }
            ui.add_space(6.0);
            if ui.button("Delete").on_hover_text("Shift+click the pad does the same").clicked() {
                self.delete_pad(pad);
            }
        });
    }

    /// The running or last loop's length in beats.
    fn loop_len_beats(&self) -> Option<f32> {
        let beat = beats_to_frames(self.grid, self.track.sample_rate, 1.0);
        self.loop_region.filter(|_| beat > 0.0).map(|r| ((r.end - r.start) / beat) as f32)
    }

    fn loop_card(&mut self, ui: &mut egui::Ui, ev: &mut PrepEvents, playing: bool) {
        let head = self.head_frames;
        let active = matches!(self.loop_region, Some(r) if r.active);
        let has = self.loop_region.is_some();
        let len = self.loop_len_beats();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui
                .selectable_label(self.loop_in.is_some(), " IN ")
                .on_hover_text("Loop in at the playhead (I)")
                .clicked()
            {
                self.loop_in_here();
            }
            if ui
                .add_enabled(self.loop_in.is_some(), egui::Button::new("OUT"))
                .on_hover_text("Loop out at the playhead; the loop starts (O)")
                .on_disabled_hover_text("Press IN first, then OUT where the loop should end (O)")
                .clicked()
            {
                self.loop_out_here(ev);
            }
            ui.add_space(6.0);
            if ui.add_enabled(has, egui::Button::new("½")).on_hover_text("Half as long ([)").clicked() {
                self.scale_loop(0.5, ev);
            }
            if ui.add_enabled(has, egui::Button::new("2×")).on_hover_text("Twice as long (])").clicked() {
                self.scale_loop(2.0, ev);
            }
            ui.add_space(6.0);
            if ui.add_enabled(has, egui::Button::new("−beat")).on_hover_text("Move the loop a beat earlier").clicked()
            {
                self.move_loop(-1.0, ev);
            }
            if ui.add_enabled(has, egui::Button::new("+beat")).on_hover_text("Move the loop a beat later").clicked() {
                self.move_loop(1.0, ev);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let text = if active { "Loop on" } else { "Loop off" };
                let b = if active {
                    egui::Button::new(RichText::new(text).color(palette::background()).strong())
                        .fill(palette::loop_on())
                } else {
                    egui::Button::new(text)
                };
                if ui.add(b).on_hover_text("Leave the loop, or start it again (L)").clicked() {
                    self.toggle_loop(ev);
                }
            });
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let w = ((ui.available_width() - 7.0 * 3.0) / 8.0).floor();
            for &b in LOOP_SIZES.iter() {
                let on = len.is_some_and(|l| (l - b).abs() < 0.01);
                let r = ui
                    .add_sized([w, 22.0], egui::SelectableLabel::new(on, beats_text(b)))
                    .on_hover_text("Loop this many beats from the playhead. Hold to hear it: it plays while held and stops when you let go; a tap sets the loop.");
                let (here, mouse) = pressed_on(ui, &r);
                self.hold_step(2000 + (b * 4.0) as u32, here, mouse, playing, ev, |me, ev| {
                    me.loop_beats = b;
                    let (start, end) = me.auto_loop(head, b)?;
                    ev.set_loop = Some(Some((start, end)));
                    Some((start, true))
                });
            }
        });
        let text = match (self.loop_region, len) {
            (Some(r), Some(l)) => {
                format!("{} beats from bar {}. Click an empty pad to keep it.", beats_text(l), self.bar_text(r.start))
            }
            _ if self.loop_in.is_some() => "IN is set: press OUT where the loop ends.".to_owned(),
            _ => "Pick a length in beats, or IN then OUT.".to_owned(),
        };
        ui.label(RichText::new(text).size(crate::theme::sz(11.0)).color(palette::text_dim()));
    }

    fn drops_card(&mut self, ui: &mut egui::Ui) {
        let head_secs = self.head_frames / self.track.sample_rate as f64;
        let w = ui.available_width();
        if ui
            .add_sized([w, 22.0], egui::Button::new("Drop here"))
            .on_hover_text("Put a drop on the bar line nearest the playhead. A drop within four bars of it is moved there. Drops you set are kept when the track is analysed again. You can also drag a DROP flag on the waveform.")
            .clicked()
        {
            self.drop_here(head_secs);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui
                .button("Remove")
                .on_hover_text("Take away the drop the playhead is in, or the nearest one within four bars")
                .clicked()
            {
                self.remove_drop(head_secs);
            }
            if ui
                .add_enabled(self.drops.is_some(), egui::Button::new("As found"))
                .on_hover_text("Forget the drops you set and use the ones found in the audio")
                .clicked()
            {
                self.found_drops();
            }
        });
        let n = self.current_drops().len();
        let whose = if self.drops.is_some() { "yours" } else { "as found" };
        ui.label(
            RichText::new(format!("{n} drop{}, {whose}", if n == 1 { "" } else { "s" }))
                .size(crate::theme::sz(11.0))
                .color(palette::text_dim()),
        );
    }

    // ── Grid ──────────────────────────────────────────────────────────────────

    fn grid_tab(&mut self, ui: &mut egui::Ui, playing: bool) {
        let w = ui.available_width();
        let (align_w, bar_w, tempo_w, gap) = (300.0, 200.0, 250.0, 8.0);
        let tap_w = (w - align_w - bar_w - tempo_w - 3.0 * gap).max(380.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            card(ui, align_w, PANEL_H, "BEATS", |ui| self.align_card(ui));
            card(ui, bar_w, PANEL_H, "BAR 1", |ui| self.bar_card(ui));
            card(ui, tempo_w, PANEL_H, "TEMPO", |ui| self.tempo_card(ui));
            card(ui, tap_w, PANEL_H, "TAP THE TEMPO", |ui| self.tap_card(ui, playing));
        });
    }

    fn align_card(&mut self, ui: &mut egui::Ui) {
        let sr = self.track.sample_rate;
        let head_secs = self.head_frames / sr as f64;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new("nudge").size(crate::theme::sz(11.0)).color(palette::text_dim()));
            for (label, ms) in [("−10", -10.0), ("−1", -1.0), ("+1", 1.0), ("+10", 10.0)] {
                if ui.button(label).on_hover_text("Move every beat by this many milliseconds").clicked() {
                    let g = shifted(self.grid, ms, sr);
                    self.change(g, Source::Edited);
                }
            }
            ui.label(RichText::new("ms").size(crate::theme::sz(11.0)).color(palette::text_dim()));
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui
                .button("Snap to the kicks")
                .on_hover_text("Move the grid onto the strongest kicks in the audio (up to 80 ms)")
                .clicked()
            {
                self.snap();
            }
            if ui
                .button("Beat at playhead")
                .on_hover_text("Move the whole grid so a beat lands exactly on the playhead")
                .clicked()
            {
                let g = beat_here(self.grid, head_secs, sr);
                self.change(g, Source::Edited);
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.grid != self.analysed, egui::Button::new("Back to detected"))
                .on_hover_text("The grid analysis found")
                .clicked()
            {
                let a = self.analysed;
                self.change(a, Source::Detected);
            }
        });
    }

    fn bar_card(&mut self, ui: &mut egui::Ui) {
        let sr = self.track.sample_rate;
        let head_secs = self.head_frames / sr as f64;
        if ui
            .add_sized([ui.available_width(), 22.0], egui::Button::new("Bar 1 at playhead"))
            .on_hover_text("Make the beat nearest the playhead the first beat of a bar. The beats do not move.")
            .clicked()
        {
            let g = bar_one_here(self.grid, head_secs, sr);
            self.change(g, Source::Edited);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let period = 60.0 / self.grid.bpm.max(1.0) as f64;
            for (text, by, tip) in [("−1 beat", -1.0, "Bar 1 a beat earlier"), ("+1 beat", 1.0, "Bar 1 a beat later")]
            {
                if ui.add_enabled(self.grid.is_valid(), egui::Button::new(text)).on_hover_text(tip).clicked() {
                    let g = bar_one_here(self.grid, downbeat_secs(self.grid, sr) + by * period, sr);
                    self.change(g, Source::Edited);
                }
            }
        });
        ui.label(
            RichText::new("The click is higher on bar 1.").size(crate::theme::sz(11.0)).color(palette::text_dim()),
        );
    }

    fn tempo_card(&mut self, ui: &mut egui::Ui) {
        let sr = self.track.sample_rate;
        let head_secs = self.head_frames / sr as f64;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let mut bpm = self.grid.bpm;
            let r = ui.add(egui::DragValue::new(&mut bpm).range(40.0..=250.0).speed(0.01).fixed_decimals(2).suffix(" BPM"));
            if r.drag_started() || r.gained_focus() {
                self.remember();
            }
            if r.changed() && bpm != self.grid.bpm {
                self.grid = retempo(self.grid, bpm, head_secs, sr);
                self.source = Source::Edited;
            }
            r.on_hover_text("Drag, or double-click to type. The beat at the playhead stays where it is, so line up a beat early in the track, go near the end, and adjust until the beats there fit too.");
            for (text, by) in [("−0.01", -0.01f32), ("+0.01", 0.01)] {
                if ui.button(text).clicked() {
                    let g = retempo(self.grid, self.grid.bpm + by, head_secs, sr);
                    self.change(g, Source::Edited);
                }
            }
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui.button("½ tempo").clicked() {
                let g = scaled(self.grid, 0.5);
                self.change(g, Source::Edited);
            }
            if ui.button("2× tempo").clicked() {
                let g = scaled(self.grid, 2.0);
                self.change(g, Source::Edited);
            }
            if ui.button("Round").on_hover_text("To the nearest whole BPM").clicked() {
                let g = retempo(self.grid, self.grid.bpm.round(), head_secs, sr);
                self.change(g, Source::Edited);
            }
        });
        ui.label(
            RichText::new("The beat at the playhead stays put.")
                .size(crate::theme::sz(11.0))
                .color(palette::text_dim()),
        );
    }

    fn tap_card(&mut self, ui: &mut egui::Ui, playing: bool) {
        let head_secs = self.head_frames / self.track.sample_rate as f64;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let b = ui.add_enabled(
                playing,
                egui::Button::new(RichText::new("TAP").size(crate::theme::sz(14.0)).strong())
                    .min_size(egui::vec2(84.0, 30.0)),
            );
            if b.clicked() {
                self.tap(head_secs);
            }
            b.on_hover_text("Play the track and tap on every beat for 20 to 30 seconds (T)")
                .on_disabled_hover_text("Play the track first, then tap on every beat (T)");
            let ok = matches!(self.fit, Some(Ok(_)));
            let use_it = if ok { primary("Use these taps") } else { egui::Button::new("Use these taps") };
            if ui.add_enabled(ok, use_it).clicked() {
                self.apply_taps();
            }
            if ui.add_enabled(!self.taps.is_empty(), egui::Button::new("Clear")).clicked() {
                self.taps.clear();
                self.fit = None;
            }
        });
        let line = match &self.fit {
            Some(Ok(f)) => format!(
                "{} taps: {:.2} BPM (±{:.2}){}, spread {:.0} ms",
                f.taps_used,
                f.bpm,
                f.bpm_error,
                if f.whole_bpm { ", whole number" } else { "" },
                f.rms_ms
            ),
            Some(Err(e)) => format!("{} taps: {e}", self.taps.len()),
            None if self.taps.is_empty() => "No taps yet".to_owned(),
            None => format!("{} tap", self.taps.len()),
        };
        ui.label(RichText::new(line).monospace().size(crate::theme::sz(11.0)).color(palette::text()));
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.snap_taps, "then snap to the kicks")
                .on_hover_text("After the taps set the tempo, put the beats exactly on the kicks in the audio");
            ui.label(RichText::new("tap delay").size(crate::theme::sz(11.0)).color(palette::text_dim()));
            if ui
                .add(egui::DragValue::new(&mut self.tap_delay_ms).range(0.0..=300.0).suffix(" ms").speed(1.0))
                .on_hover_text("How late your taps are, if you know: it is taken off")
                .changed()
            {
                self.refit();
            }
        });
    }

    /// The whole screen. `deck` is the prepare player.
    pub fn show(&mut self, ui: &mut egui::Ui, deck: &DeckHandle, status: &PrepStatus) -> PrepEvents {
        let mut ev = PrepEvents::default();
        // a hold whose button was let go of while no widget saw it (a screen change) must not stay stuck
        if self.hold.is_some() && !ui.input(|i| i.pointer.primary_down()) {
            self.release_hold(&mut ev);
        }
        style(ui);
        self.poll_curves();
        let sr = self.track.sample_rate;
        let playing = deck.shared.is_playing.load(std::sync::atomic::Ordering::Relaxed);
        let observed = deck.shared.position_frames.load(std::sync::atomic::Ordering::Relaxed) as f64;
        self.head_frames =
            self.playhead.update(observed, sr as f64, playing, std::time::Instant::now(), 0.06 * sr as f64);
        let progress = (self.head_frames / self.track.duration_frames.max(1) as f64).clamp(0.0, 1.0);

        // the waveform takes the height the rest of the screen leaves, within reason
        let room = ui.clip_rect().bottom() - ui.cursor().top();
        let wave_h = (room - REST_H).clamp(WAVE_MIN_H, WAVE_MAX_H);

        self.keys(ui, &mut ev, playing);
        self.header(ui, &mut ev);
        self.notes(ui, status);
        ui.add_space(2.0);
        self.waveform(ui, &mut ev, progress, wave_h);

        // the whole track, with its sections; click or drag to go anywhere
        ui.add_space(-2.0);
        let w = ui.available_width();
        let marks =
            Marks { cue_point: self.cue_point, cues: &self.cues, loop_region: self.loop_region, loop_in: self.loop_in };
        let (ov_rect, ov_resp) = ui.allocate_exact_size(egui::vec2(w, OVERVIEW_H), Sense::click_and_drag());
        draw_overview(
            &ui.painter_at(ov_rect),
            ov_rect,
            &self.track,
            self.grid,
            &marks,
            progress,
            palette::deck_a(),
            None,
        );
        if let Some(pos) = ov_resp.hover_pos() {
            if let Some(text) =
                crate::sections::describe(&self.track, self.grid, ((pos.x - ov_rect.left()) / ov_rect.width()) as f64)
            {
                ov_resp.clone().on_hover_text(text);
            }
        }
        if ov_resp.clicked() || ov_resp.dragged() {
            if let Some(p) = ov_resp.interact_pointer_pos() {
                ev.seek = Some(((p.x - ov_rect.left()) / ov_rect.width()).clamp(0.0, 1.0) as f64);
            }
        }
        ui.add_space(2.0);
        self.transport(ui, &mut ev, playing);
        ui.add_space(2.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        self.tabs(ui);
        ui.spacing_mut().item_spacing.y = 5.0;
        match self.tab {
            Tab::Cues => self.cues_tab(ui, &mut ev, playing),
            Tab::Grid => self.grid_tab(ui, playing),
        }
        ev
    }
}

// ── Look ──────────────────────────────────────────────────────────────────────

const RULER_H: f32 = 16.0;
const WAVE_MIN_H: f32 = 90.0;
const WAVE_MAX_H: f32 = 240.0;
/// The height of everything on the screen but the waveform.
const REST_H: f32 = 318.0;
/// The least height the screen needs to show everything without scrolling.
pub const MIN_HEIGHT: f32 = WAVE_MIN_H + REST_H + 8.0;
const OVERVIEW_H: f32 = 40.0;
const PANEL_H: f32 = 122.0;
const PAD_H: f32 = 52.0;
const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 8.0;

const SUGGEST_HELP: &str = "Fill the empty pads from the track's sections: A start, B build, C first drop, D first break, E second drop, F outro. A part the track does not have is skipped, and a pad that already holds a cue is left alone. Undo puts them back.";

const KEYS_HELP: &str = "Space  play or pause
Left, Right  one beat        Shift: one bar        Ctrl: one section
Home  to the start
1 to 8  pad A to H: set it if empty, go to it if set        Shift: delete it
C  main cue at the playhead
I, O  loop in, loop out        L  loop on or off        [ ]  half, double
+ −  zoom        M  click        G  Cues or Grid        T  tap the tempo
Ctrl+Z  undo        Ctrl+S  save        Esc  close

On the waveform: drag to scrub, click to go there, wheel to zoom, right-click for a menu.
Cues tab: drag a cue flag, a loop edge or a DROP flag (Alt: off the beat); double-click puts a cue on the next free pad.
Grid tab: Shift+drag slides the grid; double-click a beat to make it bar 1.";

/// One colour part of the way to another.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// A coloured dot and a word: a verdict in the header.
fn badge(ui: &mut egui::Ui, text: &str, col: Color32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), egui::FontId::proportional(crate::theme::sz(11.5)), col);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + egui::vec2(13.0, 2.0), Sense::hover());
    ui.painter().circle_filled(egui::pos2(rect.left() + 4.0, rect.center().y), 3.5, col);
    ui.painter().galley(egui::pos2(rect.left() + 12.0, rect.top() + 1.0), galley, col);
    resp
}

/// The button for the thing to do next.
fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(palette::background()).strong()).fill(palette::deck_a())
}

/// "1/4", "1/2", "4": a number of beats as a loop size.
fn beats_text(b: f32) -> String {
    if b > 0.0 && b < 1.0 {
        format!("1/{}", (1.0 / b).round() as u32)
    } else if (b - b.round()).abs() < 0.01 {
        format!("{}", b.round() as u32)
    } else {
        format!("{b:.1}")
    }
}

/// Buttons and toggles for this screen: flat, rounded, a little larger than the default.
fn style(ui: &mut egui::Ui) {
    let st = ui.style_mut();
    st.spacing.button_padding = egui::vec2(8.0, 3.0);
    st.spacing.item_spacing = egui::vec2(6.0, 5.0);
    st.spacing.interact_size.y = 22.0;
    let v = &mut st.visuals;
    let round = egui::Rounding::same(4.0);
    for (w, fill, edge) in [
        (&mut v.widgets.inactive, palette::hover(), palette::outline()),
        (&mut v.widgets.hovered, palette::pressed(), palette::tick()),
        (&mut v.widgets.active, palette::handle_active(), palette::deck_a()),
    ] {
        w.rounding = round;
        w.weak_bg_fill = fill;
        w.bg_fill = fill;
        w.bg_stroke = egui::Stroke::new(1.0_f32, edge);
        w.fg_stroke.color = palette::text();
    }
    v.widgets.noninteractive.rounding = round;
    v.selection.bg_fill = mix(palette::surface(), palette::deck_a(), 0.45);
    v.selection.stroke = egui::Stroke::new(1.0_f32, palette::text_bright());
}

/// A titled box of tools, `w` by `h`.
fn card<R>(ui: &mut egui::Ui, w: f32, h: f32, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), Sense::hover());
    ui.painter().rect_filled(rect, 6.0, palette::surface());
    ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(1.0_f32, palette::divider()));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(10.0, 7.0)))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(rect);
    if !title.is_empty() {
        child.label(RichText::new(title).size(crate::theme::sz(10.0)).strong().color(palette::text_dim()));
    }
    add(&mut child)
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Start,
    BackBar,
    BackBeat,
    Play,
    Pause,
    FwdBeat,
    FwdBar,
}

/// A transport button drawn from shapes (the font has no reliable arrows). `main` is the play button.
fn icon_button(ui: &mut egui::Ui, size: egui::Vec2, icon: Icon, main: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let down = resp.is_pointer_button_down_on();
    let bg = match (main, down, resp.hovered()) {
        (true, true, _) => mix(palette::deck_a(), palette::text_bright(), 0.3),
        (true, _, true) => mix(palette::deck_a(), palette::text_bright(), 0.15),
        (true, _, _) => palette::deck_a(),
        (false, true, _) => palette::handle_active(),
        (false, _, true) => palette::pressed(),
        (false, _, _) => palette::hover(),
    };
    let p = ui.painter();
    p.rect_filled(rect, 5.0, bg);
    let col = if main { palette::background() } else { palette::text() };
    let c = rect.center();
    let s = if main { 7.0 } else { 5.5 };
    // a triangle pointing right (dir 1) or left (dir -1), centred on cx
    let tri = |cx: f32, dir: f32| {
        egui::Shape::convex_polygon(
            vec![
                egui::pos2(cx - dir * s * 0.65, c.y - s),
                egui::pos2(cx + dir * s * 0.75, c.y),
                egui::pos2(cx - dir * s * 0.65, c.y + s),
            ],
            col,
            egui::Stroke::NONE,
        )
    };
    let bar = |cx: f32, w: f32| {
        egui::Shape::rect_filled(Rect::from_center_size(egui::pos2(cx, c.y), egui::vec2(w, 2.0 * s)), 0.5, col)
    };
    let shapes = match icon {
        Icon::Start => vec![bar(c.x - 5.5, 2.5), tri(c.x + 2.0, -1.0)],
        Icon::BackBar => vec![tri(c.x - 4.0, -1.0), tri(c.x + 4.5, -1.0)],
        Icon::BackBeat => vec![tri(c.x - 0.5, -1.0)],
        Icon::Play => vec![tri(c.x + 0.5, 1.0)],
        Icon::Pause => vec![bar(c.x - 4.0, 4.0), bar(c.x + 4.0, 4.0)],
        Icon::FwdBeat => vec![tri(c.x + 0.5, 1.0)],
        Icon::FwdBar => vec![tri(c.x - 4.5, 1.0), tri(c.x + 4.0, 1.0)],
    };
    p.extend(shapes);
    resp.on_hover_text(tip)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44100;
    fn grid(bpm: f32, down_secs: f64) -> BeatGrid {
        BeatGrid { bpm, offset: secs_to_chunks(down_secs, SR) }
    }
    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn nudging_moves_the_downbeat_by_the_milliseconds_asked() {
        let g = shifted(grid(174.0, 2.0), 10.0, SR);
        assert!((downbeat_secs(g, SR) - 2.010).abs() < 1e-9);
        let g = shifted(g, -1.0, SR);
        assert!((downbeat_secs(g, SR) - 2.009).abs() < 1e-9);
    }

    #[test]
    fn a_shift_before_the_start_keeps_the_bar_numbering() {
        let g0 = grid(120.0, 0.001); // beats every 0.5 s, bars every 2 s
        let g = shifted(g0, -5.0, SR);
        let d = downbeat_secs(g, SR);
        assert!((0.0..2.0).contains(&d), "{d}");
        // Same bar position as the unwrapped grid: a whole number of bars from it.
        let bars = (d - (0.001 - 0.005)) / 2.0;
        assert!((bars - bars.round()).abs() < 1e-6, "{bars}");
    }

    #[test]
    fn bar_one_here_picks_the_nearest_beat_and_leaves_the_beats_alone() {
        let g = bar_one_here(grid(120.0, 1.0), 3.6, SR); // beats at 1.0, 1.5 ... 3.5, 4.0
        assert!(near(downbeat_secs(g, SR), 3.5) || (downbeat_secs(g, SR) - 3.5).abs() < 1e-3);
        // The same beat positions: the new downbeat is a whole number of beats from the old one.
        let beats = (downbeat_secs(g, SR) - 1.0) / 0.5;
        assert!((beats - beats.round()).abs() < 1e-6);
    }

    #[test]
    fn beat_here_puts_a_beat_on_the_playhead() {
        let g = beat_here(grid(128.0, 0.0), 12.345, SR);
        assert!((downbeat_secs(g, SR) - 12.345).abs() < 1e-9);
    }

    #[test]
    fn a_tap_fit_keeps_bar_lines_on_the_same_beats() {
        // Old grid: 174 BPM, downbeat at 1.000 s. The taps say 174 with beats 25 ms later, tapped well into the song.
        let old = grid(174.0, 1.0);
        let p = 60.0 / 174.0;
        let fit = TapFit::from_grid(174.0, 1.025 + 40.0 * p);
        let g = from_fit(&fit, old, SR);
        assert_eq!(g.bpm, 174.0);
        let d = downbeat_secs(g, SR);
        assert!((d - 1.025).abs() < 1e-6, "downbeat {d}");
    }

    #[test]
    fn a_tempo_change_from_taps_still_picks_the_nearest_old_downbeat() {
        let old = grid(87.0, 2.0);
        let fit = TapFit::from_grid(174.0, 2.01 + 100.0 * (60.0 / 174.0));
        let g = from_fit(&fit, old, SR);
        assert_eq!(g.bpm, 174.0);
        assert!((downbeat_secs(g, SR) - 2.01).abs() < 1e-6);
    }

    #[test]
    fn snapping_pulls_a_late_grid_onto_the_kicks() {
        let frame = WAVEFORM_CHUNK_FRAMES as f64 / SR as f64;
        let mut curve = vec![0.0f32; (40.0 / frame) as usize];
        let p = 60.0 / 128.0;
        let mut k = 0;
        loop {
            let i = ((1.0 + k as f64 * p) / frame).round() as usize;
            if i + 1 >= curve.len() {
                break;
            }
            curve[i] = 1.0;
            k += 1;
        }
        let g = snapped(grid(128.0, 1.040), &curve, frame, SR);
        let off = downbeat_secs(g, SR) - 1.0;
        let off = off.rem_euclid(p);
        assert!(off.min(p - off) < 0.004, "{off}");
    }

    #[test]
    fn halving_and_doubling_keep_the_downbeat() {
        let g = grid(87.0, 3.0);
        let d = scaled(g, 2.0);
        assert_eq!(d.bpm, 174.0);
        assert_eq!(d.offset, g.offset);
        assert_eq!(scaled(d, 0.5).bpm, 87.0);
    }

    #[test]
    fn cues_snap_to_the_nearest_beat_and_stay_in_the_track() {
        let g = grid(120.0, 0.0); // a beat every 22050 frames
        assert!(near(snap_frame(g, SR, 1_000_000, 22_050.0 * 3.0 + 5_000.0), 22_050.0 * 3.0));
        assert!(near(snap_frame(g, SR, 1_000_000, 22_050.0 * 3.0 + 12_000.0), 22_050.0 * 4.0));
        assert!(snap_frame(g, SR, 50_000, 49_000.0) <= 49_999.0);
        assert!(near(snap_frame(BeatGrid { bpm: 0.0, offset: 0.0 }, SR, 1_000_000, 1234.5), 1234.5));
    }

    #[test]
    fn loop_lengths_follow_the_tempo() {
        let g = grid(120.0, 0.0);
        assert!(near(beats_to_frames(g, SR, 4.0), 88_200.0));
        assert!(near(beats_to_frames(g, SR, 0.25), 5_512.5));
        assert!(near(beats_to_frames(BeatGrid { bpm: 0.0, offset: 0.0 }, SR, 4.0), 0.0));
    }

    #[test]
    fn cue_sets_compare_by_pad_position_name_and_colour_whatever_their_order() {
        let a = vec![
            Cue::new(Some(1), 1000.0, None, SR, [1, 2, 3]),
            Cue::new(Some(2), 2000.0, Some(3000.0), SR, [1, 2, 3]),
        ];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(cue_key(&a), cue_key(&b));
        b[0].out_frames = Some(3500.0);
        assert_ne!(cue_key(&a), cue_key(&b));
        let mut c = a.clone();
        c[0].color = [9, 9, 9];
        assert_ne!(cue_key(&a), cue_key(&c), "a new colour is a change to save");
        let mut d = a.clone();
        d[1].label = "Drop".into();
        assert_ne!(cue_key(&a), cue_key(&d), "so is a new name");
    }

    #[test]
    fn stepping_goes_to_the_grid_line_before_or_after_the_playhead() {
        let g = grid(120.0, 1.0); // a beat every 22050 frames from 44100
        let beat = 22_050.0;
        let on = 44_100.0 + 5.0 * beat;
        assert!(near(step_frame(g, SR, on, 1.0, true), on + beat));
        assert!(near(step_frame(g, SR, on, 1.0, false), on - beat));
        // part of the way through a beat: back goes to its start, on to the next
        assert!(near(step_frame(g, SR, on + 0.3 * beat, 1.0, false), on));
        assert!(near(step_frame(g, SR, on + 0.3 * beat, 1.0, true), on + beat));
        // a playhead a hair past a line is on it, not after it
        assert!(near(step_frame(g, SR, on + 50.0, 1.0, false), on - beat));
        // bars
        let bar_one = 44_100.0;
        assert!(near(step_frame(g, SR, bar_one + 5.0 * beat, 4.0, false), bar_one + 4.0 * beat));
        assert!(near(step_frame(g, SR, bar_one + 5.0 * beat, 4.0, true), bar_one + 8.0 * beat));
    }

    #[test]
    fn a_new_tempo_keeps_the_beat_at_the_playhead_and_the_bars_on_their_beats() {
        let g = grid(120.0, 1.0);
        // the playhead is on beat 40 (bar 11 beat 1), at 21 s
        let n = retempo(g, 121.0, 21.02, SR);
        assert_eq!(n.bpm, 121.0);
        let p = 60.0 / 121.0;
        let beats = (21.0 - downbeat_secs(n, SR)) / p;
        assert!((beats - beats.round()).abs() < 1e-6, "a beat is still at 21 s: {beats}");
        assert_eq!(beats.round() as i64 % 4, 0, "and it is still the first beat of a bar");
    }

    #[test]
    fn the_waveform_maps_frames_to_pixels_and_back() {
        let v = View { left: 10.0, half_w: 500.0, head_chunks: 1000.0, zoom: 2.0 };
        assert_eq!(v.x_of(1000.0 * CHUNK), 510.0, "the playhead is in the middle");
        assert_eq!(v.x_of(1010.0 * CHUNK), 530.0);
        assert!((v.frame_at(v.x_of(123_456.0)) - 123_456.0).abs() < 1.0);
    }

    #[test]
    fn dragging_a_cue_keeps_its_name_and_a_hot_loop_keeps_its_length() {
        let mut p = prep_with_track();
        let mut c = Cue::new(Some(3), 22_050.0 * 4.0, Some(22_050.0 * 8.0), SR, [1, 2, 3]);
        c.label = "Drop".into();
        p.cues.push(c);
        let mut ev = PrepEvents::default();
        p.move_mark(Mark::Cue(3), 22_050.0 * 10.0 + 3_000.0, false, &mut ev);
        let c = p.pad_cue(3).unwrap();
        assert!(near(c.pos_frames, 22_050.0 * 10.0), "on the beat");
        assert!(near(c.out_frames.unwrap(), 22_050.0 * 14.0));
        assert_eq!((c.label.as_str(), c.time_ms), ("Drop", 5000));
        p.move_mark(Mark::Cue(3), 22_050.0 * 10.0 + 3_000.0, true, &mut ev);
        assert!(near(p.pad_cue(3).unwrap().pos_frames, 22_050.0 * 10.0 + 3_000.0), "Alt: exactly there");
        // the out point cannot cross the in point
        p.move_mark(Mark::CueEnd(3), 0.0, false, &mut ev);
        let c = p.pad_cue(3).unwrap();
        assert!(c.out_frames.unwrap() > c.pos_frames);
    }

    #[test]
    fn a_loop_halves_doubles_and_moves_by_beats() {
        let mut p = prep_with_track();
        let mut ev = PrepEvents::default();
        p.auto_loop(22_050.0 * 8.0, 4.0).unwrap();
        p.scale_loop(0.5, &mut ev);
        let r = p.loop_region.unwrap();
        assert!(near(r.end - r.start, 22_050.0 * 2.0));
        assert_eq!(ev.set_loop, Some(Some((r.start, r.end))), "a running loop is told");
        p.scale_loop(2.0, &mut ev);
        p.move_loop(-1.0, &mut ev);
        let r = p.loop_region.unwrap();
        assert!(near(r.start, 22_050.0 * 7.0) && near(r.end, 22_050.0 * 11.0));
        p.toggle_loop(&mut ev);
        assert_eq!(ev.set_loop, Some(None));
        assert!(!p.loop_region.unwrap().active);
    }

    fn prep_with_track() -> Prepare {
        let track = Arc::new(DecodedTrack {
            loudness: None,
            samples: vec![0.0; 44100 * 2 * 30],
            sample_rate: SR,
            channels: 2,
            duration_frames: 44100 * 30,
            waveform: vec![],
            wave: Default::default(),
            title: "t".into(),
            duration_secs: 30.0,
            bpm: 120.0,
            bpm_coarse: 12000,
            beat_offset: 0.0,
            beat_times_ms: vec![],
            key: None,
            cues: vec![],
        });
        Prepare::new(PathBuf::from("t.wav"), track, None, vec![], None, None)
    }

    #[test]
    fn a_drop_put_near_another_moves_it_and_one_put_far_away_is_added() {
        let drops = [30.0, 120.0];
        assert_eq!(with_drop_at(&drops, 34.0, 8.0), vec![34.0, 120.0]);
        assert_eq!(with_drop_at(&drops, 60.0, 8.0), vec![30.0, 60.0, 120.0]);
        assert_eq!(with_drop_at(&[], 5.0, 8.0), vec![5.0]);
    }

    #[test]
    fn removing_takes_the_drop_the_playhead_is_in_else_the_nearest_close_one() {
        let drops = [30.0, 120.0];
        assert_eq!(without_drop_at(&drops, 70.0, 8.0, true), Some(vec![120.0]), "well inside the first drop");
        assert_eq!(without_drop_at(&drops, 115.0, 8.0, false), Some(vec![30.0]), "just before the second");
        assert_eq!(without_drop_at(&drops, 70.0, 8.0, false), None);
    }

    #[test]
    fn setting_a_drop_is_a_change_that_undo_takes_back() {
        let mut p = prep_with_track();
        assert!(!p.dirty());
        p.drop_here(10.2); // 120 BPM: bars every 2 s
        assert_eq!(p.drops, Some(vec![10.0]));
        assert!(p.dirty());
        p.drop_here(12.9); // within four bars: the same drop, moved
        assert_eq!(p.drops, Some(vec![12.0]));
        p.undo();
        p.undo();
        assert_eq!(p.drops, None);
        assert!(!p.dirty());
    }

    #[test]
    fn holding_a_button_plays_from_the_cue_and_letting_go_returns() {
        let mut p = prep_with_track();
        let mut ev = PrepEvents::default();
        p.hold_step(1, true, true, false, &mut ev, |_, ev| {
            ev.jump = Some(500.0);
            Some((500.0, false))
        });
        assert!(ev.play && ev.jump == Some(500.0) && !ev.pause);
        // still held: nothing more happens
        let mut ev = PrepEvents::default();
        p.hold_step(1, false, true, true, &mut ev, |_, _| panic!("started twice")); // the pointer wandered off: still held
        assert!(!ev.play && !ev.pause && ev.jump.is_none());
        // released: stop and go back
        let mut ev = PrepEvents::default();
        p.hold_step(1, false, false, true, &mut ev, |_, _| panic!());
        assert!(ev.pause && ev.jump == Some(500.0));
    }

    #[test]
    fn a_player_already_running_keeps_running_after_the_release() {
        let mut p = prep_with_track();
        let mut ev = PrepEvents::default();
        p.hold_step(2, true, true, true, &mut ev, |_, ev| {
            ev.jump = Some(900.0);
            Some((900.0, false))
        });
        assert!(!ev.play && ev.jump == Some(900.0));
        let mut ev = PrepEvents::default();
        p.hold_step(2, false, false, true, &mut ev, |_, _| panic!());
        assert!(!ev.pause && ev.jump.is_none());
    }

    #[test]
    fn a_long_hold_ends_its_loop_and_a_tap_keeps_it() {
        let loop_hold = |wait_ms: u64| {
            let mut p = prep_with_track();
            let mut ev = PrepEvents::default();
            p.hold_step(3, true, true, false, &mut ev, |_, ev| {
                ev.set_loop = Some(Some((10.0, 90.0)));
                Some((10.0, true))
            });
            std::thread::sleep(std::time::Duration::from_millis(wait_ms));
            let mut ev = PrepEvents::default();
            p.hold_step(3, false, false, true, &mut ev, |_, _| panic!());
            ev
        };
        assert_eq!(loop_hold(0).set_loop, None);
        assert_eq!(loop_hold(300).set_loop, Some(None));
    }

    #[test]
    fn two_buttons_are_not_held_at_once() {
        let mut p = prep_with_track();
        let mut ev = PrepEvents::default();
        p.hold_step(1, true, true, false, &mut ev, |_, _| Some((1.0, false)));
        let mut ev2 = PrepEvents::default();
        p.hold_step(2, true, true, false, &mut ev2, |_, _| panic!("second button started"));
        p.hold_step(2, false, false, false, &mut ev2, |_, _| panic!());
        assert!(!ev2.pause && p.hold.as_ref().map(|h| h.key) == Some(1));
    }
}
