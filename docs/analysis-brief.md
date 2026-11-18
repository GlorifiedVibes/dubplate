# Brief: beat position, bar 1 and drop detection for a DJ application

This document is written to be handed to someone (or some model) who has not seen the code. It says what the feature is
for, what already exists, what is known to work and fail, what evidence there is, and what we want back. Everything
here is about **Dubplate**, a Rust desktop DJ application; the analysis code is classical signal processing, no
machine-learning model of our own (one optional separation model is used as an input, see section 4).

## 1. What we want

For each music file, a fast offline analysis that produces:

1. **A beat grid**: tempo (BPM, not rounded) and the time of a beat, constant over the whole track. A grid beat must
   sit where a DJ hears the beat *start*, to within about 5 ms.
2. **Bar 1**: which grid beat is beat 1 of a 4/4 bar (the "downbeat"). Almost all of the target music is 4/4.
3. **Sections and drops**: where the track changes character, and in particular where each **drop** is (the point
   where the bass and full groove arrive after an intro, build or break). A drop should be reported as a grid beat
   that is also a bar line, and ideally a phrase line (multiples of 4, 8, 16 or 32 bars from bar 1).
4. **A confidence** for the bar-1 decision, so the application can ask for more evidence (stems) or tell the user
   to check.

Music: mostly electronic (house, trance, drum and bass, dubstep) plus pop, rock and older tracks. The tool is for DJs,
so errors that matter are the ones you can hear when two tracks are mixed: a grid late by 20 ms, bar 1 one beat out,
a drop flagged a bar early so a loop or cue lands wrong.

Constraints: runs on a laptop CPU while the user works; one track in a few seconds is fine; no network; models and
audio must not be bundled in the repository. A solution that needs a GPU is acceptable only as an optional extra
(the stem separation below already is).

## 2. What exists today

All in `crates/engine/src/` (Rust). Numbers below are the real constants.

**Tempo and grid (`bpm.rs`).** Spectral flux onsets at 5.8 ms hops (FFT 512, hop 256 at 44.1 kHz) -> folded
autocorrelation candidates scored against a tempo prior (centre 120 BPM, sigma 1.2 octaves, range 68-185) -> robust
(Theil-Sen) line fit through the strongest onset near each predicted beat, narrowing the window 50/30/20/14 ms ->
phase = best fit of the onsets, then moved onto the **kick's click**: the averaged high-passed |sample difference|
around up to 400 beats, walked back from its peak to where the rise begins (35% of the way up from the quiet before it, rise at most 20 ms,
at most 60 ms late). One tempo and one anchor per track; no tempo changes.

**Bar 1 (`downbeat.rs`).** A weighted vote of five or six "signs", each a score per beat position mod 4 (z-scored,
capped at 6). Weights: kick 1.0, bass 1.0, drops 2.5, section boundaries 0.7, track start 1.0, and a drums-stem vote
1.0 that is used only when the other five are unsure (margin < 1.0). Meaning of each:
- *kick*: which of the four beat positions has the strongest low-band hit
- *bass*: where the bass line's weight falls
- *drops*: which position the bass-in events (below) fall on; a drop starts a bar, so this is the strongest sign
- *boundaries*: which position the novelty boundaries (below) cluster on
- *start*: where the first loud material begins
- *drums*: with a separated drums stem, the same kick test on clean drums

The vote's margin (winner minus runner-up) is the confidence. Below about 1 the signs disagree.

**Sections (`structure.rs`).** Per beat, take the energy of four bands (low, mid, high, overall) over the beat. A
multi-scale novelty curve (windows of 8, 16, 32 beats) finds boundaries (local maximum over +/-8 beats, at least 0.5
above the lower valley within 16 beats, at least 16 beats apart). Separately a **bass state** says where the bass is
in: the loudness of the bar from each beat on, against the track's loud bass (85th percentile): on above 0.7 of it, off
below 0.5, runs under 8 beats (in) or 3 beats (out) are removed. A bass entry is moved on to the first beat with a hit
at least 0.7 of loud, then (new) to the loudest bar-line beat of the next bar if that is at least 15% louder
(handles drops that swell). Boundaries are snapped to bar lines, optionally to a 16/32-beat phrase lattice when at
least half the boundaries sit on one. Sections are named Intro, Build, Drop, Groove, Break, Outro from the bass state
and rising loudness.

