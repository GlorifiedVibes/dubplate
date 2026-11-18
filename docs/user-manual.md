# Dubplate user manual

How to use Dubplate. For installing it and the optional stem-separation setup, see [install.md](install.md). Dubplate is
early software: [the README](../README.md#what-has-and-has-not-been-checked) says what has and has not been checked, and
this manual says where a feature is experimental.

Your music is never moved, renamed or modified. Everything Dubplate learns about it (tempo, key, waveform, cues,
saved grids) is kept in Dubplate's own folders (see [Where things are kept](#where-things-are-kept)).

## 1. The screen

- **Top bar**: `DUBPLATE`, the **● REC** button, `MIDI <controller>` (click for controller settings), **WAVE** (waveform
  colours and the stem-lane default), **FX** (a VST3 effect on the main output, if the program was built with plugin
  support) and, at the right, `OUTPUT <device>` (click to choose the output and headphone devices).
- **Waveforms** across the top of the working area, one per deck, each with an overview of the whole track under it.
- **Deck A** on the left and **Deck B** on the right, with the **mixer** between them.
- **Library** below, or the **Prepare** screen in its place when you open a track for checking.

## 2. The library

Add your music once; Dubplate watches the folders.

- **Add a folder**: use the folder control in the library sidebar. Dubplate scans it, reads the tags and, if
  **Auto-analyse** is on, analyses every track in the background (tempo, key, waveform). With it off, a track is
  analysed the first time you load it on a deck. **Rescan** looks for new or changed files now. At start-up the
  folders are only walked if the last scan was more than 6 hours ago (on a network share of tens of thousands of
  tracks a walk takes minutes; the top of the library shows "Scanning folders…" and then "Checking files"), but
  analysis of tracks still waiting always carries on where it stopped. **Remove folder from library** forgets a
  folder (the files stay where they are).
- **Sidebar**: your whole collection, **Flux** views, your folders and your **reels**.
- **Columns**: a mini waveform with hot cues marked, title, artist, tempo, key, genre and the Flux score. Click a header
  to sort (click again to reverse). Right-click a header to choose which columns show; **Reset columns** restores the
  default set.
- **Search** is the box above the list. Free words match title, artist, album, genre and file name. Terms with a field
  narrow it:

  | | |
  |---|---|
  | `bpm:120-128` `bpm:124` `bpm:~124` (±6 %) `bpm:>130` `bpm:<90` | tempo |
  | `key:8A` `key:~8A` (8A and its harmonic neighbours 7A, 9A, 8B) | key, in Camelot notation |
  | `artist:daft` `album:` `genre:house` `title:` `folder:techno` | text fields |
  | `year:1999` `year:1995-2005` | year |
  | `added:7d` `added:2w` `added:3m` | added recently |
  | `"exact phrase"` `-word` | a phrase; leave a word out |

  Terms combine (all must match). A term Dubplate cannot understand is flagged and ignored, so typing `bpm:` half-way
  never empties the list.
- **Reels** are named sets of tracks that work together (a track can be on many reels). *New reel…* makes one; right-click
  a track → **Add to Reel**; right-click a reel to rename or delete it, or to **Export to USB…**.
- **Flux** ranks tracks by how well they follow the track playing on a deck: tempo within the deck's range (also double
  or half), key distance on the Camelot wheel (same, neighbour, relative), and the same genre. Unknown ingredients are
  neutral, so a track with no key tag is not punished. Choose a Flux view for deck A or B in the sidebar.
- **Loading a track**: double-click it. It goes to deck A, or to the other deck if A is playing; a deck that is playing is
  never interrupted, and if both are playing you are told the load was blocked. Right-click for **Load to Deck A / B**,
  **Prepare (beat grid)…**, **Add to Reel** and more. **⏏** on a deck unloads it (not while it is playing).

## 3. A deck

Each deck has a platter with the time and the BPM, a tempo fader, the transport, key controls, hot cues, loop controls
and the stem row.

