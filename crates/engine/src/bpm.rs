use rustfft::{num_complex::Complex, FftPlanner};

use crate::deck::WAVEFORM_CHUNK_FRAMES;

// Onset analysis runs on the raw audio with a hop equal to the waveform chunk, so
// onset frame `i` and waveform chunk `i` describe the same instant.
const FFT_N: usize = 512;
const HOP: usize = WAVEFORM_CHUNK_FRAMES;
const MIN_BPM: f64 = 68.0;
const MAX_BPM: f64 = 185.0;
// Log-gaussian tempo prior (Ellis 2007) resolves octave ambiguity toward DJ tempos.
const PRIOR_CENTRE_BPM: f64 = 120.0;
const PRIOR_SIGMA_OCT: f64 = 1.2;
const FOLD_BINS: usize = 128;
/// Kick between the beats must be this many times stronger than on them for the grid to be moved half a beat.
const HALF_BEAT_RATIO: f32 = 1.6;
/// The kick between the beats only has to be this much stronger than on them when each third of the track says so too
/// (Beatbox - Radio Beat reads 1.42; the grids that are on the kick read 1.05 or less).
const HALF_BEAT_RATIO_STEADY: f32 = 1.25;
// Flux peaks lead the true attack slightly because the analysis window looks ahead.
const ONSET_LEAD_FRAMES: f64 = 0.0;
/// The first beat of the track may be this much of a beat before the first sound.
const FIRST_BEAT_BEFORE_SOUND: f64 = 0.6;

struct Onsets {
    /// Detrended, mean-normalised broadband + low-band spectral flux.
    combined: Vec<f32>,
    /// Detrended, mean-normalised flux of the kick region only.
    low: Vec<f32>,
    /// RMS per hop — used to find where the music actually starts.
    rms: Vec<f32>,
    /// Loudness (not log, not detrended) of the kick's punch, about 90-260 Hz, and of its body, about 350 Hz-2 kHz,
    /// per hop: a kick rises in both at once, a bass note only in the first and a hi-hat in neither.
    punch: Vec<f32>,
    body: Vec<f32>,
}

/// The broadband-plus-kick onset curve, for judging a grid with [`grid_quality`] (the same curve the tempo fit uses).
pub fn onset_curve_full(samples: &[f32], channels: usize, sample_rate: u32) -> (Vec<f32>, f64) {
    let on = compute_onsets(samples, channels, sample_rate);
    (on.combined, HOP as f64 / sample_rate as f64)
}

/// The kick-region onset-strength curve of the audio, one value per `frame_secs`, for placing a beat grid on the
/// music (see [`crate::tap::snap_to_onsets`]).
pub fn onset_curve(samples: &[f32], channels: usize, sample_rate: u32) -> (Vec<f32>, f64) {
    let on = compute_onsets(samples, channels, sample_rate);
    (on.low, HOP as f64 / sample_rate as f64)
}

/// How well a beat grid sits on the music, measured against the kick onsets (see [`grid_quality`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridQuality {
    pub verdict: GridVerdict,
    /// Beats that have a clear kick near them / beats between the first and last such kick.
    pub hit_rate: f32,
    pub hits: usize,
    /// Typical distance of the kicks from the grid, ms. Positive: the music is later than the grid.
    pub bias_ms: f32,
    /// Scatter of the kicks around the grid, ms.
    pub spread_ms: f32,
    /// Bias at the end of the track minus bias at the start, ms: tempo error shows up here.
    pub drift_ms: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridVerdict {
    /// Kicks sit on the grid from start to end.
    Good,
    /// Close, but loose, early/late by several ms, or drifting: worth a look.
    Check,
    /// Clearly off the kicks, or drifting off them.
    Off,
    /// Too few clear kicks to say (ambient, breakbeat, a very short file).
    Unknown,
}

/// An onset this strong (the curves are normalised to a mean of 1, broadband plus kick band = 2) counts as a hit.
const CLEAR_ONSET: f32 = 4.0;

/// Measures how well the grid `bpm` / `offset_chunks` (position of a beat, in waveform chunks) sits on the kick
/// onsets `onset` (one value per chunk, from [`onset_curve_full`]). For every beat the strongest onset within
/// +-40 ms is taken; the median distance is the bias, the median deviation the spread, and comparing the first third
/// of the track with the last gives the drift.
pub fn grid_quality(onset: &[f32], frame_secs: f64, bpm: f32, offset_chunks: f64) -> GridQuality {
    let unknown = GridQuality {
        verdict: GridVerdict::Unknown,
        hit_rate: 0.0,
        hits: 0,
        bias_ms: 0.0,
        spread_ms: 0.0,
        drift_ms: 0.0,
    };
    if bpm <= 0.0 || frame_secs <= 0.0 || onset.len() < 8 {
        return unknown;
    }
    let period = 60.0 / bpm as f64 / frame_secs;
    let n = onset.len();
    let first = ((1.0 - offset_chunks) / period).ceil() as i64;
    let last = ((n as f64 - 3.0 - offset_chunks) / period).floor() as i64;
    // (beat index, offset ms) of the strongest onset within +-`w` frames of each beat, where it is a clear one
    let scan = |w: f64| -> Vec<(i64, f64)> {
        let mut hits = Vec::new();
        for k in first..=last {
            let c = offset_chunks + k as f64 * period;
            let (a, b) = ((c - w).ceil().max(1.0) as usize, ((c + w).floor() as usize).min(n - 2));
            if a >= b {
                continue;
            }
            let mut bi = a;
            for i in a..=b {
                if onset[i] > onset[bi] {
                    bi = i;
                }
            }
            if onset[bi] < CLEAR_ONSET {
                continue;
            }
            let (y0, y1, y2) = (onset[bi - 1] as f64, onset[bi] as f64, onset[bi + 1] as f64);
            let denom = y0 - 2.0 * y1 + y2;
            let d = if denom.abs() > 1e-12 { (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5) } else { 0.0 };
            hits.push((k, (bi as f64 + d - c) * frame_secs * 1000.0));
        }
        hits
    };
    let rate =
        |h: &[(i64, f64)]| h.len() as f64 / (h.last().map_or(0, |l| l.0) - h.first().map_or(0, |f| f.0) + 1) as f64;
    let mut hits = scan((0.040 / frame_secs).max(2.0));
    if hits.len() < 16 || rate(&hits) < 0.3 {
        // Nothing near the grid. If the kicks are all one steady distance away, the grid is simply in the wrong place.
        let wide = scan((period * 0.45).max(2.0));
        if wide.len() >= 16 && rate(&wide) >= 0.5 {
            let mut v: Vec<f64> = wide.iter().map(|h| h.1).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let m = v[v.len() / 2];
            let mut dev: Vec<f64> = v.iter().map(|x| (x - m).abs()).collect();
            dev.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            if dev[dev.len() / 2] * 1.4826 < 10.0 {
                return GridQuality {
                    verdict: GridVerdict::Off,
                    hit_rate: rate(&wide) as f32,
                    hits: wide.len(),
                    bias_ms: m as f32,
                    spread_ms: (dev[dev.len() / 2] * 1.4826) as f32,
                    drift_ms: 0.0,
                };
            }
        }
        return GridQuality {
            hits: hits.len(),
            hit_rate: if hits.len() > 1 { rate(&hits) as f32 } else { 0.0 },
            ..unknown
        };
    }
    let hit_rate = rate(&hits);
    hits.shrink_to_fit();

    let med = |v: &mut Vec<f64>| -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    };
    let third = hits.len() / 3;
    let mut head: Vec<f64> = hits[..third].iter().map(|h| h.1).collect();
    let mut tail: Vec<f64> = hits[hits.len() - third..].iter().map(|h| h.1).collect();
    let (h0, t0) = (med(&mut head), med(&mut tail));
    let drift = t0 - h0;
    // Scatter is measured around the straight line between the start and the end, so a slightly wrong tempo shows
    // up as drift rather than as scatter.
    let (k0, k1) = (hits[third / 2].0 as f64, hits[hits.len() - 1 - third / 2].0 as f64);
    let line = |k: i64| h0 + (t0 - h0) * ((k as f64 - k0) / (k1 - k0).max(1.0));
    let mut dev: Vec<f64> = hits.iter().map(|h| (h.1 - line(h.0)).abs()).collect();
    let spread = med(&mut dev) * 1.4826;
    let mut all: Vec<f64> = hits.iter().map(|h| h.1).collect();
    let bias = med(&mut all);

    // Onsets scattered across the whole search window (about 23 ms for pure chance) are not kicks at all.
    if spread > 22.0 {
        return GridQuality {
            hits: hits.len(),
            hit_rate: hit_rate as f32,
            bias_ms: bias as f32,
            spread_ms: spread as f32,
            ..unknown
        };
    }

    // The offset is judged loosely: the onset curve and the grid's attack edge are different measures of "when the
    // kick starts" and differ by 10 ms or more on soft kicks, whereas scatter and drift are exact.
    let verdict = if bias.abs() > 45.0 || drift.abs() > 20.0 {
        GridVerdict::Off
    } else if bias.abs() > 25.0 || drift.abs() > 8.0 || spread > 6.0 || hit_rate < 0.6 {
        GridVerdict::Check
    } else {
        GridVerdict::Good
    };
    GridQuality {
        verdict,
        hit_rate: hit_rate as f32,
        hits: hits.len(),
        bias_ms: bias as f32,
        spread_ms: spread as f32,
        drift_ms: drift as f32,
    }
}

/// How far an imported grid (rekordbox's) sits from the kicks of the audio as Dubplate decodes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImportOffset {
    /// Median distance of the kicks from the grid's beats, ms. Positive: the music is later than the grid.
    pub median_ms: f64,
    pub spread_ms: f64,
    pub hits: usize,
}

