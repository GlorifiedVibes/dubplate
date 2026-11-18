# The waveform

What the deck and Prepare screens draw, how it is made, and how it was checked.

## What is on screen

- A **scrolling waveform** centred on the playhead and an **overview** of the whole track.
- **Height** is the real envelope of the audio: the highest and lowest sample on either channel in that stretch. The
  solid **core** is the RMS, so a loud, dense passage looks different from one that is only spiky.
- **Colour** is how the loudness divides between bass, mids and highs. The EQ and the filter dim the bands they cut, so
  the picture follows what is heard. Played audio is drawn dimmer.
- The overview is drawn from energy (RMS), not peaks, so breakdowns and drops come out as different heights even on a
  loud master.
- **WAVE** in the menu bar chooses the colours, remembered in `waveform.toml`:
  - **RGB** - red bass, green mids, blue highs (the common three-colour look),
  - **3-Band** - blue bass, amber mids, white highs (the CDJ-3000 look),
  - **Blue** - one colour that gets brighter with the highs (the older single-colour look).

## How it is made

`crates/core/src/waveform_data.rs` makes a **summary** of each track once and keeps it in the cache
(`<hash>.wave`, kept apart from the analysis entry so changing the look does not re-analyse anything; about 1 MB for a
four-minute track, about 80 ms to make for 5 minutes of audio).

- Bands are split with 4th-order Linkwitz-Riley filters at **200 Hz** and **2.5 kHz**.
- Each channel is measured on its own and the larger value kept; averaging left and right lets out-of-phase stereo
  cancel and a loud drop go quiet on screen.
- A **pyramid** of five levels (64, 256, 1024, 4096 and 16384 frames per column; each level joins four of the one
  below), so any zoom reads about one column per pixel and never the audio.
- Values are bytes on a square-root curve, which keeps quiet passages visible.

`crates/app/src/wave_draw.rs` draws it. A frame is **one mesh** with a couple of rectangles per pixel column. Pixel
columns are fixed slots in the track, not in the screen, so scrolling slides the shapes along without changing them
(no shimmer), neighbouring slots are blended 1:2:1 so one loud column does not stand alone, and adjacent rectangles
share their exact edge so no hairlines show.

Cost of two scrolling waveforms and two overviews on a 1900 px wide screen, release build: **0.6 ms** to build
and 0.03 ms to tessellate per frame, where the old drawing took 1.3 ms and 1.9 ms for the two waveforms alone.

## What it was checked against

The band edges and the height scale were set by comparing the summary, column by column, with the waveform data another
DJ program stores for four tracks (after lining up the MP3 offset). Correlation was 0.77-0.92 for the three bands and
0.88-0.94 for the height. What the comparison showed, and what Dubplate does as a result:

- The three bands are low, mid and high, with a low band that ends near 200 Hz and a high band that starts near 2.5 kHz.
  Those are the cutoffs used.
- The old low band (bins 1-5 of a 256-point FFT, 172-860 Hz) **did not follow the bass at all** (correlation about 0,
  against 0.86-0.95 for a 20-250 Hz band): a kick's weight is at 40-100 Hz and was never in it.
- Height is the **linear peak**, not a log or square-root scale.
- The mids measure 2-5 times quieter than they look in that data; the highs match. `BAND_GAIN` in `wave_draw.rs` makes up
  for the music's own tilt so a mix is not all red.

The data of the other program is not used by Dubplate and is not in this repository.

## Looking at it without a window

```
DUBPLATE_WAVE_FILE=song.ogg DUBPLATE_WAVE_OUT=/tmp/wave DUBPLATE_WAVE_AT=70 \
  cargo test --release -p dubplate wave_snapshot -- --ignored
```

writes `/tmp/wave-RGB.ppm`, `-3-Band.ppm` and `-Blue.ppm` (scrolling view, overview and a zoomed-out view with the bass
cut), drawn by a small software rasteriser. `cargo run -p dubplate-engine --release --example wavecheck -- FILE` times the
summary. `cargo test --release -p dubplate wave_draw::tests::frame_cost -- --ignored --nocapture` times the drawing.

## Bar 1 and sections

Two things are worked out from the summary, not the audio (`downbeat.rs`, `structure.rs`).

