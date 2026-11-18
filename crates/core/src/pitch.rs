//! Finding the pitch of a voice: the YIN method on audio brought down to about 12 kHz, which is plenty for a
//! singing voice and makes it cheap enough to run on every 10 ms of a whole song, or live on a microphone.

/// Lowest and highest pitch looked for, in Hz (a low male voice to a high female one, with room).
pub const F_MIN: f32 = 70.0;
pub const F_MAX: f32 = 1100.0;
/// Steps between pitch readings.
pub const HOP_MS: f32 = 10.0;
/// The integration window and what must come before it, in milliseconds.
const WINDOW_MS: f32 = 40.0;
/// A reading is trusted when the dip YIN found is this deep (1 = a perfectly periodic sound).
const MIN_CLARITY: f32 = 0.6;
/// Quieter than this (RMS) is silence, not a voice.
pub const SILENCE_RMS: f32 = 0.004;

/// The rate audio is brought down to: the nearest whole fraction of `rate` to 12 kHz.
pub fn work_rate(rate: u32) -> (u32, usize) {
    let factor = ((rate as f32 / 12_000.0).round() as usize).max(1);
    (rate / factor as u32, factor)
}

/// Mono audio at `rate` as mono audio at `rate / factor`: each output sample is the average of `factor` inputs.
pub fn decimate(samples: &[f32], factor: usize) -> Vec<f32> {
    if factor <= 1 {
        return samples.to_vec();
    }
    samples.chunks_exact(factor).map(|c| c.iter().sum::<f32>() / factor as f32).collect()
}

/// Samples a pitch reading needs at `rate` (already brought down by `work_rate`).
pub fn frame_len(rate: u32) -> usize {
    let w = (rate as f32 * WINDOW_MS / 1000.0) as usize;
    w + (rate as f32 / F_MIN) as usize + 2
}

/// The pitch of `frame` (mono, at `rate`, at least `frame_len(rate)` long) and how clear it is (0 to 1), or
/// None when it is silent or has no steady pitch.
pub fn yin(frame: &[f32], rate: u32) -> Option<(f32, f32)> {
    let r = rate as f32;
    let tau_max = (r / F_MIN) as usize;
    let tau_min = ((r / F_MAX) as usize).max(2);
    let w = (r * WINDOW_MS / 1000.0) as usize;
    if frame.len() < w + tau_max + 2 {
        return None;
    }
    let rms = (frame[..w].iter().map(|v| v * v).sum::<f32>() / w as f32).sqrt();
    if rms < SILENCE_RMS {
        return None;
    }

    // The difference function, then its cumulative-mean normalisation.
    let mut cmnd = vec![1.0f32; tau_max + 1];
    let mut running = 0.0f32;
    for tau in 1..=tau_max {
        let mut d = 0.0f32;
        for j in 0..w {
            let x = frame[j] - frame[j + tau];
            d += x * x;
        }
        running += d;
        cmnd[tau] = if running > 0.0 { d * tau as f32 / running } else { 1.0 };
    }
    // The first dip below the threshold, followed down to its bottom; failing that, the deepest dip if it is clear enough.
    let mut best = None;
    let mut tau = tau_min;
    while tau < tau_max {
        if cmnd[tau] < 1.0 - MIN_CLARITY {
            while tau + 1 < tau_max && cmnd[tau + 1] < cmnd[tau] {
                tau += 1;
            }
            best = Some(tau);
            break;
        }
        tau += 1;
    }
    let tau = best?;
    // Parabolic refinement around the minimum.
    let (a, b, c) = (cmnd[tau.saturating_sub(1).max(1)], cmnd[tau], cmnd[(tau + 1).min(tau_max)]);
    let denom = a - 2.0 * b + c;
    let shift = if denom.abs() > 1e-9 { 0.5 * (a - c) / denom } else { 0.0 };
    let t = tau as f32 + shift.clamp(-1.0, 1.0);
    let hz = r / t;
    (F_MIN..=F_MAX).contains(&hz).then_some((hz, (1.0 - cmnd[tau]).clamp(0.0, 1.0)))
}

/// Distance from `b` to `a` in cents (hundredths of a semitone).
pub fn cents(a: f32, b: f32) -> f32 {
    1200.0 * (a / b).log2()
}