/// Measures where the kicks of `onset` (from [`onset_curve_full`]) fall relative to a grid, searching +-80 ms around
/// every beat. This is what shows an MP3 decoder offset: rekordbox keeps the encoder delay at the start of an MP3 and
/// other decoders strip it, so the same grid sits a frame or two (26 or 51 ms) from the music.
pub fn measure_import_offset(onset: &[f32], frame_secs: f64, bpm: f32, offset_chunks: f64) -> Option<ImportOffset> {
    if bpm <= 0.0 || frame_secs <= 0.0 || onset.len() < 8 {
        return None;
    }
    let period = 60.0 / bpm as f64 / frame_secs;
    let w = (0.080 / frame_secs).min(period * 0.45).max(2.0);
    let n = onset.len();
    let first = ((1.0 - offset_chunks) / period).ceil() as i64;
    let last = ((n as f64 - 3.0 - offset_chunks) / period).floor() as i64;
    let mut d: Vec<f64> = Vec::new();
    for k in first..=last {
        let c = offset_chunks + k as f64 * period;
        let (a, b) = ((c - w).ceil().max(1.0) as usize, ((c + w).floor() as usize).min(n - 2));
        if a >= b {
            continue;
        }
        let mut bi = a;
        for i in a..=b {
            if onset[i] > onset[bi] {
                bi = i;
            }
        }
        if onset[bi] < CLEAR_ONSET {
            continue;
        }
        let (y0, y1, y2) = (onset[bi - 1] as f64, onset[bi] as f64, onset[bi + 1] as f64);
        let den = y0 - 2.0 * y1 + y2;
        let dl = if den.abs() > 1e-12 { (0.5 * (y0 - y2) / den).clamp(-0.5, 0.5) } else { 0.0 };
        d.push((bi as f64 + dl - c) * frame_secs * 1000.0);
    }
    if d.len() < 16 {
        return None;
    }
    d.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = d[d.len() / 2];
    let mut dev: Vec<f64> = d.iter().map(|x| (x - med).abs()).collect();
    dev.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(ImportOffset { median_ms: med, spread_ms: dev[dev.len() / 2] * 1.4826, hits: d.len() })
}

/// Known MP3 start offsets in samples: LAME's encoder delay plus the decoder's (576 + 529), one MPEG frame (1152),
/// and the two together.
const MP3_OFFSETS: [f64; 3] = [1105.0, 1152.0, 2257.0];

/// The shift (seconds, positive = later) that puts a rekordbox grid on the music when the gap looks like an MP3
/// decoder offset, else `None`: the grid is then taken exactly as rekordbox has it. A gap is taken for a decoder
/// offset only when it is steady (small scatter) and within 10 ms of 25, 26 or 51 ms; smaller gaps are just where
/// rekordbox puts a beat relative to the kick.
///
/// Heuristic: it has not been checked against pairs of rekordbox grids and the very files they came from.
pub fn mp3_offset_shift(m: &ImportOffset, sample_rate: u32) -> Option<f64> {
    if m.spread_ms > 8.0 {
        return None;
    }
    let ms = |samples: f64| samples / sample_rate as f64 * 1000.0;
    let (best, dist) = MP3_OFFSETS
        .iter()
        .flat_map(|&o| [ms(o), -ms(o)])
        .map(|c| (c, (c - m.median_ms).abs()))
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;
    (dist <= 10.0).then(|| best / 1000.0)
}

/// Analyses the decoded audio and returns `(bpm, beat_offset_chunks)`.
///
/// `beat_offset_chunks` is the position, in waveform chunks, of a **downbeat**
/// (beat 1 of a bar) near the start of the music, so beat `n` of the grid is
/// `offset + n * period` and `n % 4 == 0` / `n % 16 == 0` are bar / phrase starts.
/// Returns `(0.0, 0.0)` when no tempo can be found.
pub fn analyze_beats(samples: &[f32], channels: usize, sample_rate: u32) -> (f32, f64) {
    let a = analyze_beats_detail(samples, channels, sample_rate);
    (a.bpm, a.offset_chunks)
}

/// What the tempo analysis found and how far to trust it.
#[derive(Debug, Clone, Default)]
pub struct BeatAnalysis {
    pub bpm: f32,
    pub offset_chunks: f64,
    /// Beats the tempo was fitted on (those with a clear attack near the grid).
    pub fitted_beats: usize,
    /// Spread of those attacks around the final grid, in milliseconds (small = steady tempo, grid sits on the music).
    pub spread_ms: f32,
    /// Position in waveform chunks of beat number 0: beat `n` is at `phase_chunks + n * period`. Which beats are
    /// bar lines is `offset_chunks`' answer, or [`crate::downbeat`]'s better one.
    pub phase_chunks: f64,
    /// Strength of the kick found at each beat, `(beat number, strength)`, for judging which beat is the 1.
    pub kick: Vec<(i64, f32)>,
    /// Number of the first beat at or after the start of the music.
    pub start_beat: i64,
    /// How the kicks at half a beat from the grid compare with those on it (1.0: as strong); above about 1.5 the grid
    /// had landed between the beats and was moved.
    pub half_beat_ratio: f32,
    /// The same comparison on the final grid, for the first, middle and last third of the track (between over on):
    /// below 1 in all three is a grid on the kick; above 1 in some and below in others means the tempo is a little
    /// off or the track drifts, so the grid is on the kick in one part and half a beat from it in another.
    pub kick_thirds: [f32; 3],
}

/// Where a genre's tempo usually sits, as DJs count it (dubstep 140, not 70): a range of `half_range_oct` octaves
/// either side of `centre`. Inside the range the hint changes nothing (a dubstep track at 110 stays 110); outside it
/// a tempo is made less likely, which is how 70 loses to 140. It is a nudge for the octave, not a verdict, so a track
/// whose audio clearly says otherwise still wins.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoHint {
    pub centre: f64,
    pub half_range_oct: f64,
}

impl TempoHint {
    /// How much a tempo is believed, 1.0 inside the range, falling by e^-6 per octave outside it.
    pub fn weight(&self, bpm: f64) -> f64 {
        let excess = ((bpm / self.centre).log2().abs() - self.half_range_oct).max(0.0);
        (-6.0 * excess).exp()
    }
}

/// The usual tempo for a genre tag, if the tag names one we know (matched on words, any case, so "Drum & Bass",
/// "drum and bass" and "DnB" all work). Broad genres that cover many tempos (pop, rock, electronic) have none.
pub fn tempo_hint_for_genre(genre: &str) -> Option<TempoHint> {
    let g = genre.to_lowercase();
    let has = |w: &str| g.contains(w);
    let word = |w: &str| g.split(|c: char| !c.is_alphanumeric()).any(|x| x == w);
    let (centre, half) = if has("dubstep") || has("brostep") || has("riddim") {
        (140.0, 0.60)
    } else if has("drum") && has("bass") || word("dnb") || word("dandb") || has("jungle") || has("neurofunk") {
        (174.0, 0.45)
    } else if has("hardstyle") || has("hard dance") || has("gabber") || has("happy hardcore") {
        (150.0, 0.50)
    } else if has("trance") {
        (138.0, 0.40)
    } else if has("psy") && !has("psych") {
        (142.0, 0.35)
    } else if has("techno") {
        (130.0, 0.40)
    } else if has("house") {
        (124.0, 0.35)
    } else if has("garage") || has("2-step") || has("2 step") {
        (132.0, 0.40)
    } else if has("hip hop") || has("hip-hop") || has("hiphop") || has("rap") || word("trap") {
        (92.0, 0.55)
    } else if has("reggaeton") || has("dancehall") {
        (95.0, 0.40)
    } else if has("disco") {
        (119.0, 0.40)
    } else {
        return None;
    };
    Some(TempoHint { centre, half_range_oct: half })
}

pub fn analyze_beats_detail(samples: &[f32], channels: usize, sample_rate: u32) -> BeatAnalysis {
    analyze_beats_hinted(samples, channels, sample_rate, None)
}

