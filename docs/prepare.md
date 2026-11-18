# Prepare: checking and fixing a beat grid

Prepare is a screen for one track at a time, kept apart from the decks. Open it with a right-click on a track in
the library ("Prepare (beat grid)…") or the **PREP** button on a loaded deck that is not playing. It replaces the
library list until you close it; the decks stay usable.

## Sound

Prepare has its own player. It always plays at normal speed (the pitch fader, jog and controller never reach it).

* Nothing playing: it plays on the main output. If a deck starts, Prepare stops and closes, and you can open it again.
* A deck already playing: it plays in the headphones only, and stays there if the deck stops.
* A deck playing and no headphone output: it will not play. It is never heard in the room during a set, and never
  in a recording.

## The screen

From the top: the track's name, tempo and where bar 1 is, two lights (does the grid fit, does bar 1 fit), and **Undo**,
**Save** and **Close**. Then a bar ruler over the scrolling waveform, the whole track as a strip with its sections, the
transport, and two tabs of tools: **Cues & Loops** and **Grid**. The tab also decides what the mouse may move on the
waveform, so fixing a grid cannot shift a cue by accident.

In a window too short for everything (the default 1280x800 is), the two deck waveforms at the top give their room to
Prepare while it is open, and come back when it closes or the window is made taller.

Close with changes not saved asks: **Save and close**, **Discard** or **Cancel**. Save writes the grid, the main cue,
the eight pads (with their names and colours) and the drops; a stopped deck holding the track takes them at once.

### The waveform

| Mouse | Cues & Loops tab | Grid tab |
|---|---|---|
| Drag | Scrub | Scrub |
| Click | Go there (the nearest beat while Snap is on) | Same |
| Drag a cue flag, a loop edge, a DROP flag or the main cue | Moves it, by beats (a drop: by bars). Alt: exactly where the pointer is | Locked |
| Shift+drag | | Slides the grid under the audio |
| Double-click | A hot cue on the next free pad | That beat becomes bar 1 |
| Right-click | A menu for the mark under the pointer (go to it, move to the playhead, delete), or for that place (hot cue, main cue, loop, drop here) | Bar 1 here, a beat exactly here |
| Wheel | Zoom | Zoom |

The strip under it is the whole track: click or drag to go anywhere, hover a section to read what it is.

### Transport and keys

The buttons step to the start, back and on a bar, back and on a beat, and play. A step goes to the grid line before or
after the playhead, so from the middle of a beat "back" first goes to that beat's start. **Click** is the metronome (a
tick on every beat, higher on bar 1), **Snap** puts cues, loops and clicks on the nearest beat of the grid *as currently
edited*, so fix the grid first.

| Key | Does |
|---|---|
| Space | Play or pause |
| Left, Right | One beat; with Shift one bar; with Ctrl the section before or after |
| Home | To the start |
| 1 to 8 | Pad A to H: set it at the playhead if empty, go to it if set. With Shift: delete it |
| C | Main cue at the playhead |
| I, O, L | Loop in, loop out, loop on or off |
| [ and ] | Loop half or twice as long |
| + and − | Zoom |
| M | Click on or off |
| G | The other tab |
| T | Tap the tempo (while playing) |
| Ctrl+Z, Ctrl+S, Esc | Undo, save, close |

Hover **keys** at the right of the transport for the same list.

## Cues & Loops

Cues are saved in the same place as the decks', so a deck picks them up on load.

- **Main cue**: *hold* **CUE** to play from the cue, let go and it stops and comes back (a tap just goes there).
  **Set at playhead** (or `C`) moves it; so does dragging its marker.
- **Hot cues A-H**: click an empty pad to put it at the playhead (or, with a loop running, to keep that loop as a hot
  loop). **Hold** a set pad to hear it: it plays from its cue (or runs its loop) while held, and stops and returns when
  you let go. A tap parks the playhead there. The hold lasts as long as the mouse button is down, wherever the pointer
  drifts to; if the player is already running it keeps running. A pad shows its letter, name, bar and time.
- **The selected pad** (the last one touched, outlined in white) has a row of its own under the pads: its name, a beat
  or 10 ms earlier or later, **To playhead**, eight colours and **Delete**. Shift+click on a pad also deletes; a
  right-click opens a short menu.
