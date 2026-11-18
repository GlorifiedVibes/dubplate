//! How loud a track is, so decks can be matched without touching faders: integrated loudness as ITU-R BS.1770-4
//! defines it (K-weighting, 400 ms blocks overlapping by 75 %, an absolute gate at -70 LUFS and a relative gate
//! 10 LU below the gated mean, the method of EBU R128), and the true peak (4x oversampled, 48 taps).
//!
//! Loudness says how much to turn a track up or down; the true peak says how far it can be turned up before the
//! peaks go over full scale. `auto_trim_db` combines them.

use std::f64::consts::PI;

/// What a track measures.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Loudness {
    /// Integrated loudness, LUFS.
    pub lufs: f32,
    /// Highest peak between samples, dB relative to full scale (0 = at full scale).
    pub true_peak_db: f32,
}

/// The level auto gain aims for. Club masters sit around here, so most tracks move by little and only the quiet
/// or overloaded ones are corrected.
pub const TARGET_LUFS: f32 = -9.0;
/// Trim limits, dB.
pub const MAX_CUT_DB: f32 = -12.0;
pub const MAX_BOOST_DB: f32 = 12.0;
/// A boost may take the true peak this far over full scale: the master limiter rounds off the few peaks that
/// reach it, and refusing every boost that would touch 0 dBTP leaves modern masters (which are already there)
/// no room at all.
pub const PEAK_ALLOWANCE_DB: f32 = 1.0;

