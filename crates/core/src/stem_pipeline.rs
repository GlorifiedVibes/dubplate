//! The engine-independent half of stem separation: preparing audio for HTDemucs, cutting it into
//! overlapping segments, and blending the model's answers back into whole-track stems.
//!
//! An engine (ONNX Runtime, libtorch) only has to turn one normalised segment into four stems.
//!
//! **Normalisation matters.** Demucs removes the mean of the mono mix and divides by its standard
//! deviation *once for the whole track* (`demucs.separate` does the same). Normalising every
//! segment by its own RMS and leaving the DC offset in — what Dubplate did before — made loud tracks
//! lose most of their vocals and misplace bass (measured against the reference implementation).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use crate::stems::{StemError, TrackStems};

/// Sample rate HTDemucs expects.
pub const SAMPLE_RATE: u32 = 44_100;

/// Segment length at 44.1 kHz (about 7.8 s, the HTDemucs training segment).
pub const SEGMENT_FRAMES: usize = 343_980;

/// Stems in model output order: drums, bass, other, vocals.
pub const N_STEMS: usize = 4;

/// The message of the error a cancelled separation returns (see [`crate::stems::is_cancelled`]).
pub const CANCELLED: &str = "separation cancelled";

/// Floats the engine must return per segment: `[stem][channel][frame]`.
pub const OUTPUT_LEN: usize = N_STEMS * 2 * SEGMENT_FRAMES;

/// Mean and standard deviation of the mono mix, computed once for the whole track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Normaliser {
    pub mean: f32,
    pub std: f32,
}

impl Normaliser {
    /// `audio` is interleaved stereo.
    pub fn of(audio: &[f32]) -> Self {
        let n = (audio.len() / 2).max(1) as f64;
        let mono = |i: usize| (audio[2 * i] as f64 + audio[2 * i + 1] as f64) * 0.5;
        let frames = audio.len() / 2;
        let mean = (0..frames).map(mono).sum::<f64>() / n;
        let var = (0..frames)
            .map(|i| {
                let d = mono(i) - mean;
                d * d
            })
            .sum::<f64>()
            / n;
        Self { mean: mean as f32, std: (var.sqrt() as f32).max(1e-6) }
    }
}

/// Separates `samples` (interleaved, `channels` = 1 or 2) into four stems.
///
/// `infer` gets one segment, already normalised, as planar `[left × SEGMENT_FRAMES, right × SEGMENT_FRAMES]`
/// (zero-padded at the end of the track), and returns `OUTPUT_LEN` floats laid out `[stem][channel][frame]`.
/// The stems come back in the track's own scale with the DC offset removed.
pub fn separate_chunked(
    samples: &[f32],
    sample_rate: u32,
    channels: usize,
    progress: Option<&AtomicU32>,
    cancel: Option<&AtomicBool>,
    mut infer: impl FnMut(&[f32]) -> Result<Vec<f32>, StemError>,
) -> Result<Arc<TrackStems>, StemError> {
    let stereo = to_stereo(samples, channels);
    let audio = if sample_rate != SAMPLE_RATE && !stereo.is_empty() {
        log::info!("resampling {sample_rate} → {SAMPLE_RATE} Hz for stem separation");
        resample_stereo(&stereo, sample_rate, SAMPLE_RATE)
    } else {
        stereo
    };
    let n_frames = audio.len() / 2;
    if n_frames == 0 {
        if let Some(p) = progress {
            p.store(100, Ordering::Relaxed);
        }
        return Ok(Arc::new(TrackStems {
            drums: Vec::new(),
            bass: Vec::new(),
            other: Vec::new(),
            vocals: Vec::new(),
            sample_rate: SAMPLE_RATE,
        }));
    }

    let norm = Normaliser::of(&audio);
    let mut stems: [Vec<f32>; N_STEMS] = std::array::from_fn(|_| vec![0.0; n_frames * 2]);
    let mut weight = vec![0.0f32; n_frames];

    let overlap = SEGMENT_FRAMES / 4;
    let stride = SEGMENT_FRAMES - overlap;
    let n_chunks = (n_frames - 1) / stride + 1;

    let mut chunk = vec![0.0f32; 2 * SEGMENT_FRAMES];
    let mut start = 0usize;
    for idx in 0..n_chunks {
        // a request that matters more (a deck waiting for its stems) asks this one to stop between two segments
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(StemError(CANCELLED.into()));
        }
        let end = (start + SEGMENT_FRAMES).min(n_frames);
        let len = end - start;

        chunk.fill(0.0); // padding stays zero *after* normalisation, as in demucs
        for i in 0..len {
            chunk[i] = (audio[(start + i) * 2] - norm.mean) / norm.std;
            chunk[SEGMENT_FRAMES + i] = (audio[(start + i) * 2 + 1] - norm.mean) / norm.std;
        }

        let out = infer(&chunk)?;
        if out.len() != OUTPUT_LEN {
            return Err(StemError(format!(
                "stem engine returned {} values for a segment, expected {OUTPUT_LEN} (4 stems × 2 channels × {SEGMENT_FRAMES})",
                out.len())));
        }

        for i in 0..len {
            let w = triangle_weight(i, len);
            for (s, stem) in stems.iter_mut().enumerate() {
                let base = s * 2 * SEGMENT_FRAMES;
                stem[(start + i) * 2] += out[base + i] * norm.std * w;
                stem[(start + i) * 2 + 1] += out[base + SEGMENT_FRAMES + i] * norm.std * w;
            }
            weight[start + i] += w;
        }

        if let Some(p) = progress {
            p.store(((idx + 1) * 99 / n_chunks).min(99) as u32, Ordering::Relaxed);
        }
        if end >= n_frames {
            break;
        }
        start += stride;
    }

    for stem in &mut stems {
        for (i, w) in weight.iter().enumerate() {
            let inv = 1.0 / w.max(1e-10);
            stem[2 * i] *= inv;
            stem[2 * i + 1] *= inv;
        }
    }
    if let Some(p) = progress {
        p.store(100, Ordering::Relaxed);
    }

    let [drums, bass, other, vocals] = stems;
    Ok(Arc::new(TrackStems { drums, bass, other, vocals, sample_rate: SAMPLE_RATE }))
}

