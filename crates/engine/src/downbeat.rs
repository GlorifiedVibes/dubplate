//! Which beat is the 1 of the bar.
//!
//! The beat grid says where the beats are; the downbeat is a separate guess, and the one most often wrong (the tempo
//! can be perfect and every bar still start a beat late). No single sign settles it, so four independent ones vote,
//! each as a z-score of how much more the beats of one residue mod 4 stand out than the others:
//!
//! * **kick**: the kick on a bar's first beat is usually the strongest, or the only one that is there;
//! * **bass**: a bass note or a layered kick makes the bar's first beat the heaviest in the low band;
//! * **boundaries**: the clear section changes found by [`crate::structure`] start on bar lines, so the residue most of
//!   them share is the bar line (on the tracks tried, the sign that agrees best with the others and with the grids
//!   rekordbox made);
//! * **start**: tracks are made to begin on bar 1, so the beat nearest the start of the music gets a push as big as a
//!   fairly clear kick would give; the other signs can still outvote it.
//!
//! The margin between the winner and the runner-up is the confidence, so a track where the signs disagree can be
//! flagged instead of silently guessed.

use crate::bpm::BeatAnalysis;
use crate::deck::WAVEFORM_CHUNK_FRAMES;
use crate::structure::BeatEnergy;
use crate::waveform_data::WaveSummary;

/// Weights of the signs, in z-score units. Set by hand: there is no collection of tracks with trusted bar lines to
/// fit them on yet (see `examples/downbeatcheck.rs` for measuring them when there is one).
const W_KICK: f32 = 1.0;
const W_BASS: f32 = 1.0;
const W_DROP: f32 = 2.5;
const W_BOUNDARY: f32 = 0.7;
const W_START: f32 = 1.0;
/// The drums stem's kick, where stems exist (see [`drums_sign`]).
const W_DRUMS: f32 = 1.0;
/// The drums stem only speaks when the other signs do not agree (their margin is under this, the point the
/// confidence doc calls "the signs do not agree"). On 31 tracks it left every confident verdict alone but one, where
/// it turned a margin of 10 into 1.6, and a stem is the wrong thing to trust over six signs that agree.
pub const DRUMS_ONLY_BELOW: f32 = 1.0;
/// A z-score is capped here so one runaway sign cannot outvote the others.
const Z_CAP: f32 = 6.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Downbeat {
    /// Beat number (mod 4) that is the 1 of the bar: beat `n` with `n mod 4 == residue` starts a bar.
    pub residue: i64,
    /// Winner's total minus runner-up's, in z-score units: below about 1 the signs do not agree.
    pub confidence: f32,
    /// The five signs, each for residue 0..3: kick, bass, drops, boundaries, start.
    pub signs: [[f32; 4]; 5],
    /// Position of the first bar line at or after the start of the music, in waveform chunks.
    pub anchor_chunks: f64,
}

/// How much more the beats of each residue mod 4 stand out than the rest, from per-beat values `(beat number, value)`:
/// the difference of each residue's mean from the overall mean, over its standard error. Zeros when there is nothing
/// to tell them apart.
pub fn residue_z(series: &[(i64, f32)]) -> [f32; 4] {
    let n = series.len() as f32;
    if series.len() < 16 {
        return [0.0; 4];
    }
    let mean = series.iter().map(|s| s.1).sum::<f32>() / n;
    let var = series.iter().map(|s| (s.1 - mean).powi(2)).sum::<f32>() / n;
    let sd = var.sqrt();
    if sd < 1e-9 {
        return [0.0; 4];
    }
    let mut sum = [0.0f32; 4];
    let mut cnt = [0.0f32; 4];
    for &(b, v) in series {
        let k = b.rem_euclid(4) as usize;
        sum[k] += v;
        cnt[k] += 1.0;
    }
    [0, 1, 2, 3].map(|k| {
        if cnt[k] < 4.0 {
            0.0
        } else {
            ((sum[k] / cnt[k] - mean) / (sd / cnt[k].sqrt())).clamp(-Z_CAP, Z_CAP)
        }
    })
}

/// z-scores from counts of boundaries per residue: how far each count is above the quarter that chance would give,
/// over the binomial spread. Zeros with fewer than two (two on one residue is a one in four chance).
pub fn vote_z(votes: [f32; 4], total: usize) -> [f32; 4] {
    if total < 2 {
        return [0.0; 4];
    }
    let n = total as f32;
    let sd = (n * 0.25 * 0.75).sqrt();
    votes.map(|v| ((v - n * 0.25) / sd).clamp(-Z_CAP, Z_CAP))
}