- **The platter.** The ring round its edge fills as the track plays and turns amber for the last 30 seconds. Each hot
  cue is a tick across the ring in the pad's colour, and the main cue a short white tick inside it. **Click the ring**
  to go to that point of the track (twelve o'clock is the start). The red marker turns with the music and pulses on
  each beat, more on the first beat of a bar. Drag the top of the platter to scratch, its edge to nudge.
- **Title, BPM, bar.** The BPM is a whole number: drag it or double-click to type another, and it is saved for that
  track. The dots and `BAR n` show the beat in the bar and the bar count from bar 1.
- **TEMPO fader.** Changes the speed in whole BPM within about ±8 %. Double-click it to return to the track's own tempo.
- **CUE.** Playing: returns to the cue point and pauses. Stopped on the cue: hold to preview (it plays while held).
  Stopped elsewhere: sets the cue point here. **Shift**: go to the start of the track.
- **Play / pause**, then **SYNC**: the deck follows the other deck's tempo and beat. It needs a beat grid on both
  decks, and it never interrupts a deck that is already playing. Click SYNC again to release. While both decks play, a
  small lamp in the button's corner is green when the beats are locked (within 8 ms) and amber while they are being
  pulled together.
- **KEY** (key lock): on, the pitch stays put when you change the tempo; off, the pitch follows the tempo like a record.
  How you left KEY and the mixer's AUTO (auto gain) on each deck is remembered between runs (`decks.toml`).
  Below it: **−** and **+** shift the key a semitone at a time, the key readout (click to reset), and **KEY SYNC**, which
  shifts this deck by the smallest amount that makes its key match the other deck's Camelot number. Key lock works best
  within a few percent of the original tempo.
- **Q (quantize)**: snaps cues and loops you set to the beat grid.
- **Hot cues A–H**: eight pads. Click an empty pad to set a cue at the playhead (or save the running loop there, marked
  ↻); click a set pad to jump to it (and loop, if it holds a loop). **Shift+click** a set pad to delete it.
- **Loops**: **IN** sets the start, **OUT** sets the end and starts the loop (while looping, OUT exits; after exiting it
  re-enters). **½** and **2×** halve and double the loop, or step through the auto-loop sizes (1/4 to 32 beats) when no
  loop is running. **LOOP n** starts an auto loop of n beats at the playhead; click it again to exit.
- **Beat grid menu (GRID).** Halve or double the BPM, set beat 1 at the playhead, or reset to the detected grid. For
  anything more, use [Prepare](#5-prepare-checking-and-fixing-a-track).
- **PREP** opens Prepare for the track on this deck (only when the deck is not playing).

## 4. The mixer

- Per channel: **GAIN** (with **AUTO**, which levels the track to a common loudness: `AUTO -2.9` means it was turned down
  2.9 dB), **HI / MID / LO** (3-band EQ; centre is flat), the **FILTER** knob (centre is off, left is low-pass, right is
  high-pass), an **effect selector** (LP/HP, Echo, Flanger, Reverb, Phaser, Bit Crush, Sweep) whose amount is the same
  FILTER knob, the channel fader, and **CUE**.
- **Headphones.** The per-channel **CUE** buttons put that deck in your headphones before its fader. **MASTER ▶ HP** adds
  the main mix on top. **VOL** is the headphone level. Headphones need a second output: under `OUTPUT` choose either
  channels 3-4 of a 4-channel sound card (such as a controller's built-in audio) or a second device.
- **Crossfader**, with a **SMOOTH / FAST** curve: both decks are at full volume in the centre either way; smooth fades the
  far deck gradually, fast keeps a wide plateau and cuts sharply at the ends (for scratching).
- **FX** (top bar): a VST3 effect on the main output, before the limiter, so it is in recordings. **⚙** opens its window.
  See [vst3.md](vst3.md).
- **● REC** records the main output to a WAV file; **■ STOP** ends it. The Prepare screen is never in a recording.

## 5. Prepare: checking and fixing a track

Prepare is a screen for one track at a time, kept apart from the decks: right-click a track → **Prepare (beat grid)…**,
or **PREP** on a stopped deck. It has its own player, which plays at normal speed. With no deck playing it plays on the
main output; with a deck playing it plays in the headphones only (and says so, or says why it cannot). It is never heard
in the room during a set and never in a recording.

What you can do there is documented in [prepare.md](prepare.md). In short:

- **Hear the grid**: the **click track** ticks on every beat (higher on bar 1). A right grid is invisible; a wrong one is
  obvious. The **grid light** says whether the kicks sit on the grid (*grid fits* / *check grid* / *grid is off* / *can't
  tell*).
- **Fix it**: nudge by 10 ms or 1 ms, **½ tempo / 2× tempo** (the usual fix for a half-time or double-time result),
  **Bar 1 here**, **Beat here**, **Snap to the kicks**, or **tap tempo**: play and tap on every beat for 20-30 seconds.
  Undo is Ctrl+Z, and **Save grid** stores it for the track (**Back to detected** undoes the lot).
- **Cues and loops**: set the main cue and the eight hot cue pads, and loops. **Hold** a cue pad, the **▶ Cue** button or
  a loop-size button to hear it; let go and it stops and returns. **Suggest cues** fills the empty pads from the
  sections: A start, B build, C first drop, D first break, E second drop, F outro (a part the track lacks is skipped; a
  pad that already holds a cue is left alone).
- **Check bar 1**: next to the grid light, `● bar 1 fits` or `● bar 1?` says whether the bass and the changes in the
  track start on this grid's bars; a button moves bar 1 if they do not.

Closing without **Save** discards your edits.

## 6. The waveform and the song's structure

The scrolling waveform is centred on the playhead; the overview shows the whole track. Height is the real envelope of the
audio; colour is how loudness divides between bass, mids and highs (the EQ and filter dim what they cut). Scroll the wheel
over the waveform to zoom; click or drag on it, or on the overview, to move the playhead. **WAVE** in the top bar chooses the view: three ways of colouring one shape (RGB, 3-Band, Blue), **Stacked** (bass, mids and highs as three shapes one on another, each as tall as its band is loud) and **Beats** (only the kick and bass in colour, the rest a faint outline, for lining beats up by eye).

- **Bar and phrase lines** follow the grid: a heavier line every bar, a heavier one again every phrase.
- **Sections** are the coloured blocks on the overview, with a **flag** at the start of each one in the scrolling
  waveform. A flag reads `DROP 49`: the section's kind and **the bar it starts on**. If the section starts part-way
  through a bar, it reads `DROP 49.2` (bar 49, beat 2). Hover a block on the overview for its kind, its start and its
  length.
- The kinds are **Intro**, **Build** (rising into a drop), **Drop** (the bass comes in), **Groove**, **Break** (the bass
  is out) and **Outro**.
- **Stem lanes.** Once a track has stems, the **LANES** button in the deck's stem row shows one row per stem (drums,
  bass, other, vocals) under the waveform and on the overview. A muted stem is dimmed. **WAVE → Stem lanes on by
  default** turns them on for every deck.

The sections are an educated guess from the audio. They are right on many tracks and wrong on others (a quieter pre-drop
can be called a Drop; unusual structures confuse it). Treat them as suggestions and check against what you hear. See
[waveform.md](waveform.md) for how they are found and how well they were checked.

## 7. Stems

Stem separation splits a track into **drums, bass, other and vocals**. It is optional and needs a one-off download of a
model (see [install.md](install.md)); without it the stem controls say so.

- In a deck's **STEMS** row, **Analyze** separates the loaded track (it takes a while on a CPU, much less on a GPU; the
  result is cached, so a track is separated once).
- **Reel stems** (a checkbox in the library bar, on by default). Tracks on a Reel are the ones you mean to play, so when
  the stem model is installed they are separated in the background, one at a time, **when the track's bar line is in
  doubt** (the analysis could not tell which beat starts the bar). The drums stem then settles it, and the next time you
  load the track bar 1 may have moved. Only a few numbers are kept for these tracks, not the stems themselves. It never
  touches a track whose grid you saved in Prepare. Pressing **Analyze** on a deck always goes first: a background track
  is stopped (it starts again afterwards), and if both decks ask, A is done before B.
- Each stem has a button: click it to mute that stem, click again to bring it back. **ALL** restores every stem. Muting
  the drums and bass of one deck while the other plays is the usual use. **LANES** shows a row per stem (section 6).

## 8. Controllers

Plug a controller in **before** starting Dubplate. The top bar shows `MIDI <profile>` when it recognised the device.

- **Known device**: it is mapped automatically.
- **Unknown device**: click `MIDI …` to open **Preferences**. Either choose a profile, **Import mapping…** (a DJUCED
  `.djm`, a Mixxx `.midi.xml` or a rekordbox `.midi.csv` file), or press **Map…** next to the controller and teach
  Dubplate each control by moving it (**Learn**). The choice is remembered per device.
- **KEY** (key lock) and **AUTO** (auto gain) can be put on a button like any other function (`key_lock`,
  `auto_gain` in a profile; under Transport in **Map…**).
- The **MIDI monitor** in Preferences shows every message and what Dubplate does with it (orange: no function bound).

What a controller cannot do yet (LED feedback, more than two decks, extra pad modes) and the profile format are in
[midi/README.md](midi/README.md).

## 9. Exporting to a USB stick for CDJ / XDJ players

**Experimental: not tried on a real player.** Right-click a reel → **Export to USB…**, choose the drive's top folder and
confirm. Dubplate copies the tracks and writes the database, beat grids, waveforms and cues that these players read. Format
the drive FAT32 (or exFAT for newer players). If the drive already holds a library, tick the box to replace it. **Eject**
the drive from your system before unplugging it. Please report how it goes on a real player.

## 10. Audio and settings

- **Output.** Click `OUTPUT …` in the top bar: pick the main output, and the headphones as either channels 3-4 of the same
  card or a second device. **Refresh devices** rescans.
- **Importing beat grids** (library sidebar → **Import rekordbox grids…**): reuse the grids another program already made
  for your tracks, from its analysis folder (`PIONEER/USBANLZ` on a stick, or in the program's own data folder). Dubplate
  matches them by file name, checks each against the audio, and tells you which it used.
- **Sizes and colours** (Preferences → **Appearance**): **Interface size** scales everything (Ctrl + and Ctrl - do the
  same); **Text size** scales all text; **BPM numbers** and **Time numbers** make the deck's big readouts larger on top of
  that. **Large text** and **High contrast** are starting points. The settings are kept in `theme.toml`, which can also
  set every colour: see [Themes](theme.md).
- **Environment variables.** `DUBPLATE_DEVICE=cpu` forces the CPU for stems; `DUBPLATE_NO_CACHE=1` makes stem separation
  ignore its cache; `DUBPLATE_VST3_PATH` adds plugin folders; `DUBPLATE_THEME` names a theme file to use instead of the
  one in the settings folder.

## Where things are kept

| | Linux | Windows | macOS |
|---|---|---|---|
| Settings, MIDI profiles | `~/.config/dubplate` | `%APPDATA%\Dubplate` | `~/Library/Application Support/Dubplate` |
| Library, models, runtime | `~/.local/share/dubplate` | `%LOCALAPPDATA%\Dubplate` | `~/Library/Application Support/Dubplate` |
| Analysis and stem caches | `~/.cache/dubplate` | `%LOCALAPPDATA%\Dubplate\cache` | `~/Library/Caches/Dubplate` |

To uninstall, delete the app and those folders.

## When something is wrong

- **The grid is off, or "bar 1" is on the wrong beat.** Open Prepare and fix it (section 5). Analysis is never perfect; the
  tools to fix it are the point of the screen.
- **A deck won't load a track.** A playing deck is never interrupted; stop it first, or load the other deck.
- **Prepare says "can't play".** A deck is playing and no headphone output works. Set one under `OUTPUT` (the message says
  what the monitor reported).
- **Stem lanes or stems are missing.** The model isn't installed, or the track has not been separated yet (**Analyze**).
- **Everything re-analyses after an update.** An update that changes the analysis re-analyses the library once, in the
  background.
- **Where are the logs?** Run it from a terminal; set `RUST_LOG=dubplate=debug` for more.

Please report problems with the exact track, what you saw and what you expected; for analysis problems, the track and
the bar or time where it goes wrong is the most useful thing there is.
