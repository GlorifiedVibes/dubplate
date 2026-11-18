# Tempo and beat-grid analysis

Sync, quantise, loops, hot cues and the waveform all trust the beat grid, so this is the part of Dubplate that has to be
right. This page says how the grid is made, how it was checked, and where it is known to fail. Bar 1, sections and hot-cue
roles are in [waveform.md](waveform.md); how to fix a wrong grid by hand is in [prepare.md](prepare.md).

How sure each statement is: **measured** means run on real tracks and checked by plot or by ear; **tested** means a unit
test covers it; **not checked** is said plainly.

## What the analyser does

Code: `crates/engine/src/bpm.rs`. Classical signal processing, no model.

1. **Onsets.** Spectral flux at 5.8 ms hops.
2. **Tempo.** A folded autocorrelation gives candidates, scored against a tempo prior centred on 120 BPM.
3. **Tempo fit.** The coarse scan is good to about 0.5%, which is 20 ms of drift over four minutes at 0.01 BPM, so a
   robust (Theil-Sen) straight line is then fitted through the strongest onset near each predicted beat, narrowing the
   search window four times (50, 30, 20, 14 ms). The result is never rounded: a track at 124.011 keeps 124.011.
4. **Phase.** The grid anchor is the beat phase that fits the onsets best, then moved onto the kick's click (below).
5. **One constant tempo and one anchor for the whole track.** There is no way to represent a tempo change; a track that
   drifts needs its grid set by hand in Prepare.

Times are held as whole milliseconds in the library and in USB exports; the analysis itself works in 256-sample chunks.
The minimum is 8 s of audio.

**Whole-BPM playback.** The tempo control works in whole BPM, and the engine plays at `target / true tempo`, so a 124.011
track sounds at exactly 124 and key lock hides the 0.01% change. The grid keeps the true tempo.

## Candidates and the wrong-ratio problem

The commonest analysis errors are half or double time (70 or 140?) and, less often, a 3:2 miss (a track at 137 found at
91.3 because the rhythm is a dotted eighth). Candidates therefore come from the autocorrelation, from a sweep of the whole
68-185 BPM range (the six strongest folded-onset peaks), and from the double, half, 3:2, 2:3, 4:3 and 3:4 relatives of
every candidate; the scoring then chooses. This fixed the one 3:2 case among the 13 real tracks tried. Whether another
track built differently can still land on a wrong ratio is **not known**; the grid check light in Prepare shows it when the
kicks do not sit on the grid.

When Prepare opens a track it re-analyses and compares: if the stored tempo (or a saved grid) differs from the fresh
analysis by more than 1.5% it says so, names a simple ratio ("a 3:2 difference"), and for a saved grid offers "Use the new
analysis". The library marks tracks analysed by an older version of the analysis for analysis again.

## Genre and the octave

When the file's genre tag names a genre with a usual tempo (`bpm::tempo_hint_for_genre`: dubstep 140, drum and bass 174,
trance 138, techno 130, house 124, hip hop 92 and a few more; pop, rock and "electronic" have none), a tempo outside a range
around it is made less likely, by e^-6 per octave outside. Rules that keep it from doing harm:

* Inside the range nothing changes. A track tagged dubstep that is at 110 stays at 110.
* It only ever chooses between a tempo and its double or half. If it would move the answer by any other ratio, the audio's
  own answer stands (a track tagged Drum & Bass that is at 104 stays at 104, not 138).
* It is a nudge: a track whose audio clearly says 70 and not 140 still comes out at 70.

Checked: unit tests of the weights and tags, and `BEATCHECK_GENRE="Dubstep"` with the `beatcheck` example on files without
a tag. With a made-up dubstep or trance tag an 85 BPM track went to 170, and tracks at 104, 110, 126, 130 and 137 did not
move. **Not checked:** a real mis-octaved track with a real tag, because none is among the test files. A tag is read when a
track is analysed, so changing a tag needs a re-analysis.

## Grid on the kick's click

The first grid placement put beats on the steepest rise of the low-frequency envelope. For a soft kick the sub-bass swells
for 20-30 ms after the click, so that landed late: a person hears the kick start at the click. A final step averages the
high-passed audio around up to 400 beats and moves the grid onto the start of the burst, when that is 6 ms or more away and
never more than 60 ms late (it was 25 ms; a 160 BPM track whose kicks started 30 ms after the grid needed more). On that
track the grid's bias went from +29.9 ms to -1.6 ms and the eight other test tracks did not change at all.