/// As [`analyze_beats_detail`], with the genre's usual tempo steering the octave choice (see [`TempoHint`]).
pub fn analyze_beats_hinted(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    hint: Option<TempoHint>,
) -> BeatAnalysis {
    let none = BeatAnalysis::default();
    let on = compute_onsets(samples, channels, sample_rate);
    let n = on.combined.len();
    let fps = sample_rate as f64 / HOP as f64;
    if n < (fps * 8.0) as usize {
        return none;
    }

    let mut candidates = candidate_tempos(&on.combined, fps);
    if candidates.is_empty() {
        return none;
    }
    // The autocorrelation can peak at a dotted or triplet multiple of the beat and miss the beat itself (Pulsar: 91.3
    // and 68.5 found, the true 137 not), so also sweep the whole range and add the usual ratio relatives.
    for c in
        sweep_candidates(&on.combined, fps).into_iter().chain(candidates.clone().into_iter().flat_map(ratio_relatives))
    {
        if !candidates.iter().any(|&o| (o - c).abs() / c < 0.01) {
            candidates.push(c);
        }
    }

    // ── Fine tempo: each candidate is tuned by how much onset mass its beat grid
    //    captures; the tempo prior then settles octave / 3:2 ambiguity. ──────────
    let mut best = (f64::MIN, 0.0f64);
    let mut plain = (f64::MIN, 0.0f64);
    for &c in &candidates {
        let (bpm_c, score) = fine_tune(&on.combined, fps, c);
        let oct = (bpm_c / PRIOR_CENTRE_BPM).log2() / PRIOR_SIGMA_OCT;
        let weighted = score * (-0.5 * oct * oct).exp();
        if weighted > plain.0 {
            plain = (weighted, bpm_c);
        }
        let hinted = weighted * hint.map_or(1.0, |h| h.weight(bpm_c));
        if hinted > best.0 {
            best = (hinted, bpm_c);
        }
    }
    // The genre may only choose the octave: if it would move the tempo by anything but a factor of two, the audio
    // wins (a track tagged Drum & Bass that is really at 104 is not a 138)
    let ratio = best.1 / plain.1;
    if ![0.5, 1.0, 2.0].iter().any(|r| (ratio / r - 1.0).abs() < 0.02) {
        best = plain;
    }
    let bpm = best.1;
    let period = fps * 60.0 / bpm;
    let (_, phase) = fold_score(&on.combined, period);

    // ── Whole-track tempo: a line through where the beats really fall is far more exact than the coarse scan
    //    (0.01 BPM is 40 ms by the end of a six-minute track). It is never rounded here: the grid keeps the true
    //    tempo, and the deck plays a track at a whole BPM by adjusting its playback speed (see `set_target_bpm`). ──
    let (bpm, phase, fit_n, spread) = match fit_grid(&on.combined, fps, bpm, phase) {
        Some(f) => (f.bpm, f.phase, f.used, f.spread_frames),
        None => (bpm, phase, 0, f64::NAN),
    };
    let spread_ms = spread * 1000.0 / fps;
    let period = fps * 60.0 / bpm;
    let phase = phase - ONSET_LEAD_FRAMES;
    // ── Half a beat out? The tempo and its steadiness are right but the grid can sit on the off-beats; the kick
    //    says which (a kick on the off-beat with nothing on the beat is rare, an off-beat bass line is not, so the
    //    kick has to be clearly stronger there). ──
    let (phase, half_beat_ratio) = fix_half_beat(&on, period, phase);

    // Flux timing is only ~frame-accurate and fires early on soft kicks; snap the
    // grid to the steepest low-frequency attack edge at sub-millisecond resolution.
    let phase = refine_phase_on_attack(samples, channels, sample_rate, period, phase);
    // A soft kick's sub-bass swells after its click, so the steepest low-frequency rise can land 20-30 ms after the
    // moment a person hears the kick begin; put the grid on the click.
    let phase = refine_phase_on_click(samples, channels, sample_rate, period, phase);

    let kick_thirds = kick_ratio_in_thirds(&on, period, phase);

    // ── Downbeat: which of the four beat phases carries the strongest kick pattern ──
    let start_frame = music_start_frame(&on.rms);
    let mut strength = [0.0f64; 4];
    let mut kick: Vec<(i64, f32)> = Vec::new();
    let first_n = ((-phase) / period).ceil() as i64;
    let last_n = ((n as f64 - phase) / period).floor() as i64;
    for beat in first_n..=last_n {
        let t = phase + beat as f64 * period;
        let c = t.round() as isize;
        let mut s = 0.0f32;
        for d in -2..=2isize {
            let i = c + d;
            if i >= 0 && (i as usize) < n {
                s = s.max(on.combined[i as usize] + 1.5 * on.low[i as usize]);
            }
        }
        strength[beat.rem_euclid(4) as usize] += s as f64;
        kick.push((beat, s));
    }
    // Gentle bias toward the beat phase aligned with where the music starts. The first beat is the one nearest the
    // first sound, up to half a beat before it (a bar line that falls just before a soft first sound is still the 1).
    // (0.6 of a beat, not half: a track that opens on the off-beat, as The Dawn does with a hat, has its first sound
    // half a beat after the bar line, and a little more once the measuring of where the sound starts is counted.)
    let start_beat = ((start_frame as f64 - FIRST_BEAT_BEFORE_SOUND * period - phase) / period).ceil() as i64;
    let mean_strength = strength.iter().sum::<f64>() / 4.0;
    strength[start_beat.rem_euclid(4) as usize] += 0.08 * mean_strength;
    let downbeat_k = (0..4)
        .max_by(|&a, &b| strength[a].partial_cmp(&strength[b]).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0) as i64;

    // Anchor = first downbeat at (or just before) the start of the music.
    let mut anchor_n = start_beat;
    while anchor_n.rem_euclid(4) != downbeat_k {
        anchor_n += 1;
    }
    let anchor = phase + anchor_n as f64 * period;

    log::info!("BPM {bpm:.3}  downbeat offset {anchor:.2} chunks (phase {phase:.2}, k={downbeat_k}, {fit_n} beats fitted, spread {spread_ms:.1} ms)");
    BeatAnalysis {
        bpm: bpm as f32,
        offset_chunks: anchor,
        fitted_beats: fit_n,
        spread_ms: spread_ms as f32,
        phase_chunks: phase,
        kick,
        start_beat,
        half_beat_ratio,
        kick_thirds,
    }
}

/// Mean kick onset on the beats of the grid `phase + n * period` (chunks) and half a beat from them.
fn kick_on_and_between(low: &[f32], period: f64, phase: f64) -> (f64, f64) {
    let n = low.len();
    let at = |t: f64| -> f32 {
        let c = t.round() as isize;
        (-2..=2isize).filter_map(|d| low.get((c + d).max(0) as usize)).fold(0.0f32, |m, &v| m.max(v))
    };
    let first = ((-phase) / period).ceil() as i64;
    let last = ((n as f64 - phase - 3.0) / period).floor() as i64;
    let (mut on, mut off, mut cnt) = (0.0f64, 0.0f64, 0usize);
    for k in first..last {
        let t = phase + k as f64 * period;
        on += at(t) as f64;
        off += at(t + period / 2.0) as f64;
        cnt += 1;
    }
    if cnt == 0 {
        return (0.0, 0.0);
    }
    (on / cnt as f64, off / cnt as f64)
}

/// How much a band's loudness rises at frame `t`: the loudest of the 40 ms from it over the average of the 30 ms
/// before it.
fn rise_at(x: &[f32], t: f64) -> f32 {
    let c = t.round() as isize;
    if c < 5 || c as usize + 7 >= x.len() {
        return 0.0;
    }
    let c = c as usize;
    let before = x[c - 5..c].iter().sum::<f32>() / 5.0;
    let after = x[c..c + 7].iter().cloned().fold(0.0f32, f32::max);
    (after - before).max(0.0)
}

/// The summed kick hits on the beats of the grid `phase + k * period` and half a beat from them, for beats `ks`: for
/// each position the rise of the punch (about 90-260 Hz) and of the body (350 Hz-2 kHz) together, as their geometric
/// mean, so a bass note or a stab with only one of the two counts for little. Loudness is not compressed, so the parts
/// where the kick plays decide and a quiet lead-in does not.
fn kick_hits(on: &Onsets, period: f64, phase: f64, ks: std::ops::Range<i64>) -> (f64, f64) {
    let hit = |t: f64| (rise_at(&on.punch, t) as f64 * rise_at(&on.body, t) as f64).sqrt();
    let (mut a, mut b) = (0.0f64, 0.0f64);
    for k in ks {
        let t = phase + k as f64 * period;
        a += hit(t);
        b += hit(t + period / 2.0);
    }
    (a, b)
}

/// The beats of the track the kick is measured over.
fn kick_range(on: &Onsets, period: f64, phase: f64) -> (i64, i64) {
    let n = on.punch.len();
    (((-phase) / period).ceil() as i64, ((n as f64 - phase - 8.0) / period).floor() as i64)
}

/// Between over on, 1.0 where there was no kick to measure.
fn between_over_on((on, between): (f64, f64)) -> f32 {
    if on > 0.0 || between > 0.0 {
        (between / on.max(1e-9)) as f32
    } else {
        1.0
    }
}

/// [`kick_hits`] for the first, middle and last third of the track's beats, as between over on.
fn kick_ratio_in_thirds(on: &Onsets, period: f64, phase: f64) -> [f32; 3] {
    let (first, last) = kick_range(on, period, phase);
    let third = ((last - first) / 3).max(1);
    [0, 1, 2]
        .map(|i| between_over_on(kick_hits(on, period, phase, first + i * third..(first + (i + 1) * third).min(last))))
}

/// Moves the grid by half a beat when the kick is clearly stronger between its beats than on them. Two measures: the
/// low-band onsets (which an off-beat bass line also makes, so alone they must be clearly stronger and the kick itself
/// must not say otherwise), and the kick itself, punch and body together (Robert Miles - Children: the grid sat on the
/// open hi-hat, the low-band onsets were even at 1.02 and the kick was three times stronger between the beats).
/// Returns the phase and the kick's ratio between-over-on (1.0 when there was nothing to measure).
fn fix_half_beat(on: &Onsets, period: f64, phase: f64) -> (f64, f32) {
    let (low_on, low_off) = kick_on_and_between(&on.low, period, phase);
    let (first, last) = kick_range(on, period, phase);
    let hits = between_over_on(kick_hits(on, period, phase, first..last));
    // every third of the track agrees: a kick between the beats throughout, not in one loud part
    let steady = kick_ratio_in_thirds(on, period, phase).iter().all(|&r| r >= 1.0);
    let low = if low_on > 0.0 || low_off > 0.0 { (low_off / low_on.max(1e-6)) as f32 } else { 1.0 };
    let by_low = low >= HALF_BEAT_RATIO && low_off > 1.0 && hits >= 1.0;
    if hits >= HALF_BEAT_RATIO || (hits >= HALF_BEAT_RATIO_STEADY && steady) || by_low {
        (phase + period / 2.0, hits.max(low))
    } else {
        (phase, hits)
    }
}

struct GridFit {
    bpm: f64,
    phase: f64,
    used: usize,
    spread_frames: f64,
}