**Stems (optional).** An ONNX htdemucs model separates drums/bass/vocals/other. Cost: roughly 23-40 s per track on a
GPU, far slower on CPU, 200-600 MB per cached track. So the application keeps only the *four-number note* the drums
stem gives the bar-1 vote, and separates tracks in the background when they are added to a playlist ("Reel") and the
bar-1 confidence is low.

## 3. What we tried, and what happened

- **Beat This!** (a published transformer beat/downbeat model, ONNX, own preprocessing). Agrees with us where we are
  sure, is a coin flip where we are unsure, 20 ms frame resolution, about 1 CPU-minute per track. On Children it put
  the beats half a beat from ours and it was right (our grid was on the hi-hat; since fixed). Not adopted, not re-run.
- **Steady-section sampling** (find bars only in the middle third of the track and extend backwards): no gain.
- **Drums-stem vote ungated:** flipped a confident, correct track (margin 10.2 -> 1.6) and moved another. Hence the
  gate at margin < 1.0.
- **Click-onset refinement** improved the grid on kick-driven dense mixes (mean |offset| over 31 tracks 7.8 -> 6.3 ms)
  but one rock track got worse (+15 -> +22 ms); the measure is unreliable on such tracks.
- **Drop that swells:** the bass starts rising a bar before the hit. Fixed by the rule above on the two cases we have.
- **Drop runs:** only the first of several Drop sections in a row is a Drop, unless a short (8 bars or less) first member
  is followed in the run by a clearly louder one. Chosen on the 20-track set from 40 of 50 / 93 flags to 36 of 50 /
  52 flags. Plain energy ("louder wins") was worse (32 of 50) than first-of-run (35).
- **Bar 1 at the first sound**: start-of-music threshold 15% -> 2% of the loud level, and the first bar line may be half a
  beat before it; 15 -> 19 of 20.
- **Kick half a beat off:** the grid moves when the kick (punch 90-260 Hz and body 350 Hz-2 kHz rising together) is
  1.6x stronger between the beats, or 1.25x when each third of the track agrees.
- **Drop placement and choice (November 2026):** a bass entry a beat off the bar line goes to it; a lead-in of up to
  two bars (quieter bass, or a swell) moves the drop to the later bar line, judged on whole bars; a lone burst of five
  bars or less that the track backs out of is a tease; a Drop under 0.75 of the loudest Drop is not one; a track with
  the bass in for its first 16 bars at full energy opens on a drop. 36 -> 43 of 50, flags 52 -> 50 (86% named by ear).
  "Every step up in a run is a drop" found one more and added three flags: not kept.
- **Phrase line from the drops:** 8 bars on the beat holding half the section starts, drops counting three times:
  found on 15 of 20 tracks (was 9); 33 of the 37 by-ear drops on those tracks are on it. Using it to *move* drops was
  not tried further: the line is read from the drops, and the published rule-based work lost 45% of its misses to a
  wrongly chosen period.
- **Click step, two fixes:** background measured over the whole 200 ms (lower quartile), and only beats with a strong
  click averaged. Mean |offset| 5.0 -> 1.0 ms on the 20 tracks, 4.5 -> 2.3 ms on 12 older ones.
- **Other tools read:** Mixxx (QM-DSP, constant-BPM fit, no drop detection, a known 20 ms offset bug); rekordbox phrase
  analysis (crowd-trained, error-prone); a commercial cue-point tool describes the same shape as ours (energy surge,
  bass return, spectral change, onset density, phrase-boundary bonus, "energy must hold afterwards") with no published
  numbers.

## 4. Evidence available (small, and honest about it)

Ground truth is thin: that is the main limit on any algorithm. All times are seconds into the file, by ear.

| Track (type) | Labelled drop(s) | Notes |
|---|---|---|
| Darude - Sandstorm | 29.49, 149.43 | confident |
| Darude - Touch Me Feel Me | 111.38, 221.20 | the drums-stem vote is *wrong* here; the gate keeps it out |
| Bangarang | 26.54 | drop on bar 12 beat 4 on the old grid |
| Luude - Big City Life | 44.16, 113.10 | |
| Robert Miles - Children (OGG) | 42.42 | swell: bass rises from ~38 s. The grid was half a beat early here (on the open hi-hat) until the half-beat check was fixed; heard by ear at 42.4 |
| Robert Miles - Children, Full Length | 126.39 | same half-beat error; heard by ear at 2:06.3 |

