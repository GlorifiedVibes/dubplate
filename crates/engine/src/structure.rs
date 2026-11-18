//! The shape of a track: how loud each beat is in the bass, mids and highs, where the sound changes, and from that the
//! sections (intro, build, drop, break, outro) and the phrase length DJs count in.
//!
//! Everything here reads the waveform summary ([`WaveSummary`]), not the audio, so it takes a few milliseconds and can
//! be redone whenever the grid is edited.
//!
//! The method is the plain one from the literature on electronic music: take the average sound of the `L` beats
//! before and after every beat, and call the distance between the two the novelty of that beat (a block version of
//! Foote's checkerboard kernel, tried at three widths). Peaks of novelty are section boundaries, and they sit on bar
//! lines, mostly on 4, 8 or 16 bar boundaries; that is used both to snap them to the grid and, in [`crate::downbeat`],
//! to help decide which beat is the 1.

use crate::waveform_data::WaveSummary;

/// Half-widths, in beats, of the windows compared on either side of a beat: 2, 4 and 8 bars.
const WINDOWS: [usize; 3] = [8, 16, 32];
/// Boundaries closer than this many beats (4 bars) are one boundary.
const MIN_SECTION_BEATS: usize = 16;
/// A boundary has to rise this far (log-amplitude units, summed over the bands) above its surroundings: a bass line
/// coming in or going out is about 3.
const MIN_NOVELTY: f32 = 0.5;
/// The bass has to come in this much (log amplitude, about 2.7 times) to count as an entry.
/// Floor added before taking logarithms of amplitudes, so silence is a finite, small number.
const LOG_FLOOR: f32 = 0.002;

/// Loudness of one beat of the track: bass, mid, high and overall, as amplitudes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeatEnergy {
    /// Number of the first beat in `bands`; beat `n` is at `phase + n * period`.
    pub first: i64,
    /// Start of each beat in seconds.
    pub secs: Vec<f64>,
    /// `[low, mid, high, overall]` over each beat.
    pub bands: Vec<[f32; 4]>,
}

impl BeatEnergy {
    /// `period` seconds per beat; `phase` is the time in seconds of any beat (even one before the track starts).
    /// `None` when the summary is empty or the tempo is not usable.
    pub fn of(wave: &WaveSummary, period: f64, phase: f64) -> Option<Self> {
        if wave.is_empty() || !(period > 0.05) || wave.sample_rate == 0 {
            return None;
        }
        let sr = wave.sample_rate as f64;
        let length = wave.total_frames as f64 / sr;
        let level = wave.level_for(period * sr / 16.0)?;
        let first = ((-phase) / period).ceil() as i64;
        let mut out = BeatEnergy { first, ..Default::default() };
        let mut n = first;
        loop {
            let t = phase + n as f64 * period;
            if t + period > length + 1e-9 {
                break;
            }
            let l = level.span(t * sr, (t + period) * sr);
            out.secs.push(t);
            out.bands.push([l.bands[0], l.bands[1], l.bands[2], l.rms]);
            n += 1;
        }
        (out.secs.len() >= 8).then_some(out)
    }

    pub fn len(&self) -> usize {
        self.secs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.secs.is_empty()
    }

    /// Beat number of entry `i`.
    pub fn number(&self, i: usize) -> i64 {
        self.first + i as i64
    }

    fn log_bands(&self, i: usize) -> [f32; 4] {
        let b = self.bands[i];
        [0, 1, 2, 3].map(|k| (b[k] + LOG_FLOOR).ln())
    }

    /// How much the sound changes at the start of each beat, as the distance between the average of the `half`
    /// beats before it and the `half` beats from it (log amplitudes of the three bands).
    pub fn novelty(&self, half: usize) -> Vec<f32> {
        let n = self.len();
        let f: Vec<[f32; 4]> = (0..n).map(|i| self.log_bands(i)).collect();
        // running sums so each beat costs O(1)
        let mut sum = vec![[0.0f64; 4]; n + 1];
        for i in 0..n {
            for k in 0..4 {
                sum[i + 1][k] = sum[i][k] + f[i][k] as f64;
            }
        }
        let mean = |a: usize, b: usize| -> [f32; 4] {
            let w = (b - a).max(1) as f64;
            [0, 1, 2, 3].map(|k| ((sum[b][k] - sum[a][k]) / w) as f32)
        };
        (0..n)
            .map(|i| {
                if i < half || i + half > n {
                    return 0.0;
                }
                let (l, r) = (mean(i - half, i), mean(i, i + half));
                // the three bands count; the overall level only breaks ties
                (0..3).map(|k| (l[k] - r[k]).abs()).sum::<f32>() + 0.5 * (l[3] - r[3]).abs()
            })
            .collect()
    }

    /// Where the bass is "in": the average bass loudness of the bar from each beat on (so a bass line that is a hit
    /// and three weaker beats, as in drum and bass, reads as one steady level, and a single crash does not count)
    /// against the loud bass of the track, with a gap between the level that turns it on and the one that turns it
    /// off. Short blips of bass are dropped and tiny gaps filled. A drop is the beat where the bass comes in and
    /// stays: the bar average gets there a beat early, so the entry is moved on to the first real hit of the bass.
    pub fn bass_state(&self) -> BassState {
        let n = self.len();
        let low: Vec<f32> = self.bands.iter().map(|b| b[0]).collect();
        let ahead: Vec<f32> = (0..n)
            .map(|i| {
                let end = (i + 4).min(n);
                low[i..end].iter().sum::<f32>() / (end - i) as f32
            })
            .collect();
        let mut sorted = ahead.clone();
        sorted.sort_by(f32::total_cmp);
        let loud = sorted[((n as f32 * 0.85) as usize).min(n - 1)];
        if loud < 0.01 {
            return BassState { high: vec![false; n], flips: Vec::new(), loud };
        }
        let (on, off) = (BASS_ON * loud, BASS_OFF * loud);
        let mut high = Vec::with_capacity(n);
        let mut state = ahead[0] >= on;
        for &v in &ahead {
            if !state && v >= on {
                state = true;
            } else if state && v < off {
                state = false;
            }
            high.push(state);
        }
        // runs shorter than the minimum are the other state
        for (value, shortest) in [(true, MIN_BASS_RUN), (false, MIN_BASS_GAP)] {
            let mut i = 0;
            while i < n {
                let j = (i..n).find(|&k| high[k] != high[i]).unwrap_or(n);
                if high[i] == value && j - i < shortest && i > 0 && j < n {
                    high[i..j].fill(!value);
                }
                i = j;
            }
        }
        // The bar average crosses its level up to three beats before the bass does (it looks ahead): the bass comes in
        // on the first beat from there with a hit as loud as the bass gets, and goes out on the first beat that is
        // not above the off level.
        let (hit, quiet) = (BASS_HIT * loud, off);
        let mut cuts: Vec<usize> = Vec::new();
        for i in 1..n {
            if high[i] == high[i - 1] {
                continue;
            }
            let to = (i..(i + 4).min(n)).find(|&j| if high[i] { low[j] >= hit } else { low[j] < quiet }).unwrap_or(i);
            // moves never run into the next change: keep them in order
            let next = (i + 1..n).find(|&k| high[k] != high[k - 1]).unwrap_or(n);
            cuts.push(if to < next { to } else { i });
        }
        let mut state = high[0];
        let mut flips = Vec::new();
        let mut refined = vec![high[0]; n];
        let mut next = cuts.iter().peekable();
        for i in 0..n {
            if next.peek() == Some(&&i) {
                next.next();
                state = !state;
                flips.push((i, state));
            }
            refined[i] = state;
        }
        BassState { high: refined, flips, loud }
    }