- **Suggest cues** fills the empty pads from the track's sections (see [waveform.md](waveform.md)).
- **Loop**: **IN** then **OUT** at the playhead, or a 1/4 to 32 beat loop from the playhead. **Hold** a size to hear
  the loop (it ends when you let go); a tap sets it and leaves it set. **½** and **2×** change its length from the same
  start, **−beat** and **+beat** move it, its edges drag on the waveform, **Loop on / off** leaves it and starts it
  again. The loop runs on the prepare player only.
- **Drops**: **Drop here** puts a drop on the bar line nearest the playhead (one within four bars is moved there),
  **Remove** takes away the one the playhead is in, **As found** forgets your list. A DROP flag also drags, by bars.
  How your drops replace the ones found is in [waveform.md](waveform.md).
- Every edit shares Undo with the grid.

Hold-to-audition goes through the same player and routing as Play: in the headphones when a deck is playing (set a
headphone output in the audio settings, or it will say it can't play), on the main output otherwise.

## Grid

The click turns on while this tab is open (and goes back to how it was when you leave): a grid that is right is
invisible, one that is wrong is obvious.

- **Beats**: nudge every beat by 10 ms or 1 ms, **Snap to the kicks** (up to 80 ms), **Beat at playhead** (the whole
  grid moves so a beat lands on the playhead), or Shift+drag the waveform. **Back to detected** returns to what analysis
  found.
- **Bar 1**: **Bar 1 at playhead** (the beat nearest the playhead opens a bar; the beats do not move), or a beat
  earlier or later, or double-click the beat.
- **Tempo**: drag the BPM figure or double-click it to type; ±0.01; ½ and 2× (the usual fix for a half-time or
  double-time result); **Round** to a whole BPM. The beat at the playhead stays where it is, so the way to fit a tempo
  by hand is: line up a beat early in the track, go near the end, and change the tempo until the beats there fit too.
- **Tap the tempo**: play the track and tap (button or `T`) on every beat for 20-30 seconds. The tempo comes from a
  straight line through all the taps, so a sloppy tap or a missed beat does not matter; 80 taps give a tempo good to
  about 0.03 BPM, and a whole number is taken when the fit allows it. **Use these taps** sets the grid; with "then snap
  to the kicks" on, the beats are then put exactly on the kicks in the audio. Taps arrive late by a roughly fixed
  amount; the snap removes it, or set "tap delay" to take it off the taps. Tap on the beat, not on "1": bar 1 is
  separate.

## What has and has not been checked

The grid maths, the tap fit, stepping, moving a cue or a loop, the drop edits, the click's timing and the sound routing
have unit tests. Both tabs were rendered by the app itself and looked at, at 1920 and 1280 pixels wide
(`DUBPLATE_SHOT_TRACK=song.ogg DUBPLATE_SHOT_OUT=shot.ppm dubplate` opens Prepare on a track, writes a picture of the
window and quits; `DUBPLATE_SHOT_AT`, `DUBPLATE_SHOT_TAB=grid`, `DUBPLATE_SHOT_DEMO=1` and `DUBPLATE_SHOT_SIZE=1920x1040`
set it up). **Not done:** nothing here has been used with a real mouse or keyboard, so dragging flags, Shift+drag, the
right-click menus, double-click and the keys are untried; nor has the click been listened to on a real output, or
anything been run on Windows or macOS. Memory cues (unlettered) are not editable here.

## Grid check

In the header a light says whether the kicks sit on the grid, and it updates as you edit:

* **grid fits** (green): kicks on nearly every beat, tight scatter, no drift from the start of the track to the end.
* **check grid** (amber): close but loose, early or late by 25 ms or more, drifting a little, or kicks on fewer than 60% of beats.
* **grid is off** (red): kicks are 45 ms or more from the grid, or the grid drifts 20 ms or more across the track.
* **can't tell** (grey): too few clear kicks to judge.

Hover for the numbers. The scatter and the drift are exact; the offset is judged loosely, because a soft kick starts
earlier than its loudest edge and the two ways of measuring it differ by 10 ms or more on some tracks. On the 12 audio
files here, 10 read green, Angel and Chodźmy amber.