**Where the bass comes in.** The average bass loudness of the bar from each beat on (so drum and bass, with a hit and
three weaker beats, reads as one level, and a lone crash does not count) is compared with the loud bass of the track:
it is "in" above 70% of it and "out" below 50%, bass shorter than 8 beats is a hit and a gap of under 3 beats a
dropout. That average crosses its level up to three beats before the bass does, so the entry is then moved on to the
first beat with a real hit (70% of the loud bass); that makes it exact to the beat, because a drop is a step. A smoothed
loudness average alone does not do this: it lets a loud build-up look like a drop and puts the edge a few beats out. (Darude - Sandstorm: bars 13-16 are a build with a crash on bar 13, the drop is at bar 17; bars 63-84 a long
break with a crash every four bars, the drop is at bar 85.)

**Which beat is bar 1.** Five signs each give every beat of the bar a z-score, and are added with hand-set weights:
the kick on the beat (1.0), the bass on the beat (1.0), the bass entries (2.5; exits are not counted, they ring out and sit a beat from the entries), the broader section
changes (0.7) and the beat nearest the start of the music (1.0). The gap between the winner and the runner-up is
logged as a confidence. The bass entries and exits are by far the most consistent sign: on 15 tracks they share one
residue almost every time (20 of 20 on Turn Me On). They are what put right Touch Me Feel Me, where the kick alternates
between two beats and decided wrongly. A grid half a beat out (kick stronger between the beats than on them by 1.6
times) is moved by half a beat first.

Compared with the kick-only answer this changed four of 15 tracks: one by a beat and confirmed by ear (Touch Me Feel Me), others by a beat or two beats (half a bar, so the drops land on the new bar 1; not checked by ear). Another program's bar 1 is not ground truth: its grids start with beat 1 in the first second, even where the music starts later. The weights are guesses;
`examples/barcheck.rs` prints every sign for a file so they can be fitted when tracks with bar lines trusted by a
person exist.

Prepare shows `bar 1 fits` or `bar 1?` beside the grid light, from the bass and the sections alone, with a button to
move bar 1 when they disagree clearly.

**Sections.** The cuts are the bass flips, exactly where they are, and between each two of them the bar lines that
best split the stretch into parts of even sound (`segment`, dynamic programming, the method of openDJ's phrase finder
written afresh): a part costs how far its beats' band levels (each band scaled so its loud beats are 1, the bass
weighted 1.5, mids 1, highs 0.7) are from the part's mean, plus 1 for being a part at all, plus 0.4 when it is not a
whole number of 8-bar phrases long counted from where it starts, and parts inside a stretch are at least 4 bars. So a
change of sound without the bass becomes a section start on a bar line, preferably a phrase from the drop before it.
This replaced novelty peaks that were moved onto bar lines and then onto a phrase lattice, three steps of snapping;
on the 20-track set it finds the same drops (43 of 50) with the same number of sections (183), and a phrase line on 18
tracks instead of 15 (41 of 45 by-ear drops on it, from 33 of 37). Section ends move to phrase lines too (an outro
now tends to start on the bar line a bar or so before the bass leaves); nothing marked by ear says whether those are
better. A section with the
bass in, in a run that began where the bass came in, is a **Drop**; with the bass in from the start, a Groove; with the
bass out, an Intro or Outro at the ends, a **Build** when its loudness climbs into a Drop (up to 32 bars), else a
Break. Runs of the same kind are one section (a change of sound inside a drop is a variation, not a second drop),
except that a drop starting where the bass comes in is always its own. They are drawn as blocks along the top of the overview, and as a flag at each boundary of the scrolling
waveform with the name and the bar it starts on (`DROP 49`, or `DROP 49.2` when it starts on beat 2); the phrase lines
follow the phrase line.

**Where a drop lands.** The level test says where the bass got loud, which is often a little off the drop. Three
corrections (`BassState::with_entries_on_the_hit`):

* a bass entry a beat from a bar line goes to the bar line (a pick-up note before the drop; a quiet first beat). Half a
  bar out it stays where it is;
* a **lead-in** of up to two bars (the bass comes in early and quieter, or swells) moves the drop to the later bar
  line: the one after which whole bars are at least 1.2 times louder overall, or 1.12 times in the bass, than the
  stretch between the entry and that line, and only when that stretch's bass is under 0.9 of the track's loud bass.
  This replaced a test on single beats, which put a drop a bar late when its first beat was the quietest of its bar;
* nothing else moves a drop: not the phrase line (below), which is read from the drops and so cannot correct them.

