# Themes

Dubplate's sizes and colours come from a theme file, `theme.toml` in the settings folder (`~/.config/dubplate` on
Linux; see [Where things are kept](user-manual.md#where-things-are-kept)). `DUBPLATE_THEME=/path/to/file.toml` uses
another file instead, which is handy when trying out several.

The app reads the file again within a second of it being saved, so a theme can be worked on with the app open. If the
file has a mistake, the last good look stays in use and Preferences → Appearance says what is wrong.

Any value can be left out; what a file does not set comes from the default look. A file can be as short as this:

```toml
text_scale = 1.2

[readouts]
bpm = 1.8

[colours]
deck_a = "#00e0a0"
```

Preferences → Appearance → **Write theme file** writes every value out, as a full list to start editing from. The
Appearance sliders also save the whole theme. Either way, comments and the order of lines in a hand-written file are
not kept.

## Sizes

All sizes multiply the size the screen was designed at (1.0).

| Key | What it sizes | Range |
|---|---|---|
| `ui_scale` | Everything: text, controls, spacing. Ctrl + and Ctrl - change it as well, and the change is saved. | 0.5–3 |
| `text_scale` | All text, on top of `ui_scale`. | 0.6–2 |
| `readouts.bpm` | The BPM above the tempo and in the middle of the platter, on top of `text_scale`. | 0.5–3 |
| `readouts.time` | The time left on the track and the time on the platter, on top of `text_scale`. | 0.5–2.5 |

A value outside its range is brought back into it. The deck row grows with the BPM, the time and the text, so nothing is
cut off. In a small window, `ui_scale` makes the decks run out of room sooner than `text_scale` does: at 1280×800, the
Large text look still fits (the elapsed / total time gets shortened with "…").

## Colours

Colours are `"#rrggbb"`, or `"#rrggbbaa"` for see-through.

| Key | Used for | Default |
|---|---|---|
| `background` | Behind the panels; dark text on a coloured button | `#0a0a0f` |
| `surface` | Decks, windows | `#12121a` |
| `inset` | Sunk into a surface: platter face, menu bar, fader slots, library sidebar | `#0c0c12` |
| `raised` | Controls at rest: knob bodies, empty pads, buttons | `#161620` |
| `hover`, `pressed` | A control under the pointer, and held down | `#20202e`, `#303041` |
| `divider` | Lines between panels and rows | `#1c1c2a` |
| `outline` | The edge of a knob, fader slot, pad or panel | `#37374b` |
| `track` | The ring a knob's value runs round; the platter's ring | `#262637` |
| `handle`, `handle_active` | A fader handle, and one being moved | `#28283a`, `#3a3a50` |
| `tick` | Scale marks; the line along a fader slot | `#50506e` |
| `handle_line` | The grip line on a fader handle | `#9191a5` |
| `text`, `text_dim`, `text_mid` | Main text, labels and secondary text, second line of a library row | `#d2d2e4`, `#5f5f73`, `#b9b9cd` |
| `text_bright` | The brightest marks: BPM, playhead, knob pointers | `#ffffff` |
| `label`, `button_text` | Text in windows and menus (Preferences, Prepare's tools) | `#8c8c8c`, `#b4b4b4` |
| `deck_a`, `deck_b` | Each deck's colour | `#00c4ff`, `#ff641e` |
| `selected` | The selected library row | `#004a6e` |
| `ok`, `warn`, `bad` | Status: fine, worth a look, a problem | `#46c86e`, `#e6aa3c`, `#eb5050` |
| `record` | The REC button | `#a01e1e` |
| `beat_marker` | The dot on the platter that turns with the music | `#eb3c3c` |
| `ending` | The platter's ring in the last 30 seconds | `#eb961e` |
| `cue`, `play`, `loop_on` | The CUE and PLAY buttons and the main cue marker; loops | `#dc9b00`, `#00c34b`, `#00c85a` |
| `filter_low`, `filter_high` | The filter knob below and above the middle | `#ff8200`, `#0096ff` |
| `stems` | Drums, bass, other, vocals, as a list of four | |
| `intro`, `build`, `drop`, `groove`, `break`, `outro` | The sections drawn over a track | |

Not themed: the waveform's colours (choose them with **WAVE** in the top bar), hot cue colours (they belong to the track
and go to the USB stick with it), the colours of keys in the library, and the FLUX score's scale. The theme sets colours
and sizes; where things go on the screen is fixed.

## For developers

Code that draws takes its colours from `crate::theme::palette` (`palette::text()`, `palette::deck(d)`, …) and its text
sizes through `crate::theme::sz(size)`, or `bpm_size` and `time_size` for the readouts. A size written as a plain
number, or a colour written as `Color32::from_rgb(…)`, will not follow the theme. A new colour role is a field in
`Colours` (with its default), a name in the `roles!` list, and a row in the table above.