/// The drums stem's vote: how much more the kick of each residue mod 4 stands out, from the stem alone (interleaved
/// stereo at `sample_rate`) and the grid `phase_secs + n * period_secs` the beat analysis found. With the bass line,
/// pads and vocals taken out, the kick and the hats of the drums stem are what is left, which is where the mix itself
/// is confused (a syncopated bass line, a piano riff). Two readings are averaged, the part under 150 Hz (the kick) and
/// the whole stem, since either alone is wrong on some tracks. A few numbers: this is all that needs keeping.
pub fn drums_sign(drums: &[f32], sample_rate: u32, phase_secs: f64, period_secs: f64) -> [f32; 4] {
    let low = residue_z(&stem_per_beat(drums, sample_rate as f64, 150.0, phase_secs, period_secs));
    let full = residue_z(&stem_per_beat(drums, sample_rate as f64, 8000.0, phase_secs, period_secs));
    [0, 1, 2, 3].map(|k| (low[k] + full[k]) * 0.5)
}

/// The steepest rise of the 4 ms energy of the stem, low-passed at `cut` Hz, within -20..+40 ms of each beat.
fn stem_per_beat(x: &[f32], sr: f64, cut: f64, phase: f64, period: f64) -> Vec<(i64, f32)> {
    let n = x.len() / 2;
    let a = (-2.0 * std::f64::consts::PI * cut / sr).exp() as f32;
    let w = ((0.004 * sr) as usize).max(1);
    let (mut lp, mut acc) = (0.0f32, 0.0f32);
    let mut ring = vec![0.0f32; w];
    let mut env = vec![0.0f32; n];
    for i in 0..n {
        lp = a * lp + (1.0 - a) * (x[i * 2] + x[i * 2 + 1]) * 0.5;
        let e = lp * lp;
        acc += e - ring[i % w];
        ring[i % w] = e;
        env[i] = (acc / w as f32).max(0.0).sqrt();
    }
    let step = ((0.002 * sr) as usize).max(1);
    let mut out = Vec::new();
    let mut k = ((0.5 - phase) / period).ceil().max(0.0) as i64;
    loop {
        let t = phase + k as f64 * period;
        let (lo, hi) = (((t - 0.020) * sr) as usize, ((t + 0.040) * sr) as usize);
        if hi + 1 >= n {
            break;
        }
        let mut best = 0.0f32;
        let mut i = lo + 2 * step;
        while i < hi {
            best = best.max(env[i] - env[i - 2 * step]);
            i += (step / 2).max(1);
        }
        out.push((k, best));
        k += 1;
    }
    out
}

/// The winning residue and its margin over the runner-up from the five signs' totals, with the drums stem's vote added
/// only where those signs do not agree (see [`DRUMS_ONLY_BELOW`]).
fn decide(base: [f32; 4], drums: Option<[f32; 4]>) -> (usize, f32) {
    let best = |t: &[f32; 4]| {
        let mut order = [0usize, 1, 2, 3];
        order.sort_by(|&x, &y| t[y].total_cmp(&t[x]));
        (order[0], t[order[0]] - t[order[1]])
    };
    let (residue, margin) = best(&base);
    match drums {
        Some(d) if margin < DRUMS_ONLY_BELOW => best(&[0, 1, 2, 3].map(|k| base[k] + W_DRUMS * d[k])),
        _ => (residue, margin),
    }
}

/// Decides the downbeat from the beat analysis and the track's summary. `None` when there is not enough to judge
/// (no tempo, a very short track); the caller keeps the analysis' own answer then.
pub fn choose(wave: &WaveSummary, a: &BeatAnalysis, sample_rate: u32) -> Option<Downbeat> {
    choose_with_drums(wave, a, sample_rate, None)
}

