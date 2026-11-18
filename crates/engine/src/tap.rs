//! Tap tempo: the user taps along to a track playing at normal speed and the grid is fitted to the taps.
//!
//! Taps are track positions in seconds (the playhead when the key or button went down). A person lands within
//! about 15 ms of the beat, but 30 s of taps (80+ of them at drum & bass tempos) pin the tempo much more tightly
//! than any single gap does, so the fit is a straight line through *all* the taps, not an average of gaps:
//! `time = beat_secs + k * period`, `k` being the beat number of each tap. Missed beats are handled by
//! numbering, double taps and stray taps are thrown out, and a whole-number tempo is taken when the fit allows it.
//!
//! The first tap is only a start, not "beat 1": which beat opens a bar is a separate question.

/// Fewest taps and shortest span that give a trustworthy tempo.
pub const MIN_TAPS: usize = 12;
pub const MIN_SPAN_SECS: f64 = 6.0;
/// A tap further than this from the fitted line is never kept, however sloppy the rest are.
const MIN_OUTLIER_SECS: f64 = 0.020;
/// The usual error of a tap is under this; it is only used to avoid dropping good taps from an unusually steady fit.
const SNAP_MAX_BPM: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TapFit {
    pub bpm: f64,
    /// Time of the grid's beat `k = 0`, the beat nearest the first kept tap (latency already removed).
    pub beat_secs: f64,
    pub taps_used: usize,
    pub taps_dropped: usize,
    pub span_secs: f64,
    /// Spread of the kept taps around the fitted grid.
    pub rms_ms: f64,
    /// How far the tempo may be out (one standard error).
    pub bpm_error: f64,
    /// True when the fit was close enough to a whole number to take that number.
    pub whole_bpm: bool,
}

impl TapFit {
    /// A grid of `bpm` with a beat at `beat_secs`, with no tapping behind it (for snapping or shifting an
    /// existing grid).
    pub fn from_grid(bpm: f64, beat_secs: f64) -> TapFit {
        TapFit {
            bpm,
            beat_secs,
            taps_used: 0,
            taps_dropped: 0,
            span_secs: 0.0,
            rms_ms: 0.0,
            bpm_error: 0.0,
            whole_bpm: false,
        }
    }
    pub fn period_secs(&self) -> f64 {
        60.0 / self.bpm
    }
    /// Time of beat `k` (any integer, before or after the taps).
    pub fn beat_time(&self, k: i64) -> f64 {
        self.beat_secs + k as f64 * self.period_secs()
    }
    /// The first beat at or after `t`.
    pub fn first_beat_from(&self, t: f64) -> f64 {
        let k = ((t - self.beat_secs) / self.period_secs()).ceil() as i64;
        self.beat_time(k)
    }
    /// The same grid moved by `shift` seconds.
    pub fn shifted(&self, shift: f64) -> TapFit {
        TapFit { beat_secs: self.beat_secs + shift, ..*self }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapError {
    /// Fewer than [`MIN_TAPS`] usable taps.
    TooFew,
    /// The taps cover less than [`MIN_SPAN_SECS`].
    TooShort,
    /// The taps do not follow one steady beat.
    Unsteady,
}

impl std::fmt::Display for TapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TapError::TooFew => "need more taps",
            TapError::TooShort => "keep tapping for longer",
            TapError::Unsteady => "the taps are not steady enough to follow",
        })
    }
}