Seen on real files by waveform plot as well as by number: tracks that were 20-33 ms late now start exactly on the grid.
**Known limits:** a track whose click starts about 15 ms before its body (the estimator sits between them), and steady-bass
tracks where no click shows up in the average, which keep the old placement.

**Two fixes to the click step (November 2026).** It failed on two kinds of track, both found with the test set's
drift check below:

* It judged the click against the quiet of the 35 ms furthest before the beat. In a busy sixteenth-note pattern that
  is where the previous hat is, so the click did not look clear and the step did nothing (The Dawn: 31 ms late). The
  quiet is now the lower quartile of the whole 200 ms around the beat.
* It averaged the first 400 beats whatever was on them. A track with minutes of intro and break averaged mostly beats
  with no kick (Somewhere: 18 ms late). It now averages the beats with the strongest clicks, up to 400.

`gridscore` on the 20 drop-test tracks: mean distance 5.0 ms before, 1.0 ms after; on 12 older tracks 4.5 ms before,
2.3 ms after, none worse by more than a millisecond. The Dawn +30.8 to -1.5 ms, Somewhere +17.6 to -1.5, Touch Me Feel
Me +14.8 to -1.3, Children (Full Length) +13.8 to -2.0. **Not independent:** `gridscore` also measures where clicks
start, by a different route, so this says the step now does what it means to; whether that is where a person hears
the beat is for ears. On The Dawn the first sound of the file is an off-beat hat, and the by-ear label for beat 1 is
on it, 0.4 of a beat after the kick grid; the second drop's label, two minutes in, is on the grid exactly.

## Does the tempo hold: `driftcheck`

`cargo run --release -p dubplate-engine --example driftcheck -- FILE` prints, for each 16-bar block, how far the kicks
start from the grid (median and quartiles). A flat line is a grid that holds; a slope is a tempo slightly out; a bend
is a track that changes speed. `DRIFT_PROFILE=1` also prints the average beat in 5 ms steps (clicks, and the level
under 150 Hz), which shows what the grid is sitting on.

Superstring (Rank 1 Remix) bends: +13.5 ms at 0:28, -12.6 ms at 1:23, -8.5 at 1:51, +4.4 at 2:19, +5.9 at 2:46, each
within about 3 ms across its block. That is about 0.05% of tempo wander, as from a recording made off hardware. One
tempo cannot fit it better than about 13 ms either way; the grid check already marks the track **Check**. Fixing it
needs a grid with more than one anchor (rekordbox calls its version Dynamic analysis; Serato lets a grid have several
beat markers), which the decks, the waveform and the export would all have to follow. Not done.

## Importing grids from another program

Library sidebar, **Import rekordbox grids…**: pick the analysis folder of that program (`PIONEER/USBANLZ` on a stick, or the
same folder in its data folder). `crates/engine/src/anlz_read.rs` reads each `ANLZ0000.DAT`; tracks are matched by file name
(and length, when several share a name); each grid is judged against the audio (`crates/app/src/grid_import.rs`) before it
is saved as that track's grid:

* The tempo is a line through the beat positions, not the stored value (which is rounded to 0.01).
* MP3s: some programs keep the encoder delay at the start of an MP3 and other decoders strip it, so the same grid can sit
  a frame or two off the music. The offset between grid and kicks is measured; if it is steady and within 10 ms of 25, 26 or
  51 ms, the grid is moved by that amount. Anything else is left as it was.
* A grid that still does not fit the audio (wrong tempo, another edit of the song) is left out, with the reason. Grids with
  tempo changes are left out too, because a Dubplate grid has one tempo.
* Grids you saved yourself are kept unless you tick the box.

Checked on four tracks that had both a grid file and audio. The audio was not the file the grid was made from, so only part
of this is a fair test: one track agreed with Dubplate's own independent grid to about 2 ms after the 26 ms shift; one had a
matching tempo but a steady 77 ms offset (three MP3 frames, more than a decoder delay) and was left out; two had tempos that
disagreed with the audio and were left out. **Not known:** how MP3 delay is treated in general; the rule above is a
heuristic until exact (grid, source file) pairs are available. `crates/engine/examples/gridimport.rs` runs one pair.

## Tools

`cargo run --release -p dubplate-engine --example beatcheck -- FILE...` prints the tempo fit (beats used, spread) and how
well the kicks sit on the grid; `barcheck` and `barscore` cover bar 1 and sections (see [development.md](development.md)).
`python3 scripts/drop_tests/score.py` scores bar 1 and drops against a folder of tracks marked by ear. When a change alters
the analysis, bump `CACHE_VERSION`, and run `barscore` and `score.py` before and after.

