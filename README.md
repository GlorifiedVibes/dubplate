# Dubplate

A DJ program for your own music library, written in Rust. Two decks, a mixer, a library with tempo and key analysis,
stem separation (drums, bass, other, vocals), controller support, and a waveform that shows where the bars, the drops
and the sections are. It can also write a USB stick that CDJ / XDJ players read.

> **Status: early and experimental.** It is used and tested on Linux by its author. Windows and macOS builds compile and
> pass the tests on CI, but nobody has sat at one of those systems with it yet. The USB export has **not** been tried on
> a real player. See [what has and has not been checked](#what-has-and-has-not-been-checked).

## What it does

- **Decks and mixer.** Two decks with tempo (whole BPM), sync, key lock, key shift, quantised cues and loops, eight
  hot-cue pads, a 3-band EQ, a sweep filter, per-channel effects, a crossfader with two curves, headphone cue, and
  recording of the main output.
- **Library.** Watched folders, fast search (`bpm:120-128 key:~8A artist:daft`), your own **reels** (named sets of
  tracks), and **Flux**, which ranks the library by how well each track follows the one that is playing (tempo, key,
  genre).
- **Analysis.** Tempo and beat grid, which beat is bar 1, the key, and the song's sections (intro, build, drop, break,
  outro). Hot-cue suggestions come from the sections.
- **A waveform made for DJing.** Bass, mids and highs in colour, bar and phrase lines, section blocks and flags on the
  overview, and one lane per stem under each deck's waveform.
- **Prepare.** A screen for checking and fixing one track: a click track to hear the grid, tap tempo, nudge, halve or
  double the tempo, set bar 1, hold a cue or loop button to hear it, and a light that says whether the kicks sit on the
  grid.
- **Stems.** Splits a track into four stems with HTDemucs (ONNX Runtime, on the GPU when there is one); each stem can be
  muted on a deck.
- **Controllers.** Ships profiles for a number of controllers, learns any others (MIDI learn), and imports maps from
  other DJ programs.
- **Effects.** A VST3 effect on the main output, and a few built-in per-deck effects.
- **USB export** for CDJ / XDJ players (experimental): the tracks, database, beat grids, waveforms and cues.

## Getting it

See [docs/install.md](docs/install.md) for packages and for the optional stem-separation setup. To build from source:

```sh
cargo build --release -p dubplate      # needs Rust; on Linux also a few system libraries, see docs/development.md
./target/release/dubplate
```

## Documentation

| | |
|---|---|
| [User manual](docs/user-manual.md) | Using the program: library, decks, mixer, Prepare, stems, controllers, USB export. |
| [Installing](docs/install.md) | Packages, stem separation, controllers, where things are kept. |
| [Development](docs/development.md) | Building, requirements, tests, the crates, tools, conventions. |
| [Crates](docs/crates.md) | How the code is divided (`dubplate-core`, `dubplate-engine`, the app). |
| [Waveform](docs/waveform.md), [Beat analysis](docs/beat-analysis.md), [Prepare](docs/prepare.md) | How the waveform, tempo and bar-1 analysis, sections and the Prepare screen work, and how they were checked. |
| [Stems](docs/stems.md), [Key lock and gain](docs/keylock-and-gain.md), [VST3](docs/vst3.md) | The stem engine, key lock and auto gain, and the effect host. |
| [MIDI](docs/midi/README.md) | Controller profiles, the profile format, importers. |
| [Themes](docs/theme.md) | Interface and text sizes, and the colours, in `theme.toml`. |
| [CI](docs/ci.md) | The checks that run on every push. |

## What has and has not been checked

| | |
|---|---|
| Decks, mixer, library, Prepare, waveform, stems, controllers | Used on Linux. Unit tests cover the logic; the audio and the screens are checked by ear and by eye. |
| Tempo, bar 1 and sections | Measured on a small set of real tracks that were also checked by ear (see [docs/waveform.md](docs/waveform.md) and [docs/beat-analysis.md](docs/beat-analysis.md)). It works on those; **it will be wrong on some tracks**, which is why Prepare exists. |
| Windows, macOS | Built and tested by CI; **never run**. Expect rough edges. |
| USB export for CDJ / XDJ players | The files are checked against an independent reader of the format; **not tried on a real player**. |
| Stem separation on the GPU | Linux with an NVIDIA GPU. DirectML (Windows) and CoreML (macOS) are written, not run. |

## Third-party names

Dubplate is an independent project. It is **not affiliated with, endorsed by or sponsored by** any other DJ software or
hardware maker. rekordbox, Pioneer DJ, CDJ, XDJ, AlphaTheta, Serato, Traktor, DJUCED, Hercules, Mixxx, VST and other
names are trademarks of their owners. They appear in this project only to say what it can read, convert or write (for
example "import rekordbox beat grids", "export for CDJ / XDJ players"); the formats are documented by the community
projects credited in the source (see `crates/engine/src/pdb.rs` and `docs/beat-analysis.md`), and no vendor code, data
or model weights are used.

The stem-separation model (HTDemucs) is not included and is downloaded on your machine; read the licence notes in
[docs/stems.md](docs/stems.md) before redistributing anything built on it.

## Contact

Questions, bug reports and security concerns: dubplate.dj@proton.me.

## Licence

Dual-licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option. Unless you say
otherwise, any contribution you submit is licensed the same way, with no extra terms. The licence covers the code only,
not the stem-separation model (see [docs/stems.md](docs/stems.md)) or any music.
