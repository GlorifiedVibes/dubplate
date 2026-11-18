# MIDI controllers in Dubplate

Dubplate maps a controller to its functions with a **profile**: one small TOML file per
controller. This folder documents the format and the tools that convert other programs' maps into it.

| Document | What it is |
|---|---|
| [dubplate-functions.md](dubplate-functions.md) | Every function a profile can bind, and the names other programs use for it. *(generated)* |

## Using a controller

1. Plug it in. Dubplate picks a profile from the MIDI port name. The top bar shows which one
   (`MIDI  <profile>`).
2. Wrong or missing? Click it to open **Preferences**, choose the profile for that port (or *Off*).
   The choice is remembered per device; set it once.
3. Not in the list? **Import mapping…** converts a DJUCED `.djm`, Mixxx `.midi.xml` or rekordbox
   `.midi.csv` file into a profile in `~/.config/dubplate/midi/`.
4. The **MIDI monitor** in Preferences shows every message the controller sends and what Dubplate does
   with it (orange = no function bound), which is the quickest way to debug a profile.

Shipped profiles live in `crates/app/profiles/` and are built into the binary. A file in
`~/.config/dubplate/midi/` with the same name replaces a shipped one.

## Profile format

```toml
[device]
name = "DJControl Inpulse 200 Mk2"
port_match = ["DJControl Inpulse 200 Mk2", "DJControl Inpulse 200"]  # case-insensitive, substring of the port name
priority = 1                  # breaks ties when several profiles fit one port (newer revision wins)
source = "DJUCED .djm — Default Hercules Inpulse 200 MK2"
notes  = ""

[[bind]]
msg   = "91 06"               # status byte + note / CC number, hex
func  = "cue"                 # see dubplate-functions.md
deck  = 0                     # 0 = A, 1 = B; leave out for global functions (crossfader, browse)
arg   = 3                     # only for functions that take one (pad, stem, loop_beats, browse)
hires = true                  # 14-bit fader / knob: the LSB arrives on the CC 0x20 above
label = "CUE_A"               # the control's name on the hardware (documentation only)
```

* **Shift layers** need nothing special. Controllers send shifted buttons on another channel or note,
  so a shifted function is just another `[[bind]]`.
* **Note-off** (`8n`) is treated as note-on with velocity 0, so write `9n` rows only.
* Buttons are recognised by function: `cue`, `loop_in` and `pad` receive press *and* release; the rest
  fire on press.
* Relative encoders (jog wheels, browse) send two's-complement steps: `01` = +1, `7F` = −1.
* A profile that fails validation is skipped with a log line naming the offending `[[bind]]`.

## Importers

`dubplate --import FILE` converts one map; `--import-dir SOURCE DIR OUT` converts a folder.
`--midi-survey`, `--midi-docs` and `--midi-compare` produce reports about a folder of maps you have (which
functions they use, which Dubplate lacks, whether two programs' maps of one controller agree); `dubplate --midi-help`
lists them. Those reports describe other programs' files, so they are **not kept in this repository**: write them
to `inspiration/midi-survey/` (git-ignored), and `scripts/midi_crosscheck.sh` does the same.

| Program | File | Importer | Reads |
|---|---|---|---|
| DJUCED | `.djm` (XML) | `importers/djuced.rs` | Hardware messages, 14-bit controls, DJUCED action → Dubplate function. LED outputs are not read yet. |
| Mixxx | `.midi.xml` (+ JavaScript) | `importers/mixxx.rs` | Plain control rows only. Anything handled by the preset's JavaScript (jog wheels, shift layers, pads, LEDs: about two thirds of all rows in Mixxx's bundled presets) is listed as skipped. |
| rekordbox | `.midi.csv` | `importers/rekordbox.rs` | Messages with per-deck channel offsets, 14-bit controls, hot-cue pads. |

An importer never guesses: a control with no Dubplate equivalent is reported as skipped with the
reason, and an inexact conversion (`stop` → jump-to-start) is reported as an approximation. Every
importer records the program's name for each function, which is what `dubplate-functions.md` and the
survey reports are built from, so they cannot go out of date.

### Adding an importer or a function

* **New program:** implement `import(text) -> (Profile, Report)` in `importers/`, using
  `Builder::add` for each control (it enforces one function per message, validity and hi-res), add the
  source to `Source` in `importers/mod.rs`, and run `--midi-docs`.
* **New Dubplate function:** add it to `Func` / `FUNCS` in `midi_profile.rs`, give `MidiMapper` its
  behaviour, then add the other programs' names for it to each importer's `translate` table.
  Re-running `--midi-docs` moves it from "Not implemented" to "Implemented".

## What a controller cannot do yet

* **LED and display feedback.** Profiles carry input messages only; no output messages are sent to the controller.
* **Start-up (sysex / init) messages** that switch a controller's modes or lights on.
* **More than two decks.** Controllers' deck 3 and 4 layers have nothing to control.
* **One profile per device.** Alternate layouts of one controller are separate profiles, chosen in Preferences.
* **Pads have one job.** Hot cues are bound; other pad modes (sampler, beat jump, loop roll) have no function yet.

To see what a particular map would lose, convert it and read the report: every control with no Dubplate equivalent
is listed with the reason.

## A note on the maps

The conversions of DJUCED maps in `crates/app/profiles/` contain only MIDI message numbers and the name
of each hardware control, which are facts about the hardware. The original files are the work of
their authors (Hercules / Guillemot for DJUCED, the Mixxx contributors, Pioneer DJ for rekordbox) and are
not copied here. Check the licence of any map before redistributing a converted one.
