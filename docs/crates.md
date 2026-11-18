# How the code is divided

One workspace, three packages:

| Package | What it is | Depends on |
|---|---|---|
| `dubplate-core` (`crates/core`) | The shared audio engine: decoding (`decode_pcm`, `decode_track`), the deck players and mixer, audio output, microphones, key lock and time stretch, pitch tracking, EQ, recording, VST3 hosting, stem separation (ONNX / libtorch), the waveform summary, the analysis cache, tags and key detection. Nothing in it knows what a beat grid analysis is. | no other Dubplate crate |
| `dubplate-engine` (`crates/engine`) | The DJ side: tempo and beat analysis (`bpm`), bar 1 (`downbeat`), sections (`structure`), hot-cue roles (`cue_roles`), tap tempo, the ANLZ / PDB analysis and database files that CDJ / XDJ players read, and the USB export. `decode_file` here is `dubplate_core::decode_pcm` plus all that analysis. It re-exports everything in `dubplate-core`, so `dubplate_engine::DeckPlayer`, `dubplate_engine::deck::...` and so on still work. | `dubplate-core` |
| `dubplate` (`crates/app`) | The DJ app (egui). | `dubplate-engine` |

Rules that keep it that way:

* `dubplate-core` must never depend on `dubplate-engine`. If something in core needs a DJ type, move the type down, not the
  dependency up (`beat_times_ms` moved to core's `deck` for this reason; `dubplate_engine::bpm::beat_times_ms` re-exports it).
* New audio or playback code goes in core when another program built on the engine could use it; analysis, grids, cues and export go in the engine.
* Cargo features (`onnx-inference`, `torch-inference`, `vst3-host`) are defined in core; engine and app forward them.
* The analysis cache (`cache.rs`, `CACHE_VERSION`) lives in core because the stem and waveform caches share its directory
  and key; bump `CACHE_VERSION` when *analysis output* changes, which is an engine change.

Tests: `cargo test --workspace`. Core's tests run with `-p dubplate-core`, the DJ analysis with `-p dubplate-engine`.
