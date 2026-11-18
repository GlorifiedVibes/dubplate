//! CUE / hot-cue / loop behaviour, shared by the GUI and MIDI controllers.
//!
//! Modelled on rekordbox / Pioneer decks:
//! * CUE while playing returns to the cue point and pauses; held while stopped on the
//!   cue it previews (release returns); pressed while stopped elsewhere it sets the cue.
//! * Hot cues A-H: empty pad sets (a running loop is saved as a hot loop), set pad
//!   jumps/engages, held while stopped it previews; delete removes it.
//! * LOOP IN / OUT set a manual loop, OUT also exits and re-loops, a long LOOP IN makes
//!   an auto loop. 1/2X and 2X resize an active loop in place, or the next auto loop.
//! * Quantize snaps cue and loop points to the beat grid.

use std::time::{Duration, Instant};

use dubplate_engine::{Cue, DeckCommand, LoopRegion, LOOP_SIZES};

use super::DubplateApp;

/// One user intent on a deck; produced by GUI buttons and MIDI alike.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeckAction {
    CueDown,
    CueUp,
    ToStart,
    PadDown(u8),
    PadUp(u8),
    PadDelete(u8),
    LoopInDown,
    LoopInUp,
    LoopOut,
    LoopAuto,
    /// Auto loop of this many beats (a controller's fixed-length loop button).
    LoopBeats(f32),
    LoopHalf,
    LoopDouble,
    ToggleQuantize,
}

/// Hot-cue pad colours (A-H).
pub const PAD_COLORS: [[u8; 3]; 8] = [
    [40, 200, 90],
    [0, 160, 255],
    [230, 120, 20],
    [200, 50, 210],
    [235, 50, 70],
    [0, 205, 205],
    [200, 200, 20],
    [240, 240, 240],
];

const LONG_PRESS: Duration = Duration::from_millis(450);
/// The playhead counts as "on the cue" within this many seconds.
const AT_CUE_SECS: f64 = 0.015;

#[derive(Default, Clone, Copy)]
pub struct CueState {
    cue_held: bool,
    pad_preview: Option<u8>,
    loop_in_down: Option<(Instant, f64)>,
}

/// Next auto-loop size one step up/down the 1/4..32 beat ladder.
pub fn step_loop_size(current: f32, up: bool) -> f32 {
    let last = LOOP_SIZES.len() - 1;
    // First ladder size that is >= current.
    let ge = LOOP_SIZES.iter().position(|&s| s >= current - 1e-4).unwrap_or(last);
    let on_ladder = (LOOP_SIZES[ge] - current).abs() < 1e-4;
    let idx = match (up, on_ladder) {
        (true, true) => (ge + 1).min(last),
        (true, false) => ge,                // off the ladder: next size above
        (false, _) => ge.saturating_sub(1), // next size below
    };
    LOOP_SIZES[idx]
}

/// Halves/doubles a loop keeping its start. Returns the new end.
pub fn resized_loop_end(start: f64, end: f64, double: bool, min_len: f64, max_end: f64) -> f64 {
    let len = end - start;
    let new_len = if double { len * 2.0 } else { (len / 2.0).max(min_len) };
    (start + new_len).min(max_end)
}

/// Where a playhead inside `[old_start, old_end)` should continue after the loop was
/// shortened to `new_len`, so it stays inside the loop instead of running past it.
pub fn refold_into_loop(pos: f64, start: f64, new_len: f64) -> f64 {
    start + (pos - start).rem_euclid(new_len)
}

impl DubplateApp {
    fn deck_loop_frames(&self, d: usize, beats: f32) -> f64 {
        match self.decks[d].frames_per_beat() {
            Some(fpb) => fpb * beats as f64,
            None => beats as f64 * 0.5 * self.decks[d].track.as_ref().map(|t| t.sample_rate as f64).unwrap_or(44_100.0),
        }
    }

    fn deck_min_loop(&self, d: usize) -> f64 {
        // 1/32 beat, but never below the engine's minimum.
        let q = self.deck_loop_frames(d, 1.0 / 32.0);
        q.max(64.0)
    }

