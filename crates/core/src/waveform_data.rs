//! What the waveform display draws from: a pyramid of summaries of the track, made once and kept in the cache.
//!
//! Each column of a level covers a fixed number of frames and holds
//!
//! - the track's real envelope there: the lowest and highest sample on either channel (height),
//! - how loud it is: the RMS of the signal (the solid core of the shape),
//! - how that loudness divides between low, mid and high (the colour).
//!
//! The bands split at [`LOW_CUT_HZ`] and [`HIGH_CUT_HZ`] with 4th-order Linkwitz-Riley filters, so a kick lands in the
//! low band (rekordbox's own 3-band waveform was measured to follow a band that ends near 200 Hz with a correlation of
//! 0.86 to 0.95; the 172 - 860 Hz band this replaces had none) and one scale is shared by all three, so the bands can
//! be compared with each other. Each channel is measured on its own and the larger value kept: a left-right average
//! lets out-of-phase stereo cancel and a loud drop go quiet on screen.
//!
//! Level 0 has a column every [`BASE_FRAMES`] frames and each level above covers [`FACTOR`] times as many, so a view
//! at any zoom reads from a level with about one column per pixel and never from the audio. Values are stored as
//! bytes with a square-root curve, which keeps quiet passages visible, about 1 MB for a four-minute track.

use serde::{Deserialize, Serialize};

/// Bump when the numbers in a summary change, so older ones are made again.
pub const WAVE_VERSION: u8 = 1;
/// Frames in a column of the finest level (1.5 ms at 44.1 kHz).
pub const BASE_FRAMES: usize = 64;
/// Each level covers this many times as many frames per column as the one below.
pub const FACTOR: usize = 4;
/// Levels in a summary: 64, 256, 1024, 4096 and 16384 frames per column.
pub const LEVELS: usize = 5;
/// The bass band ends here.
pub const LOW_CUT_HZ: f64 = 200.0;
/// The treble band starts here.
pub const HIGH_CUT_HZ: f64 = 2500.0;

/// What a column of a level holds, in the byte form it is stored in. Read it with [`Column::decode`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub min: i8,
    pub max: i8,
    pub rms: u8,
    pub low: u8,
    pub mid: u8,
    pub high: u8,
}

/// A column, or several joined, in plain numbers: amplitudes where 1.0 is full scale.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Level {
    pub min: f32,
    pub max: f32,
    pub rms: f32,
    /// RMS of the low, mid and high bands.
    pub bands: [f32; 3],
}

impl Level {
    /// Largest distance from zero.
    pub fn peak(&self) -> f32 {
        self.max.max(-self.min)
    }

    /// Joins `other` onto this one as if the two were one longer column. `weight` is how many columns of the same
    /// size this one stands for already, so the energy of a long run counts every column in it equally.
    pub fn merge(&mut self, other: &Level, weight: f32) {
        let w = weight / (weight + 1.0);
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        let sq = |a: f32, b: f32| (a * a * w + b * b * (1.0 - w)).sqrt();
        self.rms = sq(self.rms, other.rms);
        for k in 0..3 {
            self.bands[k] = sq(self.bands[k], other.bands[k]);
        }
    }
}

const fn square_table() -> [f32; 256] {
    let mut t = [0.0f32; 256];
    let mut i = 0;
    while i < 256 {
        let v = i as f32 / 255.0;
        t[i] = v * v;
        i += 1;
    }
    t
}
static SQUARES: [f32; 256] = square_table();

fn encode_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0).sqrt() * 255.0).round() as u8
}
fn decode_u8(b: u8) -> f32 {
    SQUARES[b as usize]
}
fn encode_i8(v: f32) -> i8 {
    (v.abs().clamp(0.0, 1.0).sqrt() * 127.0).round() as i8 * if v < 0.0 { -1 } else { 1 }
}
fn decode_i8(b: i8) -> f32 {
    let m = SQUARES[(b.unsigned_abs().min(127) as usize) * 255 / 127];
    if b < 0 {
        -m
    } else {
        m
    }
}