/// Fits `position = phase + n * period` through the strongest onset near every predicted beat, with a median
/// (Theil-Sen) line so breakdowns, fills and stray hits do not pull it. The search window narrows as the line
/// improves. `phase` is in onset frames, and is where beat 0 falls.
fn fit_grid(onset: &[f32], fps: f64, bpm0: f64, phase0: f64) -> Option<GridFit> {
    let n = onset.len();
    let (mut period, mut phase) = (fps * 60.0 / bpm0, phase0);
    let mut result = None;
    for &win_ms in &[50.0f64, 30.0, 20.0, 14.0] {
        let w = ((win_ms / 1000.0 * fps).min(period * 0.25)).max(2.0);
        let first = ((-phase) / period).ceil() as i64;
        let last = ((n as f64 - 2.0 - phase) / period).floor() as i64;
        if last - first < 24 {
            return None;
        }
        let mut pts: Vec<(f64, f64, f32)> = Vec::new(); // (beat number, onset position, strength)
        for k in first..=last {
            let c = phase + k as f64 * period;
            let (a, b) = ((c - w).ceil().max(1.0) as usize, ((c + w).floor() as usize).min(n - 2));
            if a >= b {
                continue;
            }
            let mut bi = a;
            for i in a..=b {
                if onset[i] > onset[bi] {
                    bi = i;
                }
            }
            let (y0, y1, y2) = (onset[bi - 1] as f64, onset[bi] as f64, onset[bi + 1] as f64);
            let denom = y0 - 2.0 * y1 + y2;
            let d = if denom.abs() > 1e-12 { (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5) } else { 0.0 };
            pts.push((k as f64, bi as f64 + d, onset[bi]));
        }
        if pts.len() < 24 {
            return None;
        }
        let mut st: Vec<f32> = pts.iter().map(|p| p.2).collect();
        st.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let cut = st[st.len() / 2] * 0.5;
        pts.retain(|p| p.2 >= cut);
        if pts.len() < 24 {
            return None;
        }
        // Theil-Sen on a thinned set so it stays cheap on long tracks.
        let step = (pts.len() / 400).max(1);
        let sub: Vec<&(f64, f64, f32)> = pts.iter().step_by(step).collect();
        let min_dk = (sub.len() as f64 * 0.1).max(4.0);
        let mut slopes = Vec::new();
        for i in 0..sub.len() {
            for j in i + 1..sub.len() {
                let dk = (sub[j].0 - sub[i].0) / step as f64;
                if dk >= min_dk {
                    slopes.push((sub[j].1 - sub[i].1) / (sub[j].0 - sub[i].0));
                }
            }
        }
        if slopes.len() < 8 {
            return None;
        }
        slopes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        period = slopes[slopes.len() / 2];
        let mut icpt: Vec<f64> = pts.iter().map(|p| p.1 - period * p.0).collect();
        icpt.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        phase = icpt[icpt.len() / 2];
        let mut res: Vec<f64> = pts.iter().map(|p| (p.1 - (phase + period * p.0)).abs()).collect();
        res.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // median absolute deviation, scaled to a standard deviation
        let spread = res[res.len() / 2] * 1.4826;
        result = Some(GridFit { bpm: fps * 60.0 / period, phase, used: pts.len(), spread_frames: spread });
    }
    result
}

/// Shifts the grid phase (chunks) so beats sit on the steepest rise of the
/// low-frequency envelope — the visible edge of a kick. Returns the input phase
/// unchanged if the track has no clear low-frequency attack structure.
fn refine_phase_on_attack(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    period_chunks: f64,
    phase_chunks: f64,
) -> f64 {
    const DEC: usize = 16;
    let ch = channels.max(1);
    let n = samples.len() / ch;
    let sr = sample_rate as f64;

    let a_lp = (-2.0 * std::f64::consts::PI * 150.0 / sr).exp() as f32;
    let a_env = (-1.0 / (0.003 * sr)).exp() as f32;
    let (mut lp, mut env) = (0.0f32, 0.0f32);
    let mut dec_env = Vec::with_capacity(n / DEC + 2);
    for i in 0..n {
        let m = if ch == 1 { samples[i] } else { (samples[i * ch] + samples[i * ch + 1]) * 0.5 };
        lp = a_lp * lp + (1.0 - a_lp) * m;
        env = a_env * env + (1.0 - a_env) * lp.abs();
        if i % DEC == 0 {
            dec_env.push(env);
        }
    }
    let m = dec_env.len();
    let slope = |j: isize| -> f32 {
        if j < 1 || j as usize + 1 >= m {
            0.0
        } else {
            dec_env[j as usize + 1] - dec_env[j as usize - 1]
        }
    };

    let samples_per_chunk = WAVEFORM_CHUNK_FRAMES as f64;
    let period_samples = period_chunks * samples_per_chunk;
    let phase_samples = phase_chunks * samples_per_chunk;
    let win = (0.040 * sr / DEC as f64) as isize; // search +-40 ms
                                                  // For every beat find its own steepest attack edge; the median offset across
                                                  // beats is robust to pads/sidechain swells that would skew a plain sum.
    let mut edges: Vec<(f64, f32)> = Vec::new(); // (offset in dec samples, edge strength)
    let mut beat = ((-phase_samples) / period_samples).ceil().max(0.0) as i64;
    loop {
        let t = phase_samples + beat as f64 * period_samples;
        if t >= n as f64 {
            break;
        }
        let c = (t / DEC as f64) as isize;
        let mut best = (0.0f32, 0isize);
        for d in -win..=win {
            let sl = slope(c + d);
            if sl > best.0 {
                best = (sl, d);
            }
        }
        if best.0 > 0.0 {
            edges.push((best.1 as f64, best.0));
        }
        beat += 1;
    }
    if edges.len() < 8 {
        return phase_chunks;
    }

    let mut strengths: Vec<f32> = edges.iter().map(|e| e.1).collect();
    strengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let strong_cut = strengths[strengths.len() / 2] * 0.3;
    let offs: Vec<f64> = edges.iter().filter(|e| e.1 >= strong_cut).map(|e| e.0).collect();
    if offs.len() < 8 {
        return phase_chunks;
    }
    // Median edge position; accept only if enough beats agree with it (beats with
    // no kick scatter, beats with one cluster around the true attack).
    let ms_per_dec = DEC as f64 / sr * 1000.0;
    let mut sorted = offs.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med_ms = sorted[sorted.len() / 2] * ms_per_dec;
    let near: Vec<f64> = offs.iter().map(|o| o * ms_per_dec).filter(|ms| (ms - med_ms).abs() <= 8.0).collect();
    if (near.len() as f64) < offs.len() as f64 * 0.20 {
        return phase_chunks;
    }
    let shift_ms = near.iter().sum::<f64>() / near.len() as f64;
    phase_chunks + shift_ms / 1000.0 * sr / samples_per_chunk
}

/// How late after a grid beat a kick may start and still be found (ms). BASS DOWN LOW's kicks start 30 ms after the
/// grid the earlier stages left, which a 25 ms limit could not see.
const CLICK_LATE_MS: i64 = 60;

/// How far (ms, negative = earlier than the grid) the kicks start from the grid beats, found by averaging the
/// high-passed audio around the beats with the strongest clicks (up to 400) and taking the first moment it rises to 35% of its peak. `None` when
/// there is no clear kick to average (steady bass or pads hide it).
fn click_offset_ms(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    period_chunks: f64,
    phase_chunks: f64,
) -> Option<f64> {
    let ch = channels.max(1);
    let n = samples.len() / ch;
    let sr = sample_rate as f64;
    let ms = (sr / 1000.0).round() as usize;
    let (before, after) = ((0.100 * sr) as usize, (0.100 * sr) as usize);
    let period_s = period_chunks * WAVEFORM_CHUNK_FRAMES as f64 / sr;
    let first_s = phase_chunks * WAVEFORM_CHUNK_FRAMES as f64 / sr;
    let mono = |i: usize| if ch == 1 { samples[i] } else { (samples[i * ch] + samples[i * ch + 1]) * 0.5 };
    // The beats that have a click at all: the stronger half, by the largest click within 30 ms of the grid beat, and
    // at most 400 of those. (It was the first 400 beats whatever was on them, and a track that spends its first
    // minutes without a kick averaged mostly beats with nothing to find: Somewhere's grid stayed 18 ms late.)
    let near = (0.03 * sr) as usize;
    let click = |i: usize| (mono(i) - mono(i - 1)).abs();
    let mut beats: Vec<(usize, f32)> = Vec::new();
    let mut k = ((0.5 - first_s) / period_s).ceil().max(4.0) as i64;
    loop {
        let c = ((first_s + k as f64 * period_s) * sr) as usize;
        k += 1;
        if c + after + 1 >= n {
            break;
        }
        if c < before + 1 {
            continue;
        }
        let m = (c - near..c + near).map(click).fold(0.0, f32::max);
        if m >= 1e-4 {
            beats.push((c, m));
        }
    }
    beats.sort_by(|a, b| b.1.total_cmp(&a.1));
    beats.truncate((beats.len() / 2).clamp(40.min(beats.len()), 400));
    let mut acc = vec![0.0f32; before + after];
    let cnt = beats.len();
    for &(c, m) in &beats {
        for (a, i) in acc.iter_mut().zip(c - before..c + after) {
            *a += click(i) / m;
        }
    }
    if cnt < 40 {
        return None;
    }
    // 1 ms peak envelope, index 0 = 100 ms before the beat
    let env: Vec<f32> = acc.chunks(ms.max(1)).map(|c| c.iter().cloned().fold(0.0, f32::max) / cnt as f32).collect();
    let at = |t_ms: i64| (t_ms + 100) as usize;
    // The quiet the click stands out of: the lower quartile of the whole 200 ms. (It was the median of the 35 ms
    // furthest before the beat, which in a busy pattern is where the previous sixteenth's hat is: The Dawn's kick
    // click then failed the test below and its grid stayed 31 ms late, on the swell of the sub.)
    let mut base: Vec<f32> = env[at(-100)..at(99)].to_vec();
    base.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let base = base[base.len() / 4];
    let (peak_at, peak) =
        (-60..=CLICK_LATE_MS).map(|t| (t, env[at(t)])).fold((0, 0.0f32), |b, c| if c.1 > b.1 { c } else { b });
    if peak < base * 1.8 || peak < 0.05 {
        return None;
    }
    // the start of the rise to that peak, found by walking back down it, so noise in the quiet before the beat (a
    // dense mix is never silent there) cannot put the start earlier than the rise is
    let thr = base + 0.35 * (peak - base);
    let mut t = peak_at;
    while t > -70 && env[at(t - 1)] >= thr {
        t -= 1;
    }
    // a click rises in a few milliseconds; a slow rise is a swell or a loose rhythm, which has no start to find
    (peak_at - t <= 20).then_some(t as f64)
}