/// The pitch every `HOP_MS` through `mono` (at `rate`): Hz, or 0 where nothing steady is sung. Brief single-reading
/// blips are removed.
pub fn contour(mono: &[f32], rate: u32) -> Vec<f32> {
    let (wr, factor) = work_rate(rate);
    let low = decimate(mono, factor);
    let (frame, hop) = (frame_len(wr), (wr as f32 * HOP_MS / 1000.0).round() as usize);
    let half = frame / 2;
    let n = low.len() / hop;
    let mut out = vec![0.0f32; n];
    for (i, o) in out.iter_mut().enumerate() {
        // The reading belongs to the middle of its frame.
        let start = (i * hop).saturating_sub(half);
        if start + frame > low.len() {
            continue;
        }
        if let Some((hz, _)) = yin(&low[start..start + frame], wr) {
            *o = hz;
        }
    }
    // A voiced reading alone between two silent ones is noise.
    for i in 1..n.saturating_sub(1) {
        if out[i] > 0.0 && out[i - 1] == 0.0 && out[i + 1] == 0.0 {
            out[i] = 0.0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, rate: u32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * amp)
            .collect()
    }

    /// A voice-like sound: a fundamental with harmonics falling away.
    fn voice(hz: f32, rate: u32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| {
                let t = i as f32 / rate as f32;
                (1..=6).map(|h| (t * hz * h as f32 * std::f32::consts::TAU).sin() / h as f32).sum::<f32>() * 0.2
            })
            .collect()
    }

    fn pitch_of(x: &[f32], rate: u32) -> Option<f32> {
        let (wr, f) = work_rate(rate);
        let low = decimate(x, f);
        let start = low.len() / 2;
        yin(&low[start..start + frame_len(wr)], wr).map(|p| p.0)
    }

    #[test]
    fn steady_tones_are_found_across_the_voice_range_and_the_common_rates() {
        for rate in [44_100u32, 48_000] {
            for hz in [82.4f32, 110.0, 196.0, 261.6, 440.0, 659.3, 880.0] {
                let found = pitch_of(&tone(hz, rate, 1.0, 0.3), rate).unwrap_or(0.0);
                assert!(cents(found, hz).abs() < 15.0, "{hz} Hz at {rate}: found {found}");
                let found = pitch_of(&voice(hz, rate, 1.0), rate).unwrap_or(0.0);
                assert!(cents(found, hz).abs() < 15.0, "voice {hz} Hz at {rate}: found {found}");
            }
        }
    }

    #[test]
    fn silence_and_noise_have_no_pitch() {
        assert_eq!(pitch_of(&vec![0.0; 48_000], 48_000), None);
        assert_eq!(pitch_of(&tone(300.0, 48_000, 1.0, 0.0005), 48_000), None, "too quiet to be a voice");
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 8) as f32 / 8_388_608.0 - 1.0) * 0.3
            })
            .collect();
        assert_eq!(pitch_of(&noise, 48_000), None, "hiss has no steady pitch");
    }

    #[test]
    fn a_contour_follows_a_melody_and_leaves_gaps_where_nothing_is_sung() {
        let rate = 44_100;
        let mut x = voice(220.0, rate, 1.0);
        x.extend(vec![0.0; rate as usize / 2]);
        x.extend(voice(330.0, rate, 1.0));
        let c = contour(&x, rate);
        assert!((c.len() as i32 - 250).abs() <= 1, "{} readings for 2.5 s", c.len());
        assert!(cents(c[50], 220.0).abs() < 20.0 && cents(c[200], 330.0).abs() < 20.0);
        assert_eq!(c[115], 0.0, "the gap");
        let voiced = c.iter().filter(|&&h| h > 0.0).count();
        assert!(voiced > 180 && voiced < 205, "{voiced}");
    }

    #[test]
    fn cents_are_hundredths_of_a_semitone() {
        assert!((cents(440.0, 220.0) - 1200.0).abs() < 1e-3);
        assert!((cents(466.16, 440.0) - 100.0).abs() < 0.1);
        assert!(cents(220.0, 440.0) < 0.0);
    }

    #[test]
    fn decimating_keeps_whole_frames_only() {
        assert_eq!(decimate(&[1.0, 3.0, 5.0, 7.0, 9.0], 2), vec![2.0, 6.0]);
        assert_eq!(work_rate(48_000), (12_000, 4));
        assert_eq!(work_rate(44_100), (11_025, 4));
    }

    /// On a real vocal stem: `PITCH_FILE=song.vocals.mp3 cargo test -p dubplate-engine --release real_voice -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_voice() {
        let Ok(path) = std::env::var("PITCH_FILE") else { return };
        let t = crate::decode::decode_track(std::path::Path::new(&path)).unwrap();
        let mono: Vec<f32> = t.samples.chunks(t.channels).map(|f| f.iter().sum::<f32>() / t.channels as f32).collect();
        let start = std::time::Instant::now();
        let c = contour(&mono, t.sample_rate);
        let voiced: Vec<f32> = c.iter().copied().filter(|&h| h > 0.0).collect();
        let mut sorted = voiced.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!("{:.0} s of audio in {:.2} s: {} readings, {:.0}% voiced, pitch 10th/50th/90th percentile {:.0}/{:.0}/{:.0} Hz",
            t.duration_secs, start.elapsed().as_secs_f32(), c.len(), voiced.len() as f32 / c.len() as f32 * 100.0,
            sorted[sorted.len() / 10], sorted[sorted.len() / 2], sorted[sorted.len() * 9 / 10]);
        let jumps = c.windows(2).filter(|w| w[0] > 0.0 && w[1] > 0.0 && cents(w[1], w[0]).abs() > 300.0).count();
        let fold = |c: f32| {
            let m = c.rem_euclid(1200.0);
            if m > 600.0 {
                m - 1200.0
            } else {
                m
            }
        };
        let real_jumps =
            c.windows(2).filter(|w| w[0] > 0.0 && w[1] > 0.0 && fold(cents(w[1], w[0])).abs() > 300.0).count();
        eprintln!("{jumps} jumps of more than 3 semitones between neighbouring readings, {real_jumps} of them not just an octave");
    }
}