impl Column {
    pub fn encode(l: &Level) -> Self {
        Column {
            min: encode_i8(l.min),
            max: encode_i8(l.max),
            rms: encode_u8(l.rms),
            low: encode_u8(l.bands[0]),
            mid: encode_u8(l.bands[1]),
            high: encode_u8(l.bands[2]),
        }
    }

    pub fn decode(&self) -> Level {
        Level {
            min: decode_i8(self.min),
            max: decode_i8(self.max),
            rms: decode_u8(self.rms),
            bands: [decode_u8(self.low), decode_u8(self.mid), decode_u8(self.high)],
        }
    }
}

/// One resolution of the summary.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WaveLevel {
    pub frames_per_col: u32,
    pub cols: Vec<Column>,
}

/// The whole pyramid for one track.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WaveSummary {
    pub version: u8,
    pub sample_rate: u32,
    pub total_frames: u64,
    /// Finest first.
    pub levels: Vec<WaveLevel>,
    /// A loud, steady value for the track's peaks (the 99.5th percentile), for scaling the display.
    pub ref_peak: f32,
}

impl WaveSummary {
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty() || self.total_frames == 0
    }

    /// Summarises interleaved `samples`.
    pub fn build(samples: &[f32], channels: usize, sample_rate: u32) -> Self {
        Self::build_with(samples, channels, sample_rate, true)
    }

    /// The same without the bass / mid / high split (the bands are left at zero): about twenty times quicker, for what
    /// is drawn in one colour, like a stem's lane.
    pub fn build_envelope(samples: &[f32], channels: usize, sample_rate: u32) -> Self {
        Self::build_with(samples, channels, sample_rate, false)
    }

    fn build_with(samples: &[f32], channels: usize, sample_rate: u32, bands: bool) -> Self {
        let ch = channels.max(1);
        let total = (samples.len() / ch) as u64;
        if total == 0 || sample_rate == 0 {
            return WaveSummary { version: WAVE_VERSION, sample_rate, ..Default::default() };
        }
        let base = base_level(samples, ch, sample_rate, bands);
        let mut levels = Vec::with_capacity(LEVELS);
        levels.push(WaveLevel { frames_per_col: BASE_FRAMES as u32, cols: base });
        for i in 1..LEVELS {
            let below = &levels[i - 1];
            let cols = below
                .cols
                .chunks(FACTOR)
                .map(|group| {
                    let mut joined = group[0].decode();
                    for (n, c) in group[1..].iter().enumerate() {
                        joined.merge(&c.decode(), (n + 1) as f32);
                    }
                    Column::encode(&joined)
                })
                .collect();
            levels.push(WaveLevel { frames_per_col: below.frames_per_col * FACTOR as u32, cols });
        }
        let mut peaks: Vec<f32> = levels[0].cols.iter().map(|c| c.decode().peak()).collect();
        let at = ((peaks.len() as f32 * 0.995) as usize).min(peaks.len().saturating_sub(1));
        let ref_peak = if peaks.is_empty() { 1.0 } else { *peaks.select_nth_unstable_by(at, f32::total_cmp).1 };
        WaveSummary { version: WAVE_VERSION, sample_rate, total_frames: total, levels, ref_peak }
    }

    /// `cols` columns of the bass, mid and high loudness over the whole track, as bytes in the same square-root form
    /// the summary keeps them in: what the library list draws its small waveforms from.
    pub fn preview(&self, cols: usize) -> Vec<[u8; 3]> {
        let Some(level) = self.level_for(self.total_frames as f64 / cols.max(1) as f64) else {
            return vec![[0; 3]; cols];
        };
        let step = self.total_frames as f64 / cols as f64;
        (0..cols)
            .map(|i| {
                let c = Column::encode(&level.span(i as f64 * step, (i + 1) as f64 * step));
                [c.low, c.mid, c.high]
            })
            .collect()
    }

    /// The level whose columns are closest to, but not wider than, `frames_per_pixel`: drawing from it needs about one
    /// column per pixel. Zoomed in further than the finest, that is the finest.
    pub fn level_for(&self, frames_per_pixel: f64) -> Option<&WaveLevel> {
        let first = self.levels.first()?;
        Some(self.levels.iter().rev().find(|l| l.frames_per_col as f64 <= frames_per_pixel).unwrap_or(first))
    }
}