## Beat This! (tried, not adopted)

Beat This! (ISMIR 2024; code and published weights MIT, with a note from its authors that some training audio is under
limited licences) was run over 31 tracks (the labelled drops, other electronic tracks and some rock) and compared with
this analysis. `scripts/beat_this_eval/` repeats the test; the models are not in the repository.

* **Bar 1.** Where our downbeat confidence is 3 or more, the larger model puts its downbeats on our bar lines on 20 of 23
  tracks (the smaller one 21 of 23), and the three it disagrees on are the 4-on-the-floor tracks where it is itself unsure
  (about 50% of its downbeats on one beat). Where our confidence is under 3 it agrees on 4 of 8 (small: 5 of 8), which is
  no better than a coin between two choices. The three copies of Children, our weakest, got three different answers.
* **Drops.** 7 of the 8 labelled drops fell on one of its downbeats. The eighth was Children, where every beat it
  found was half a beat from ours on all three copies. **It was right and we were wrong** (settled by ear and by
  measurement afterwards, see "Half a beat out" below): our grid sat on the open hi-hat. This test should be read
  with that in mind; it has not been re-run since the fix.
* **Timing.** Its beats are on a 20 ms frame, so against ours they are within about 20 ms either way; it cannot refine the
  grid below that and does not replace the click estimator.
* **Tempo.** From beat spacing it gives 136.36 where ours is 137.1 (frame rounding), and sometimes half or double.
* **Cost.** The larger model takes about 10 s of wall time and a CPU-minute per track here (all cores); the smaller one
  6 s. For a 20,000 track library that is days, for an answer that agrees with ours where ours is sure and does not help
  where ours is not.

Not added. It would earn a place if a track set where our bar 1 is wrong more often turned up; the scripts make
that quick to re-check.

## Half a beat out: the grid on the hi-hat

Robert Miles - Children (three copies) had its grid half a beat early: every grid line on the open hi-hat, every kick
half a beat after it. Found by ear, then measured: averaged over the drop, the kick's click, punch (90-250 Hz) and sub
arrive in that order 0.49 of a beat after the grid line, and the grid line has only a bright sound that dies away over
200 ms. The first sound of the file (0.384 s) is where a beat should be. The old check compared the log-compressed
onsets of 86-344 Hz on and between the beats over every beat of the track and wanted 1.6 times: it read 1.02 (the
lead-in bass notes and the off-beat sub counted as much as the kicks).

`fix_half_beat` now also measures the kick itself: at each position, the rise in loudness (not log) of the punch
(about 90-260 Hz) and of the body (350 Hz-2 kHz) together, as their geometric mean, summed over the whole track. A
bass note has no body and a hi-hat has neither, and because loudness is not compressed the parts where the kick plays
decide. The grid moves when that is 1.6 times stronger between the beats; the old low-band test still moves it too,
but only when the kick does not say otherwise. On 31 tracks the new ratio is 2.4 on the three Children copies and
1.03 or less on every other, and only those three grids moved. Children now has bar 1 at the first sound and its
drop at 42.4 s, 24 bars later, which is the time heard by ear before any grid was looked at.

What this undid: the labels 42.20 and 126.17 had been snapped to the wrong grid (now 42.42 and 126.39), so results
quoted against them before this fix were not evidence. Re-scored after it: bar lines on every labelled drop 6 of 6
files and Drop sections on 9 of 9 drops, **without stems**. The swell rule is still needed (7 of 9 without it).

## Twenty tracks heard by ear

