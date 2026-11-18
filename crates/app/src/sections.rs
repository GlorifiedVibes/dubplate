//! The track's sections (intro, build, drop, break, outro) on the waveforms: coloured blocks along the overview and a
//! name flag at each boundary of the scrolling waveform. The sections come from the engine's [`Structure`]; here they
//! are only found for the deck's current grid (kept, so editing the grid finds them again) and drawn.

use std::{cell::RefCell, rc::Rc};

use crate::theme::palette;

use dubplate_engine::{BeatGrid, DecodedTrack, SectionKind, Structure, WAVEFORM_CHUNK_FRAMES};
use egui::{Color32, FontId, Painter, Rect, Shape, Stroke};

/// Grids remembered, newest last: two decks and Prepare, with room to spare.
const KEPT: usize = 6;

thread_local! {
    static KEPT_STRUCTURES: RefCell<Vec<(Key, Rc<Structure>)>> = const { RefCell::new(Vec::new()) };
    /// Drops the user put in by hand, per track (by the address of its summary), in seconds.
    static NAMED_DROPS: RefCell<Vec<Named>> = const { RefCell::new(Vec::new()) };
}

/// A track's hand-set drops, and the sections made with them for the grid they were last asked for on (the waveform,
/// the overview and the cue tools all ask every frame).
struct Named {
    wave: usize,
    drops: Vec<f64>,
    made: Option<(Key, Rc<Structure>)>,
}

/// Tracks whose hand-set drops are remembered at once: the decks, Prepare, and some that were just unloaded.
const NAMED_KEPT: usize = 8;

fn wave_id(track: &DecodedTrack) -> usize {
    std::sync::Arc::as_ptr(&track.wave) as usize
}

/// Says where the drops of `track` are (seconds), instead of the ones found; `None` goes back to the ones found. Call
/// it whenever a track is loaded, so a track without hand-set drops never inherits another's.
pub fn set_drops(track: &DecodedTrack, drops: Option<&[f64]>) {
    let id = wave_id(track);
    NAMED_DROPS.with(|n| {
        let mut n = n.borrow_mut();
        if let (Some(have), Some(d)) = (n.iter().find(|x| x.wave == id), drops) {
            if have.drops == d {
                return; // unchanged: keep the sections already made
            }
        }
        n.retain(|x| x.wave != id);
        if let Some(d) = drops {
            if n.len() >= NAMED_KEPT {
                n.remove(0);
            }
            n.push(Named { wave: id, drops: d.to_vec(), made: None });
        }
    });
}

/// The sections of `track` on `grid` with the drops where the user put them, if they did.
pub fn of(track: &DecodedTrack, grid: BeatGrid) -> Option<Rc<Structure>> {
    let found = found(track, grid)?;
    let id = wave_id(track);
    let key = key_of(track, grid);
    NAMED_DROPS.with(|n| {
        let mut n = n.borrow_mut();
        let Some(named) = n.iter_mut().find(|x| x.wave == id) else { return Some(found) };
        if let Some((k, s)) = &named.made {
            if *k == key {
                return Some(s.clone());
            }
        }
        // on the beats of this grid, so drops follow a grid that is nudged
        let period = 60.0 / grid.bpm as f64;
        let down = grid.offset * WAVEFORM_CHUNK_FRAMES as f64 / track.sample_rate.max(1) as f64;
        let on_beats: Vec<f64> = named.drops.iter().map(|t| down + ((t - down) / period).round() * period).collect();
        let s = Rc::new(found.with_drops(&on_beats, period));
        named.made = Some((key, s.clone()));
        Some(s)
    })
}

fn key_of(track: &DecodedTrack, grid: BeatGrid) -> Key {
    Key {
        wave: wave_id(track),
        frames: track.wave.total_frames,
        bpm: grid.bpm.to_bits(),
        offset: grid.offset.to_bits(),
    }
}

/// A track's summary (by address), and the grid the sections were found for.
#[derive(Clone, Copy, PartialEq)]
struct Key {
    wave: usize,
    frames: u64,
    bpm: u32,
    offset: u64,
}

/// The sections found in `track` on `grid`, `None` without a usable grid or summary. Found once per grid and track.
pub fn found(track: &DecodedTrack, grid: BeatGrid) -> Option<Rc<Structure>> {
    if !grid.is_valid() || track.wave.is_empty() {
        return None;
    }
    let key = key_of(track, grid);
    KEPT_STRUCTURES.with(|k| {
        let mut k = k.borrow_mut();
        if let Some((_, s)) = k.iter().find(|(kk, _)| *kk == key) {
            return Some(s.clone());
        }
        let chunk_secs = WAVEFORM_CHUNK_FRAMES as f64 / track.sample_rate.max(1) as f64;
        // beat 0 of the analysis is the grid's downbeat, so bar lines are residue 0
        let s = Rc::new(Structure::analyse(&track.wave, 60.0 / grid.bpm as f64, grid.offset * chunk_secs, 0));
        if k.len() >= KEPT {
            k.remove(0);
        }
        k.push((key, s.clone()));
        Some(s)
    })
}