impl WaveLevel {
    /// Everything between frames `from` and `to` (`to` exclusive), joined. At least one column is always read, so a
    /// span narrower than a column shows that column.
    pub fn span(&self, from: f64, to: f64) -> Level {
        if self.cols.is_empty() {
            return Level::default();
        }
        let fpc = self.frames_per_col as f64;
        let last = self.cols.len() - 1;
        let a = ((from / fpc).floor().max(0.0) as usize).min(last);
        let b = (((to / fpc).ceil() as usize).max(a + 1)).min(last + 1);
        let mut joined = self.cols[a].decode();
        for (n, c) in self.cols[a + 1..b].iter().enumerate() {
            joined.merge(&c.decode(), (n + 1) as f32);
        }
        joined
    }
}

// ── analysis ─────────────────────────────────────────────────────────────────

/// A second-order Butterworth section.
#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn new(hz: f64, sample_rate: f64, high_pass: bool) -> Self {
        let w = 2.0 * std::f64::consts::PI * hz.min(sample_rate * 0.45) / sample_rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = if high_pass {
            ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0)
        } else {
            ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0)
        };
        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: -2.0 * cos / a0, a2: (1.0 - alpha) / a0, z1: 0.0, z2: 0.0 }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// A 4th-order Linkwitz-Riley filter: two Butterworth sections in a row.
#[derive(Clone, Copy)]
struct Lr4([Biquad; 2]);

impl Lr4 {
    fn new(hz: f64, sample_rate: f64, high_pass: bool) -> Self {
        let b = Biquad::new(hz, sample_rate, high_pass);
        Lr4([b, b])
    }
    fn run(&mut self, x: f64) -> f64 {
        let y = self.0[0].run(x);
        self.0[1].run(y)
    }
}

/// The three bands of one channel.
struct Splitter {
    low: Lr4,
    mid_hp: Lr4,
    mid_lp: Lr4,
    high: Lr4,
}

impl Splitter {
    fn new(sample_rate: f64) -> Self {
        Splitter {
            low: Lr4::new(LOW_CUT_HZ, sample_rate, false),
            mid_hp: Lr4::new(LOW_CUT_HZ, sample_rate, true),
            mid_lp: Lr4::new(HIGH_CUT_HZ, sample_rate, false),
            high: Lr4::new(HIGH_CUT_HZ, sample_rate, true),
        }
    }

    fn run(&mut self, x: f64) -> [f64; 3] {
        [self.low.run(x), self.mid_lp.run(self.mid_hp.run(x)), self.high.run(x)]
    }
}

/// What one channel did in one column.
#[derive(Clone, Copy, Default)]
struct Acc {
    min: f32,
    max: f32,
    sq: f64,
    band_sq: [f64; 3],
}