/// How much to change a track's level, in dB, to bring it to `target_lufs`: cuts always, boosts only while the
/// true peak stays within `PEAK_ALLOWANCE_DB` of full scale; never beyond the trim limits.
pub fn auto_trim_db(m: Loudness, target_lufs: f32) -> f32 {
    let wanted = target_lufs - m.lufs;
    let headroom = -m.true_peak_db + PEAK_ALLOWANCE_DB;
    wanted.min(headroom.max(0.0)).clamp(MAX_CUT_DB, MAX_BOOST_DB)
}

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Biquad { b, a, z: [0.0; 2] }
    }
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The two K-weighting filters for any sample rate (BS.1770-4 gives them for 48 kHz only; these are the same
/// analogue filters mapped to `rate`).
fn k_weighting(rate: f64) -> [Biquad; 2] {
    // Stage 1: a high shelf of about +4 dB from 1.7 kHz (the head's acoustic effect).
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad::new(
        [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    );
    // Stage 2: a high-pass at 38 Hz (revised low-frequency B curve).
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad::new([1.0, -2.0, 1.0], [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0]);
    [shelf, hp]
}

/// The 48 taps of the 4x interpolator (a windowed sinc), as four phases of 12.
fn interpolator() -> [[f32; 12]; 4] {
    let mut p = [[0f32; 12]; 4];
    for n in 0..48usize {
        let x = n as f64 - 23.5;
        let sinc = if x.abs() < 1e-9 { 1.0 } else { (PI * x / 4.0).sin() / (PI * x / 4.0) };
        let w = 0.5 - 0.5 * (2.0 * PI * (n as f64 + 0.5) / 48.0).cos();
        p[n % 4][n / 4] = (sinc * w) as f32;
    }
    p
}

/// Highest sample magnitude, after 4x oversampling wherever the signal is near full scale.
fn true_peak(chan: &[f32]) -> f32 {
    let taps = interpolator();
    let mut peak = 0f32;
    for &v in chan {
        peak = peak.max(v.abs());
    }
    // Only stretches that come near the sample peak can hide a higher one.
    let floor = peak * 0.5;
    let mut i = 0;
    while i + 12 <= chan.len() {
        let end = (i + 12).min(chan.len());
        if chan[i..end].iter().any(|v| v.abs() >= floor) {
            for ph in taps.iter() {
                let mut s = 0f32;
                for (k, c) in ph.iter().enumerate() {
                    s += c * chan[i + 11 - k];
                }
                peak = peak.max(s.abs());
            }
        }
        i += 1;
    }
    peak
}

/// Measures interleaved samples. `None` for silence (nothing above the gate) or no audio at all.
pub fn measure(samples: &[f32], channels: usize, sample_rate: u32) -> Option<Loudness> {
    let ch = channels.max(1);
    let frames = samples.len() / ch;
    let rate = sample_rate as f64;
    let block = (0.4 * rate).round() as usize;
    let hop = (0.1 * rate).round() as usize;
    if frames < block || hop == 0 {
        return None;
    }

    // K-weighted power of each channel (left and right; a mono file plays through both, so counts twice).
    let used = ch.min(2);
    let mut power = vec![0f64; frames]; // sum over channels of the squared weighted signal
    let mut peak = 0f32;
    let mut chan = vec![0f32; frames];
    for c in 0..used {
        let [mut shelf, mut hp] = k_weighting(rate);
        for i in 0..frames {
            let x = samples[i * ch + c];
            chan[i] = x;
            let y = hp.run(shelf.run(x as f64));
            power[i] += y * y;
        }
        peak = peak.max(true_peak(&chan));
    }
    if used == 1 {
        for p in power.iter_mut() {
            *p *= 2.0;
        }
    }

    // Mean power of every 400 ms block, from a running sum.
    let mut prefix = Vec::with_capacity(frames + 1);
    prefix.push(0f64);
    let mut acc = 0f64;
    for p in &power {
        acc += p;
        prefix.push(acc);
    }
    let mut blocks: Vec<f64> = Vec::new();
    let mut start = 0;
    while start + block <= frames {
        blocks.push((prefix[start + block] - prefix[start]) / block as f64);
        start += hop;
    }
    let lufs_of = |z: f64| -0.691 + 10.0 * z.log10();
    let above: Vec<f64> = blocks.iter().copied().filter(|&z| z > 0.0 && lufs_of(z) > -70.0).collect();
    if above.is_empty() {
        return None;
    }
    let gate = lufs_of(above.iter().sum::<f64>() / above.len() as f64) - 10.0;
    let kept: Vec<f64> = above.into_iter().filter(|&z| lufs_of(z) > gate).collect();
    if kept.is_empty() {
        return None;
    }
    let lufs = lufs_of(kept.iter().sum::<f64>() / kept.len() as f64);
    let true_peak_db = if peak > 0.0 { 20.0 * peak.log10() } else { -120.0 };
    Some(Loudness { lufs: lufs as f32, true_peak_db })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, secs: f64, hz: f64, dbfs: f64, phase: f64) -> Vec<f32> {
        let a = 10f64.powf(dbfs / 20.0);
        let mut v = Vec::new();
        for i in 0..(rate as f64 * secs) as usize {
            let s = (a * (2.0 * PI * hz * i as f64 / rate as f64 + phase).sin()) as f32;
            v.push(s);
            v.push(s);
        }
        v
    }

    #[test]
    fn a_sine_at_minus_23_reads_minus_23_lufs() {
        // EBU Tech 3341: a 997 Hz sine at -23 dBFS in both channels is -23.0 LUFS.
        let m = measure(&sine(48000, 20.0, 997.0, -23.0, 0.0), 2, 48000).unwrap();
        assert!((m.lufs + 23.0).abs() < 0.1, "{}", m.lufs);
    }

    #[test]
    fn the_reading_does_not_depend_on_the_sample_rate() {
        let a = measure(&sine(48000, 20.0, 997.0, -20.0, 0.0), 2, 48000).unwrap().lufs;
        let b = measure(&sine(44100, 20.0, 997.0, -20.0, 0.0), 2, 44100).unwrap().lufs;
        let c = measure(&sine(96000, 20.0, 997.0, -20.0, 0.0), 2, 96000).unwrap().lufs;
        assert!((a - b).abs() < 0.05 && (a - c).abs() < 0.05, "{a} {b} {c}");
    }

    #[test]
    fn six_db_more_signal_reads_six_lu_more() {
        let a = measure(&sine(44100, 12.0, 440.0, -26.0, 0.0), 2, 44100).unwrap().lufs;
        let b = measure(&sine(44100, 12.0, 440.0, -20.0, 0.0), 2, 44100).unwrap().lufs;
        assert!((b - a - 6.0).abs() < 0.02, "{}", b - a);
    }

    #[test]
    fn silence_and_very_quiet_parts_do_not_count() {
        let mut v = sine(44100, 10.0, 997.0, -20.0, 0.0);
        let plain = measure(&v, 2, 44100).unwrap().lufs;
        v.extend(vec![0f32; 44100 * 2 * 10]); // ten seconds of silence
        v.extend(sine(44100, 10.0, 997.0, -50.0, 0.0)); // and a part far under the relative gate
        let gated = measure(&v, 2, 44100).unwrap().lufs;
        assert!((plain - gated).abs() < 0.15, "{plain} {gated}");
    }

    #[test]
    fn digital_silence_has_no_loudness() {
        assert!(measure(&vec![0f32; 44100 * 2 * 5], 2, 44100).is_none());
        assert!(measure(&[], 2, 44100).is_none());
        assert!(measure(&vec![0.1f32; 100], 2, 44100).is_none()); // shorter than one block
    }

    #[test]
    fn the_true_peak_finds_what_lies_between_samples() {
        // A sine at a quarter of the sample rate, 45 degrees in: every sample is +-0.707, the wave reaches 1.0.
        let v = sine(44100, 3.0, 44100.0 / 4.0, 0.0, PI / 4.0);
        let sample_peak = v.iter().fold(0f32, |m, x| m.max(x.abs()));
        let m = measure(&v, 2, 44100).unwrap();
        assert!((sample_peak - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.001);
        assert!(m.true_peak_db > -0.5 && m.true_peak_db < 0.3, "{}", m.true_peak_db);
    }

    #[test]
    fn mono_counts_as_both_speakers() {
        let st = sine(44100, 10.0, 997.0, -20.0, 0.0);
        let mono: Vec<f32> = st.iter().step_by(2).copied().collect();
        let a = measure(&st, 2, 44100).unwrap().lufs;
        let b = measure(&mono, 1, 44100).unwrap().lufs;
        assert!((a - b).abs() < 0.02, "{a} {b}");
    }

    #[test]
    fn auto_trim_cuts_freely_and_boosts_only_with_room() {
        let m = |lufs, tp| Loudness { lufs, true_peak_db: tp };
        assert!((auto_trim_db(m(-6.0, -0.1), -9.0) + 3.0).abs() < 1e-4); // too loud: cut 3
        assert!((auto_trim_db(m(-14.0, -8.0), -9.0) - 5.0).abs() < 1e-4); // quiet with room: up 5
                                                                          // quiet but peaky: only the headroom plus the allowance
        let t = auto_trim_db(m(-16.0, -2.0), -9.0);
        assert!((t - 3.0).abs() < 1e-4, "{t}");
        // already past the allowance and too quiet: no boost
        assert!(auto_trim_db(m(-14.0, 1.5), -9.0).abs() < 1e-4);
        // limits
        assert_eq!(auto_trim_db(m(6.0, -30.0), -9.0), MAX_CUT_DB);
        assert_eq!(auto_trim_db(m(-60.0, -60.0), -9.0), MAX_BOOST_DB);
    }
}