A second, larger set was marked by ear before looking at the grid: 20 EDM tracks (house, trance, drum and bass,
dubstep), the first beat of bar 1 and 50 drops (not in the repository; `scripts/drop_tests/score.py`). Current scores
on it: the bar line (which beat of four) is right on 20 of 20 files and bar 1 itself within 0.4 beat on 18 (the other
two are a 50 s atmospheric intro the person does not count, and a track whose first sound is an off-beat hat); 43 of
50 drops have one of our Drop sections within 0.6 of a beat, from 50 flags (86% of our flags are drops the person
named). These thresholds were chosen on this set. People often called a first bass entry
"a build" and counted one drop per run of Drop sections; some tracks (bass that never leaves) they called subjective.

Current scores on the first six files, without stems: bar line on every labelled drop 6/6; a Drop section starts on the
labelled drop 9/9. (Earlier figures were scored against two Children labels that had been snapped to a wrong grid.) Fitting six files is easy; **generalisation is untested**, and the
rest of a 55-track library has no labels. There are no first-beat ground-truth times for the grid either, so grid
accuracy is judged by our own independent "attack start" measure (median +2.0 ms, mean |offset| 6.3 ms over 31
tracks), which is unreliable on dense or non-kick tracks. Users' reading of a waveform by eye lags the true onset by
0.1-0.2 s, so visual labels are not trusted below that.

## 5. Open problems, in order of how much they matter

0. **Which of several drops is the drop.** 7 of the 50 drops named by ear are still not found: later steps up inside
   one long run, a second section preferred to an equally loud first, drops where the bass never leaves. Loudness in
   four bands per beat does not separate these; what the listener hears is probably the drum pattern becoming full.
0b. **Tempo that wanders** (Superstring: the kicks move +-13 ms against one tempo). Needs a grid with several anchors.
1. **Bar 1 when the signs disagree** (Children: confidence 0.6). A better source of evidence than the six signs, or a
   better way of combining them than a hand-weighted vote.
2. **Drops that are not a bass-in event**: swells, risers, filter sweeps, a drop where the bass never leaves, genres
   with no bass (e.g. guitar or vocal-led music with a "chorus drop").
3. **Phrase structure**: a reliable 4/8/16/32-bar lattice anchored to bar 1, used to correct drop and boundary
   placement. We have a lattice finder but it only fires when half the boundaries already agree.
4. **Grid accuracy on non-kick material** and tracks with live (drifting) tempo.
5. **Confidence calibration**: is margin 1.0 the right place to ask for stems?

## 6. What to send back

For each proposal:
- the **idea in a few sentences**, and which of problems 1-5 it addresses;
- the **signal it uses** (which bands, which stem, what window, what statistic) and why it should separate the
  cases above, with numbers where you can;
- a **sketch of the algorithm** concrete enough to implement in Rust on a per-beat array
  `bands[beat] = [low, mid, high, overall]` plus beat times (and optionally the drums/bass stems at 44.1 kHz);
- the **failure cases** you expect, and what labelled data would show whether it works;
- a **cost estimate** (CPU seconds per track, memory).

Please do **not** propose "train a large model on a big dataset" unless it is something we could run offline in
seconds and ship small; say so explicitly if a pretrained published model is the suggestion and name it with its
licence. Say plainly where an idea is a guess. A short, well-argued negative ("this will not help because...") is as
useful as a positive.

## 7. Where to look in the repository

- `docs/beat-analysis.md`: tempo and grid, the experiments in prose.
- `docs/waveform.md`: bar 1, sections, hot-cue roles.
- `crates/engine/src/bpm.rs`, `downbeat.rs`, `structure.rs`, `cue_roles.rs`: the code.
- `crates/engine/examples/`: `barcheck FILE` (signs, sections, per-beat dump), `barscore LABELS` (score against the
  table above), `gridscore` (grid vs attack start), `gridjson` (grid export for comparing with other trackers).
- `scripts/beat_this_eval/`: the Beat This! comparison.