/// Fits a grid to tap times. `latency_secs` is how late taps arrive on average (screen, keyboard, audio output);
/// it is subtracted from every tap and moves the grid, not the tempo.
pub fn fit_taps(taps: &[f64], latency_secs: f64) -> Result<TapFit, TapError> {
    let mut t: Vec<f64> = taps.iter().copied().filter(|v| v.is_finite()).map(|v| v - latency_secs).collect();
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if t.len() < MIN_TAPS {
        return Err(TapError::TooFew);
    }
    if t[t.len() - 1] - t[0] < MIN_SPAN_SECS {
        return Err(TapError::TooShort);
    }

    // First guess of the period: the middle gap. Robust unless most taps are off.
    let mut gaps: Vec<f64> = t.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).collect();
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut period = gaps[gaps.len() / 2];
    if !(period > 0.05) {
        return Err(TapError::Unsteady);
    }

    // Number the taps in order, re-fitting the period as they come so a slightly wrong first guess does not
    // slip a beat over a long run. A gap near two periods means a missed beat; a gap under half a period is a
    // double tap and is skipped.
    let mut pts: Vec<(f64, f64)> = vec![(0.0, t[0])]; // (beat number, time)
    let mut last = 0.0f64;
    for &x in &t[1..] {
        let gap = x - pts[pts.len() - 1].1;
        if gap < period * 0.5 {
            continue;
        }
        let step = (gap / period).round().max(1.0);
        last += step;
        pts.push((last, x));
        if pts.len() >= 4 {
            if let Some((_, b)) = line(&pts) {
                if b > 0.05 {
                    period = b;
                }
            }
        }
    }

    // Straight-line fit, throwing out the worst taps until the rest agree.
    let mut dropped = t.len() - pts.len();
    for _ in 0..4 {
        let Some((a, b)) = line(&pts) else { return Err(TapError::Unsteady) };
        let res: Vec<f64> = pts.iter().map(|&(k, x)| x - (a + b * k)).collect();
        let sigma = robust_sigma(&res);
        let limit = (3.0 * sigma).max(MIN_OUTLIER_SECS);
        let kept: Vec<(f64, f64)> = pts.iter().zip(&res).filter(|(_, r)| r.abs() <= limit).map(|(p, _)| *p).collect();
        if kept.len() == pts.len() {
            break;
        }
        dropped += pts.len() - kept.len();
        pts = kept;
        if pts.len() < MIN_TAPS {
            return Err(TapError::TooFew);
        }
    }
    let Some((mut a, mut b)) = line(&pts) else { return Err(TapError::Unsteady) };
    let span = pts[pts.len() - 1].1 - pts[0].1;
    if span < MIN_SPAN_SECS {
        return Err(TapError::TooShort);
    }

    let res: Vec<f64> = pts.iter().map(|&(k, x)| x - (a + b * k)).collect();
    let rms = (res.iter().map(|r| r * r).sum::<f64>() / res.len() as f64).sqrt();
    // Anything this loose is not one beat: the person stopped, changed pace or tapped something else. A good
    // run keeps most of its taps and sits within a tenth of a beat of the line.
    if rms > 0.12 * b || dropped * 5 > t.len() * 2 {
        return Err(TapError::Unsteady);
    }

    let n = pts.len() as f64;
    let kbar = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let skk: f64 = pts.iter().map(|p| (p.0 - kbar).powi(2)).sum();
    let b_err = rms / skk.sqrt().max(1e-9);
    let mut bpm = 60.0 / b;
    let bpm_error = bpm * b_err / b;

    // Produced dance music is nearly always a whole tempo; take it when the fit is that close.
    let whole = bpm.round();
    let whole_bpm = whole >= 40.0 && (bpm - whole).abs() <= (2.5 * bpm_error).clamp(0.03, SNAP_MAX_BPM);
    if whole_bpm {
        bpm = whole;
        b = 60.0 / bpm;
        a = pts.iter().map(|&(k, x)| x - b * k).sum::<f64>() / n;
    }

    // Report the beat nearest the first kept tap as beat 0.
    let k0 = pts[0].0;
    let beat_secs = a + b * k0;
    Ok(TapFit {
        bpm,
        beat_secs,
        taps_used: pts.len(),
        taps_dropped: dropped,
        span_secs: span,
        rms_ms: rms * 1000.0,
        bpm_error,
        whole_bpm,
    })
}

/// Least-squares line `x = a + b*k` through `(k, x)` points: `(a, b)`.
fn line(pts: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = pts.len() as f64;
    if pts.len() < 2 {
        return None;
    }
    let kbar = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let xbar = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let skk: f64 = pts.iter().map(|p| (p.0 - kbar).powi(2)).sum();
    if skk <= 0.0 {
        return None;
    }
    let skx: f64 = pts.iter().map(|p| (p.0 - kbar) * (p.1 - xbar)).sum();
    let b = skx / skk;
    Some((xbar - b * kbar, b))
}