**Tried, not adopted: the kick arriving as the sign of a drop** (from openDJ's phrase finder, 2026-11-17). Per beat, how
much louder the bass band is over the first quarter of the beat than over the rest: a kick has plenty, a bass line or
riser swelling under the beat has none. On Children it does what it promises (next to nothing through the swell, 0.08 to
0.15 from the hit on). On drum and bass and dubstep it reads zero inside the drop itself, because a sustained sub fills
the whole beat in the same band (Danny Byrd - Superstylin': 0.05 in the lead-in, 0 from the drop on). As extra evidence
for a lead-in it changed nothing or lost two drops; as a condition for moving to the hit, or a veto when the kick is
already in, it lost three (43 of 50 to 40 or 41), whatever the thresholds (ratio 1.3 to 3, jump 0.05 to 0.3 of the loud
bars). The bass band of the waveform summary cannot tell a kick from a sub; the drums stem could, where there is one.

**Which Drop sections are drops** (`one_drop_per_run`): the first of a run, unless it is a short lead-in to a clearly
louder one; not a lone burst of five bars or less that the track backs out of (a tease, shown as a Build); not one
under 0.75 of the loudest Drop's energy (the next of its run that is loud enough is the drop). A track that opens with
the bass in for 16 bars or more at 0.9 of full energy opens on a Drop.

**The phrase line** (`phrase_of`) is 8 bars, or failing that 4, on the beat that holds at least half the section
starts, drops counting three times. It is read from the track, not counted from bar 1: a two-bar pick-up shifts every
phrase by two bars (Calvin Harris - Miracle).

**Scores on the 20-track set** (`cargo run --release -p dubplate-engine --example dropscore -- samples/drop-tests
CACHE_DIR`; a second run takes milliseconds, delete the cache folder after changing grid code):

| | before | after |
|---|---|---|
| by-ear drops with a Drop section within 0.6 beat | 36 of 50 | 43 of 50 |
| Drop flags, and the share that are drops named by ear | 52, 69% | 50, 86% |
| tracks with a phrase line; by-ear drops on it | 9 | 15; 33 of 37 (18; 41 of 45 since sections are cut by dynamic programming) |
| bar line right (which beat of four) | 20 of 20 | 20 of 20 |
| the older six files: Drop section on the drop | 9 of 9 | 9 of 9 |

The thresholds were chosen on these same 20 tracks, so the score on new tracks will be lower. The seven still missed:
a drop the bass never left before (Kickers, two), an early bass entry the listener counts as the drop where we call it
a lead-in (One, and the listener said either reading is fine), two later steps up inside one long run (Dance Now), a
second section the listener prefers to the first of the same loudness (Bloodstream), and one two bars before any bass
(Capsule). Bar 1's *number* differs on two tracks while the bar lines agree: Capsule (a 50 s atmospheric intro the
listener does not count) and The Dawn (see docs/beat-analysis.md).

**The bar-1 weights, checked not fitted.** On the 20 tracks each sign alone picks the right beat of the bar: kick 13
(7 wrong), bass 15 (4 wrong), drops 18 (0 wrong, 2 silent), boundaries 10 (1 wrong, 9 silent), start 16 (4 wrong).
With the hand-set weights all 20 are right and the smallest margin is 3.1; with every weight 1, 19 are right; without
the drops sign, 16. Refitting weights to a set they already get entirely right would only fit noise, so they are
unchanged.

**Against the published work and other programs.** What was read, and what it says about this design:

* *Yadati et al., ISMIR 2014* (drop detection, 100 tracks): F1 0.71 with a 15 s window, 0.61 at 3 s, with a trained
  classifier on segment boundaries. Our 86% / 86% is at 0.6 of a beat (about a quarter of a second), on our own set.
* *Zehren et al. 2020* (cue points for DJ mixing, 134 tracks, rules taken from DJs): a cue is a point of high novelty,
  on a downbeat at the start of a period, followed by a section that stands on its own; 86% precision, 49% recall at
  0.5 s, against 65% precision for Mixed In Key on the same set. 45% of their misses were the period chosen wrongly,
  which is why the phrase line here corrects nothing.
* *Argüello et al. 2024 (CUE-DETR)*: 21,000 cue points from DJs' rekordbox libraries fall mostly 16 bars apart, then
  8; a trained detector reaches about 0.4 F1 at one beat against 0.13 for the rule-based method and 0.22 for Mixed In
  Key. So rules do well on drops (a narrower question than "where would a DJ put a cue") and a model does better on
  cues in general; a model of that size is outside what this project ships.
* *felixmeyer6/drop-detector* (GPL, read for ideas only): the features that matter are loudness after against the
  30 s before, bass share after against before, whether the energy holds, and position on a 4/8/16/32-bar count:
  the same ingredients as here, weighed by a trained classifier.
* *rekordbox* phrase analysis names phrases Intro/Up/Down/Chorus/Outro for high-energy tracks and is not documented
  further; *Serato* puts bar 1 on the first transient and lets the user move it, and documents no drop or phrase
  detection. Neither publishes accuracy. We could not run either here, so there is no side-by-side number.
* Not read: Vande Veire & De Bie 2018 (the drum and bass auto-DJ paper) was behind a login.

Checked against the phrase starts another program stored for four tracks (within 1.5 s): 20-62% of the section cuts agree,
against about 12% when the same boundaries are shifted by an arbitrary time. That program cuts a track into about twice as
many phrases, so this says the cuts fall where the track changes; it does not make the labels the same.

**Drops set by hand.** Which bass entry is "the drop" is partly a matter of taste, so Prepare lets you say: **Drop
here** puts a drop on the bar line nearest the playhead (a drop within four bars of it is moved there), **Remove drop**
takes one away and **Drops as found** forgets your list. Once you have set one, your list is the track's drops:
every Drop found becomes a Groove, and each of yours moves the nearest section start onto itself when that is within two
bars, or cuts the section it falls in (`Structure::with_drops`). The list is saved with the grid and cues (table
`drop_edits`, seconds) and is not touched when the track is analysed again; it is put on the beats of the current grid
each time, so it follows a nudged grid. The decks and Suggest cues use the same sections.

**Scoring against what a person heard.** `samples/bar_labels.txt` (git-ignored; format and an example in `scripts/bar_labels.example.txt`) lists the drops checked by ear on four tracks and `cargo run --release -p dubplate-engine --example barscore -- samples/bar_labels.txt` reports
whether a bar line and a Drop section fall on each. Every track checked by ear should be added to one of the two sets, and a change to the weights or thresholds should
not lower either score.

## Hot-cue roles

Prepare, **Suggest cues**: fills the *empty* pads from the sections (`crates/engine/src/cue_roles.rs`): **A** start
(bar 1), **B** build (the last build before the drop), **C** the main drop, **D** the first break after it, **E** the
second drop (the first drop after that break), **F** outro. A part the track does not have is skipped; a pad that holds
a cue is never touched; Undo puts it back. The pad shows its role ("C Drop") and the name is saved as the cue's label.

The main drop skips a Drop section that runs straight into a drop at least 10% more energetic (Bangarang's bar 5 part
leads into the real drop at bar 13); a quieter first drop after a break (Sandstorm's bar 17) still counts. Checked on the labelled tracks by `barcheck` (it prints the
pads): Bangarang C = 26.5 s (labelled 26.54), Big City Life C = 44.2 s and E = 113.1 s (labelled 44.16 and 113.1).
Cues are only as good as the sections; they are suggestions to look at on the waveform, not something to trust blind.

## Stem lanes

Once a track has been split into stems (the **Analyze** button, or a cached split from before), a deck's stem row has a
**LANES** button. On, a strip of four rows appears under that deck's waveform, one per stem (drums red, bass blue, other
green, vocals purple), scrolling and zooming with the waveform and lined up with its playhead; the overview strip
becomes the same four rows for the whole track, under the section blocks. Muting a stem with its D / B / O / V button
dims its lane (it keeps its shape, so you can see where it is). Off by default; **WAVE > Stem lanes on by default**
turns it on for every deck and is remembered. A deck with no stems, or whose summaries are still being made, shows
the ordinary waveform. The panel grows by one strip per deck showing lanes.