/// Moves the grid onto the start of the kicks when they begin 6 ms or more away from it (never more than 60 ms).
fn refine_phase_on_click(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    period_chunks: f64,
    phase_chunks: f64,
) -> f64 {
    let mut phase = phase_chunks;
    for _ in 0..2 {
        let Some(off) = click_offset_ms(samples, channels, sample_rate, period_chunks, phase) else { break };
        if off.abs() < 6.0 || off.abs() > 60.0 {
            break;
        }
        phase += off / 1000.0 * sample_rate as f64 / WAVEFORM_CHUNK_FRAMES as f64;
    }
    phase
}

pub use dubplate_core::deck::beat_times_ms;

fn compute_onsets(samples: &[f32], channels: usize, sample_rate: u32) -> Onsets {
    let ch = channels.max(1);
    let n_frames = samples.len() / ch;
    let frames = n_frames / HOP + 1;

    let mono = |f: isize| -> f32 {
        if f < 0 || f as usize >= n_frames {
            return 0.0;
        }
        let b = f as usize * ch;
        if ch == 1 {
            samples[b]
        } else {
            (samples[b] + samples[b + 1]) * 0.5
        }
    };

    let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_N);
    let hann: Vec<f32> =
        (0..FFT_N).map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (FFT_N - 1) as f32).cos())).collect();
    let bin_hz = sample_rate as f32 / FFT_N as f32;
    let k_max = ((10_000.0 / bin_hz) as usize).clamp(8, FFT_N / 2 - 1);

    let mut buf = vec![Complex::new(0.0f32, 0.0); FFT_N];
    let mut prev = vec![0.0f32; k_max + 1];
    let mut full = vec![0.0f32; frames];
    let mut low = vec![0.0f32; frames];
    let mut rms = vec![0.0f32; frames];
    let (mut punch, mut body) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let body_max = ((2_000.0 / bin_hz) as usize).clamp(5, k_max);
    let lead = (FFT_N / 2 - HOP / 2) as isize;

    for i in 0..frames {
        let start = (i * HOP) as isize - lead;
        for j in 0..FFT_N {
            buf[j] = Complex::new(mono(start + j as isize) * hann[j], 0.0);
        }
        fft.process(&mut buf);
        let (mut f_sum, mut l_sum) = (0.0f32, 0.0f32);
        for k in 1..=k_max {
            let mag = buf[k].norm() / FFT_N as f32;
            if k <= 3 {
                punch[i] += mag;
            } else if k <= body_max {
                body[i] += mag;
            }
            let v = (1.0 + 1000.0 * mag).ln();
            let d = (v - prev[k]).max(0.0);
            f_sum += d;
            if k <= 4 {
                l_sum += d;
            }
            prev[k] = v;
        }
        full[i] = f_sum / k_max as f32;
        low[i] = l_sum / 4.0;

        let mut e = 0.0f32;
        for j in 0..HOP {
            let s = mono((i * HOP + j) as isize);
            e += s * s;
        }
        rms[i] = (e / HOP as f32).sqrt();
    }

    let full = normalise(detrend(&full));
    let low = normalise(detrend(&low));
    let combined: Vec<f32> = full.iter().zip(&low).map(|(a, b)| a + b).collect();
    Onsets { combined, low, rms, punch, body }
}

/// Subtract a centred moving average (about 0.45 s) and half-wave rectify.
fn detrend(x: &[f32]) -> Vec<f32> {
    let half = 40usize;
    let mut prefix = vec![0.0f64; x.len() + 1];
    for (i, &v) in x.iter().enumerate() {
        prefix[i + 1] = prefix[i] + v as f64;
    }
    (0..x.len())
        .map(|i| {
            let a = i.saturating_sub(half);
            let b = (i + half + 1).min(x.len());
            let mean = (prefix[b] - prefix[a]) / (b - a) as f64;
            (x[i] as f64 - mean).max(0.0) as f32
        })
        .collect()
}

fn normalise(mut x: Vec<f32>) -> Vec<f32> {
    let mean = x.iter().sum::<f32>() / x.len().max(1) as f32;
    if mean > 1e-9 {
        for v in &mut x {
            *v /= mean;
        }
    }
    x
}

/// Local maxima of the onset autocorrelation that are close to the strongest one,
/// as BPM values. Only proposes candidates; `fine_tune` + the prior decide.
fn candidate_tempos(onset: &[f32], fps: f64) -> Vec<f64> {
    let n = onset.len();
    let lag_min = (fps * 60.0 / MAX_BPM).floor() as usize;
    let lag_max = (fps * 60.0 / MIN_BPM).ceil() as usize;
    if lag_min < 2 || lag_max * 2 >= n {
        return Vec::new();
    }

    let mean = onset.iter().sum::<f32>() / n as f32;
    let x: Vec<f32> = onset.iter().map(|v| v - mean).collect();
    let mut ac = vec![0.0f32; lag_max + 2];
    for lag in lag_min - 1..=lag_max + 1 {
        let mut s = 0.0f32;
        for t in 0..n - lag {
            s += x[t] * x[t + lag];
        }
        ac[lag] = s / (n - lag) as f32;
    }
    let max_ac = ac[lag_min..=lag_max].iter().cloned().fold(0.0f32, f32::max);
    if max_ac <= 0.0 {
        return Vec::new();
    }

    let mut peaks: Vec<(f32, f64)> = Vec::new();
    for lag in lag_min..=lag_max {
        if ac[lag] >= ac[lag - 1] && ac[lag] >= ac[lag + 1] && ac[lag] >= 0.75 * max_ac {
            let (y0, y1, y2) = (ac[lag - 1] as f64, ac[lag] as f64, ac[lag + 1] as f64);
            let denom = y0 - 2.0 * y1 + y2;
            let delta = if denom.abs() > 1e-12 { (0.5 * (y0 - y2) / denom).clamp(-1.0, 1.0) } else { 0.0 };
            peaks.push((ac[lag], fps * 60.0 / (lag as f64 + delta)));
        }
    }
    peaks.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    peaks.truncate(8);
    peaks.into_iter().map(|p| p.1).collect()
}

/// The strongest local maxima of the folded onset score over the whole tempo range, every 0.5 BPM.
fn sweep_candidates(onset: &[f32], fps: f64) -> Vec<f64> {
    let mut scores: Vec<(f64, f64)> = Vec::new();
    let mut b = MIN_BPM;
    while b <= MAX_BPM {
        scores.push((b, fold_score(onset, fps * 60.0 / b).0));
        b += 0.5;
    }
    let mut peaks: Vec<(f64, f64)> = (1..scores.len() - 1)
        .filter(|&i| scores[i].1 >= scores[i - 1].1 && scores[i].1 >= scores[i + 1].1)
        .map(|i| (scores[i].1, scores[i].0))
        .collect();
    peaks.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    peaks.into_iter().take(6).map(|p| p.1).collect()
}

/// The tempos a beat can be confused with: double, half, and the dotted / triplet ratios.
fn ratio_relatives(bpm: f64) -> Vec<f64> {
    [2.0, 0.5, 1.5, 2.0 / 3.0, 4.0 / 3.0, 0.75]
        .iter()
        .map(|r| bpm * r)
        .filter(|b| (MIN_BPM..=MAX_BPM).contains(b))
        .collect()
}

/// Scans +-1.5 % around `bpm0` for the tempo whose beat grid captures the most
/// onset mass. Returns `(bpm, score)`.
fn fine_tune(onset: &[f32], fps: f64, bpm0: f64) -> (f64, f64) {
    let step = 0.005 * (bpm0 / 120.0).max(0.5);
    let mut scores: Vec<(f64, f64)> = Vec::new();
    let mut b = bpm0 * 0.985;
    while b <= bpm0 * 1.015 {
        scores.push((b, fold_score(onset, fps * 60.0 / b).0));
        b += step;
    }
    let (i, &(mut bpm, score)) = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1 .1.partial_cmp(&b.1 .1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();
    if i > 0 && i + 1 < scores.len() {
        let (y0, y1, y2) = (scores[i - 1].1, scores[i].1, scores[i + 1].1);
        let denom = y0 - 2.0 * y1 + y2;
        if denom.abs() > 1e-12 {
            bpm += (0.5 * (y0 - y2) / denom).clamp(-1.0, 1.0) * step;
        }
    }
    (bpm, score)
}

/// Folds the onset signal modulo `period` and returns `(score, phase_frames)`.
/// `score` is the fraction of total onset mass inside the best phase bin, so it
/// is directly comparable across different periods.
fn fold_score(onset: &[f32], period: f64) -> (f64, f64) {
    let mut bins = [0.0f64; FOLD_BINS];
    let mut total = 0.0f64;
    for (t, &v) in onset.iter().enumerate() {
        if v <= 0.0 {
            continue;
        }
        let ph = (t as f64).rem_euclid(period) / period * FOLD_BINS as f64;
        let b0 = ph.floor() as usize;
        let fr = ph - b0 as f64;
        bins[b0 % FOLD_BINS] += v as f64 * (1.0 - fr);
        bins[(b0 + 1) % FOLD_BINS] += v as f64 * fr;
        total += v as f64;
    }
    if total <= 0.0 {
        return (0.0, 0.0);
    }

    let mut sm = [0.0f64; FOLD_BINS];
    for i in 0..FOLD_BINS {
        let p = bins[(i + FOLD_BINS - 1) % FOLD_BINS];
        let n = bins[(i + 1) % FOLD_BINS];
        sm[i] = 0.25 * p + 0.5 * bins[i] + 0.25 * n;
    }
    let (bi, &peak) =
        sm.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal)).unwrap();
    let (y0, y2) = (sm[(bi + FOLD_BINS - 1) % FOLD_BINS], sm[(bi + 1) % FOLD_BINS]);
    let denom = y0 - 2.0 * peak + y2;
    let delta = if denom.abs() > 1e-12 { (0.5 * (y0 - y2) / denom).clamp(-1.0, 1.0) } else { 0.0 };
    let phase = ((bi as f64 + delta).rem_euclid(FOLD_BINS as f64)) / FOLD_BINS as f64 * period;
    (peak / total, phase)
}

