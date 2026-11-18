# Development

Everything you need to build, test and change Dubplate. For how the code is divided, see [crates.md](crates.md); for the
checks that run on every push, [ci.md](ci.md).

## Requirements

**Rust.** A recent stable toolchain (CI uses 1.97). Install it with [rustup](https://rustup.rs). The workspace is edition
2021 and builds with plain `cargo`.

**A C++ compiler and CMake.** Key lock uses the Signalsmith Stretch library, whose C++ is compiled by Cargo
(`signalsmith-stretch`). `g++` or `clang++` is enough; install `cmake` and `clang` if the build asks for them.

**System libraries**

| System | What to install |
|---|---|
| Debian / Ubuntu | `sudo apt install libasound2-dev libudev-dev libxkbcommon-dev libwayland-dev libx11-dev pkg-config cmake clang` |
| Arch / CachyOS | `sudo pacman -S alsa-lib systemd-libs libxkbcommon wayland libx11 pkgconf cmake clang` |
| macOS | Xcode command line tools (`xcode-select --install`) |
| Windows | Visual Studio Build Tools (the "Desktop development with C++" workload) |

The window is drawn with `wgpu`, so a Vulkan, Metal or DirectX 12 driver is needed to *run* the app (not to build it).

**Optional, only for stem separation** (the app builds and runs without them): the HTDemucs model and ONNX Runtime, fetched
by `scripts/setup_onnx.sh` (or `.ps1` on Windows); see [install.md](install.md) and [stems.md](stems.md).

## Build and run

```sh
cargo build --release -p dubplate          # the DJ app: target/release/dubplate
cargo run   --release -p dubplate          # build and start it
cargo build --workspace                   # everything, including the examples
```

Use `--release` for anything you listen to: decoding, FFT and DSP are far too slow unoptimised. Plain `cargo build` is fine
for checking that it compiles (dependencies are optimised even in dev builds, see the root `Cargo.toml`).

**Features** (defined in `dubplate-core`, forwarded by `dubplate-engine` and `dubplate`):

| Feature | Default | What it does |
|---|---|---|
| `onnx-inference` | yes | Stem separation through ONNX Runtime, opened at run time (no link-time dependency). |
| `vst3-host` | in the app | Hosts a VST3 effect on the main output. |
| `torch-inference` | no | The older libtorch stem engine. Needs `LIBTORCH` set (`scripts/configure.sh` finds it). Cannot be combined with `onnx-inference`. |

**Cross-compiling for Windows from Linux** uses [`cargo-xwin`](https://github.com/rust-cross/cargo-xwin):
`cargo xwin check --target x86_64-pc-windows-msvc -p dubplate`. It proves the Windows build compiles; it does not run it.

## Tests

```sh
cargo test --workspace --locked           # everything that needs no audio hardware, model or music
cargo test -p dubplate-core                # the audio engine only
cargo test -p dubplate-engine              # beat analysis, sections, the USB export formats
cargo test -p dubplate                     # the app (logic, importers, the software-rendered screenshots)
```

The default tests use synthetic signals and generated files, so they run anywhere (CI included). Tests marked `#[ignore]`
need something the machine may not have; run them on purpose with `-- --ignored`:

| Needs | Variable |
|---|---|
| a folder of songs | `DUBPLATE_SAMPLES` (default: `samples/` in the repository, which is git-ignored) |
| one song | `DUBPLATE_TEST_FILE`, `DUBPLATE_TEST_TRACK`, `DUBPLATE_WAVE_FILE` |
| an audio device / microphone | `DUBPLATE_TEST_DEVICE`, `DUBPLATE_TEST_MIC` |
| a VST3 plugin | `DUBPLATE_TEST_VST3` (a `.vst3` bundle) |

**Never commit audio, models, runtime libraries, plugins or archives.** `.gitignore` covers the usual extensions and
`samples/`; keep your test songs there.

## Tools

Small programs in `crates/engine/examples/` for looking at what the analysis does on real files
(`cargo run --release -p dubplate-engine --example NAME -- ARGS`):

| Example | Use |
|---|---|
| `beatcheck FILE…` | tempo, grid and how well the kicks sit on it (set `BEATCHECK_GENRE=Dubstep` to try a genre tag) |
| `barcheck FILE…` | which beat is bar 1 and why (each sign's score), the bass entries, the sections, the suggested hot cues |
| `barscore LABELS` | scores bar 1 and the Drop sections against drops you checked by ear: see `scripts/bar_labels.example.txt` |
| `gridimport DAT AUDIO` | compares another program's analysis file with the audio it was made for |
| `loudcheck`, `keylockcheck`, `wavecheck`, `vst3check` | auto gain, key lock, the waveform summary (and its timing), the plugin host |

The app also has command-line tools for controller maps (`dubplate --midi-help`).

## Where things are

| | |
|---|---|
| `crates/core` | The shared audio engine ([crates.md](crates.md)). |
| `crates/engine` | Tempo and beat analysis, bar 1, sections, hot-cue roles, tap tempo, the ANLZ / PDB formats and USB export, plus the examples above. |
| `crates/app` | The DJ app: `app.rs` (the top level), `deck_view.rs`, `library*.rs`, `prepare.rs`, `waveform.rs` / `wave_draw.rs`, `midi*.rs`, `usb.rs`, `profiles/` (the built-in controller profiles). |
| `scripts/` | `setup_onnx.sh` / `.ps1` (model and runtime), `export_htdemucs*.py` (how the model was exported), `configure.sh` (libtorch), `midi_crosscheck.sh`. |
| `docs/` | These documents. |

A karaoke application, Pitched, is a separate project that builds on `dubplate-core`.

## Conventions

- **Formatting and lints.** `cargo fmt --all` (settings in `rustfmt.toml`: width 120, which suits the dense style here) and
  `cargo clippy --workspace --all-targets`: errors fail CI; the style warnings are being worked down, so do not add new
  ones. Run `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked && cargo test --workspace --locked`
  before you push.
- **Comments say why.** A comment explains a decision or a measurement, not what the next line does. Modules start with a
  `//!` paragraph saying what they are for.
- **Audio thread rules.** Nothing in the audio callback allocates, locks or waits. The UI talks to a deck through a
  lock-free command queue and atomics (`DeckCommand`, `DeckShared`).
- **Analysis changes.** If a change alters what analysis produces for a track, bump `CACHE_VERSION` (`crates/core/src/cache.rs`)
  so existing libraries re-analyse, say so in the commit, and run `barscore` on your labelled tracks before and after: a
  change should not lower the score. If it changes stems, bump `STEMS_VERSION` (`crates/core/src/stems.rs`).
- **Claims need evidence.** Documentation says what was checked and how, and says plainly what was not. Please keep it that
  way: "works" means tested or heard, and "written but not run" is a useful thing to write down.
- **Dependencies.** Prefer permissive licences (MIT / Apache-2.0; MPL-2.0 is fine for linking). `cargo audit` runs in CI; an
  advisory that is ignored needs a reason in `.cargo/audit.toml`.
- **Other programs.** Dubplate reads and writes other programs' *formats* (beat grids, USB export, controller maps) for
  interoperability. Do not add code, data, model weights or documents taken from another program, its installer or its
  files you did not make yourself; evaluate against the output such a program writes for *your own* tracks.

## Keeping the sound and the picture smooth

Three kinds of work run at once: the audio callback (a late one is a click), the screen (a late frame is a hitch in the
scrolling waveforms) and long jobs nobody is waiting on. The rules that keep them apart:

- **The audio callback never frees, allocates, logs or waits.** A track or stems it is done with (a new one was
  loaded) go back to the screen's side through a small queue (`Trash` in `deck.rs`, emptied by
  `DeckHandle::empty_trash` every frame): freeing a few hundred megabytes in the callback is heard on the other deck.
  A channel effect is built by `DeckHandle::send` and swapped in whole, so its delay lines are not allocated there
  either. The plugin on the main output is tried for a few microseconds instead of skipped when the screen happens to
  be reading its parameters (a skipped block is a click), and its bus buffers are made once.
- **A knob sets a target, the sound glides to it.** The EQ's gains move over about 5 ms and the filters keep their
  state while their coefficients change (`DeckEq`); replacing a filter on every knob message put a step in the signal
  each time.
- **The screen thread does nothing that scales with the library or the track.** Reading the library, its folder
  counts, its search text and its sort order all happen on a thread (`Loaded::read` in `library.rs`) and are swapped
  in whole; sorting by a text column is then sorting whole numbers. The onset curves Prepare needs (a seventh of a
  second each) are worked out on a thread while the screen opens. The database syncs to disk at checkpoints, not on
  every small write.
- **The playhead on screen is steered by the top of the audio's position staircase** (`playhead.rs`): the audio thread
  reports where it is once a block, and following each report makes the waveform shimmer by a fraction of a pixel a
  frame.
- **Background work runs at low priority** (`threads::background`: library analysis, the stem queue, reading cached
  stems), library analysis keeps to one thread while a deck is playing, and the audio callback asks for real-time
  scheduling (`threads::boost_audio_thread`; the log says what it got). Linux and macOS only, and only Linux has been
  run.

To check a change: `DUBPLATE_SHOT_TRACK=song.ogg DUBPLATE_SHOT_OUT=shot.ppm DUBPLATE_SHOT_DECK=1 dubplate` plays the
track on deck A (silently) over the library for four seconds and logs the processor time per frame (`frame cpu`:
median, 95th percentile, the worst three); without `DUBPLATE_SHOT_DECK` it does the same in Prepare. Point
`XDG_DATA_HOME`, `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` at scratch folders (with a copy of `library.db` for a library
of real size) so the run does not touch your own. With a 29,600-track library being analysed in the background, on
the machine this was written on: median 2.1 ms a frame, worst 6.9 ms (before this work the worst frames were 28 to
48 ms, each one a visible hitch, every time the library list was re-read).

## Data and caches

Dubplate writes analysis (`.bin`), waveform summaries (`.wave`) and stems (`.stems`) to its cache folder, keyed by a hash of
the file's path. Delete the cache folder to force a fresh analysis; `DUBPLATE_NO_CACHE=1` makes stem separation ignore its
cache. The library database is SQLite (`library.db`) in the data folder. Paths are in [install.md](install.md).

## Releasing

Packages are built by the release workflow (`.github/workflows/release.yml`) for Linux, Windows and macOS; none of them
includes the model or ONNX Runtime. Windows and macOS packages are unsigned.