pub fn colour(kind: SectionKind) -> Color32 {
    match kind {
        SectionKind::Intro => palette::intro(),
        SectionKind::Build => palette::build(),
        SectionKind::Drop => palette::drop(),
        SectionKind::Groove => palette::groove(),
        SectionKind::Break => palette::break_(),
        SectionKind::Outro => palette::outro(),
    }
}

/// Blocks along the top of an overview strip, one per section, with a hairline gap between them.
pub fn draw_strip(painter: &Painter, rect: Rect, s: &Structure, duration_secs: f64) {
    if duration_secs <= 0.0 || s.sections.len() < 2 {
        return;
    }
    let h = (rect.height() * 0.16).clamp(3.0, 6.0);
    let x_of = |t: f64| rect.left() + (t / duration_secs).clamp(0.0, 1.0) as f32 * rect.width();
    for sec in &s.sections {
        let (x0, x1) = (x_of(sec.start_secs), x_of(sec.end_secs));
        if x1 - x0 < 1.0 {
            continue;
        }
        let r = Rect::from_min_max(egui::pos2(x0 + 0.5, rect.top()), egui::pos2(x1 - 0.5, rect.top() + h));
        painter.rect_filled(r, 1.0, colour(sec.kind).linear_multiply(0.9));
    }
}

/// The bar a time falls in, counted as the phrase lines on the scrolling waveform count (bar 1 is the grid's first
/// downbeat), and the beat in it: `(bar, beat)` from 1. For a flag's name.
pub fn bar_and_beat(secs: f64, grid: BeatGrid, sample_rate: u32) -> (i64, i64) {
    let down = grid.offset * WAVEFORM_CHUNK_FRAMES as f64 / sample_rate.max(1) as f64;
    let beats = ((secs - down) * grid.bpm as f64 / 60.0).round() as i64;
    (beats.div_euclid(4) + 1, beats.rem_euclid(4) + 1)
}

/// What the section under `fraction` (0..1 along the track) is, for a hover tooltip: its name, what that name means,
/// where it starts (the number on its flag) and how long it is.
pub fn describe(track: &DecodedTrack, grid: BeatGrid, fraction: f64) -> Option<String> {
    let s = of(track, grid)?;
    let t = fraction.clamp(0.0, 1.0) * track.duration_secs;
    let sec = s.sections.iter().find(|x| t >= x.start_secs && t < x.end_secs)?;
    let (bar, beat) = bar_and_beat(sec.start_secs, grid, track.sample_rate);
    let means = match sec.kind {
        SectionKind::Intro => "the start, before the bass comes in",
        SectionKind::Build => "the bass is out and the music climbs toward a drop",
        SectionKind::Drop => "the bass comes in and stays",
        SectionKind::Groove => "the bass is in from the start of this part, not after a break",
        SectionKind::Break => "the bass is out",
        SectionKind::Outro => "the end, after the bass has gone",
    };
    let at = if beat == 1 { format!("bar {bar}") } else { format!("bar {bar}, beat {beat}") };
    Some(format!("{}: {means}\nstarts at {at} (the number on its flag), {} bars long\n\nFound from the audio, not exact: check it by ear.", sec.kind.name(), sec.beats / 4))
}

/// A flag at every section start inside `rect`: a line, and a tab along the bottom with the section's name and the
/// bar it starts on (`49` for beat 1, `49.2` for beat 2). `x_of` turns seconds into the x of the waveform being drawn;
/// `bar_of` the bar and beat of a time.
pub fn draw_flags(
    painter: &Painter,
    rect: Rect,
    s: &Structure,
    x_of: impl Fn(f64) -> f32,
    bar_of: impl Fn(f64) -> (i64, i64),
) {
    for sec in s.sections.iter().skip(1) {
        let x = x_of(sec.start_secs);
        if x < rect.left() || x > rect.right() {
            continue;
        }
        let c = colour(sec.kind);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.5_f32, c.linear_multiply(0.75)),
        );
        let (bar, beat) = bar_of(sec.start_secs);
        let at = if beat == 1 { format!("{bar}") } else { format!("{bar}.{beat}") };
        let label = format!("{} {at}", sec.kind.name().to_uppercase());
        let galley = painter.layout_no_wrap(label, FontId::monospace(crate::theme::sz(9.0)), palette::inset());
        let size = galley.size() + egui::vec2(8.0, 3.0);
        let tab = Rect::from_min_size(egui::pos2(x, rect.bottom() - size.y), size);
        painter.add(Shape::rect_filled(tab, 2.0, c));
        painter.galley(tab.min + egui::vec2(4.0, 1.5), galley, palette::background());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_are_counted_from_the_first_downbeat_like_the_phrase_lines() {
        let grid = BeatGrid { bpm: 120.0, offset: 44100.0 * 2.0 / WAVEFORM_CHUNK_FRAMES as f64 }; // bar 1 at 2 s, 2 s a bar
        assert_eq!(bar_and_beat(2.0, grid, 44100), (1, 1));
        assert_eq!(bar_and_beat(2.0 + 2.0 * 48.0, grid, 44100), (49, 1));
        assert_eq!(bar_and_beat(2.0 + 2.0 * 48.0 + 0.5, grid, 44100), (49, 2));
        assert_eq!(bar_and_beat(1.5, grid, 44100), (0, 4), "the beat before bar 1");
    }
}
