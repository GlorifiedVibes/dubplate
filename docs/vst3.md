# VST3 effects: one on the main output, one on each deck

In the app: the menu bar has **FX ○ / ●** (click to switch the effect on or off; green and lit when it is on) and a
**⚙** that opens the window: *Find plugins* scans the VST3 folders, a list chooses one, **ON / OFF** switches it, and
every parameter has a slider with the plugin's own readout ("-6.0 dB", "Delay") and a filter box. What you chose, its
state and its settings are remembered between runs (`master_fx.toml`).

The effect sits on the main output before the limiter, so it is in recordings. It is not on the headphone cue of a
single deck.

**A ○ / ●** and **B ○ / ●** beside it are the same thing for each deck (`deck_a_fx.toml`, `deck_b_fx.toml`). A
deck's effect sits after its channel fader and before the crossfader, so an echo or reverb rings on when the fader
closes and the crossfader still cuts it. The headphone cue is taken before it, so you do not hear it in the cue.
While a deck's effect is off the mixer does exactly what it did before; while it is on, that deck is rendered into
its own block, the plugin processes it, and it joins the mix through the crossfader. Checked by a test with a real
plugin (Surge XT Effects): on changes the mix, off gives back the identical mix, and with the crossfader hard over
nothing of the deck or its effect is left. Not listened to.

Engine: `crates/core/src/vst3_host.rs` (feature `vst3-host`, on in the app), the slot in `crates/core/src/output.rs` (the mixer sums a
block, the plugin processes it, then the limiter, headphones, recording and prepare player), the window in
`crates/app/src/master_fx.rs`.

Safety nets: a plugin that crashes while loading at start-up leaves a marker file, and next time it is left out (with a
message) instead of stopping Dubplate starting; the same for a plugin that crashes during a scan, which is then skipped.
The audio thread never waits for the screen: if a parameter call holds the plugin at that instant, that block goes out
without the effect.

## What works

* Finds `.vst3` bundles (`~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3`, Windows `Common Files\VST3`, macOS
  `/Library/Audio/Plug-Ins/VST3`, plus any folders in `DUBPLATE_VST3_PATH`) and lists the plugins in each.
* Loads one, sets it up for stereo at a sample rate, runs interleaved audio through it in blocks of up to 1024
  frames, reports its latency, lists its parameters (names, units, steps) and sets them, with the plugin's own
  text for a value ("Delay", "-6.0 dB").
* Gives the plugin a tempo and play state (tempo-synced effects), and unloads it cleanly.
* Builds for Windows (`cargo xwin check`); not tried on macOS.

Tried with Surge XT Effects 1.3.4 (open source, Linux x86_64): loads, 14 parameters, 18 s of a real track in 21 ms
(0.1% of a core), bypass gives output identical to the input, and the FX Type parameter switches between Delay,
Frequency Shifter, Flanger, CHOW and Tape with audibly different output (measured, not listened to).

Try it: `cargo run -p dubplate-engine --release --features vst3-host --example vst3check -- "Surge XT Effects.vst3" song.ogg "FX Type=0.4" "Bypass=0"`

Tests: `cargo test -p dubplate-core --features vst3-host`; set `DUBPLATE_TEST_VST3` to a bundle to include the real
plugin test.

## What does not exist yet

* **One effect per place.** No chains, and no send/return effect channels.
* **No plugin windows.** Editors need a native window handle (X11 on Linux, HWND, NSView). The first app version
  would draw a generic panel from the parameter list.
* **Settings are saved as parameter values**, not as the plugin's own state (`getState` / `setState`), so anything a
  plugin keeps outside its parameters (a loaded impulse response, say) is not remembered.
* **Latency compensation** (the latency is shown in the window, not yet compensated: a look-ahead limiter delays the
  main output relative to the headphone cue), **MIDI and sidechain** (not passed),
  **sample-accurate parameter automation** (a change applies at the start of the next block), and plugins that need
  more than stereo in / stereo out.
* **No crash protection while running.** The plugin runs in this process: a plugin that crashes takes the whole app down, in the middle
  of a set. A guard cannot catch that. The proper design runs each plugin in its own process with audio passed
  through shared memory (one block more latency). Until that exists, a plugin you have not tried is a risk in a live set.

## Notes

* Plugins are third-party and are not shipped with Dubplate: users install them (Tokyo Dawn's Kotelnikov and the
  rest are free to download, not redistributable by us).
* The bindings are the `vst3` crate (MIT / Apache-2.0). The VST3 SDK licence should be checked before shipping.