    /// Playhead, snapped to the beat grid when Quantize is on.
    fn cue_pos(&self, d: usize) -> f64 {
        let p = self.decks[d].position();
        if self.decks[d].quantize {
            self.decks[d].snap_to_beat(p)
        } else {
            p
        }
    }

    fn set_loop(&mut self, d: usize, region: Option<LoopRegion>) {
        self.decks[d].loop_region = region;
        let cmd = region.filter(|r| r.active).map(|r| (r.start, r.end));
        self.decks[d].send(DeckCommand::SetLoop(cmd));
    }

    fn start_auto_loop(&mut self, d: usize, start: f64) {
        let Some(track) = self.decks[d].track.clone() else { return };
        let len = self.deck_loop_frames(d, self.decks[d].auto_loop_beats);
        let max_end = track.duration_frames as f64 - 1.0;
        let end = (start + len).min(max_end);
        if end - start < 64.0 {
            return;
        }
        self.decks[d].loop_in = None;
        self.set_loop(d, Some(LoopRegion { start, end, active: true }));
    }

    fn save_cue_to_db(&self, d: usize, cue: &Cue) {
        if let (Some(db), Some(path)) = (&self.db, &self.decks[d].track_path) {
            if let Err(e) = db.save_cue(path, cue) {
                log::warn!("saving cue failed: {e}");
            }
        }
    }

    fn pad_cue(&self, d: usize, pad: u8) -> Option<Cue> {
        self.decks[d].cues.iter().find(|c| c.hot_cue == Some(pad)).cloned()
    }

    /// Restores the saved main cue and hot cues after a track load and parks on the cue.
    pub(super) fn restore_cues(&mut self, d: usize) {
        let Some(path) = self.decks[d].track_path.clone() else { return };
        self.cue_state[d] = CueState::default();
        if let Some(db) = &self.db {
            self.decks[d].cues = db.load_cues(&path).into_iter().filter(|c| matches!(c.hot_cue, Some(1..=8))).collect();
            if let Some(p) = db.load_main_cue(&path) {
                self.decks[d].cue_point = p;
            }
        }
        let cue = self.decks[d].cue_point;
        self.decks[d].send(DeckCommand::StopAt(cue));
    }

    pub(super) fn handle_action(&mut self, d: usize, action: DeckAction) {
        if self.decks[d].track.is_none() {
            return;
        }
        match action {
            DeckAction::CueDown => self.cue_down(d),
            DeckAction::CueUp => {
                if self.cue_state[d].cue_held {
                    self.cue_state[d].cue_held = false;
                    let cue = self.decks[d].cue_point;
                    self.decks[d].send(DeckCommand::StopAt(cue));
                }
            }
            DeckAction::ToStart => {
                if self.deck_playing(d) {
                    self.decks[d].send(DeckCommand::Jump(0.0));
                } else {
                    self.decks[d].send(DeckCommand::StopAt(0.0));
                }
            }
            DeckAction::PadDown(n) => self.pad_down(d, n),
            DeckAction::PadUp(n) => {
                if self.cue_state[d].pad_preview == Some(n) {
                    self.cue_state[d].pad_preview = None;
                    if let Some(c) = self.pad_cue(d, n + 1) {
                        self.decks[d].send(DeckCommand::StopAt(c.pos_frames));
                    }
                }
            }
            DeckAction::PadDelete(n) => {
                let pad = n + 1;
                self.decks[d].cues.retain(|c| c.hot_cue != Some(pad));
                if let (Some(db), Some(path)) = (&self.db, &self.decks[d].track_path) {
                    if let Err(e) = db.delete_cue(path, pad) {
                        log::warn!("deleting cue failed: {e}");
                    }
                }
                self.library.invalidate_cues();
            }
            DeckAction::LoopInDown => {
                let pos = self.cue_pos(d);
                self.cue_state[d].loop_in_down = Some((Instant::now(), pos));
                match self.decks[d].loop_region {
                    Some(r) if r.active && pos < r.end => {
                        self.set_loop(d, Some(LoopRegion { start: pos, ..r }));
                    }
                    _ => self.decks[d].loop_in = Some(pos),
                }
            }
            DeckAction::LoopInUp => {
                if let Some((at, pos)) = self.cue_state[d].loop_in_down.take() {
                    if at.elapsed() >= LONG_PRESS {
                        // Long press = auto loop from where the button went down.
                        self.decks[d].loop_in = None;
                        self.start_auto_loop(d, pos);
                    }
                }
            }
            DeckAction::LoopOut => self.loop_out(d),
            DeckAction::LoopAuto => {
                if matches!(self.decks[d].loop_region, Some(r) if r.active) {
                    self.exit_loop(d);
                } else {
                    let start = self.cue_pos(d);
                    self.start_auto_loop(d, start);
                }
            }
            DeckAction::LoopBeats(beats) => {
                if matches!(self.decks[d].loop_region, Some(r) if r.active) {
                    self.exit_loop(d);
                } else {
                    self.decks[d].auto_loop_beats = beats;
                    let start = self.cue_pos(d);
                    self.start_auto_loop(d, start);
                }
            }
            DeckAction::LoopHalf => self.resize_loop(d, false),
            DeckAction::LoopDouble => self.resize_loop(d, true),
            DeckAction::ToggleQuantize => self.decks[d].quantize = !self.decks[d].quantize,
        }
    }