    /// Which beat of the bar the bass comes in on, as a count per residue mod 4 and the total. On the tracks tried
    /// this is the most reliable sign of the bar line: a drop starts a bar. Where it goes out is left out: it rings
    /// out, is cut a beat early or late, and was often a beat from the entries (Bangarang, Sandstorm).
    pub fn flip_residues(&self) -> ([f32; 4], usize) {
        let entries: Vec<usize> = self.bass_state().flips.into_iter().filter(|f| f.1).map(|f| f.0).collect();
        let mut votes = [0.0f32; 4];
        for &i in &entries {
            votes[self.number(i).rem_euclid(4) as usize] += 1.0;
        }
        (votes, entries.len())
    }

    /// Which beat of the bar the broader section changes (see [`boundaries`]) start on, leaving out those within
    /// four beats of a bass change, which [`Self::flip_residues`] already counts.
    pub fn boundary_residues(&self) -> ([f32; 4], usize) {
        let flips = self.bass_state().flips;
        let mut votes = [0.0f32; 4];
        let mut total = 0;
        for c in boundaries(&self.combined_novelty()) {
            if flips.iter().any(|&(i, _)| i.abs_diff(c) <= 4) {
                continue;
            }
            votes[self.number(c).rem_euclid(4) as usize] += 1.0;
            total += 1;
        }
        (votes, total)
    }

    /// The three widths of [`Self::novelty`] averaged. The values are in log-amplitude units, so a steady track
    /// stays near zero and a bass line entering is a few units whatever the loudness.
    pub fn combined_novelty(&self) -> Vec<f32> {
        let mut total = vec![0.0f32; self.len()];
        for &w in &WINDOWS {
            for (t, x) in total.iter_mut().zip(self.novelty(w)) {
                *t += x / WINDOWS.len() as f32;
            }
        }
        total
    }
}

/// Whether the bass is in at each beat, and the beats where that changes: `(index, bass now in)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BassState {
    pub high: Vec<bool>,
    pub flips: Vec<(usize, bool)>,
    /// The loud bass of the track the levels are measured against (a bar's average, 85th percentile).
    pub loud: f32,
}

/// The bass is in at this share of the loud bass of the track (its 85th percentile), and out below the lower one.
const BASS_ON: f32 = 0.7;
const BASS_OFF: f32 = 0.5;
/// A beat is a hit of the bass at this share of the loud bass.
const BASS_HIT: f32 = 0.7;
/// Bass shorter than this many beats is a hit, not a drop; a gap shorter than the other number is a dropout.
const MIN_BASS_RUN: usize = 8;
const MIN_BASS_GAP: usize = 3;