/// First hop whose RMS is audible against the track's loud level (2% of it, about -34 dB): a quiet lead-in is the
/// start of the music, and DJs count bar 1 from the first sound (19 of 20 hand-labelled tracks; a threshold of 15% put
/// bar 1 a whole bar late on a track whose first bar was a soft pad).
fn music_start_frame(rms: &[f32]) -> usize {
    let mut sorted: Vec<f32> = rms.iter().copied().filter(|&v| v > 0.0).collect();
    if sorted.is_empty() {
        return 0;
    }
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p90 = sorted[(sorted.len() as f32 * 0.9) as usize % sorted.len()];
    rms.iter().position(|&v| v > p90 * 0.02).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    /// Synthetic 4/4 track: kick every beat (accented on beat 1), off-beat hats.
    /// `first_kick_s` is the time of the first downbeat.
    fn synth(bpm: f64, first_kick_s: f64, secs: f64, bars_before_pickup: usize) -> Vec<f32> {
        let n = (secs * SR as f64) as usize;
        let mut out = vec![0.0f32; n * 2];
        let beat = 60.0 / bpm;
        let mut idx = 0usize;
        loop {
            let t = first_kick_s + idx as f64 * beat;
            if t >= secs {
                break;
            }
            let accent = if idx.is_multiple_of(4) { 1.0 } else { 0.7 };
            let s0 = (t * SR as f64) as usize;
            for j in 0..(0.12 * SR as f64) as usize {
                let tt = j as f64 / SR as f64;
                let f = 50.0 + 100.0 * (-tt * 40.0).exp();
                let v = (2.0 * std::f64::consts::PI * f * tt).sin() * (-tt * 22.0).exp() * accent;
                if s0 + j < n {
                    out[(s0 + j) * 2] += v as f32;
                    out[(s0 + j) * 2 + 1] += v as f32;
                }
            }
            // off-beat hat (noise-ish burst)
            let h0 = ((t + beat / 2.0) * SR as f64) as usize;
            let mut seed = (idx as u32).wrapping_mul(2654435761);
            for j in 0..(0.015 * SR as f64) as usize {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let r = (seed >> 16) as f32 / 32768.0 - 1.0;
                if h0 + j < n {
                    let env = (-(j as f32) / 200.0).exp();
                    out[(h0 + j) * 2] += 0.15 * r * env;
                    out[(h0 + j) * 2 + 1] += 0.15 * r * env;
                }
            }
            idx += 1;
        }
        let _ = bars_before_pickup;
        out
    }

    /// Onset curves of a made-up track: beats every 75 frames from frame 10. `kick_shift` is where the kick is, in
    /// frames after each beat; a bass note (punch only) sits half a beat from the kick. The low-band onsets are even.
    fn onsets_with_kick_at(kick_shift: usize) -> Onsets {
        let n = 75 * 200;
        let (mut punch, mut body) = (vec![0.01f32; n], vec![0.01f32; n]);
        let low = vec![1.0f32; n];
        for b in 0..198 {
            let kick = 10 + b * 75 + kick_shift;
            for j in 0..12 {
                let decay = (-(j as f32) / 4.0).exp();
                punch[kick + j] += decay;
                body[kick + j] += 0.6 * decay;
                // the bass note, half a beat from the kick: as loud in the punch band, nothing in the body
                punch[kick + 37 + j] += decay;
            }
        }
        Onsets { combined: low.clone(), low, rms: vec![0.1; n], punch, body }
    }

    #[test]
    fn a_grid_on_the_hats_moves_onto_the_kick_half_a_beat_on() {
        // the kick is 37 frames (half a beat) after every grid beat, as on Robert Miles - Children
        let (phase, ratio) = fix_half_beat(&onsets_with_kick_at(37), 75.0, 10.0);
        assert!((phase - 47.5).abs() < 1e-9, "phase {phase}");
        assert!(ratio > HALF_BEAT_RATIO, "ratio {ratio}");
    }

    #[test]
    fn an_off_beat_bass_line_does_not_move_a_grid_that_is_on_the_kick() {
        let (phase, ratio) = fix_half_beat(&onsets_with_kick_at(0), 75.0, 10.0);
        assert_eq!(phase, 10.0);
        assert!(ratio < 1.0, "ratio {ratio}");
    }

    /// A kick between the beats with a weaker one on them (the on-grid hits are `share` of the others), over the whole
    /// track, except that in `quiet_third` (0, 1 or 2) the on-grid hits are the stronger ones.
    fn kick_mostly_between(share: f32, quiet_third: Option<usize>) -> Onsets {
        let mut on = onsets_with_kick_at(37);
        // the helper's bass note falls on the grid with a punch and no body: take it out so only the kicks are left
        on.punch.iter_mut().for_each(|v| *v = 0.01);
        on.body.iter_mut().for_each(|v| *v = 0.01);
        for b in 0..198usize {
            let strength = if quiet_third == Some(b / 66) { 1.5 } else { share };
            for (at, amp) in [(10 + b * 75, strength), (10 + b * 75 + 38, 1.0)] {
                for j in 0..12 {
                    let decay = (-(j as f32) / 4.0).exp();
                    on.punch[at + j] += amp * decay;
                    on.body[at + j] += 0.6 * amp * decay;
                }
            }
        }
        on
    }

    #[test]
    fn a_kick_a_little_stronger_between_the_beats_moves_the_grid_when_every_third_says_so() {
        // between over on is 1 / 0.7 = 1.4 in every third: above 1.25 but under 1.6
        let (phase, ratio) = fix_half_beat(&kick_mostly_between(0.7, None), 75.0, 10.0);
        assert!(ratio > HALF_BEAT_RATIO_STEADY && ratio < HALF_BEAT_RATIO, "ratio {ratio}");
        assert!((phase - 47.5).abs() < 1e-9, "phase {phase}");
        // the same overall, but one third has the kick on the beats: not moved
        let (phase, _) = fix_half_beat(&kick_mostly_between(0.7, Some(1)), 75.0, 10.0);
        assert_eq!(phase, 10.0);
    }

    #[test]
    fn the_thirds_show_a_kick_that_leaves_the_grid_part_way_through() {
        // the kick is on the grid for the first 66 beats and half a beat after it for the rest (a tempo a little off)
        let mut on = onsets_with_kick_at(0);
        let later = onsets_with_kick_at(37);
        let cut = 10 + 66 * 75;
        on.punch[cut..].copy_from_slice(&later.punch[cut..]);
        on.body[cut..].copy_from_slice(&later.body[cut..]);
        let t = kick_ratio_in_thirds(&on, 75.0, 10.0);
        assert!(t[0] < 1.0 && t[2] > HALF_BEAT_RATIO, "{t:?}");
        // a steady kick on the grid is below 1 in all three
        let t = kick_ratio_in_thirds(&onsets_with_kick_at(0), 75.0, 10.0);
        assert!(t.iter().all(|&r| r < 1.0), "{t:?}");
    }

    fn check(bpm: f64, first_kick_s: f64) {
        let audio = synth(bpm, first_kick_s, 60.0, 0);
        let (found, offset) = analyze_beats(&audio, 2, SR);
        assert!((found as f64 - bpm).abs() < 0.03, "bpm {found} vs {bpm}");

        let chunk_s = WAVEFORM_CHUNK_FRAMES as f64 / SR as f64;
        let period_s = 60.0 / found as f64;
        // distance (seconds) from detected anchor to the true downbeat, modulo a bar
        let diff = (offset * chunk_s - first_kick_s).rem_euclid(period_s * 4.0);
        let diff = diff.min(period_s * 4.0 - diff);
        assert!(diff < 0.008, "downbeat off by {:.1} ms (offset {offset})", diff * 1000.0);
    }

    #[test]
    fn finds_110_exactly_and_the_downbeat() {
        check(110.0, 0.37);
    }
    #[test]
    fn finds_128_and_the_downbeat() {
        check(128.0, 1.02);
    }
    #[test]
    fn finds_non_integer_tempo() {
        check(97.5, 0.5);
    }
    #[test]
    fn finds_fast_tempo_without_halving() {
        check(174.0, 0.2);
    }

    /// A long track at a tempo a hair off a whole number: the grid has to stay on the beat to the very end.
    fn check_long(bpm: f64, first_kick_s: f64, secs: f64, tol_bpm: f64) {
        let audio = synth(bpm, first_kick_s, secs, 0);
        let (found, offset) = analyze_beats(&audio, 2, SR);
        assert!((found as f64 - bpm).abs() < tol_bpm, "bpm {found} vs {bpm}");
        // where the grid puts the last beat versus where the last kick really is
        let chunk_s = WAVEFORM_CHUNK_FRAMES as f64 / SR as f64;
        let k = ((secs - 2.0 - first_kick_s) / (60.0 / bpm)).floor();
        let truth = first_kick_s + k * 60.0 / bpm;
        let grid_k = k + ((first_kick_s - offset * chunk_s) / (60.0 / found as f64)).round();
        let grid = offset * chunk_s + grid_k * 60.0 / found as f64;
        assert!((grid - truth).abs() < 0.008, "the last beat is {:.1} ms off", (grid - truth) * 1000.0);
    }

    #[test]
    fn keeps_a_fractional_tempo_over_a_long_track() {
        check_long(120.03, 0.5, 150.0, 0.004);
    }
    #[test]
    fn keeps_a_slightly_slow_tempo_over_a_long_track() {
        check_long(127.985, 0.8, 150.0, 0.004);
    }
    #[test]
    fn a_whole_number_tempo_is_found_closely() {
        check_long(126.0, 0.3, 120.0, 0.002);
    }

    fn quality_of(audio: &[f32], bpm: f32, offset_s: f64) -> GridQuality {
        let (curve, frame) = onset_curve_full(audio, 2, SR);
        grid_quality(&curve, frame, bpm, offset_s / frame)
    }

    #[test]
    fn a_grid_on_the_kicks_is_good() {
        let audio = synth(124.0, 0.5, 60.0, 0);
        let q = quality_of(&audio, 124.0, 0.5);
        assert_eq!(q.verdict, GridVerdict::Good, "{q:?}");
    }

    #[test]
    fn a_grid_late_or_early_is_called_out_with_the_amount() {
        let audio = synth(124.0, 0.5, 60.0, 0);
        let q = quality_of(&audio, 124.0, 0.5 + 0.035); // grid 35 ms late: kicks are 35 ms earlier
        assert_eq!(q.verdict, GridVerdict::Check, "{q:?}");
        assert!((q.bias_ms + 35.0).abs() < 8.0, "{q:?}");
        let q = quality_of(&audio, 124.0, 0.5 + 0.060); // most of the way to the next beat: plainly wrong
        assert_eq!(q.verdict, GridVerdict::Off, "{q:?}");
        let q = quality_of(&audio, 124.0, 0.5 - 0.012);
        assert_eq!(q.verdict, GridVerdict::Good, "{q:?}");
        assert!(q.bias_ms > 5.0, "{q:?}");
    }

    #[test]
    fn a_tempo_that_is_a_little_wrong_shows_as_drift() {
        let audio = synth(124.0, 0.5, 120.0, 0);
        let q = quality_of(&audio, 124.15, 0.5);
        assert!(q.drift_ms.abs() > 20.0 && q.verdict == GridVerdict::Off, "{q:?}");
    }

    #[test]
    fn silence_and_noise_are_unknown() {
        let q = quality_of(&vec![0.0f32; SR as usize * 2 * 30], 120.0, 0.0);
        assert_eq!(q.verdict, GridVerdict::Unknown);
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..SR as usize * 2 * 30)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 16) as f32 / 32768.0 - 1.0
            })
            .map(|v| v * 0.1)
            .collect();
        assert_eq!(quality_of(&noise, 120.0, 0.0).verdict, GridVerdict::Unknown);
    }

    #[test]
    fn an_mp3_decoder_offset_is_measured_and_snapped() {
        let audio = synth(124.0, 1.0, 60.0, 0);
        let (curve, frame) = onset_curve_full(&audio, 2, SR);
        // the imported grid is 51 ms early: the music is 51 ms later than it
        let m = measure_import_offset(&curve, frame, 124.0, (1.0 - 0.0512) / frame).unwrap();
        assert!((m.median_ms - 51.2).abs() < 8.0, "{m:?}");
        let s = mp3_offset_shift(&m, SR).expect("looks like a decoder offset");
        assert!((s * 1000.0 - 51.2).abs() < 1.5, "{s}");
        // on the music already: nothing to do
        let m = measure_import_offset(&curve, frame, 124.0, 1.0 / frame).unwrap();
        assert!(m.median_ms.abs() < 8.0 && mp3_offset_shift(&m, SR).is_none(), "{m:?}");
        // 35 ms is neither: leave it
        let m = ImportOffset { median_ms: 37.0, spread_ms: 2.0, hits: 100 };
        assert!(mp3_offset_shift(&m, SR).is_none());
        // scattered: never
        let m = ImportOffset { median_ms: 26.0, spread_ms: 15.0, hits: 100 };
        assert!(mp3_offset_shift(&m, SR).is_none());
    }

    /// Kicks whose sub-bass swells for ~35 ms after a click: the loudest low-frequency rise is late, the click is the start.
    fn soft_kicks(bpm: f64, first_s: f64, secs: f64) -> Vec<f32> {
        let n = (secs * SR as f64) as usize;
        let mut out = vec![0.0f32; n * 2];
        let beat = 60.0 / bpm;
        let mut seed = 7u32;
        let mut k = 0;
        loop {
            let t = first_s + k as f64 * beat;
            if t >= secs - 0.3 {
                break;
            }
            let s0 = (t * SR as f64) as usize;
            for j in 0..(0.25 * SR as f64) as usize {
                let tt = j as f64 / SR as f64;
                // a 6 ms noise click, then a 55 Hz sub that fades in over 35 ms and decays
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let r = (seed >> 16) as f32 / 32768.0 - 1.0;
                let click = if tt < 0.006 { 0.6 * r * (1.0 - tt as f32 / 0.006) } else { 0.0 };
                let sub = ((2.0 * std::f64::consts::PI * 55.0 * tt).sin() * (tt / 0.035).min(1.0) * (-tt * 12.0).exp())
                    as f32
                    * 0.8;
                out[(s0 + j) * 2] += click + sub;
                out[(s0 + j) * 2 + 1] += click + sub;
            }
            k += 1;
        }
        out
    }

    #[test]
    fn the_grid_goes_on_the_click_of_a_soft_kick_not_on_the_swell_after_it() {
        let audio = soft_kicks(126.0, 0.8, 60.0);
        let (_, offset) = analyze_beats(&audio, 2, SR);
        let chunk_s = WAVEFORM_CHUNK_FRAMES as f64 / SR as f64;
        let period_s = 60.0 / 126.0;
        let diff = (offset * chunk_s - 0.8).rem_euclid(period_s * 4.0);
        let diff = diff.min(period_s * 4.0 - diff);
        assert!(diff < 0.008, "the grid is {:.1} ms from the click", diff * 1000.0);
    }

    /// Pulsar (Tiesto remix edit) is 137 BPM; its dotted-eighth pattern used to make the analysis 91.33 (2:3). The file is not
    /// in the repository, so the test is skipped when `samples/` does not have it.
    #[test]
    fn pulsar_is_137_and_not_two_thirds_of_it() {
        let f = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../samples/001-007. Pulsar (DJ Tiesto Remix Edit).ogg");
        if !f.is_file() {
            return;
        }
        let t = crate::decode::decode_file(&f).unwrap();
        let (bpm, _) = analyze_beats(&t.samples, t.channels, t.sample_rate);
        assert!((bpm - 137.0).abs() < 0.05, "found {bpm}");
    }

    #[test]
    fn the_ratio_relatives_include_the_beat_a_dotted_pattern_hides() {
        let r = ratio_relatives(91.3);
        assert!(r.iter().any(|b| (b - 137.0).abs() < 1.0), "{r:?}");
        assert!(ratio_relatives(180.0).iter().all(|b| (MIN_BPM..=MAX_BPM).contains(b)));
    }

    /// Kicks whose tempo drifts linearly from `bpm0` to `bpm1` over the track (a live drummer, a vinyl rip).
    fn drifting(bpm0: f64, bpm1: f64, secs: f64) -> Vec<f32> {
        let n = (secs * SR as f64) as usize;
        let mut out = vec![0.0f32; n * 2];
        let mut t = 0.5;
        while t < secs - 0.2 {
            let s0 = (t * SR as f64) as usize;
            for j in 0..(0.12 * SR as f64) as usize {
                let tt = j as f64 / SR as f64;
                let v = ((2.0 * std::f64::consts::PI * (50.0 + 100.0 * (-tt * 40.0).exp()) * tt).sin()
                    * (-tt * 22.0).exp()) as f32;
                out[(s0 + j) * 2] += v;
                out[(s0 + j) * 2 + 1] += v;
            }
            let bpm = bpm0 + (bpm1 - bpm0) * t / secs;
            t += 60.0 / bpm;
        }
        out
    }

    #[test]
    fn a_drifting_tempo_gets_an_answer_and_the_grid_check_says_it_does_not_fit() {
        let audio = drifting(120.0, 124.0, 150.0);
        let a = analyze_beats_detail(&audio, 2, SR);
        assert!(a.bpm > 119.0 && a.bpm < 125.0, "{a:?}");
        let (curve, frame) = onset_curve_full(&audio, 2, SR);
        let q = grid_quality(&curve, frame, a.bpm, a.offset_chunks);
        assert!(q.verdict != GridVerdict::Good, "a constant grid on a drifting tempo was called good: {q:?}");
    }

    #[test]
    fn a_long_beatless_intro_does_not_move_the_tempo_or_the_first_downbeat() {
        let mut audio = vec![0.0f32; SR as usize * 2 * 25];
        // an ambient pad for 25 s, then the beat
        for (i, v) in audio.iter_mut().enumerate() {
            *v = ((i / 2) as f32 * 0.02).sin() * 0.05;
        }
        audio.extend(synth(126.0, 0.0, 60.0, 0));
        let (bpm, offset) = analyze_beats(&audio, 2, SR);
        assert!((bpm - 126.0).abs() < 0.05, "{bpm}");
        let chunk_s = WAVEFORM_CHUNK_FRAMES as f64 / SR as f64;
        let period = 60.0 / 126.0;
        let d = (offset * chunk_s - 25.0).rem_euclid(period * 4.0);
        assert!(d.min(period * 4.0 - d) < 0.012, "downbeat off by {:.1} ms", d.min(period * 4.0 - d) * 1000.0);
    }

    #[test]
    fn short_mono_silent_and_garbage_audio_do_not_panic() {
        assert_eq!(analyze_beats(&[], 2, SR), (0.0, 0.0));
        assert_eq!(analyze_beats(&vec![0.0; SR as usize * 2 * 30], 2, SR), (0.0, 0.0));
        let short = synth(128.0, 0.2, 7.0, 0);
        let _ = analyze_beats(&short, 2, SR);
        let ten = synth(128.0, 0.2, 10.0, 0);
        let (b, _) = analyze_beats(&ten, 2, SR);
        assert!(b == 0.0 || (b - 128.0).abs() < 0.5, "{b}");
        let mono: Vec<f32> = synth(128.0, 0.2, 40.0, 0).chunks(2).map(|c| c[0]).collect();
        let (bm, _) = analyze_beats(&mono, 1, SR);
        assert!((bm - 128.0).abs() < 0.05, "{bm}");
        let nan = vec![f32::NAN; SR as usize * 2 * 20];
        let _ = analyze_beats(&nan, 2, SR);
        let _ = onset_curve_full(&nan, 2, SR);
    }

    #[test]
    fn beat_times_start_on_a_downbeat() {
        let t = beat_times_ms(120.0, 100.0, SR, SR as u64 * 20);
        assert!(!t.is_empty());
        assert!(t[1] - t[0] >= 499 && t[1] - t[0] <= 501);
    }

    /// Manual check against a real file: DUBPLATE_TEST_FILE=/path cargo test real_file -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_file() {
        let Ok(path) = std::env::var("DUBPLATE_TEST_FILE") else { return };
        let t = crate::decode::decode_file(std::path::Path::new(&path)).unwrap();
        let ms = t.beat_offset * WAVEFORM_CHUNK_FRAMES as f64 / t.sample_rate as f64 * 1000.0;
        println!(
            "RESULT bpm={:.3} downbeat_ms={:.1} first_beats={:?}",
            t.bpm,
            ms,
            &t.beat_times_ms[..6.min(t.beat_times_ms.len())]
        );
    }

    /// Measures how far grid beats sit from the real low-frequency attack edges.
    #[test]
    #[ignore]
    fn grid_alignment_report() {
        let Ok(path) = std::env::var("DUBPLATE_TEST_FILE") else { return };
        let t = crate::decode::decode_file(std::path::Path::new(&path)).unwrap();
        let sr = t.sample_rate as f64;
        let ch = t.channels.max(1);
        let n = t.samples.len() / ch;
        // low-pass (~150 Hz) rectified envelope, ~3 ms smoothing
        let a_lp = (-2.0 * std::f64::consts::PI * 150.0 / sr).exp() as f32;
        let a_env = (-1.0 / (0.003 * sr)).exp() as f32;
        let (mut lp, mut env) = (0.0f32, 0.0f32);
        let mut e = vec![0.0f32; n];
        for i in 0..n {
            let m = if ch == 1 { t.samples[i] } else { (t.samples[i * ch] + t.samples[i * ch + 1]) * 0.5 };
            lp = a_lp * lp + (1.0 - a_lp) * m;
            env = a_env * env + (1.0 - a_env) * lp.abs();
            e[i] = env;
        }
        let period_s = 60.0 / t.bpm as f64;
        let start_s = t.beat_offset * WAVEFORM_CHUNK_FRAMES as f64 / sr;
        let win = (0.06 * sr) as isize;
        let mut deltas: Vec<(f64, f64)> = Vec::new(); // (time_s, delta_ms)
        let mut k = 0i64;
        loop {
            let bt = start_s + k as f64 * period_s;
            k += 1;
            if bt * sr >= n as f64 - win as f64 {
                break;
            }
            if bt < 0.3 {
                continue;
            }
            let c = (bt * sr) as isize;
            let mut best = (f32::MIN, 0isize);
            for d in -win..win {
                let i = (c + d) as usize;
                if i == 0 || i + 1 >= n {
                    continue;
                }
                let slope = e[i + 1] - e[i - 1];
                if slope > best.0 {
                    best = (slope, d);
                }
            }
            // only count beats where there really is a strong edge
            if best.0 > 0.002 {
                deltas.push((bt, best.1 as f64 / sr * 1000.0));
            }
        }
        let med = |v: &mut Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if v.is_empty() {
                f64::NAN
            } else {
                v[v.len() / 2]
            }
        };
        let all: Vec<f64> = deltas.iter().map(|d| d.1).collect();
        let half = deltas.len() / 2;
        let mut first: Vec<f64> = deltas[..half].iter().map(|d| d.1).collect();
        let mut last: Vec<f64> = deltas[half..].iter().map(|d| d.1).collect();
        let within = all.iter().filter(|d| d.abs() < 15.0).count() as f64 / all.len().max(1) as f64;
        println!(
            "ALIGN beats={} median_first_half={:.1}ms median_second_half={:.1}ms within15ms={:.0}%",
            all.len(),
            med(&mut first),
            med(&mut last),
            within * 100.0
        );
    }
}

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn genre_tags_map_to_usual_tempos() {
        let c = |g: &str| tempo_hint_for_genre(g).map(|h| h.centre);
        assert_eq!(c("Dubstep"), Some(140.0));
        assert_eq!(c("Drum & Bass"), Some(174.0));
        assert_eq!(c("drum and bass"), Some(174.0));
        assert_eq!(c("DnB"), Some(174.0));
        assert_eq!(c("Deep House"), Some(124.0));
        assert_eq!(c("Hip-Hop/Rap"), Some(92.0));
        assert_eq!(c("Psychedelic Rock"), None);
        assert_eq!(c("Pop"), None);
        assert_eq!(c("Electronic"), None);
    }

    #[test]
    fn a_hint_decides_the_octave_and_nothing_else() {
        let dub = tempo_hint_for_genre("dubstep").unwrap();
        // inside the range: untouched (Bangarang is dubstep at 110, a 3:2 or 4:3 relative of 140 must not be pulled to)
        for bpm in [110.0, 140.0, 150.0, 174.0] {
            assert_eq!(dub.weight(bpm), 1.0, "{bpm}");
        }
        // an octave off: clearly less likely
        assert!(dub.weight(70.0) < 0.25 && dub.weight(280.0) < 0.1);
        let hip = tempo_hint_for_genre("hip hop").unwrap();
        assert_eq!(hip.weight(90.0), 1.0);
        assert!(hip.weight(180.0) < 0.3);
    }
}