/// [`choose`] with one more voice when the track's drums stem has been separated: `drums` is [`drums_sign`] of it.
pub fn choose_with_drums(
    wave: &WaveSummary,
    a: &BeatAnalysis,
    sample_rate: u32,
    drums: Option<[f32; 4]>,
) -> Option<Downbeat> {
    if a.bpm <= 0.0 || sample_rate == 0 || a.kick.len() < 16 {
        return None;
    }
    let period_chunks = sample_rate as f64 / WAVEFORM_CHUNK_FRAMES as f64;
    let period_chunks = period_chunks * 60.0 / a.bpm as f64;
    let chunk_secs = WAVEFORM_CHUNK_FRAMES as f64 / sample_rate as f64;
    let (period, phase) = (period_chunks * chunk_secs, a.phase_chunks * chunk_secs);

    let kick = residue_z(&a.kick);
    let (bass, drops, boundary) = match BeatEnergy::of(wave, period, phase) {
        Some(be) => {
            let audible = |i: usize| be.bands[i][3] > 0.01;
            let bass: Vec<(i64, f32)> =
                (0..be.len()).filter(|&i| audible(i)).map(|i| (be.number(i), (be.bands[i][0] + 0.002).ln())).collect();
            let (flips, nf) = be.flip_residues();
            let (cuts, nc) = be.boundary_residues();
            (residue_z(&bass), vote_z(flips, nf), vote_z(cuts, nc))
        }
        None => ([0.0; 4], [0.0; 4], [0.0; 4]),
    };
    let mut start = [0.0f32; 4];
    start[a.start_beat.rem_euclid(4) as usize] = 1.0;

    let signs = [kick, bass, drops, boundary, start];
    let w = [W_KICK, W_BASS, W_DROP, W_BOUNDARY, W_START];
    let base: [f32; 4] = [0, 1, 2, 3].map(|k| (0..signs.len()).map(|s| w[s] * signs[s][k]).sum());
    let (residue, confidence) = decide(base, drums);
    let residue = residue as i64;

    // the first bar line at or after the start of the music, as the analysis always placed it
    let mut n = a.start_beat;
    while n.rem_euclid(4) != residue {
        n += 1;
    }
    let anchor_chunks = a.phase_chunks + n as f64 * period_chunks;
    Some(Downbeat { residue, confidence, signs, anchor_chunks })
}

/// A second opinion on a grid that is already placed, from the track's own sections (no audio needed): the bass and
/// the section boundaries, which are what is left when the kick cannot be heard separately.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opinion {
    /// Beats after the grid's bar 1 (0..3) that the signs would put bar 1: 0 when they agree with the grid.
    pub shift: i64,
    /// How much more the signs like that beat than the grid's own, in z-score units.
    pub margin: f32,
}