    fn cue_down(&mut self, d: usize) {
        let cue = self.decks[d].cue_point;
        let armed = self.decks[d].shared.armed.load(std::sync::atomic::Ordering::Relaxed);
        if self.deck_playing(d) || armed {
            self.decks[d].send(DeckCommand::StopAt(cue));
            return;
        }
        let sr = self.decks[d].track.as_ref().map(|t| t.sample_rate as f64).unwrap_or(44_100.0);
        let pos = self.decks[d].position();
        if (pos - cue).abs() <= AT_CUE_SECS * sr {
            // Stopped on the cue: play while held, return on release.
            self.cue_state[d].cue_held = true;
            self.decks[d].send(DeckCommand::Jump(cue));
            self.decks[d].send(DeckCommand::Play);
        } else {
            let p = self.cue_pos(d);
            self.decks[d].cue_point = p;
            self.decks[d].send(DeckCommand::StopAt(p));
            if let (Some(db), Some(path)) = (&self.db, &self.decks[d].track_path) {
                if let Err(e) = db.save_main_cue(path, p) {
                    log::warn!("saving cue point failed: {e}");
                }
            }
        }
    }

    fn pad_down(&mut self, d: usize, n: u8) {
        if n > 7 {
            return;
        }
        let pad = n + 1;
        let playing = self.deck_playing(d);
        match self.pad_cue(d, pad) {
            Some(cue) => {
                if let Some(end) = cue.out_frames {
                    self.decks[d].loop_in = None;
                    self.set_loop(d, Some(LoopRegion { start: cue.pos_frames, end, active: true }));
                }
                self.decks[d].send(DeckCommand::Jump(cue.pos_frames));
                if !playing {
                    // Stopped: preview while the pad is held.
                    self.cue_state[d].pad_preview = Some(n);
                    self.decks[d].send(DeckCommand::Play);
                }
            }
            None => {
                let sr = self.decks[d].track.as_ref().map(|t| t.sample_rate).unwrap_or(44_100);
                let color = PAD_COLORS[n as usize];
                let cue = match self.decks[d].loop_region {
                    Some(r) if r.active => Cue::new(Some(pad), r.start, Some(r.end), sr, color),
                    _ => Cue::new(Some(pad), self.cue_pos(d), None, sr, color),
                };
                self.save_cue_to_db(d, &cue);
                self.library.invalidate_cues();
                self.decks[d].cues.retain(|c| c.hot_cue != Some(pad));
                self.decks[d].cues.push(cue);
            }
        }
    }

    fn exit_loop(&mut self, d: usize) {
        if let Some(r) = self.decks[d].loop_region {
            self.set_loop(d, Some(LoopRegion { active: false, ..r }));
        }
    }