#[cfg(test)]
mod late_click_tests {
    use super::*;

    /// Clicks (a short noise burst) every beat of `bpm`, the first at `first_s`.
    fn clicks(bpm: f64, first_s: f64, secs: f64, sr: u32) -> Vec<f32> {
        let mut x = vec![0.0f32; (secs * sr as f64) as usize];
        let (period, mut seed) = (60.0 / bpm, 12345u32);
        let mut t = first_s;
        while t < secs - 0.1 {
            let i0 = (t * sr as f64) as usize;
            for j in 0..(0.01 * sr as f64) as usize {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                x[i0 + j] =
                    ((seed >> 9) as f32 / (1u32 << 22) as f32 - 1.0) * 0.8 * (1.0 - j as f32 / (0.01 * sr as f32));
            }
            t += period;
        }
        x
    }

    #[test]
    fn kicks_27_ms_after_the_grid_are_found() {
        let sr = 44100;
        let x = clicks(160.0, 0.5, 60.0, sr);
        let chunk = WAVEFORM_CHUNK_FRAMES as f64;
        let period = 60.0 / 160.0 * sr as f64 / chunk;
        // a grid 27 ms early (the old search stopped at 25)
        let phase = (0.5 - 0.027) * sr as f64 / chunk;
        let off = click_offset_ms(&x, 1, sr, period, phase).expect("a clear click");
        assert!((off - 27.0).abs() <= 3.0, "{off}");
        let fixed = refine_phase_on_click(&x, 1, sr, period, phase);
        assert!(((fixed - 0.5 * sr as f64 / chunk) * chunk / sr as f64 * 1000.0).abs() < 3.0);
    }