Twenty EDM tracks (6 house, 6 trance, 5 drum and bass, 3 dubstep, picked at random from the library's dance folders)
were marked by ear before looking at the grid: the first beat of bar 1 and every drop, 50 drops in all
(`scripts/drop_tests/score.py` scores them; the audio and labels stay out of git). What they showed:

* **Bar 1 is the first sound.** On 19 of 20 the person put bar 1 at the first sound of the file, even under an
  atmospheric intro (the exception chose where "the music" starts, 49.8 s in). The start of the music used to be the
  first hop at 15% of the loud level, so a soft lead-in put bar 1 a whole bar late; it is now 2% (about -34 dB) and
  the first bar line may fall up to half a beat before it. Bar 1 on 19 of 20 (15 before). A bar line just before
  time zero is still bar 1 (the live grid counts beats from the offset, so a negative one is fine; only the ANLZ beat
  list, which starts on the first bar line after zero, does not show it).
* **Which beat is the 1 is right.** 45 of the 50 drops land on beat 1 of our bars; four of the five misses are the
  grids below.
* **Grids that are not on the kick.** Beatbox - Radio Beat had its beats on a bongo-like sound half a beat before every
  kick (reading 1.42 on the kick test, under the 1.6 bar): between-over-on of the kick is now allowed to be as low as
  1.25 when the first, middle and last third of the track each say so (`HALF_BEAT_RATIO_STEADY`). It is the only
  grid of 52 that moved, and its bar-1 confidence went from 5.5 to 16.4, the other signs agreeing much better on
  the new grid. The same three-way reading (`BeatAnalysis::kick_thirds`, printed by `barcheck`) shows a tempo that is
  a little out: Superstring has the kick on the grid in the first third and half a beat off in the last two. It is
  reported, not yet acted on.
* **Drops.** 93 Drop sections against 50 named drops: when several Drop sections follow one another people count one.
  Now only the first of a run is a Drop (the rest become Groove), except a short first member (32 beats or fewer) that
  a clearly louder member (at least 10%) follows in the same run: that is a lead-in (Build) and the louder one is the
  drop (Bangarang's bar 5 part, Calvin Harris - Miracle, Dance Now). 36 of 50 found with 52 flags, 69% of them named
  by the person (before: 40 of 50 found with 93 flags, 43%). The labelled set of six files keeps 9 of 9.
* **Not solved.** The real drop in a run of equal-sounding sections (Shapeshifter - Bloodstream: the first "drop" is
  a build whose energy is the same as the second's), drops where the bass never leaves (Kickers, Superstylin' and others,
  which people call subjective), and drops one or two bars from our flag when the bass leads in (drum and bass
  and dubstep: 8-bar snapping from bar 1 found 8 of 9 of those; but only 28 of 50 drops are a whole number of 8-bar
  phrases from bar 1, so the phrase line has to be found per track).

## The drums stem as a tie-breaker

Where the mix is confusing (a syncopated bass line or piano riff hides the kick) the separated drums stem is not:
`downbeat::drums_sign` reads which beat of the bar its kick favours, from four numbers per track, and
`choose_with_drums` adds it as a sixth voice. It only speaks when the other signs do not agree (margin under 1); on 31
tracks, letting it vote everywhere flipped one verdict that was sure (margin 10 down to 1.6) and moved another away from
the answer Beat This! gave, so it is gated. With the gate no confident verdict changes. The case that
motivated it, Children - Full Length, turned out to be a grid half a beat out (above) and is right without stems
now, so there is at present **no labelled track that needs the drums vote**; it is kept, gated, until more labels
say whether it earns its place.

Stems cost about 40 s a track on the GPU, so this is for tracks that have stems anyway (a deck), or a Reel in the
background (`stem_queue.rs`: one worker, a deck's request first and cancelling a background job between segments; only
the four numbers are kept, in `<hash>.drums` beside the analysis, and the analysis is forgotten so the next load counts
them; tracks with a saved grid or a margin of 1 or more are skipped); `barscore` takes `BARSCORE_STEMS=1` to score it (use `DUBPLATE_NO_CACHE=1` or each run caches 200-600 MB).

## A drop that swells

Some drops arrive over a bar of rising bass (Robert Miles, Children): the bass first gets loud a bar before the hit, so
the Drop flag landed a bar early on both copies we have (times here are on the corrected grid: 40.7 s for a hit at 42.4 s; 124.6 s for 126.4 s). A bass entry
now moves to the loudest bar-line beat of the bar after it when that beat is at least 15% louder than the entry beat,
and never so far that the drop is left shorter than the shortest section. A drop whose next bar is no louder stays
where the bass came in. The move is made only in the section finder: the bar line is still voted from the entries as
they were, so this cannot change where bar 1 is. On the labelled files every drop now starts a Drop section (9 of 9
without stems once the grid was put right; 7 of 9 with this rule switched off).

## Not done

* Tempo changes within a track (a dynamic grid).
* Phrase boundaries from a model; the section labels are rule based, see [waveform.md](waveform.md).
* Writing phrase data to USB exports.
* Storing the grid as a sample-exact anchor plus tempo instead of whole milliseconds.

## Sources

* The ANLZ file format (beat grid, cues, waveforms): <https://djl-analysis.deepsymmetry.org/rekordbox-export-analysis/anlz.html>