The summaries of the four stems (`StemWaves`) are made only for a deck that has its lanes on, one thread, one stem after
the other with a short pause between, and without the bass / mid / high split the track's own summary makes (the lanes
are one colour, and the split was nearly all the cost): about 0.35 s of one thread for a three-minute track, not stored.
A first version built them with the full summary on four threads as soon as any stems loaded, which was seconds of CPU
at every track load and made playback stutter. Lanes are drawn at half the horizontal detail (2 points a column).
Drawing four lanes on each deck and their overviews costs about 2.6 ms a frame on top of 1.4 ms for the waveforms
(`wave_draw::tests::frame_cost`). Each stem is lifted up to 6x (the main waveform 2.5x), because a stem is quieter than
the mix. `cargo test --release -p dubplate lane_snapshot --
--ignored` renders the lanes without a window: from the file's cached stems when it has them (the cache is keyed on the file's
path), else from stand-in stems (the track split into bands). Looked at with real stems on Bangarang: the bass lane is
empty until the drop and pulses after it, the vocal lane shows the phrases, the drums run throughout.

## Not done yet

The USB export still writes the older per-chunk waveform; the CDJ files do not have the better bass yet. Beats-only and
stacked three-band views are planned. Clicking a lane to mute its stem, and using the bass and drums stems to
sharpen drop and bar-1 detection, are not done.