/// Triangle fade-in / fade-out weight for frame `i` of a `len`-frame segment.
#[inline]
fn triangle_weight(i: usize, len: usize) -> f32 {
    let half = len / 2;
    if i < half {
        (i + 1) as f32 / (half + 1) as f32
    } else {
        (len - i) as f32 / (half + 1) as f32
    }
}

fn to_stereo(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels >= 2 {
        samples.to_vec()
    } else {
        let mut v = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            v.push(s);
            v.push(s);
        }
        v
    }
}

fn resample_stereo(samples: &[f32], from_sr: u32, to_sr: u32) -> Vec<f32> {
    let n_in = samples.len() / 2;
    let n_out = ((n_in as u64 * to_sr as u64) / from_sr as u64) as usize;
    let ratio = from_sr as f64 / to_sr as f64;
    let mut out = Vec::with_capacity(n_out * 2);
    for i in 0..n_out {
        let src = i as f64 * ratio;
        let s0 = src as usize;
        let s1 = (s0 + 1).min(n_in - 1);
        let t = (src - s0 as f64) as f32;
        out.push(samples[s0 * 2] + (samples[s1 * 2] - samples[s0 * 2]) * t);
        out.push(samples[s0 * 2 + 1] + (samples[s1 * 2 + 1] - samples[s0 * 2 + 1]) * t);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in model: every stem gets a quarter of the segment, so the four stems sum back to it.
    fn quarter(chunk: &[f32]) -> Result<Vec<f32>, StemError> {
        let mut out = Vec::with_capacity(OUTPUT_LEN);
        for _ in 0..N_STEMS {
            out.extend(chunk.iter().map(|v| v / 4.0));
        }
        Ok(out)
    }

    fn signal(frames: usize, dc: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f32 / 44_100.0;
                [
                    dc + 0.3 * (t * 440.0 * std::f32::consts::TAU).sin(),
                    dc + 0.2 * (t * 97.0 * std::f32::consts::TAU).sin(),
                ]
            })
            .collect()
    }

    fn sum_of_stems(t: &TrackStems) -> Vec<f32> {
        (0..t.drums.len()).map(|i| t.drums[i] + t.bass[i] + t.other[i] + t.vocals[i]).collect()
    }

    #[test]
    fn stems_sum_back_to_the_track_minus_its_dc_offset_across_segment_boundaries() {
        // 2.5 segments: three chunks, the last one short and zero-padded.
        let audio = signal(SEGMENT_FRAMES * 5 / 2, 0.1);
        let stems = separate_chunked(&audio, SAMPLE_RATE, 2, None, None, quarter).unwrap();
        let sum = sum_of_stems(&stems);
        assert_eq!(sum.len(), audio.len());
        let mean = Normaliser::of(&audio).mean;
        let worst = audio.iter().zip(&sum).map(|(a, s)| (a - mean - s).abs()).fold(0.0f32, f32::max);
        assert!(worst < 2e-4, "overlap-add distorted the signal: worst error {worst}");
    }

    #[test]
    fn normalisation_is_once_per_track_not_per_segment() {
        // Loud first half, near-silent second half. Per-segment scaling would hand the model a full-scale
        // signal in the quiet part; whole-track scaling keeps it quiet.
        let mut audio = signal(SEGMENT_FRAMES * 3, 0.0);
        let half = audio.len() / 2;
        for v in &mut audio[half..] {
            *v *= 0.01;
        }
        let mut seg_rms = Vec::new();
        separate_chunked(&audio, SAMPLE_RATE, 2, None, None, |c| {
            seg_rms.push((c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt());
            quarter(c)
        })
        .unwrap();
        let (first, last) = (seg_rms[0], *seg_rms.last().unwrap());
        assert!(first > 0.5, "loud segment should be near unit scale, got {first}");
        assert!(last < first * 0.1, "quiet tail must stay quiet, got {last} vs {first}");
    }

    #[test]
    fn dc_offset_is_removed_before_the_model_sees_the_audio() {
        let audio = signal(SEGMENT_FRAMES, 0.25);
        let mut mean_seen = f32::NAN;
        separate_chunked(&audio, SAMPLE_RATE, 2, None, None, |c| {
            mean_seen = c[..SEGMENT_FRAMES].iter().sum::<f32>() / SEGMENT_FRAMES as f32;
            quarter(c)
        })
        .unwrap();
        assert!(mean_seen.abs() < 0.01, "model input still carries DC: {mean_seen}");
    }

    #[test]
    fn mono_and_other_sample_rates_are_accepted() {
        let mono: Vec<f32> = signal(48_000, 0.0).iter().step_by(2).copied().collect();
        let stems = separate_chunked(&mono, 48_000, 1, None, None, quarter).unwrap();
        assert_eq!(stems.sample_rate, SAMPLE_RATE);
        let expected = 48_000u64 * SAMPLE_RATE as u64 / 48_000;
        assert_eq!(stems.drums.len() / 2, expected as usize);
    }

    #[test]
    fn empty_input_and_engine_errors_are_handled() {
        let p = AtomicU32::new(0);
        let empty = separate_chunked(&[], SAMPLE_RATE, 2, Some(&p), None, quarter).unwrap();
        assert!(empty.drums.is_empty());
        assert_eq!(p.load(Ordering::Relaxed), 100);

        let err = separate_chunked(&signal(1000, 0.0), SAMPLE_RATE, 2, None, None, |_| Err(StemError("boom".into())));
        assert_eq!(err.err().unwrap().0, "boom");
        let short = separate_chunked(&signal(1000, 0.0), SAMPLE_RATE, 2, None, None, |_| Ok(vec![0.0; 10]));
        assert!(short.err().unwrap().0.contains("expected"));
    }

    #[test]
    fn progress_reaches_one_hundred() {
        let p = AtomicU32::new(0);
        separate_chunked(&signal(SEGMENT_FRAMES * 2, 0.0), SAMPLE_RATE, 2, Some(&p), None, quarter).unwrap();
        assert_eq!(p.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn silent_audio_does_not_blow_up() {
        let stems = separate_chunked(&vec![0.0; 20_000], SAMPLE_RATE, 2, None, None, quarter).unwrap();
        assert!(stems.vocals.iter().all(|v| v.is_finite() && *v == 0.0));
    }

    #[test]
    fn a_cancelled_separation_stops_between_segments_and_says_so() {
        let cancel = AtomicBool::new(false);
        let mut segments = 0;
        // three segments of audio; the request to stop comes while the first is being separated
        let r = separate_chunked(&signal(SEGMENT_FRAMES * 3, 0.0), SAMPLE_RATE, 2, None, Some(&cancel), |c| {
            segments += 1;
            cancel.store(true, Ordering::Relaxed);
            quarter(c)
        });
        let e = r.err().expect("cancelled");
        assert_eq!(e.0, CANCELLED);
        assert!(crate::stems::is_cancelled(&e));
        assert_eq!(segments, 1, "it went on after the request");
    }
}