    /// The same clicks over a steady bed of noise, as a dense mix has: the quiet before a click is not quiet.
    fn clicks_over_a_bed(bpm: f64, first_s: f64, secs: f64, sr: u32, bed: f32) -> Vec<f32> {
        let mut x = clicks(bpm, first_s, secs, sr);
        let mut seed = 99u32;
        for v in x.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *v += ((seed >> 9) as f32 / (1u32 << 22) as f32 - 1.0) * bed;
        }
        x
    }

    #[test]
    fn a_click_over_a_dense_bed_is_found_where_it_starts() {
        let sr = 44100;
        let x = clicks_over_a_bed(128.0, 0.5, 60.0, sr, 0.25);
        let chunk = WAVEFORM_CHUNK_FRAMES as f64;
        let period = 60.0 / 128.0 * sr as f64 / chunk;
        // a grid 18 ms late: the clicks start 18 ms before it
        let phase = (0.5 + 0.018) * sr as f64 / chunk;
        let off = click_offset_ms(&x, 1, sr, period, phase).expect("a click over the bed");
        assert!((off + 18.0).abs() <= 3.0, "{off}");
    }

    #[test]
    fn a_bed_with_no_clicks_has_no_start_to_find() {
        let sr = 44100;
        let mut x = vec![0.0f32; 60 * sr as usize];
        let mut seed = 5u32;
        for v in x.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *v = ((seed >> 9) as f32 / (1u32 << 22) as f32 - 1.0) * 0.3;
        }
        let chunk = WAVEFORM_CHUNK_FRAMES as f64;
        let period = 60.0 / 128.0 * sr as f64 / chunk;
        assert_eq!(click_offset_ms(&x, 1, sr, period, 0.5 * sr as f64 / chunk), None);
    }
}
