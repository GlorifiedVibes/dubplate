//! Changes a song's key and tempo independently of each other. It works on the whole decoded song, off the
//! audio thread, and gives back a new song to play: a phase vocoder (with identity phase locking, so chords
//! stay clear) stretches it in time, and resampling then moves the pitch. Playing a tempo change as a plain
//! speed change, like a record, moves both together, which is why this exists.
//!
//! Good for a few semitones and tempo changes of up to about 20 %; far beyond that, drums blur and voices
//! take on a processed sound.

use std::f64::consts::PI;
use std::sync::Arc;

use rustfft::{num_complex::Complex, FftPlanner};

use crate::deck::DecodedTrack;

/// The widest key change offered, in semitones either way.
pub const MAX_SEMITONES: i32 = 6;
/// The tempo range offered, as a speed multiplier.
pub const TEMPO_RANGE: (f32, f32) = (0.7, 1.3);

const FRAME: usize = 4096;
const HOP: usize = FRAME / 4;

/// How much a key and tempo setting changes the song; `None` for no change at all.
pub fn is_identity(semitones: i32, tempo: f32) -> bool {
    semitones == 0 && (tempo - 1.0).abs() < 0.005
}

fn hann() -> Vec<f32> {
    (0..FRAME).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / FRAME as f64).cos()) as f32).collect()
}

fn wrap(x: f64) -> f64 {
    x - 2.0 * PI * (x / (2.0 * PI)).round()
}

/// Makes `x` last `s` times as long without changing its pitch.
fn stretch_channel(x: &[f32], s: f64) -> Vec<f32> {
    let n_out = (x.len() as f64 * s).round() as usize;
    let bins = FRAME / 2 + 1;
    let window = hann();
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FRAME);
    let ifft = planner.plan_fft_inverse(FRAME);
    let omega: Vec<f64> = (0..bins).map(|k| 2.0 * PI * k as f64 / FRAME as f64).collect();

    // Output position `p` lives at `out[p + offset]`, so frames that start before the song are kept too.
    let offset = 3 * HOP + FRAME / 2;
    let mut out = vec![0f32; n_out + offset + FRAME * 2];
    let frames = (n_out + FRAME / 2) / HOP + 6;

    let mut buf = vec![Complex::new(0f32, 0f32); FRAME];
    let mut mag = vec![0f64; bins];
    let mut phase = vec![0f64; bins];
    let mut prev_phase = vec![0f64; bins];
    let mut synth = vec![0f64; bins];
    let mut peaks: Vec<usize> = Vec::new();
    let mut prev_at: i64 = 0;

    for j in 0..frames {
        let centre = (j as i64 - 3) * HOP as i64;
        let at = (centre as f64 / s).round() as i64;
        let start = at - (FRAME / 2) as i64;
        for i in 0..FRAME {
            let idx = start + i as i64;
            let v = if idx >= 0 && (idx as usize) < x.len() { x[idx as usize] } else { 0.0 };
            buf[i] = Complex::new(v * window[i], 0.0);
        }
        fft.process(&mut buf);
        for k in 0..bins {
            mag[k] = buf[k].norm() as f64;
            phase[k] = buf[k].im.atan2(buf[k].re) as f64;
        }

        if j == 0 {
            synth.copy_from_slice(&phase);
        } else {
            let dt = (at - prev_at).max(1) as f64;
            // Spectral peaks, each owning the bins up to halfway to the next peak.
            peaks.clear();
            for k in 2..bins.saturating_sub(2) {
                if mag[k] > mag[k - 1]
                    && mag[k] > mag[k - 2]
                    && mag[k] >= mag[k + 1]
                    && mag[k] >= mag[k + 2]
                    && mag[k] > 1e-6
                {
                    peaks.push(k);
                }
            }
            if peaks.is_empty() {
                peaks.extend(0..bins);
            }
            let mut next_synth = vec![0f64; bins];
            for &p in &peaks {
                let dphi = wrap(phase[p] - prev_phase[p] - omega[p] * dt);
                next_synth[p] = synth[p] + (omega[p] + dphi / dt) * HOP as f64;
            }
            let mut pi = 0;
            for k in 0..bins {
                while pi + 1 < peaks.len()
                    && (peaks[pi + 1] as i64 - k as i64).abs() < (peaks[pi] as i64 - k as i64).abs()
                {
                    pi += 1;
                }
                let p = peaks[pi];
                if k != p {
                    next_synth[k] = next_synth[p] + (phase[k] - phase[p]);
                }
            }
            synth = next_synth;
        }
        prev_phase.copy_from_slice(&phase);
        prev_at = at;

        for k in 0..bins {
            let c = Complex::from_polar(mag[k] as f32, synth[k] as f32);
            buf[k] = c;
            if k > 0 && k < FRAME / 2 {
                buf[FRAME - k] = c.conj();
            }
        }
        buf[0].im = 0.0;
        buf[FRAME / 2].im = 0.0;
        ifft.process(&mut buf);
        let base = centre - (FRAME / 2) as i64 + offset as i64;
        for i in 0..FRAME {
            let pos = base + i as i64;
            if pos >= 0 && (pos as usize) < out.len() {
                out[pos as usize] += buf[i].re / FRAME as f32 * window[i];
            }
        }
    }
    // Hann squared, overlapped four times, adds up to 1.5.
    out[offset..offset + n_out].iter().map(|v| v / 1.5).collect()
}