/// Spread of the middle half of the values, scaled to a standard deviation; ignores a few wild ones.
fn robust_sigma(v: &[f64]) -> f64 {
    let mut a: Vec<f64> = v.iter().map(|x| x.abs()).collect();
    a.sort_by(|x, y| x.partial_cmp(y).unwrap());
    if a.is_empty() {
        return 0.0;
    }
    1.4826 * a[a.len() / 2]
}

/// Moves the grid to the nearest strong onsets in the audio so its position is not limited by how steady the
/// taps were or how late they arrived. `onsets` is an onset-strength curve with one value per `frame_secs`;
/// the grid is tried at shifts within `±max_shift_secs` and the one whose beats sit on the most onset energy
/// wins. Returns the grid unchanged when no shift is clearly better (flat or empty curve).
pub fn snap_to_onsets(fit: &TapFit, onsets: &[f32], frame_secs: f64, max_shift_secs: f64) -> TapFit {
    if onsets.len() < 8 || frame_secs <= 0.0 {
        return *fit;
    }
    let period = fit.period_secs();
    let at = |t: f64| -> f64 {
        let x = t / frame_secs;
        if x < 0.0 {
            return 0.0;
        }
        let i = x.floor() as usize;
        if i + 1 >= onsets.len() {
            return 0.0;
        }
        let f = x - i as f64;
        onsets[i] as f64 * (1.0 - f) + onsets[i + 1] as f64 * f
    };
    // Score over every beat inside the audio the curve covers, so the whole song has a vote, not only the
    // stretch that was tapped (it is a straight grid, so the same shift fits all of it).
    let end = onsets.len() as f64 * frame_secs;
    let k_lo = ((0.0 - fit.beat_secs) / period).ceil() as i64;
    let k_hi = ((end - fit.beat_secs) / period).floor() as i64;
    if k_hi - k_lo < 8 {
        return *fit;
    }
    let step = (frame_secs / 8.0).max(0.0005);
    let steps = (max_shift_secs / step).floor() as i64;
    let mut best = (f64::MIN, 0.0f64);
    let mut sum_all = 0.0;
    let mut count = 0.0;
    for s in -steps..=steps {
        let shift = s as f64 * step;
        let score: f64 = (k_lo..=k_hi).map(|k| at(fit.beat_time(k) + shift)).sum();
        sum_all += score;
        count += 1.0;
        if score > best.0 {
            best = (score, shift);
        }
    }
    // Needs to stand clear of the average shift, or the curve says nothing.
    let mean = sum_all / count;
    if !(best.0 > mean * 1.15) {
        return *fit;
    }
    fit.shifted(best.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small deterministic noise source so the tests do not depend on a random crate.
    struct Noise(u64);
    impl Noise {
        fn uniform(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 1.0)
        }
        fn gauss(&mut self, sigma: f64) -> f64 {
            let (u, v) = (self.uniform(), self.uniform());
            sigma * (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
        }
    }

    /// Taps at a steady tempo for `secs`, each off by `sigma` seconds, all late by `late`.
    fn taps(bpm: f64, first: f64, secs: f64, sigma: f64, late: f64, seed: u64) -> Vec<f64> {
        let mut n = Noise(seed);
        let p = 60.0 / bpm;
        (0..)
            .map(|k| first + k as f64 * p)
            .take_while(|t| *t < first + secs)
            .map(|t| t + late + n.gauss(sigma))
            .collect()
    }

    #[test]
    fn thirty_seconds_of_sloppy_taps_give_the_tempo() {
        for (bpm, seed) in [(174.0, 1), (128.0, 2), (87.0, 3), (140.0, 4)] {
            let t = taps(bpm, 12.3, 30.0, 0.015, 0.0, seed);
            let f = fit_taps(&t, 0.0).unwrap();
            assert!((f.bpm - bpm).abs() < 0.05, "{bpm}: got {}", f.bpm);
            assert!(f.whole_bpm, "{bpm} should be taken as a whole tempo (error {})", f.bpm_error);
            assert!(f.bpm_error < 0.1);
        }
    }

    #[test]
    fn a_tempo_between_whole_numbers_is_kept() {
        let t = taps(137.54, 5.0, 40.0, 0.010, 0.0, 9);
        let f = fit_taps(&t, 0.0).unwrap();
        assert!((f.bpm - 137.54).abs() < 0.08, "got {}", f.bpm);
        assert!(!f.whole_bpm);
    }

    #[test]
    fn the_tap_delay_moves_the_grid_not_the_tempo() {
        let t = taps(174.0, 10.0, 30.0, 0.012, 0.060, 5);
        let without = fit_taps(&t, 0.0).unwrap();
        let with = fit_taps(&t, 0.060).unwrap();
        assert!((without.bpm - with.bpm).abs() < 1e-9);
        let d = (without.beat_secs - with.beat_secs - 0.060).rem_euclid(without.period_secs());
        let d = d.min(without.period_secs() - d);
        assert!(d < 0.002, "{d}");
        // With the delay removed the grid sits on the true beats (10.0 + k * period) to within a few ms.
        let off = (with.beat_secs - 10.0).rem_euclid(with.period_secs());
        assert!(off.min(with.period_secs() - off) < 0.006, "{off}");
    }

    #[test]
    fn missed_beats_and_stray_taps_do_not_matter() {
        let mut t = taps(128.0, 8.0, 36.0, 0.012, 0.0, 6);
        t.remove(20); // a missed beat
        t.remove(21);
        t.remove(40);
        t.push(t[30] + 0.09); // a double tap
        t.push(14.9); // a stray one, between beats
        let f = fit_taps(&t, 0.0).unwrap();
        assert!((f.bpm - 128.0).abs() < 0.08, "got {}", f.bpm);
        assert!(f.taps_dropped >= 1);
    }

    #[test]
    fn a_slightly_wrong_first_guess_does_not_lose_count_over_a_long_run() {
        // Tempo that drifts only a little; 3 minutes of taps. The beat numbers must stay right to the end.
        let t = taps(175.0, 3.0, 180.0, 0.014, 0.0, 7);
        let f = fit_taps(&t, 0.0).unwrap();
        assert_eq!(f.bpm, 175.0);
        assert!(f.taps_dropped <= 3, "{}", f.taps_dropped);
    }

    #[test]
    fn too_little_is_refused() {
        assert_eq!(fit_taps(&[1.0, 1.5, 2.0], 0.0), Err(TapError::TooFew));
        let short = taps(240.0, 0.0, 4.0, 0.01, 0.0, 8); // 16 taps in 4 s
        assert_eq!(fit_taps(&short, 0.0), Err(TapError::TooShort));
        // Random times are not a beat.
        let mut n = Noise(11);
        let mut r: Vec<f64> = (0..40).map(|_| n.uniform() * 30.0).collect();
        r.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(fit_taps(&r, 0.0).is_err());
    }

    #[test]
    fn onsets_pull_the_grid_onto_the_audio() {
        // Real kicks every beat at 174 BPM starting 1.000 s; the taps ran 55 ms late and are loose.
        let frame = 0.00580499; // 256 samples at 44.1 kHz
        let period = 60.0 / 174.0;
        let mut on = vec![0.0f32; (60.0 / frame) as usize];
        let mut k = 0;
        loop {
            let t = 1.0 + k as f64 * period;
            let i = (t / frame).round() as usize;
            if i + 2 >= on.len() {
                break;
            }
            on[i] = 1.0;
            on[i + 1] = 0.4;
            k += 1;
        }
        let t = taps(174.0, 10.0 + 0.0, 30.0, 0.015, 0.055, 12);
        let f = fit_taps(&t, 0.0).unwrap();
        let snapped = snap_to_onsets(&f, &on, frame, 0.100);
        let off = (snapped.beat_secs - 1.0).rem_euclid(period);
        let off = off.min(period - off);
        assert!(off < 0.004, "grid is {off} s from the kicks");
        // A silent curve leaves the grid alone.
        let flat = vec![0.0f32; on.len()];
        assert_eq!(snap_to_onsets(&f, &flat, frame, 0.100), f);
    }
}