/// `downbeat_secs` is where the grid has a bar 1 (beat 0 of the numbering); `period` the seconds per beat.
pub fn from_sections(wave: &WaveSummary, period: f64, downbeat_secs: f64) -> Option<Opinion> {
    let be = BeatEnergy::of(wave, period, downbeat_secs)?;
    let bass: Vec<(i64, f32)> = (0..be.len())
        .filter(|&i| be.bands[i][3] > 0.01)
        .map(|i| (be.number(i), (be.bands[i][0] + 0.002).ln()))
        .collect();
    let (flips, nf) = be.flip_residues();
    let (cuts, nc) = be.boundary_residues();
    let (bz, fz, cz) = (residue_z(&bass), vote_z(flips, nf), vote_z(cuts, nc));
    let total: [f32; 4] = [0, 1, 2, 3].map(|k| W_BASS * bz[k] + W_DROP * fz[k] + W_BOUNDARY * cz[k]);
    let best = (0..4).max_by(|&a, &b| total[a].total_cmp(&total[b]))?;
    Some(Opinion { shift: best as i64, margin: total[best] - total[0] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn residue_z_picks_the_standing_out_beat() {
        let s: Vec<(i64, f32)> =
            (0..200i64).map(|n| (n, if n.rem_euclid(4) == 2 { 2.0 } else { 1.0 } + 0.1 * (n % 7) as f32)).collect();
        let z = residue_z(&s);
        let best = (0..4).max_by(|&a, &b| z[a].total_cmp(&z[b])).unwrap();
        assert_eq!(best, 2);
        assert!(z[2] > 3.0 && z[0] < 0.0);
        // negative beat numbers count the same way
        let s: Vec<(i64, f32)> = (-100..100i64).map(|n| (n, if n.rem_euclid(4) == 1 { 3.0 } else { 0.0 })).collect();
        assert_eq!((0..4).max_by(|&a, &b| residue_z(&s)[a].total_cmp(&residue_z(&s)[b])), Some(1));
    }

    #[test]
    fn vote_z_needs_a_few_boundaries_and_a_clear_majority() {
        assert_eq!(vote_z([1.0, 0.0, 0.0, 0.0], 1), [0.0; 4]);
        let z = vote_z([6.0, 1.0, 1.0, 0.0], 8);
        assert!(z[0] > 2.5 && z[3] < 0.0);
        assert!(vote_z([2.0, 2.0, 2.0, 2.0], 8).iter().all(|v| v.abs() < 1e-6));
    }

    #[test]
    fn residue_z_is_quiet_without_evidence() {
        assert_eq!(residue_z(&[]), [0.0; 4]);
        assert_eq!(residue_z(&vec![(0, 1.0); 100]), [0.0; 4]);
        assert!(residue_z(&(0..8).map(|n| (n, n as f32)).collect::<Vec<_>>()).iter().all(|z| *z == 0.0));
    }

    /// 124 BPM, 64 bars. A kick on every beat; a bass note at the start of each bar that rings for most of it; a
    /// pad that holds for four bars and moves between the mids and the highs. `offset_beats` shifts where the bars
    /// start relative to the beginning of the file.
    fn song(offset_beats: usize) -> Vec<f32> {
        let sr = 44100usize;
        let beat = (60.0 / 124.0 * sr as f64) as usize;
        let beats = 4 * 64 + 8;
        let mut x = vec![0.0f32; beat * beats];
        let tone = |x: &mut [f32], at: usize, len: usize, hz: f32, amp: f32, decay: f32| {
            for i in 0..len.min(x.len().saturating_sub(at)) {
                let t = i as f32 / sr as f32;
                x[at + i] += amp * (t * hz * std::f32::consts::TAU).sin() * (-t * decay).exp();
            }
        };
        for b in 0..beats {
            tone(&mut x, b * beat, beat, 60.0, 0.4, 14.0);
            if b >= offset_beats && (b - offset_beats).is_multiple_of(4) {
                tone(&mut x, b * beat, beat * 4, 90.0, 0.3, 1.5);
                let bar = (b - offset_beats) / 4;
                let hz = if (bar / 4).is_multiple_of(2) { 700.0 } else { 5000.0 };
                tone(&mut x, b * beat, beat * 4, hz, 0.12, 0.0);
            }
        }
        x
    }

    #[test]
    fn finds_the_bar_line_wherever_it_is() {
        for shift in 0..4usize {
            let x = song(shift);
            let wave = WaveSummary::build(&x, 1, 44100);
            let a = crate::bpm::analyze_beats_detail(&x, 1, 44100);
            assert!((a.bpm - 124.0).abs() < 0.3, "tempo {}", a.bpm);
            let d = choose(&wave, &a, 44100).expect("a verdict");
            // the bar lines are at beats shift, shift+4, ...: in seconds
            let period = 60.0 / a.bpm as f64;
            let first = d.anchor_chunks * 256.0 / 44100.0;
            let beats_from_start = ((first / period) - shift as f64).rem_euclid(4.0);
            let off = beats_from_start.min(4.0 - beats_from_start);
            assert!(off < 0.15, "shift {shift}: downbeat at {first:.3} s, {beats_from_start:.2} beats from a bar line (signs {:?}, confidence {})", d.signs, d.confidence);
        }
    }

    #[test]
    fn second_opinion_agrees_with_the_right_grid_and_corrects_a_late_one() {
        let x = song(0);
        let wave = WaveSummary::build(&x, 1, 44100);
        let period = 60.0 / 124.0;
        // the song's bar lines are at 0, 4, 8, ... beats from the start of the file
        let right = from_sections(&wave, period, 0.0).expect("an opinion");
        assert_eq!(right.shift, 0, "{right:?}");
        let late = from_sections(&wave, period, period).expect("an opinion"); // grid bar 1 one beat late
        assert_eq!(late.shift, 3, "bar 1 is three beats after a grid that is a beat late: {late:?}");
        assert!(late.margin > 1.0);
    }

    #[test]
    fn nothing_to_judge_is_none() {
        let a = BeatAnalysis::default();
        assert!(choose(&WaveSummary::default(), &a, 44100).is_none());
    }

    #[test]
    fn the_drums_stem_breaks_a_tie_but_does_not_overrule_signs_that_agree() {
        // five signs that barely agree (margin 0.4, residue 3 ahead): the stem's vote for residue 0 decides
        let (r, m) = decide([2.0, 0.0, 0.0, 2.4], Some([2.5, 0.0, 0.0, -0.5]));
        assert_eq!(r, 0);
        assert!(m > 1.0, "{m}");
        // a margin of 4 stays, however loudly the stem disagrees
        assert_eq!(decide([2.0, 0.0, 0.0, 6.0], Some([6.0, 0.0, 0.0, -6.0])).0, 3);
        // no stem, no change
        let (r, m) = decide([2.0, 0.0, 0.0, 2.4], None);
        assert_eq!(r, 3);
        assert!((m - 0.4).abs() < 1e-5, "{m}");
    }
}