/// Reads `x` at `p` times the speed (cubic interpolation): the pitch goes up by `p` and the length down by it.
fn resample_channel(x: &[f32], p: f64) -> Vec<f32> {
    let n_out = (x.len() as f64 / p).floor() as usize;
    let at = |i: i64| -> f32 {
        if i >= 0 && (i as usize) < x.len() {
            x[i as usize]
        } else {
            0.0
        }
    };
    (0..n_out)
        .map(|i| {
            let t = i as f64 * p;
            let k = t.floor() as i64;
            let f = (t - k as f64) as f32;
            let (a, b, c, d) = (at(k - 1), at(k), at(k + 1), at(k + 2));
            b + 0.5 * f * (c - a + f * (2.0 * a - 5.0 * b + 4.0 * c - d + f * (3.0 * (b - c) + d - a)))
        })
        .collect()
}

/// Brings peaks above `CEILING` back down by easing the volume down around them (and back up after), so the
/// few samples a stretch pushes past full scale are not clipped. Nothing changes where there are no peaks.
fn limit(samples: &mut [f32], channels: usize) {
    const CEILING: f32 = 0.95;
    const BLOCK: usize = 64;
    const SPAN: usize = 8; // blocks either side that a peak's reduction reaches
    let frames = samples.len() / channels;
    if frames == 0 || samples.iter().all(|v| v.abs() <= 0.98) {
        return;
    }
    let blocks = frames.div_ceil(BLOCK);
    let need: Vec<f32> = (0..blocks)
        .map(|b| {
            let peak = samples[b * BLOCK * channels..((b + 1) * BLOCK).min(frames) * channels]
                .iter()
                .fold(0f32, |a, v| a.max(v.abs()));
            if peak > CEILING {
                CEILING / peak
            } else {
                1.0
            }
        })
        .collect();
    let lo = |b: usize| b.saturating_sub(SPAN);
    let hi = |b: usize| (b + SPAN).min(blocks - 1);
    let held: Vec<f32> = (0..blocks).map(|b| need[lo(b)..=hi(b)].iter().copied().fold(1.0, f32::min)).collect();
    let gain: Vec<f32> =
        (0..blocks).map(|b| held[lo(b)..=hi(b)].iter().sum::<f32>() / (hi(b) - lo(b) + 1) as f32).collect();
    for f in 0..frames {
        // Between the centres of two blocks the gain glides from one block's value to the next.
        let pos = (f as f32 + 0.5) / BLOCK as f32 - 0.5;
        let (a, t) = (pos.floor().max(0.0) as usize, (pos - pos.floor()).clamp(0.0, 1.0));
        let (g0, g1) = (gain[a.min(blocks - 1)], gain[(a + 1).min(blocks - 1)]);
        let g = if pos < 0.0 { gain[0] } else { g0 + (g1 - g0) * t };
        for c in 0..channels {
            samples[f * channels + c] *= g;
        }
    }
}

