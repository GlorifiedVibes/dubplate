//! Smooth playhead for the scrolling waveform.
//!
//! The audio thread publishes its position once per audio block, so the value the UI reads moves in
//! steps that don't line up with screen frames: some frames see no movement, the next sees two blocks.
//! The waveform then scrolls unevenly. This keeps a position that advances with wall-clock time at the
//! playback rate and is gently pulled towards the published value, so the motion is even and still
//! locked to the audio.

use std::collections::VecDeque;
use std::time::Instant;

pub struct Playhead {
    pos: f64,
    at: Option<Instant>,
    /// How far the published position was ahead of the estimate at each recent update.
    recent: VecDeque<(Instant, f64)>,
}

/// How fast the estimate is pulled towards the audio (per second) while it is close to it.
const PULL_STEADY: f64 = 2.0;
/// Added to the pull as the error nears the snap distance (by its square), so a playhead that really is somewhere
/// else (a pitch bend, a nudge, the tempo fader moving) is caught up with quickly instead of trailing and snapping.
const PULL_CHASE: f64 = 40.0;
/// The published position is a staircase: it stands still between audio blocks, then jumps a block. Against a steady
/// estimate it is therefore a sawtooth, and pulling towards each reading copies the sawtooth into the picture as a
/// shimmer. The top of the sawtooth (the reading taken just after a block) is the audio's real position, and over this
/// long there is always a reading near the top, so the estimate is steered by the largest recent reading instead.
const WINDOW_SECS: f64 = 0.25;

impl Default for Playhead {
    fn default() -> Self {
        Self::new()
    }
}

impl Playhead {
    pub fn new() -> Self {
        Self { pos: 0.0, at: None, recent: VecDeque::with_capacity(64) }
    }

    /// `observed` is the published position and `rate` the playback speed, both in source frames
    /// (per second for the rate). `snap` is the error beyond which the estimate jumps to the published
    /// value (a seek, a scratch). Returns the position to draw.
    pub fn update(&mut self, observed: f64, rate: f64, playing: bool, now: Instant, snap: f64) -> f64 {
        let Some(at) = self.at.filter(|_| playing) else {
            self.pos = observed;
            self.at = Some(now);
            self.recent.clear();
            return self.pos;
        };
        let dt = now.saturating_duration_since(at).as_secs_f64().min(0.1);
        let predicted = self.pos + dt * rate;
        let err = observed - predicted;
        self.at = Some(now);
        if err.abs() > snap {
            self.pos = observed;
            self.recent.clear();
            return self.pos;
        }
        while self.recent.front().is_some_and(|(t, _)| now.saturating_duration_since(*t).as_secs_f64() > WINDOW_SECS) {
            self.recent.pop_front();
        }
        if self.recent.len() == self.recent.capacity() {
            self.recent.pop_front();
        }
        self.recent.push_back((now, err));
        let top = self.recent.iter().map(|r| r.1).fold(f64::MIN, f64::max);
        let far = (top / snap) * (top / snap);
        let step = top * (1.0 - (-dt * (PULL_STEADY + PULL_CHASE * far)).exp());
        // the readings were measured against the estimate as it was: move them with it
        for r in self.recent.iter_mut() {
            r.1 -= step;
        }
        self.pos = predicted + step;
        self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const RATE: f64 = 44100.0;
    const BLOCK: f64 = 1024.0;

    /// The audio thread publishes in blocks; the screen samples at 60 Hz.
    fn run(smooth: bool) -> Vec<f64> {
        run_at(smooth, BLOCK, 60.0, 1.0)
    }

    /// `block` frames an audio block, `hz` screen frames a second, and the audio really moving `actual` times as fast
    /// as the rate the smoother is told (a pitch bend it does not know about).
    fn run_at(smooth: bool, block: f64, hz: f64, actual: f64) -> Vec<f64> {
        let t0 = Instant::now();
        let mut p = Playhead::new();
        let mut out = Vec::new();
        for i in 0..(4.0 * hz) as usize {
            let t = i as f64 / hz;
            let published = (t * RATE * actual / block).floor() * block; // staircase
            let at = t0 + Duration::from_secs_f64(t);
            out.push(if smooth { p.update(published, RATE, true, at, 0.06 * RATE) } else { published });
        }
        out
    }

    #[test]
    fn the_picture_does_not_shimmer_with_large_audio_blocks_or_a_fast_screen() {
        // a frame's movement may differ from the next by a tenth of a pixel at one pixel per 256 frames
        for (block, hz) in [(1024.0, 60.0), (2048.0, 60.0), (1024.0, 144.0), (512.0, 75.0)] {
            let u = unevenness(&run_at(true, block, hz, 1.0));
            assert!(u < 26.0, "blocks of {block} at {hz} Hz: steps differ by {u} frames");
        }
    }

    #[test]
    fn a_pitch_bend_it_was_not_told_about_is_followed_without_snapping() {
        let s = run_at(true, BLOCK, 60.0, 1.08);
        let truth = |i: usize| i as f64 / 60.0 * RATE * 1.08;
        for i in 60..s.len() {
            assert!((s[i] - truth(i)).abs() < 0.04 * RATE, "frame {i}: {} behind", truth(i) - s[i]);
            let step = s[i] - s[i - 1];
            assert!(step > 0.0 && step < 2.0 * RATE / 60.0, "frame {i} jumped {step}");
        }
    }

    fn unevenness(v: &[f64]) -> f64 {
        let steps: Vec<f64> = v.windows(2).skip(30).map(|w| w[1] - w[0]).collect();
        let max = steps.iter().cloned().fold(f64::MIN, f64::max);
        let min = steps.iter().cloned().fold(f64::MAX, f64::min);
        max - min
    }

    #[test]
    fn the_smoothed_playhead_moves_far_more_evenly_than_the_published_one() {
        let (raw, smooth) = (unevenness(&run(false)), unevenness(&run(true)));
        assert!(smooth < raw * 0.4, "smooth {smooth} vs raw {raw}");
    }

    #[test]
    fn it_stays_locked_to_the_audio() {
        let s = run(true);
        let t = 239.0 / 60.0;
        let truth = t * RATE;
        assert!((s[239] - truth).abs() < BLOCK * 1.5, "drifted: {} vs {}", s[239], truth);
    }

    #[test]
    fn a_seek_snaps_and_a_stopped_deck_follows_exactly() {
        let t0 = Instant::now();
        let mut p = Playhead::new();
        p.update(1000.0, RATE, true, t0, 0.06 * RATE);
        let jumped = p.update(500_000.0, RATE, true, t0 + Duration::from_millis(16), 0.06 * RATE);
        assert_eq!(jumped, 500_000.0);
        assert_eq!(p.update(123.0, RATE, false, t0 + Duration::from_millis(32), 0.06 * RATE), 123.0);
    }
}
