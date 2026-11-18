//! Click track for checking a beat grid by ear: a short tick on every beat, a higher and louder one on the
//! downbeat. It is driven by the playhead's position in the track, so it stays on the grid whatever the
//! audio does, and a seek simply restarts the count.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Click settings shared between the screen and the audio thread.
pub struct ClickControls {
    pub on: AtomicBool,
    /// Grid tempo, f64 bits.
    bpm_bits: AtomicU64,
    /// Time of a downbeat in the track, seconds, f64 bits.
    downbeat_bits: AtomicU64,
    /// Sample rate of the track whose position the prepare player reports.
    pub source_rate: AtomicU32,
    /// Click level, f32 bits.
    level_bits: AtomicU32,
}

impl Default for ClickControls {
    fn default() -> Self {
        Self {
            on: AtomicBool::new(false),
            bpm_bits: AtomicU64::new(0f64.to_bits()),
            downbeat_bits: AtomicU64::new(0f64.to_bits()),
            source_rate: AtomicU32::new(44100),
            level_bits: AtomicU32::new(0.5f32.to_bits()),
        }
    }
}

impl ClickControls {
    pub fn set_grid(&self, bpm: f64, downbeat_secs: f64) {
        self.bpm_bits.store(bpm.to_bits(), Ordering::Relaxed);
        self.downbeat_bits.store(downbeat_secs.to_bits(), Ordering::Relaxed);
    }
    pub fn grid(&self) -> (f64, f64) {
        (
            f64::from_bits(self.bpm_bits.load(Ordering::Relaxed)),
            f64::from_bits(self.downbeat_bits.load(Ordering::Relaxed)),
        )
    }
    pub fn set_level(&self, l: f32) {
        self.level_bits.store(l.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level_bits.load(Ordering::Relaxed))
    }
}

const TICK_SECS: f32 = 0.030;
const BEAT_HZ: f32 = 1500.0;
const DOWNBEAT_HZ: f32 = 2400.0;

/// Makes the click samples. Call [`Click::sample`] once per output frame with that frame's track time.
pub struct Click {
    last_beat: Option<i64>,
    /// Samples left in the current tick, its pitch and its loudness.
    left: u32,
    total: u32,
    hz: f32,
    amp: f32,
    phase: f32,
}

impl Default for Click {
    fn default() -> Self {
        Self { last_beat: None, left: 0, total: 1, hz: BEAT_HZ, amp: 0.0, phase: 0.0 }
    }
}

impl Click {
    /// Forgets where it was (the click was switched off, or the playhead jumped).
    pub fn reset(&mut self) {
        self.last_beat = None;
        self.left = 0;
    }

    /// The click at track time `t` (seconds) for a grid of `bpm` with a downbeat at `down` (seconds).
    /// A tick starts when `t` crosses a beat going forward by one; any other change of beat (a seek) only
    /// resynchronises.
    pub fn sample(&mut self, t: f64, bpm: f64, down: f64, out_rate: f32, level: f32) -> f32 {
        if bpm > 0.0 {
            let k = ((t - down) * bpm / 60.0).floor() as i64;
            if let Some(prev) = self.last_beat {
                if k == prev + 1 {
                    let downbeat = k.rem_euclid(4) == 0;
                    self.hz = if downbeat { DOWNBEAT_HZ } else { BEAT_HZ };
                    self.amp = if downbeat { 1.0 } else { 0.6 };
                    self.total = (TICK_SECS * out_rate) as u32;
                    self.left = self.total;
                    self.phase = 0.0;
                }
            }
            self.last_beat = Some(k);
        }
        if self.left == 0 {
            return 0.0;
        }
        let x = self.left as f32 / self.total as f32;
        let env = x * x; // fast decay
        let s = (self.phase * std::f32::consts::TAU).sin() * env * self.amp * level;
        self.phase = (self.phase + self.hz / out_rate).fract();
        self.left -= 1;
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the click over `secs` of track time starting at `from`, returning the times each tick began.
    fn ticks(bpm: f64, down: f64, from: f64, secs: f64) -> Vec<(f64, f32)> {
        let rate = 48000.0f32;
        let mut c = Click::default();
        let mut out = vec![];
        let n = (secs * rate as f64) as usize;
        let mut was_quiet = true;
        for i in 0..n {
            let t = from + i as f64 / rate as f64;
            let _s = c.sample(t, bpm, down, rate, 1.0);
            if c.left == c.total - 1 && was_quiet && c.total > 1 {
                out.push((t, c.hz));
            }
            was_quiet = c.left == 0;
        }
        out.into_iter().collect()
    }

    #[test]
    fn a_tick_lands_on_every_beat_and_the_downbeat_is_higher() {
        let t = ticks(120.0, 1.0, 0.0, 6.0); // beats every 0.5 s from 1.0 s
        let times: Vec<f64> = t.iter().map(|x| x.0).collect();
        assert!(times.len() >= 9, "{times:?}");
        for w in times.windows(2) {
            assert!(((w[1] - w[0]) - 0.5).abs() < 0.001);
        }
        // The high tick is exactly the downbeat (1.0 s) and every fourth beat from it, before it as well.
        for (time, hz) in &t {
            let beat = ((time - 1.0) / 0.5).round() as i64;
            assert!(((time - 1.0) / 0.5 - beat as f64).abs() < 0.01, "{time} is off the grid");
            assert_eq!(*hz == DOWNBEAT_HZ, beat.rem_euclid(4) == 0, "beat {beat}");
        }
        assert!(t.iter().any(|x| x.1 == DOWNBEAT_HZ));
    }

    #[test]
    fn the_tick_is_on_the_grid_to_a_sample() {
        let t = ticks(174.0, 0.3, 5.0, 4.0);
        for (time, _) in t {
            let beat = (time - 0.3) * 174.0 / 60.0;
            assert!((beat - beat.round()).abs() < 48000.0f64.recip() * 174.0 / 60.0 * 1.5, "{time} is {beat} beats");
        }
    }

    #[test]
    fn a_seek_does_not_click_but_the_next_beat_does() {
        let mut c = Click::default();
        let r = 48000.0;
        assert_eq!(c.sample(10.0, 120.0, 0.0, r, 1.0), 0.0);
        // Jump forward a long way: resynchronise silently.
        assert_eq!(c.sample(100.2, 120.0, 0.0, r, 1.0), 0.0);
        assert_eq!(c.left, 0);
        let mut heard = false;
        for i in 0..(0.5 * r as f64) as usize {
            if c.sample(100.2 + i as f64 / r as f64, 120.0, 0.0, r, 1.0).abs() > 0.0 {
                heard = true;
            }
        }
        assert!(heard);
    }

    #[test]
    fn no_grid_no_click() {
        let mut c = Click::default();
        for i in 0..4800 {
            assert_eq!(c.sample(i as f64 / 48000.0, 0.0, 0.0, 48000.0, 1.0), 0.0);
        }
    }
}