fn base_level(samples: &[f32], ch: usize, sample_rate: u32, bands: bool) -> Vec<Column> {
    let frames = samples.len() / ch;
    let cols = frames.div_ceil(BASE_FRAMES);
    let mut out = Vec::with_capacity(cols);
    let used = ch.min(2);
    let mut split: Vec<Splitter> =
        if bands { (0..used).map(|_| Splitter::new(sample_rate as f64)).collect() } else { Vec::new() };
    for block in samples.chunks(BASE_FRAMES * ch) {
        let n = block.len() / ch;
        let mut acc = [Acc::default(); 2];
        for frame in block.chunks_exact(ch) {
            for (c, a) in acc.iter_mut().enumerate().take(used) {
                // A garbage sample must not spread through the filters.
                let x = if frame[c].is_finite() { frame[c] } else { 0.0 };
                a.min = a.min.min(x);
                a.max = a.max.max(x);
                a.sq += (x as f64) * (x as f64);
                if bands {
                    let b = split[c].run(x as f64);
                    for k in 0..3 {
                        a.band_sq[k] += b[k] * b[k];
                    }
                }
            }
        }
        let inv = 1.0 / n.max(1) as f64;
        let mut l = Level::default();
        for a in &acc[..used] {
            l.min = l.min.min(a.min);
            l.max = l.max.max(a.max);
            l.rms = l.rms.max((a.sq * inv).sqrt() as f32);
            for k in 0..3 {
                l.bands[k] = l.bands[k].max((a.band_sq[k] * inv).sqrt() as f32);
            }
        }
        out.push(Column::encode(&l));
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn envelope_only_summary_has_the_same_shape_and_no_bands() {
        let sr = 44100u32;
        let x: Vec<f32> = (0..sr as usize * 2)
            .flat_map(|i| {
                let v = 0.6 * (i as f32 * 440.0 * std::f32::consts::TAU / sr as f32).sin();
                [v, -v * 0.5]
            })
            .collect();
        let (full, env) = (WaveSummary::build(&x, 2, sr), WaveSummary::build_envelope(&x, 2, sr));
        assert_eq!(full.levels.len(), env.levels.len());
        for (a, b) in full.levels.iter().zip(&env.levels) {
            assert_eq!(a.cols.len(), b.cols.len());
            for (ca, cb) in a.cols.iter().zip(&b.cols) {
                assert_eq!((ca.min, ca.max, ca.rms), (cb.min, cb.max, cb.rms));
                assert_eq!((cb.low, cb.mid, cb.high), (0, 0, 0));
            }
        }
        assert_eq!(full.ref_peak, env.ref_peak);
    }

    use super::*;
    use std::f32::consts::TAU;

    const SR: u32 = 44_100;

    fn tone(hz: f32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(SR as f32 * secs) as usize)
            .flat_map(|i| {
                let v = (TAU * hz * i as f32 / SR as f32).sin() * amp;
                [v, v]
            })
            .collect()
    }

    fn whole(s: &WaveSummary) -> Level {
        s.levels[0].span(0.0, s.total_frames as f64)
    }

    #[test]
    fn a_kick_is_low_a_vocal_is_mid_and_a_hat_is_high() {
        for (hz, band) in [(55.0, 0), (1000.0, 1), (8000.0, 2)] {
            let s = WaveSummary::build(&tone(hz, 1.0, 0.5), 2, SR);
            let l = s.levels[0].span(SR as f64 * 0.3, SR as f64 * 0.7);
            let own = l.bands[band];
            let others: f32 = (0..3).filter(|&k| k != band).map(|k| l.bands[k]).fold(0.0, f32::max);
            assert!(own > 0.25 && own > others * 8.0, "{hz} Hz: bands {:?}", l.bands);
        }
    }

    #[test]
    fn the_envelope_is_the_samples_own_peaks() {
        let s = WaveSummary::build(&tone(1000.0, 0.5, 0.6), 2, SR);
        let l = whole(&s);
        assert!((l.max - 0.6).abs() < 0.03 && (l.min + 0.6).abs() < 0.03, "{l:?}");
        assert!((l.rms - 0.6 / 2f32.sqrt()).abs() < 0.03, "{}", l.rms);
    }

    #[test]
    fn out_of_phase_stereo_does_not_cancel() {
        let mono = tone(100.0, 1.0, 0.5);
        let opposed: Vec<f32> = mono.chunks(2).flat_map(|f| [f[0], -f[1]]).collect();
        let (a, b) = (WaveSummary::build(&mono, 2, SR), WaveSummary::build(&opposed, 2, SR));
        let (a, b) = (a.levels[0].span(20_000.0, 30_000.0), b.levels[0].span(20_000.0, 30_000.0));
        assert!((a.bands[0] - b.bands[0]).abs() < 0.02 && b.bands[0] > 0.2, "{a:?} {b:?}");
    }

    #[test]
    fn a_click_is_in_the_column_it_happens_in() {
        let mut x = vec![0.0f32; SR as usize * 2];
        let at = 33_333usize;
        x[at * 2] = 0.9;
        x[at * 2 + 1] = 0.9;
        let s = WaveSummary::build(&x, 2, SR);
        for lvl in &s.levels {
            let fpc = lvl.frames_per_col as usize;
            let loud: Vec<usize> = lvl.cols.iter().enumerate().filter(|(_, c)| c.max > 0).map(|(i, _)| i).collect();
            assert_eq!(loud, vec![at / fpc], "{fpc} frames per column");
        }
    }

    #[test]
    fn each_level_agrees_with_the_one_below() {
        let mut x = tone(80.0, 3.0, 0.4);
        for i in (0..x.len()).step_by(20_000) {
            x[i] = 0.95;
        }
        let s = WaveSummary::build(&x, 2, SR);
        assert_eq!(s.levels.len(), LEVELS);
        for w in s.levels.windows(2) {
            assert_eq!(w[1].frames_per_col, w[0].frames_per_col * FACTOR as u32);
            assert_eq!(w[1].cols.len(), w[0].cols.len().div_ceil(FACTOR));
            for (i, up) in w[1].cols.iter().enumerate() {
                let kids = &w[0].cols[i * FACTOR..((i + 1) * FACTOR).min(w[0].cols.len())];
                assert_eq!(up.max, kids.iter().map(|c| c.max).max().unwrap());
                assert_eq!(up.min, kids.iter().map(|c| c.min).min().unwrap());
            }
        }
        // the loud clicks survive to the coarsest level
        assert!(decode_i8(s.levels[LEVELS - 1].cols.iter().map(|c| c.max).max().unwrap()) > 0.9);
    }

    #[test]
    fn the_right_level_is_chosen_for_each_zoom() {
        let s = WaveSummary::build(&tone(100.0, 1.0, 0.3), 2, SR);
        assert_eq!(s.level_for(32.0).unwrap().frames_per_col, 64);
        assert_eq!(s.level_for(256.0).unwrap().frames_per_col, 256);
        assert_eq!(s.level_for(900.0).unwrap().frames_per_col, 256);
        assert_eq!(s.level_for(5000.0).unwrap().frames_per_col, 4096);
        assert_eq!(s.level_for(1e9).unwrap().frames_per_col, 16384);
    }

    #[test]
    fn a_preview_has_the_columns_asked_for_and_the_right_band_in_each() {
        let s = WaveSummary::build(&tone(60.0, 4.0, 0.5), 2, SR);
        let p = s.preview(160);
        assert_eq!(p.len(), 160);
        assert!(p[80][0] > 100 && p[80][2] < 30, "{:?}", p[80]);
        assert_eq!(WaveSummary::default().preview(8), vec![[0u8; 3]; 8]);
    }

    #[test]
    fn bytes_keep_quiet_detail() {
        for v in [0.0, 0.001, 0.01, 0.1, 0.5, 1.0] {
            let back = decode_u8(encode_u8(v));
            assert!((back - v).abs() <= 0.02f32.max(v * 0.05), "{v} -> {back}");
        }
        assert_eq!(decode_i8(encode_i8(-0.5)).signum(), -1.0);
        assert!((decode_i8(encode_i8(-0.5)) + 0.5).abs() < 0.02);
    }

    #[test]
    fn it_survives_a_round_trip_through_the_cache_format() {
        let s = WaveSummary::build(&tone(60.0, 1.0, 0.5), 2, SR);
        let bytes = postcard::to_allocvec(&s).unwrap();
        assert_eq!(postcard::from_bytes::<WaveSummary>(&bytes).unwrap(), s);
        eprintln!("1 s of stereo audio: {} bytes", bytes.len());
    }

    #[test]
    fn odd_audio_gets_a_summary_without_panicking() {
        assert!(WaveSummary::build(&[], 2, SR).is_empty());
        assert!(WaveSummary::build(&[0.5], 2, SR).is_empty());
        let s = WaveSummary::build(&[0.3; 10], 1, SR);
        assert!(!s.is_empty());
        let silent = WaveSummary::build(&vec![0.0; 100_000], 2, SR);
        assert_eq!(whole(&silent).peak(), 0.0);
        let mut bad = tone(100.0, 0.5, 0.4);
        bad[100] = f32::NAN;
        bad[101] = f32::INFINITY;
        let s = WaveSummary::build(&bad, 2, SR);
        assert!(whole(&s).peak() < 1.01 && whole(&s).bands.iter().all(|b| b.is_finite()));
        let six = WaveSummary::build(&vec![0.1; 6 * 5000], 6, SR);
        assert!(!six.is_empty());
        assert_eq!(six.total_frames, 5000);
    }
}