    fn loop_out(&mut self, d: usize) {
        let Some(track) = self.decks[d].track.clone() else { return };
        let max_end = track.duration_frames as f64 - 1.0;
        let min_len = self.deck_min_loop(d);
        match (self.decks[d].loop_in, self.decks[d].loop_region) {
            (Some(start), _) => {
                let mut end = self.cue_pos(d);
                if end < start + min_len {
                    end = start + self.deck_loop_frames(d, 1.0); // at least one beat
                }
                self.decks[d].loop_in = None;
                self.set_loop(d, Some(LoopRegion { start, end: end.min(max_end), active: true }));
            }
            (None, Some(r)) if r.active => self.exit_loop(d),
            (None, Some(r)) => {
                // RELOOP: back to the loop start and running again.
                self.set_loop(d, Some(LoopRegion { active: true, ..r }));
                self.decks[d].send(DeckCommand::Jump(r.start));
            }
            (None, None) => {
                let start = self.cue_pos(d);
                self.start_auto_loop(d, start);
            }
        }
    }

    fn resize_loop(&mut self, d: usize, double: bool) {
        let Some(track) = self.decks[d].track.clone() else { return };
        match self.decks[d].loop_region {
            Some(r) if r.active => {
                let max_end = track.duration_frames as f64 - 1.0;
                let end = resized_loop_end(r.start, r.end, double, self.deck_min_loop(d), max_end);
                self.set_loop(d, Some(LoopRegion { end, ..r }));
                let pos = self.decks[d].position();
                if !double && pos >= end && pos < r.end && self.deck_playing(d) {
                    let folded = refold_into_loop(pos, r.start, end - r.start);
                    self.decks[d].send(DeckCommand::Jump(folded));
                }
                if let Some(fpb) = self.decks[d].frames_per_beat() {
                    let beats = ((end - r.start) / fpb) as f32;
                    if let Some(&s) = LOOP_SIZES.iter().find(|&&s| (s - beats).abs() < beats * 0.02 + 1e-3) {
                        self.decks[d].auto_loop_beats = s;
                    }
                }
            }
            _ => {
                // No loop running: change the size of the next auto loop.
                self.decks[d].auto_loop_beats = step_loop_size(self.decks[d].auto_loop_beats, double);
            }
        }
    }

    /// Called when a plain Play/Pause takes over: a held CUE/pad preview becomes real playback.
    pub(super) fn cancel_previews(&mut self, d: usize) {
        self.cue_state[d].cue_held = false;
        self.cue_state[d].pad_preview = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_size_ladder_steps_and_clamps() {
        assert_eq!(step_loop_size(4.0, true), 8.0);
        assert_eq!(step_loop_size(4.0, false), 2.0);
        assert_eq!(step_loop_size(32.0, true), 32.0);
        assert_eq!(step_loop_size(0.25, false), 0.25);
        assert_eq!(step_loop_size(3.0, true), 4.0, "off-ladder: next size above");
        assert_eq!(step_loop_size(3.0, false), 2.0, "off-ladder: next size below");
        assert_eq!(step_loop_size(100.0, true), 32.0);
    }

    #[test]
    fn halving_and_doubling_keep_the_start() {
        assert_eq!(resized_loop_end(1000.0, 5000.0, false, 64.0, 1e9), 3000.0);
        assert_eq!(resized_loop_end(1000.0, 5000.0, true, 64.0, 1e9), 9000.0);
    }

    #[test]
    fn halving_never_goes_below_the_minimum_and_doubling_stops_at_the_end() {
        assert_eq!(resized_loop_end(0.0, 100.0, false, 80.0, 1e9), 80.0);
        assert_eq!(resized_loop_end(1000.0, 5000.0, true, 64.0, 6000.0), 6000.0);
    }

    #[test]
    fn playhead_is_refolded_into_a_shortened_loop() {
        // loop 1000..5000 halved to 1000..3000; a playhead at 4500 (now past the end)
        // folds back to 2500, one inside the shortened loop is left where it is.
        assert_eq!(refold_into_loop(4500.0, 1000.0, 2000.0), 2500.0);
        assert_eq!(refold_into_loop(2500.0, 1000.0, 2000.0), 2500.0);
        assert_eq!(refold_into_loop(1000.0, 1000.0, 2000.0), 1000.0);
    }
}