/// What a stretch of the track is doing, from its loudness against the rest of the track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Intro,
    /// Rising energy leading into a drop.
    Build,
    /// Full energy with the bass in.
    Drop,
    /// Plenty going on, but not the loudest part.
    Groove,
    /// The bass is out.
    Break,
    Outro,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Intro => "Intro",
            Kind::Build => "Build",
            Kind::Drop => "Drop",
            Kind::Groove => "Groove",
            Kind::Break => "Break",
            Kind::Outro => "Outro",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Section {
    pub start_secs: f64,
    pub end_secs: f64,
    /// Length in beats (a whole number when the boundaries are on the grid).
    pub beats: u32,
    pub kind: Kind,
    /// Overall loudness against the loud parts of the track: about 1.0 is as loud as the track gets.
    pub energy: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Structure {
    pub sections: Vec<Section>,
    /// Phrase length in beats the boundaries follow (16 or 32), with the beat number (`n mod phrase`) they start on.
    /// `None` when the boundaries do not agree on one.
    pub phrase: Option<(u32, i64)>,
}

impl Structure {
    /// Sections of the track. `bar_residue` is the beat number (mod 4) that is the 1 of the bar, so boundaries can be
    /// put on bar lines: beat `n` is at `phase + n * period`, and `n mod 4 == bar_residue` is a downbeat.
    pub fn analyse(wave: &WaveSummary, period: f64, phase: f64, bar_residue: i64) -> Self {
        let Some(be) = BeatEnergy::of(wave, period, phase) else { return Self::default() };
        Self::from_beats(&be, bar_residue, wave.total_frames as f64 / wave.sample_rate as f64)
    }

    pub fn from_beats(be: &BeatEnergy, bar_residue: i64, length_secs: f64) -> Self {
        let bass = be.bass_state().with_entries_on_the_hit(be, bar_residue);
        // the bass coming in or going out is a section start exactly where it happens; between those, the starts are
        // chosen on bar lines to fit the band levels in phrase-long stretches
        let fixed: Vec<usize> = bass.flips.iter().map(|f| f.0).collect();
        let mut cuts = segment(be, bar_residue, &fixed);
        cuts.extend(fixed);
        cuts.sort_unstable();
        cuts.dedup();
        let phrase = phrase_lattice(be, &cuts);
        cuts.retain(|&c| c > 0 && c < be.len());
        let mut edges = vec![0usize];
        edges.extend(cuts);
        edges.push(be.len());
        // join sections that came out shorter than the minimum after snapping
        // (the bass coming in is kept whatever: a short break before a drop is folded into what came before, so the
        // drop still starts where the bass arrives)
        let came_in = |e: usize| bass.flips.iter().any(|f| f.0 == e && f.1);
        let mut keep = vec![edges[0]];
        for &e in &edges[1..edges.len() - 1] {
            if came_in(e) {
                while keep.len() > 1 && e - *keep.last().unwrap() < MIN_SECTION_BEATS && !came_in(*keep.last().unwrap())
                {
                    keep.pop();
                }
                keep.push(e);
            } else if e - *keep.last().unwrap() >= MIN_SECTION_BEATS
                && edges[edges.len() - 1] - e >= MIN_SECTION_BEATS / 2
            {
                keep.push(e);
            }
        }
        keep.push(edges[edges.len() - 1]);
        let mut sections = label(be, &bass, &keep, length_secs);
        one_drop_per_run(&mut sections);
        let phrase = phrase_of(be, &sections).or(phrase);
        Structure { sections, phrase }
    }
}

/// A drop the user names takes over a section start this close to it (beats); further away it starts a section of
/// its own.
const TAKES_OVER_BEATS: f64 = 8.0;

impl Structure {
    /// Where the Drop sections start, in seconds.
    pub fn drops(&self) -> Vec<f64> {
        self.sections.iter().filter(|s| s.kind == Kind::Drop).map(|s| s.start_secs).collect()
    }

    /// The same sections with the drops where a person says they are (`drops`, seconds, on beats of the grid), for a
    /// track whose drops were corrected by hand. Every Drop found becomes a Groove; then each named drop either moves
    /// the nearest section start onto itself (when that is within two bars and no other section is in the way) or
    /// cuts the section it falls in, and the section that starts there is a Drop. `period` is seconds a beat.
    pub fn with_drops(&self, drops: &[f64], period: f64) -> Structure {
        let mut secs = self.sections.clone();
        if secs.is_empty() || period <= 0.0 {
            return self.clone();
        }
        for s in secs.iter_mut().filter(|s| s.kind == Kind::Drop) {
            s.kind = Kind::Groove;
        }
        let (start, end) = (secs[0].start_secs, secs[secs.len() - 1].end_secs);
        let mut named: Vec<f64> = drops.iter().copied().filter(|t| *t > start - period / 2.0 && *t < end).collect();
        named.sort_by(f64::total_cmp);
        named.dedup_by(|a, b| (*a - *b).abs() < period / 2.0);
        for t in named {
            let near = (0..secs.len())
                .min_by(|&a, &b| (secs[a].start_secs - t).abs().total_cmp(&(secs[b].start_secs - t).abs()))
                .unwrap();
            let gap = (secs[near].start_secs - t).abs();
            // a start can move only between its neighbours, and not onto a drop already named
            let free = near > 0
                && secs[near].kind != Kind::Drop
                && t > secs[near - 1].start_secs + period / 2.0
                && t < secs[near].end_secs - period / 2.0;
            if gap < period / 2.0 {
                secs[near].kind = Kind::Drop;
            } else if free && gap <= TAKES_OVER_BEATS * period {
                secs[near - 1].end_secs = t;
                secs[near].start_secs = t;
                secs[near].kind = Kind::Drop;
            } else if let Some(i) = secs.iter().position(|s| t > s.start_secs && t < s.end_secs) {
                let mut after = secs[i];
                after.start_secs = t;
                after.kind = Kind::Drop;
                secs[i].end_secs = t;
                secs.insert(i + 1, after);
            }
        }
        for s in secs.iter_mut() {
            s.beats = ((s.end_secs - s.start_secs) / period).round().max(0.0) as u32;
        }
        Structure { sections: secs, phrase: self.phrase }
    }
}

/// How much louder (overall, a bar or two averaged) the stretch after a later bar line has to be than the stretch
/// between the bass entry and that line for the drop to be there and the stretch before it a lead-in.
const HIT_OVER_LEAD_IN: f32 = 1.2;
/// The same for the bass band alone: a swell (Children) gets no louder overall at the hit, the bass does.
const BASS_OVER_LEAD_IN: f32 = 1.12;
/// A lead-in's bass is under this share of the loud bass of the track.
const LEAD_IN_BASS: f32 = 0.9;
/// A lead-in is at most this many beats (two bars).
const LEAD_IN_MAX: usize = 8;

impl BassState {
    /// Puts each bass entry on the beat the drop lands on. The level test in [`BeatEnergy::bass_state`] says where the
    /// bass got loud, which is often not quite it:
    ///
    /// * **off the bar line.** A pick-up note a beat before the drop (Skepta - Can't Play Myself), or a first beat
    ///   that is quieter than the rest, puts the entry a beat or two from the bar line. A drop starts a bar (every
    ///   study of DJ cue points finds them on downbeats; of 50 drops marked by ear here, all did), so the entry goes
    ///   to the bar line a beat away. Half a bar out it stays: neither line is nearer.
    /// * **a lead-in.** The bass comes in a bar or two early, quieter, or swells (Children; Danny Byrd -
    ///   Superstylin', two bars of bass and a gap before the drop). The drop is then the later bar line: the one,
    ///   within two bars, after which the track is clearly louder than it was between the entry and that line. Whole
    ///   bars are compared, not single beats: one loud beat a bar later is not a drop (Tangaroa).
    ///
    /// Only the entries move, and only here: the bar line is voted from the bass entries as they were.
    fn with_entries_on_the_hit(mut self, be: &BeatEnergy, bar_residue: i64) -> Self {
        let n = be.len();
        let mean = |band: usize, a: usize, b: usize| {
            let b = b.min(n);
            be.bands[a.min(b)..b].iter().map(|x| x[band]).sum::<f32>() / (b.saturating_sub(a)).max(1) as f32
        };
        let places: Vec<usize> = self.flips.iter().map(|f| f.0).collect();
        for k in 0..self.flips.len() {
            let (i, up) = self.flips[k];
            if !up {
                continue;
            }
            // room to move: after the change before this one, and leaving the shortest section before the next
            let floor = if k > 0 { self.flips[k - 1].0 + 1 } else { 0 };
            let stop = places.get(k + 1).copied().unwrap_or(n).min(n);
            let last = stop.saturating_sub(MIN_SECTION_BEATS);
            let off = (be.number(i) - bar_residue).rem_euclid(4) as usize;
            let mut at = i;
            if off != 0 {
                let (back, on) = (i.checked_sub(off).filter(|&b| b >= floor), Some(i + 4 - off).filter(|&f| f <= last));
                at = match (back, on) {
                    // half a bar out is as near one line as the other: left where it is
                    _ if off == 2 => i,
                    (Some(b), _) if off == 1 => b,
                    (_, Some(f)) => f,
                    (Some(b), None) => b,
                    (None, None) => i,
                };
            }
            if (be.number(at) - bar_residue).rem_euclid(4) == 0 {
                // how far over its threshold the step up at a later bar line is, overall or in the bass alone
                let mut best = (1.0, at);
                for c in [at + 4, at + LEAD_IN_MAX] {
                    if c > last {
                        break;
                    }
                    let step = |band: usize| mean(band, c, c + (c - at).max(8)) / mean(band, at, c).max(1e-6);
                    // bass already as loud as the track gets is the drop itself, however the next bars compare
                    if mean(0, at, c) > LEAD_IN_BASS * self.loud {
                        continue;
                    }
                    let over = (step(3) / HIT_OVER_LEAD_IN).max(step(0) / BASS_OVER_LEAD_IN);
                    if over >= best.0 {
                        best = (over, c);
                    }
                }
                at = best.1;
            }
            self.flips[k].0 = at;
        }
        self
    }
}

/// The price of one more section, in the squared band levels [`segment`] fits (the loud beats of each band are 1). On
/// the 20-track set 0.5 to 2 all keep the drops found; 1 gives as many sections as the novelty peaks did.
const PER_SECTION: f32 = 1.0;
/// The price of a section that is not a whole number of 8-bar phrases long, and the further price of one (first or
/// last between two fixed starts) that is not even a whole number of 4-bar ones.
const NOT_EIGHT_BARS: f32 = 0.4;
const NOT_FOUR_BARS: f32 = 0.8;
/// How much each band counts in the fit: the bass most, as that is what drops and breaks change.
const BAND_WEIGHT: [f32; 3] = [1.5, 1.0, 0.7];

/// Section starts between the fixed ones (`fixed`, beat indices: where the bass comes in or goes out), on bar lines,
/// chosen by dynamic programming (as openDJ's phrase finder does, written afresh here): each stretch costs how far its
/// beats' band levels are from its own mean, plus [`PER_SECTION`], plus a price when its length is not a whole number
/// of 8-bar phrases counted from where it starts. Stretches between two fixed starts are fitted on their own, so a
/// drop always starts a phrase. Sections inside a stretch are at least [`MIN_SECTION_BEATS`]; the first and last of
/// it may be shorter.
fn segment(be: &BeatEnergy, bar_residue: i64, fixed: &[usize]) -> Vec<usize> {
    let n = be.len();
    if n < 2 * MIN_SECTION_BEATS {
        return Vec::new();
    }
    // each band scaled so the loud beats (90th percentile) are 1, then weighted
    let mut scale = [1.0f32; 3];
    for (k, sc) in scale.iter_mut().enumerate() {
        let mut v: Vec<f32> = be.bands.iter().map(|b| b[k]).collect();
        v.sort_by(f32::total_cmp);
        *sc = BAND_WEIGHT[k] / v[((n as f32 * 0.9) as usize).min(n - 1)].max(1e-6);
    }
    let x: Vec<[f32; 3]> = be.bands.iter().map(|b| [0, 1, 2].map(|k| b[k] * scale[k])).collect();
    let mut s1 = vec![[0f32; 3]; n + 1];
    let mut s2 = vec![0f32; n + 1];
    for i in 0..n {
        s1[i + 1] = [0, 1, 2].map(|k| s1[i][k] + x[i][k]);
        s2[i + 1] = s2[i] + x[i].iter().map(|v| v * v).sum::<f32>();
    }
    let sse = |a: usize, b: usize| {
        let m = (b - a) as f32;
        s2[b] - s2[a] - [0, 1, 2].iter().map(|&k| (s1[b][k] - s1[a][k]).powi(2)).sum::<f32>() / m
    };
    let on_bar = |i: usize| (be.number(i) - bar_residue).rem_euclid(4) == 0;
    let mut stops: Vec<usize> = fixed.iter().copied().filter(|&c| c > 0 && c < n).collect();
    stops.insert(0, 0);
    stops.push(n);
    stops.sort_unstable();
    stops.dedup();
    let mut out = Vec::new();
    for w in stops.windows(2) {
        let (a, b) = (w[0], w[1]);
        // possible starts: the bar lines in between, at least a bar from either end
        let mut pos: Vec<usize> = vec![a];
        pos.extend((a + 4..b.saturating_sub(3)).filter(|&i| on_bar(i)));
        pos.push(b);
        let m = pos.len();
        let mut best = vec![f32::INFINITY; m];
        let mut from = vec![0usize; m];
        best[0] = 0.0;
        for j in 1..m {
            for i in 0..j {
                let len = pos[j] - pos[i];
                let edge = i == 0 || j == m - 1;
                if !edge && len < MIN_SECTION_BEATS {
                    continue;
                }
                // lengths count from the stretch's first bar line, so a part bar before it is not held against it
                let price = if len.is_multiple_of(32) {
                    0.0
                } else if len.is_multiple_of(16) {
                    NOT_EIGHT_BARS
                } else if edge {
                    NOT_EIGHT_BARS + NOT_FOUR_BARS
                } else {
                    continue;
                };
                let c = best[i] + sse(pos[i], pos[j]) + PER_SECTION + price;
                if c < best[j] {
                    best[j] = c;
                    from[j] = i;
                }
            }
        }
        let mut j = m - 1;
        while j > 0 {
            j = from[j];
            if j > 0 {
                out.push(pos[j]);
            }
        }
    }
    out.sort_unstable();
    out
}

/// Indices of the beats where the novelty peaks clearly: a local maximum over 8 beats either side that stands at least
/// [`MIN_NOVELTY`] above the lower of the two valleys within 16 beats (a peak on a long slope is not a boundary), the
/// strongest kept when two are closer than a section can be.
fn boundaries(novelty: &[f32]) -> Vec<usize> {
    let n = novelty.len();
    if n < 2 * MIN_SECTION_BEATS {
        return Vec::new();
    }
    let valley = |a: usize, b: usize| novelty[a..=b].iter().cloned().fold(f32::MAX, f32::min);
    let mut peaks: Vec<usize> = (1..n - 1)
        .filter(|&i| {
            let (a, b) = (i.saturating_sub(8), (i + 8).min(n - 1));
            if !(a..=b).all(|j| novelty[j] <= novelty[i]) {
                return false;
            }
            let (wa, wb) = (i.saturating_sub(16), (i + 16).min(n - 1));
            novelty[i] - valley(wa, i).max(valley(i, wb)) >= MIN_NOVELTY
        })
        .collect();
    peaks.sort_by(|&a, &b| novelty[b].total_cmp(&novelty[a]));
    let mut kept: Vec<usize> = Vec::new();
    for p in peaks {
        if kept.iter().all(|&k| k.abs_diff(p) >= MIN_SECTION_BEATS) {
            kept.push(p);
        }
    }
    kept
}

/// 16 or 32 beats, and the beat number mod that they start on, when at least half the boundaries (and at least
/// three) sit on one lattice; 32 is preferred when it does as well as 16 (8 bars is the common phrase).
fn phrase_lattice(be: &BeatEnergy, cuts: &[usize]) -> Option<(u32, i64)> {
    if cuts.len() < 3 {
        return None;
    }
    let mut best: Option<(u32, i64, usize)> = None;
    for &p in &[32u32, 16] {
        for &c in cuts {
            let r = be.number(c).rem_euclid(p as i64);
            let hits = cuts.iter().filter(|&&d| (be.number(d) - r).rem_euclid(p as i64) == 0).count();
            if best.is_none_or(|(bp, _, bh)| hits > bh || (hits == bh && p > bp)) {
                best = Some((p, r, hits));
            }
        }
    }
    let (p, r, hits) = best?;
    (hits >= 3 && hits * 2 >= cuts.len()).then_some((p, r))
}

/// How much a drop counts for against any other section start when the phrase line is chosen.
const PHRASE_DROP_WEIGHT: f32 = 3.0;

/// The phrase the track is built in, from the sections once they are named: 8 bars (32 beats) or failing that 4 bars,
/// and the beat number mod that its lines fall on. A drop is where a phrase starts if anything is (DJs' own cue
/// points cluster on 8- and 16-bar lines counted from a drop; of the 50 drops marked by ear here, all but a handful
/// share one 8-bar line with the other drops of their track), so drops count three times what any other section start
/// does, and the line is the one holding at least half of that weight and at least two starts. The line is usually,
/// but not always, a whole number of phrases from bar 1: a two-bar pick-up before bar 3 shifts every phrase by two
/// bars (Calvin Harris - Miracle), which is why it is read from the track and not counted from bar 1.
fn phrase_of(be: &BeatEnergy, sections: &[Section]) -> Option<(u32, i64)> {
    let marks: Vec<(i64, f32)> = sections
        .iter()
        .skip(1)
        .filter_map(|s| {
            let i = be.secs.partition_point(|&t| t < s.start_secs - 1e-6);
            (i < be.len()).then(|| (be.number(i), if s.kind == Kind::Drop { PHRASE_DROP_WEIGHT } else { 1.0 }))
        })
        .collect();
    let total: f32 = marks.iter().map(|m| m.1).sum();
    for p in [32i64, 16] {
        let mut best = (0.0f32, 0i64, 0usize);
        for &(n, _) in &marks {
            let r = n.rem_euclid(p);
            let on = marks.iter().filter(|m| m.0.rem_euclid(p) == r);
            let (w, count) = on.fold((0.0, 0), |a, m| (a.0 + m.1, a.1 + 1));
            if w > best.0 {
                best = (w, r, count);
            }
        }
        if best.2 >= 2 && best.0 * 2.0 >= total {
            return Some((p as u32, best.1));
        }
    }
    None
}

/// A drop of at most this many beats (8 bars) that is followed, in the same run, by a clearly louder one is a lead-in.
const LEAD_IN_BEATS: u32 = 32;
/// How much louder the next drop has to be.
const LOUDER: f32 = 1.1;

/// A lone Drop of at most this many beats (five bars) that the track backs out of (the bass goes again) is a tease.
const TEASE_BEATS: u32 = 20;
/// A Drop under this share of the loudest Drop's energy is not one.
const WEAK_DROP: f32 = 0.75;
/// A track that opens with the bass in for at least this many beats (16 bars), about as loud as it gets, opens on a
/// drop.
const OPENING_DROP_BEATS: u32 = 64;
const OPENING_DROP_ENERGY: f32 = 0.9;

/// Which of the Drop sections [`label`] found are drops, as people count them. The rules, each from what was heard by
/// ear on the 20-track set (`scripts/drop_tests`) and each the same idea as a rule in the published work on DJ cue
/// points (a cue is where something new starts *and holds*):
///
/// * **One per run.** Of several in a row (the bass stays in while the sound changes) the first is the drop and the
///   others are the same stretch carrying on: of 50 drops heard by ear, 35 were the first of a run; flagging every one
///   found 40 with 93 flags. The exception is a short first member (8 bars at most) that the run builds up from into a
///   clearly louder one (Bangarang's bar 5; Calvin Harris - Miracle; Dance Now): that is a lead-in and the louder one
///   is the drop. Lead-ins become Build, followers Groove.
/// * **It holds.** A lone burst of bass of a few bars that the track then backs out of is a tease before the drop
///   (Stereo Fiction, twice; A Little Sound - Stars), so a Build.
/// * **It is loud.** A Drop well under the loudest Drop of the track is not one, and the next member of its run that
///   is loud enough is the drop instead (Dance Now's second half). What is not a drop is a Build if a drop follows it,
///   else a Groove.
///
/// Together the last two took 52 flags to 48 on the set and found one more drop. Telling drops apart by energy
/// alone within a run ("each step up is a drop") found one more and added three flags, so it was left out.
fn one_drop_per_run(sections: &mut [Section]) {
    let loudest = sections.iter().filter(|s| s.kind == Kind::Drop).map(|s| s.energy).fold(0.0, f32::max);
    let weak = |s: &Section| s.energy < WEAK_DROP * loudest;
    // Drop sections that turned out not to be drops and come before the run's drop, if it has one
    let mut demoted: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < sections.len() {
        if sections[i].kind != Kind::Drop {
            i += 1;
            continue;
        }
        let end = (i..sections.len()).find(|&k| sections[k].kind != Kind::Drop).unwrap_or(sections.len());
        let lead_in = |k: usize| {
            sections[k].beats <= LEAD_IN_BEATS
                && sections[k + 1..end].iter().any(|later| later.energy >= LOUDER * sections[k].energy)
        };
        // the drop of the run: the first member that is neither a lead-in nor too quiet to be one
        let main = (i..end).find(|&k| !weak(&sections[k]) && (k == end - 1 || !lead_in(k)));
        let tease = end - i == 1 && sections[i].beats <= TEASE_BEATS && end < sections.len();
        for (k, s) in sections.iter_mut().enumerate().take(end).skip(i) {
            s.kind = match main {
                Some(m) if k == m && !tease => Kind::Drop,
                Some(m) if k > m => Kind::Groove,
                _ => {
                    demoted.push(k);
                    Kind::Build
                }
            };
        }
        i = end;
    }
    // a Build leads to a drop: with none after it, it is a groove
    for k in demoted {
        if !sections[k + 1..].iter().any(|s| s.kind == Kind::Drop) {
            sections[k].kind = Kind::Groove;
        }
    }
}

/// Names the sections from where the bass is in (see [`BeatEnergy::bass_state`]): a stretch with the bass in that
/// began where it came in is a Drop, one with the bass out is an Intro or Outro at the ends, a Build when its loudness
/// climbs into a Drop (up to 32 bars), else a Break.
fn label(be: &BeatEnergy, bass: &BassState, edges: &[usize], length_secs: f64) -> Vec<Section> {
    let overall: Vec<f32> = be.bands.iter().map(|b| b[3]).collect();
    let mut sorted = overall.clone();
    sorted.sort_by(f32::total_cmp);
    let e_ref = sorted[((sorted.len() as f32 * 0.75) as usize).min(sorted.len() - 1)].max(1e-4);
    let mean = |a: usize, b: usize| overall[a..b].iter().sum::<f32>() / (b - a).max(1) as f32;
    let spans: Vec<(usize, usize)> = edges.windows(2).map(|w| (w[0], w[1])).collect();
    let bass_in = |a: usize, b: usize| bass.high[a..b].iter().filter(|&&h| h).count() * 2 >= (b - a).max(1);
    // a bass-in stretch is a Drop when the run of bass it belongs to began with the bass coming in
    let came_in = |a: usize| {
        let start = (0..=a).rev().find(|&k| k == 0 || !bass.high[k - 1]).unwrap_or(0);
        start > 0
    };
    let last = spans.len() - 1;
    let sections: Vec<Section> = spans
        .iter()
        .enumerate()
        .map(|(i, &(a, b))| {
            let quarter = ((b - a) / 4).max(1);
            let rising = mean(b - quarter, b) >= 1.25 * mean(a, a + quarter).max(1e-4);
            let next_drop =
                spans.get(i + 1).is_some_and(|&(x, y)| bass_in(x, y) && bass.flips.iter().any(|f| f.0 == x && f.1));
            let kind = if bass_in(a, b) {
                if came_in(a) {
                    Kind::Drop
                } else {
                    Kind::Groove
                }
            } else if i == 0 && spans.len() > 1 {
                Kind::Intro
            } else if i == last && spans.len() > 1 {
                Kind::Outro
            } else if rising && next_drop && b - a <= 128 {
                Kind::Build
            } else {
                Kind::Break
            };
            let start = be.secs[a].max(0.0);
            let end = if b < be.len() { be.secs[b] } else { length_secs };
            Section {
                start_secs: start,
                end_secs: end,
                beats: (b - a) as u32,
                kind,
                energy: (mean(a, b) / e_ref).min(1.5),
            }
        })
        .collect();
    // A change inside one stretch (the mids shift halfway through a drop) is a variation, not a new section: runs of
    // the same kind are one, unless the second starts where the bass comes in (a drop after a short break is its own).
    let mut merged: Vec<Section> = Vec::with_capacity(sections.len());
    for (s, &(start, _)) in sections.into_iter().zip(&spans) {
        let bass_came_in = bass.flips.iter().any(|f| f.0 == start && f.1);
        match merged.last_mut() {
            Some(m) if m.kind == s.kind && !bass_came_in => {
                let (wm, ws) = (m.beats as f32, s.beats as f32);
                m.energy = (m.energy * wm + s.energy * ws) / (wm + ws);
                m.beats += s.beats;
                m.end_secs = s.end_secs;
            }
            _ => merged.push(s),
        }
    }
    // a track that starts at full tilt starts on a drop (Push Through: "it's all go from the start")
    if let Some(first) = merged.first_mut() {
        if first.kind == Kind::Groove && first.beats >= OPENING_DROP_BEATS && first.energy >= OPENING_DROP_ENERGY {
            first.kind = Kind::Drop;
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A track at 120 BPM (0.5 s a beat) with sections of known loudness: `parts` is (bars, bass, mid, high).
    fn track(parts: &[(usize, f32, f32, f32)]) -> BeatEnergy {
        let mut be = BeatEnergy::default();
        let mut t = 0.0;
        for &(bars, lo, mi, hi) in parts {
            for _ in 0..bars * 4 {
                be.secs.push(t);
                be.bands.push([lo, mi, hi, (lo * lo + mi * mi + hi * hi).sqrt()]);
                t += 0.5;
            }
        }
        be
    }

    const INTRO: (usize, f32, f32, f32) = (8, 0.0, 0.05, 0.05);
    const BUILD: (usize, f32, f32, f32) = (8, 0.05, 0.2, 0.3);
    const DROP: (usize, f32, f32, f32) = (16, 0.5, 0.3, 0.3);
    const BREAK: (usize, f32, f32, f32) = (8, 0.0, 0.15, 0.1);
    const OUTRO: (usize, f32, f32, f32) = (8, 0.0, 0.05, 0.05);

    #[test]
    fn sections_found_on_bar_lines_and_labelled() {
        let be = track(&[INTRO, BUILD, DROP, BREAK, DROP, OUTRO]);
        let s = Structure::from_beats(&be, 0, be.secs.len() as f64 * 0.5);
        let starts: Vec<f64> = s.sections.iter().map(|x| x.start_secs).collect();
        // 8, 16, 32, 40, 56 bars = 16, 32, 64, 80, 112 seconds at two seconds a bar
        for want in [16.0, 32.0, 64.0, 80.0, 112.0] {
            assert!(starts.iter().any(|&t| (t - want).abs() <= 2.0), "no boundary near {want}: {starts:?}");
        }
        let kinds: Vec<Kind> = s.sections.iter().map(|x| x.kind).collect();
        assert_eq!(kinds.first(), Some(&Kind::Intro));
        assert_eq!(kinds.last(), Some(&Kind::Outro));
        assert!(kinds.contains(&Kind::Drop) && kinds.contains(&Kind::Break), "{kinds:?}");
        assert!(s.sections.iter().all(|x| x.beats % 4 == 0), "off the bar lines: {:?}", s.sections);
        assert_eq!(s.phrase.map(|p| p.0), Some(32), "8-bar phrases expected: {:?}", s.phrase);
    }

    #[test]
    fn contiguous_and_covering() {
        let be = track(&[INTRO, DROP, BREAK, DROP]);
        let s = Structure::from_beats(&be, 0, be.secs.len() as f64 * 0.5);
        for w in s.sections.windows(2) {
            assert!((w[0].end_secs - w[1].start_secs).abs() < 1e-9);
        }
        assert_eq!(s.sections.iter().map(|x| x.beats).sum::<u32>() as usize, be.len());
    }

    #[test]
    fn steady_track_is_one_section() {
        let be = track(&[(48, 0.4, 0.3, 0.3)]);
        let s = Structure::from_beats(&be, 0, 96.0 * 2.0);
        assert_eq!(s.sections.len(), 1);
    }

    #[test]
    fn a_change_without_the_bass_starts_a_section_on_a_bar_line() {
        // 34 beats of one sound, then the mids and highs jump for 62: the bass never changes, so no start is fixed
        let mut be = track(&[(24, 0.4, 0.1, 0.1)]);
        for b in &mut be.bands[34..] {
            *b = [0.4, 0.5, 0.5, (0.4f32 * 0.4 + 0.5 * 0.5 * 2.0).sqrt()];
        }
        // bar lines on beats 2, 6, 10, ...: the start is on one of them, and the nearest to the change
        assert_eq!(segment(&be, 2, &[]), vec![34]);
        assert_eq!(segment(&be, 0, &[]), vec![32]);
    }

    #[test]
    fn a_drop_starts_on_the_beat_the_bass_comes_in_whatever_the_bar_line_says() {
        // 8 bars with no bass, then the bass from beat 33 (not a multiple of four) on
        let mut be = track(&[(8, 0.0, 0.1, 0.1)]);
        let rest = track(&[(24, 0.5, 0.2, 0.2)]);
        let offset = be.secs.len();
        be.secs.extend(rest.secs.iter().map(|t| t + offset as f64 * 0.5));
        be.bands.extend(rest.bands);
        let be = BeatEnergy { first: 0, ..be };
        let state = be.bass_state();
        assert_eq!(state.flips, vec![(32, true)]);
        let s = Structure::from_beats(&be, 0, 128.0);
        assert!(
            s.sections.iter().any(|x| x.kind == Kind::Drop && (x.start_secs - 16.0).abs() < 1e-9),
            "{:?}",
            s.sections
        );
    }

    #[test]
    fn a_drop_after_a_short_break_is_on_the_bar_line_the_bass_came_back_at() {
        // bass for 8 bars, out for 3 beats, back for 16 bars: the bass is back on beat 35, a beat before the bar line,
        // and a drop starts a bar, so it is beat 36
        let mut parts = vec![(8, 0.5, 0.2, 0.2)];
        let mut be = track(&parts);
        parts.clear();
        let gap = track(&[(1, 0.0, 0.2, 0.2)]);
        let back = track(&[(16, 0.5, 0.2, 0.2)]);
        let shift = |b: &BeatEnergy, by: usize| b.secs.iter().map(|t| t + by as f64 * 0.5).collect::<Vec<_>>();
        let n0 = be.len();
        be.secs.extend(shift(&gap, n0));
        be.bands.extend(gap.bands[..3].to_vec());
        be.secs.truncate(n0 + 3);
        let n1 = be.len();
        be.secs.extend(shift(&back, n1));
        be.bands.extend(back.bands);
        let s = Structure::from_beats(&be, 0, 200.0);
        let drop =
            s.sections.iter().find(|x| x.kind == Kind::Drop && x.start_secs > 10.0).expect("a drop after the break");
        assert_eq!(n1, 35);
        assert!((drop.start_secs - 18.0).abs() < 1e-9, "{:?}", s.sections);
    }

    #[test]
    fn a_hit_and_weaker_beats_bass_line_still_comes_in_on_its_first_hit() {
        // drum and bass: the bass is a hit then three weaker beats (0.45, 0.25, 0.25, 0.45), from beat 32; before it
        // there is almost none, and a weaker bass beat the beat before (0.17)
        let mut be = BeatEnergy::default();
        for i in 0..96usize {
            let low = if i < 31 {
                0.04
            } else if i == 31 {
                0.17
            } else {
                [0.45, 0.25, 0.25, 0.45][(i - 32) % 4]
            };
            be.secs.push(i as f64 * 0.35);
            be.bands.push([low, 0.2, 0.1, low + 0.2]);
        }
        let state = be.bass_state();
        assert_eq!(state.flips, vec![(32, true)], "{:?}", state.flips);
    }

    #[test]
    fn one_run_of_bass_is_one_drop_even_where_the_sound_changes_inside_it() {
        // 8 bars no bass, then 24 bars of bass in which the mids change at bar 16: one Drop section, not two
        let mut parts = vec![(8, 0.0, 0.1, 0.1), (8, 0.5, 0.2, 0.2), (16, 0.5, 0.5, 0.45)];
        let be = track(&parts);
        parts.clear();
        let s = Structure::from_beats(&be, 0, 200.0);
        let drops = s.sections.iter().filter(|x| x.kind == Kind::Drop).count();
        assert_eq!(drops, 1, "{:?}", s.sections);
    }

    #[test]
    fn of_several_drops_in_a_row_the_first_is_the_drop() {
        use Kind::*;
        let kinds = |v: &[(Kind, u32, f32)]| {
            let mut s: Vec<Section> = v
                .iter()
                .enumerate()
                .map(|(i, &(kind, beats, energy))| Section {
                    start_secs: i as f64 * 10.0,
                    end_secs: i as f64 * 10.0 + 10.0,
                    beats,
                    kind,
                    energy,
                })
                .collect();
            one_drop_per_run(&mut s);
            s.iter().map(|x| x.kind).collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(&[
                (Intro, 32, 0.3),
                (Drop, 64, 0.9),
                (Drop, 64, 0.95),
                (Drop, 32, 0.9),
                (Break, 32, 0.3),
                (Drop, 32, 0.9)
            ]),
            vec![Intro, Drop, Groove, Groove, Break, Drop]
        );
        // a short drop running into a clearly louder one is a lead-in; the louder one is the drop
        assert_eq!(
            kinds(&[(Intro, 32, 0.3), (Drop, 32, 0.8), (Drop, 136, 0.97), (Drop, 53, 1.0), (Outro, 8, 0.1)]),
            vec![Intro, Build, Drop, Groove, Outro]
        );
        // two short ones, the louder drop after both
        assert_eq!(
            kinds(&[(Drop, 32, 0.81), (Drop, 32, 0.79), (Drop, 136, 0.97), (Drop, 53, 1.0)]),
            vec![Build, Build, Drop, Groove]
        );
        // but not a long one, or one that is hardly louder
        assert_eq!(kinds(&[(Drop, 96, 0.83), (Drop, 34, 0.99)]), vec![Drop, Groove]);
        assert_eq!(kinds(&[(Drop, 32, 0.9), (Drop, 64, 0.93)]), vec![Drop, Groove]);
        // a lone short burst the track backs out of is a tease: a build when a drop follows, else just a groove
        assert_eq!(kinds(&[(Intro, 32, 0.3), (Drop, 12, 0.9), (Break, 32, 0.3)]), vec![Intro, Groove, Break]);
        assert_eq!(
            kinds(&[(Intro, 32, 0.3), (Drop, 16, 0.8), (Break, 16, 0.6), (Drop, 128, 0.92)]),
            vec![Intro, Build, Break, Drop]
        );
        // a short one at the very end has nothing to back out of
        assert_eq!(kinds(&[(Intro, 32, 0.3), (Drop, 16, 0.9)]), vec![Intro, Drop]);
        // a drop far under the loudest is not one, and the next of its run that is loud enough is
        assert_eq!(
            kinds(&[(Drop, 136, 0.97), (Break, 35, 0.3), (Drop, 40, 0.64), (Drop, 40, 0.82), (Drop, 113, 1.0)]),
            vec![Drop, Break, Build, Drop, Groove]
        );
        assert_eq!(kinds(&[(Drop, 136, 0.97), (Break, 35, 0.3), (Drop, 80, 0.6)]), vec![Drop, Break, Groove]);
    }

    #[test]
    fn a_pick_up_note_before_the_bar_line_does_not_move_the_drop_off_it() {
        // the bass comes in on the last beat of bar 8 (Skepta - Can't Play Myself): the drop is bar 9
        let mut be = track(&[(8, 0.0, 0.1, 0.1), (16, 0.5, 0.3, 0.3)]);
        be.bands[31] = be.bands[32];
        let s = Structure::from_beats(&be, 0, 96.0);
        let drop = s.sections.iter().find(|x| x.kind == Kind::Drop).expect("a drop");
        assert!((drop.start_secs - 16.0).abs() < 1e-9, "{:?}", s.sections);
    }

    #[test]
    fn two_bars_of_quieter_bass_before_the_drop_are_a_lead_in() {
        // Danny Byrd - Superstylin': the bass is in two bars early at two thirds of its level, then the drop
        let be = track(&[(8, 0.0, 0.2, 0.1), (2, 0.34, 0.2, 0.1), (16, 0.5, 0.3, 0.3)]);
        let s = Structure::from_beats(&be, 0, 104.0);
        let drop = s.sections.iter().find(|x| x.kind == Kind::Drop).expect("a drop");
        assert!((drop.start_secs - 20.0).abs() < 1e-9, "{:?}", s.sections);
    }

    #[test]
    fn one_louder_beat_a_bar_into_the_drop_does_not_move_it() {
        // Tangaroa: the first beat of the drop is the quietest of its bar, and the single-beat test this replaced
        // put the drop a bar late
        let mut be = track(&[(8, 0.0, 0.1, 0.1), (16, 0.5, 0.3, 0.3)]);
        be.bands[32] = [0.38, 0.25, 0.2, 0.5];
        let s = Structure::from_beats(&be, 0, 96.0);
        let drop = s.sections.iter().find(|x| x.kind == Kind::Drop).expect("a drop");
        assert!((drop.start_secs - 16.0).abs() < 1e-9, "{:?}", s.sections);
    }

    #[test]
    fn a_track_that_opens_at_full_tilt_opens_on_a_drop() {
        let be = track(&[DROP, BREAK, DROP, OUTRO]);
        let s = Structure::from_beats(&be, 0, be.secs.len() as f64 * 0.5);
        assert_eq!(s.sections[0].kind, Kind::Drop, "{:?}", s.sections);
        // eight bars of it is an intro groove, not a drop
        let be = track(&[(8, 0.5, 0.3, 0.3), BREAK, DROP, OUTRO]);
        let s = Structure::from_beats(&be, 0, be.secs.len() as f64 * 0.5);
        assert_eq!(s.sections[0].kind, Kind::Groove, "{:?}", s.sections);
    }

    #[test]
    fn a_drop_that_swells_for_a_bar_starts_on_the_hit() {
        // quiet, then a bar of bass at a swell level, then the hit a bar later and the full bass: the drop is the hit
        let be = track(&[(8, 0.0, 0.05, 0.05), (1, 0.4, 0.15, 0.1), (16, 0.55, 0.3, 0.3)]);
        let s = Structure::from_beats(&be, 0, 128.0);
        let drop = s.sections.iter().find(|x| x.kind == Kind::Drop).expect("a drop");
        assert!((drop.start_secs - 18.0).abs() < 1e-9, "{:?}", s.sections);
    }

    #[test]
    fn a_drop_that_is_no_louder_a_bar_on_stays_where_the_bass_came_in() {
        let be = track(&[(8, 0.0, 0.05, 0.05), (1, 0.5, 0.3, 0.3), (16, 0.5, 0.3, 0.3)]);
        let s = Structure::from_beats(&be, 0, 128.0);
        let drop = s.sections.iter().find(|x| x.kind == Kind::Drop).expect("a drop");
        assert!((drop.start_secs - 16.0).abs() < 1e-9, "{:?}", s.sections);
    }

    #[test]
    fn a_loud_build_is_not_a_drop_and_the_drop_is_where_the_bass_arrives() {
        // 4 bars of quiet, 4 of a rising build with a little bass, then the full bass: the Drop is the last
        let be = track(&[(8, 0.0, 0.05, 0.05), (8, 0.1, 0.3, 0.3), (16, 0.5, 0.3, 0.3)]);
        let s = Structure::from_beats(&be, 0, 128.0);
        let drop = s.sections.iter().find(|x| x.kind == Kind::Drop).expect("a drop");
        assert!((drop.start_secs - 32.0).abs() < 1e-9, "{:?}", s.sections);
    }

    fn named(kinds: &[(f64, Kind)], end: f64) -> Structure {
        let sections = kinds
            .iter()
            .enumerate()
            .map(|(i, &(start, kind))| {
                let e = kinds.get(i + 1).map_or(end, |n| n.0);
                Section { start_secs: start, end_secs: e, beats: ((e - start) / 0.5) as u32, kind, energy: 1.0 }
            })
            .collect();
        Structure { sections, phrase: None }
    }

    #[test]
    fn a_drop_named_a_bar_from_one_found_moves_it_there() {
        let s = named(&[(0.0, Kind::Intro), (16.0, Kind::Drop), (48.0, Kind::Break), (64.0, Kind::Drop)], 96.0);
        let got = s.with_drops(&[18.0, 64.0], 0.5);
        assert_eq!(got.drops(), vec![18.0, 64.0]);
        assert_eq!(got.sections.len(), 4);
        assert_eq!(got.sections[0].end_secs, 18.0);
        assert_eq!((got.sections[0].beats, got.sections[1].beats), (36, 60));
    }

    #[test]
    fn a_drop_named_far_from_any_start_cuts_the_section_it_is_in() {
        let s = named(&[(0.0, Kind::Intro), (16.0, Kind::Drop), (80.0, Kind::Outro)], 96.0);
        let got = s.with_drops(&[48.0], 0.5);
        let kinds: Vec<Kind> = got.sections.iter().map(|x| x.kind).collect();
        assert_eq!(kinds, vec![Kind::Intro, Kind::Groove, Kind::Drop, Kind::Outro], "the drop found is a drop no more");
        assert_eq!(got.sections[2].start_secs, 48.0);
        assert!(got.sections.windows(2).all(|w| w[0].end_secs == w[1].start_secs));
    }

    #[test]
    fn no_drops_named_leaves_none_and_two_close_together_both_stand() {
        let s = named(&[(0.0, Kind::Intro), (16.0, Kind::Drop), (80.0, Kind::Outro)], 96.0);
        assert!(s.with_drops(&[], 0.5).drops().is_empty());
        assert_eq!(s.with_drops(&[16.0, 18.0], 0.5).drops(), vec![16.0, 18.0]);
        assert_eq!(s.with_drops(&[0.0, 500.0], 0.5).drops(), vec![0.0], "one at the very start, one past the end");
    }

    #[test]
    fn short_or_empty_does_not_panic() {
        assert_eq!(Structure::analyse(&WaveSummary::default(), 0.5, 0.0, 0), Structure::default());
        let be = track(&[(2, 0.3, 0.3, 0.3)]);
        let s = Structure::from_beats(&be, 0, 4.0);
        assert!(s.sections.len() <= 1);
    }

    #[test]
    fn beat_energy_reads_the_summary() {
        // two seconds of a 100 Hz tone, then silence: the bass of the first beats is up, the later ones down
        let sr = 44100u32;
        let mut x = vec![0.0f32; sr as usize * 6];
        for i in 0..sr as usize * 3 {
            x[i] = 0.5 * (i as f32 * 2.0 * std::f32::consts::PI * 100.0 / sr as f32).sin();
        }
        let w = WaveSummary::build(&x, 1, sr);
        let be = BeatEnergy::of(&w, 0.5, 0.0).unwrap();
        assert_eq!(be.len(), 12);
        assert!(be.bands[1][0] > 0.2 && be.bands[1][2] < 0.05);
        assert!(be.bands[9][0] < 0.01);
        assert!(BeatEnergy::of(&w, 0.5, -0.25).unwrap().first == 1, "first beat after the start");
    }
}