/// `samples` (interleaved, `channels` per frame) with the key moved by `semitones` and the speed changed by
/// `tempo` (1.0 = unchanged, 1.1 = ten percent faster). The result is `1 / tempo` times as long.
pub fn change(samples: &[f32], channels: usize, semitones: f32, tempo: f32) -> Vec<f32> {
    let channels = channels.max(1);
    let (tempo, pitch) = (tempo.clamp(0.25, 4.0) as f64, 2f64.powf(semitones as f64 / 12.0));
    if (tempo - 1.0).abs() < 1e-4 && (pitch - 1.0).abs() < 1e-6 {
        return samples.to_vec();
    }
    // Stretch by pitch / tempo, then play it `pitch` times faster: the length comes out at 1 / tempo and
    // the pitch at `pitch` times.
    let s = pitch / tempo;
    let split: Vec<Vec<f32>> =
        (0..channels).map(|c| samples.iter().skip(c).step_by(channels).copied().collect()).collect();
    let done: Vec<Vec<f32>> = std::thread::scope(|scope| {
        let handles: Vec<_> = split
            .iter()
            .map(|ch| {
                scope.spawn(move || {
                    let stretched = if (s - 1.0).abs() > 1e-4 { stretch_channel(ch, s) } else { ch.clone() };
                    if (pitch - 1.0).abs() > 1e-6 {
                        resample_channel(&stretched, pitch)
                    } else {
                        stretched
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("stretch thread")).collect()
    });
    let frames = done.iter().map(Vec::len).min().unwrap_or(0);
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for ch in &done {
            out.push(ch[i]);
        }
    }
    limit(&mut out, channels);
    out
}

/// A copy of `track` with its key and tempo changed.
pub fn change_track(track: &DecodedTrack, semitones: i32, tempo: f32) -> Arc<DecodedTrack> {
    let samples = change(&track.samples, track.channels, semitones as f32, tempo);
    let frames = (samples.len() / track.channels.max(1)) as u64;
    let wave = Arc::new(crate::waveform_data::WaveSummary::build(&samples, track.channels, track.sample_rate));
    Arc::new(DecodedTrack {
        loudness: None,
        wave,
        samples,
        sample_rate: track.sample_rate,
        channels: track.channels,
        duration_frames: frames,
        waveform: track.waveform.clone(),
        title: track.title.clone(),
        duration_secs: frames as f64 / track.sample_rate.max(1) as f64,
        bpm: track.bpm * tempo,
        bpm_coarse: track.bpm_coarse,
        beat_offset: track.beat_offset,
        beat_times_ms: vec![],
        key: track.key,
        cues: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 44100.0;

    fn tones(secs: f32, freqs: &[f32]) -> Vec<f32> {
        (0..(secs * RATE) as usize)
            .map(|i| freqs.iter().map(|f| (i as f32 * f * std::f32::consts::TAU / RATE).sin() * 0.3).sum())
            .collect()
    }

    /// Strongest frequency in the middle second, by FFT.
    fn dominant(x: &[f32]) -> f32 {
        let n = 1 << 15;
        let mid = x.len() / 2;
        let seg: Vec<Complex<f32>> = (0..n)
            .map(|i| {
                let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos();
                Complex::new(x.get(mid + i).copied().unwrap_or(0.0) * w, 0.0)
            })
            .collect();
        let mut buf = seg;
        FftPlanner::<f32>::new().plan_fft_forward(n).process(&mut buf);
        let (k, _) =
            buf[..n / 2].iter().enumerate().max_by(|a, b| a.1.norm().partial_cmp(&b.1.norm()).unwrap()).unwrap();
        k as f32 * RATE / n as f32
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn no_change_is_a_copy() {
        let x = tones(0.5, &[440.0]);
        assert_eq!(change(&x, 1, 0.0, 1.0), x);
        assert!(is_identity(0, 1.0) && is_identity(0, 1.002) && !is_identity(1, 1.0) && !is_identity(0, 0.9));
    }

    #[test]
    fn slowing_down_lengthens_the_song_and_keeps_the_pitch() {
        let x = tones(4.0, &[440.0]);
        let y = change(&x, 1, 0.0, 0.8);
        assert!(
            (y.len() as f32 / x.len() as f32 - 1.25).abs() < 0.005,
            "length ratio {}",
            y.len() as f32 / x.len() as f32
        );
        let f = dominant(&y);
        assert!((f - 440.0).abs() < 4.0, "pitch {f}");
        assert!((rms(&y[20000..y.len() - 20000]) / rms(&x) - 1.0).abs() < 0.12, "level kept");
    }

    #[test]
    fn speeding_up_shortens_the_song_and_keeps_the_pitch() {
        let x = tones(4.0, &[440.0]);
        let y = change(&x, 1, 0.0, 1.2);
        assert!((y.len() as f32 / x.len() as f32 - 1.0 / 1.2).abs() < 0.005);
        assert!((dominant(&y) - 440.0).abs() < 4.0);
    }

    #[test]
    fn a_key_change_moves_the_pitch_and_keeps_the_length() {
        let x = tones(4.0, &[440.0]);
        for (semis, want) in [
            (2.0f32, 440.0 * 2f32.powf(2.0 / 12.0)),
            (-3.0, 440.0 * 2f32.powf(-3.0 / 12.0)),
            (6.0, 440.0 * 2f32.powf(0.5)),
        ] {
            let y = change(&x, 1, semis, 1.0);
            assert!((y.len() as f32 - x.len() as f32).abs() < 4.0, "length {} vs {}", y.len(), x.len());
            let f = dominant(&y);
            assert!((f - want).abs() < want * 0.01, "{semis} semitones: {f} Hz, wanted {want}");
        }
    }

    #[test]
    fn key_and_tempo_together_are_independent() {
        let x = tones(4.0, &[330.0]);
        let y = change(&x, 1, 2.0, 0.9);
        assert!((y.len() as f32 / x.len() as f32 - 1.0 / 0.9).abs() < 0.005);
        let want = 330.0 * 2f32.powf(2.0 / 12.0);
        assert!((dominant(&y) - want).abs() < want * 0.01, "{}", dominant(&y));
    }

    #[test]
    fn a_chord_keeps_its_intervals() {
        // A major triad: after a whole-tone shift each note moved by the same ratio.
        let x = tones(4.0, &[220.0, 277.18, 329.63]);
        let y = change(&x, 1, 2.0, 1.0);
        let n = 1 << 15;
        let mid = y.len() / 2;
        let mut buf: Vec<Complex<f32>> = (0..n)
            .map(|i| Complex::new(y[mid + i] * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()), 0.0))
            .collect();
        FftPlanner::<f32>::new().plan_fft_forward(n).process(&mut buf);
        let at = |f: f32| {
            let k = (f * n as f32 / RATE).round() as usize;
            (k - 3..=k + 3).map(|i| buf[i].norm()).fold(0.0, f32::max)
        };
        let ratio = 2f32.powf(2.0 / 12.0);
        let floor = buf[..n / 2].iter().map(|c| c.norm()).sum::<f32>() / (n / 2) as f32;
        for f in [220.0, 277.18, 329.63] {
            assert!(at(f * ratio) > 40.0 * floor, "missing {f} -> {}", f * ratio);
        }
    }

    #[test]
    fn peaks_are_eased_down_without_touching_the_rest() {
        let mut x: Vec<f32> = (0..44100).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        x[20000] = 1.8;
        x[30000] = -1.5;
        let far = x[5000];
        limit(&mut x, 1);
        assert!(x.iter().all(|v| v.abs() <= 0.99), "peak {}", x.iter().fold(0f32, |a, b| a.max(b.abs())));
        assert_eq!(x[5000], far, "far from a peak nothing changes");
        let mut quiet = vec![0.5f32; 1000];
        limit(&mut quiet, 1);
        assert!(quiet.iter().all(|&v| v == 0.5));
    }

    #[test]
    fn silence_stays_silent_and_nothing_blows_up_on_a_burst() {
        let z = change(&vec![0.0; 30000], 1, 3.0, 0.85);
        assert!(z.iter().all(|v| v.abs() < 1e-6));
        let mut click = vec![0.0f32; 44100];
        click[20000] = 1.0;
        let y = change(&click, 1, -2.0, 1.1);
        assert!(y.iter().all(|v| v.is_finite()) && y.iter().map(|v| v.abs()).fold(0.0, f32::max) < 2.0);
    }

    #[test]
    fn stereo_keeps_both_channels_the_same_length_and_their_own_pitch() {
        let l = tones(3.0, &[440.0]);
        let r = tones(3.0, &[660.0]);
        let inter: Vec<f32> = l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect();
        let y = change(&inter, 2, 2.0, 1.0);
        assert_eq!(y.len() % 2, 0);
        let (yl, yr): (Vec<f32>, Vec<f32>) =
            (y.iter().step_by(2).copied().collect(), y.iter().skip(1).step_by(2).copied().collect());
        let k = 2f32.powf(2.0 / 12.0);
        assert!((dominant(&yl) - 440.0 * k).abs() < 6.0 && (dominant(&yr) - 660.0 * k).abs() < 8.0);
    }

    #[test]
    fn a_track_keeps_its_details_and_gets_the_new_length() {
        let samples: Vec<f32> = tones(2.0, &[440.0]).iter().flat_map(|v| [*v, *v]).collect();
        let frames = samples.len() as u64 / 2;
        let t = DecodedTrack {
            loudness: None,
            samples,
            sample_rate: 44100,
            channels: 2,
            duration_frames: frames,
            waveform: vec![],
            wave: Default::default(),
            title: "x".into(),
            duration_secs: 2.0,
            bpm: 100.0,
            bpm_coarse: 100,
            beat_offset: 0.0,
            beat_times_ms: vec![1, 2],
            key: None,
            cues: vec![],
        };
        let y = change_track(&t, 0, 0.8);
        assert_eq!((y.sample_rate, y.channels, y.title.as_str()), (44100, 2, "x"));
        assert!((y.duration_secs - 2.5).abs() < 0.01 && y.duration_frames as usize * 2 == y.samples.len());
        assert!((y.bpm - 80.0).abs() < 1e-3);
    }

    /// On a real song: `STRETCH_SONG=a.mp3 STRETCH_OUT=out.wav cargo test -p dubplate-engine --release real_song -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_song() {
        let Ok(path) = std::env::var("STRETCH_SONG") else { return };
        let t = crate::decode::decode_track(std::path::Path::new(&path)).unwrap();
        let start = std::time::Instant::now();
        let y = change_track(&t, 3, 0.9);
        eprintln!(
            "{:.1} s of song -> {:.1} s in {:.2} s; peak {:.3} -> {:.3}; rms {:.4} -> {:.4}",
            t.duration_secs,
            y.duration_secs,
            start.elapsed().as_secs_f32(),
            t.samples.iter().fold(0f32, |a, b| a.max(b.abs())),
            y.samples.iter().fold(0f32, |a, b| a.max(b.abs())),
            rms(&t.samples),
            rms(&y.samples)
        );
        let over = |v: &[f32], t: f32| v.iter().filter(|x| x.abs() > t).count() as f64 / v.len() as f64 * 100.0;
        eprintln!(
            "samples over 0.8 / 1.0: before {:.4}% / {:.4}%, after {:.4}% / {:.4}%",
            over(&t.samples, 0.8),
            over(&t.samples, 1.0),
            over(&y.samples, 0.8),
            over(&y.samples, 1.0)
        );
        if let Ok(out) = std::env::var("STRETCH_OUT") {
            let spec = hound::WavSpec {
                channels: t.channels as u16,
                sample_rate: t.sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(out, spec).unwrap();
            for v in &y.samples {
                w.write_sample((v.clamp(-1.0, 1.0) * 32767.0) as i16).unwrap();
            }
            w.finalize().unwrap();
        }
    }

    /// How long a song takes: `cargo test -p dubplate-engine --release song_length_speed -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn song_length_speed() {
        let x: Vec<f32> = tones(240.0, &[220.0, 330.0, 440.0]).iter().flat_map(|v| [*v, *v * 0.8]).collect();
        let t = std::time::Instant::now();
        let y = change(&x, 2, 2.0, 0.9);
        eprintln!("4 minutes of stereo in {:.2} s -> {} samples", t.elapsed().as_secs_f32(), y.len());
    }
}
