# Installing Dubplate

Once it is running, see the [user manual](user-manual.md). To build it yourself or work on it: [development.md](development.md).

Dubplate runs on Linux, Windows and macOS. **Only Linux has been run by its author.** The Windows and macOS
builds are compiled and tested by CI, but nobody has sat at a Windows or macOS machine with them yet, so
expect rough edges and please report them.

## 1. The app

Download the package for your system from the Releases page, unpack it, and run `dubplate`
(`dubplate.exe` on Windows). It plays, mixes, analyses and records without anything else installed.

| System | Package | Notes |
|---|---|---|
| Linux (x86-64) | `dubplate-…-linux-x86_64.tar.gz` | Needs ALSA (or PipeWire/PulseAudio's ALSA layer) and a Vulkan or OpenGL driver. |
| Windows 10/11 (x86-64) | `dubplate-…-windows-x86_64.zip` | Windows may warn about an unsigned app: choose "More info" → "Run anyway". |
| macOS (Apple silicon) | `dubplate-…-macos-arm64.tar.gz` | The app is not signed or notarised: right-click → Open the first time. |

To build from source instead: install Rust, then `cargo build --release -p dubplate`
(on Linux also `libasound2-dev libudev-dev libxkbcommon-dev libwayland-dev libx11-dev`).

## 2. Stem separation (optional)

Splitting a track into drums, bass, other and vocals needs a 372 MB model and ONNX Runtime. They are not
bundled; one script downloads them (the model is checked against a fixed checksum).

| System | Command | GPU |
|---|---|---|
| Linux | `./setup_onnx.sh` (add `--cpu` for the 28 MB CPU-only runtime) | NVIDIA through CUDA 13 + cuDNN 9 (about 2 GB); falls back to the CPU |
| Windows | `powershell -ExecutionPolicy Bypass -File setup_onnx.ps1` (add `-Cpu` for CPU only) | Any DirectX 12 GPU through DirectML (about 25 MB) |
| macOS | `./setup_onnx.sh` | Apple GPU / Neural Engine through CoreML |

Dubplate uses the GPU when it can and says which device it picked in its log (`ONNX model ready on …`).
Set `DUBPLATE_DEVICE=cpu` to force the CPU. See [stems.md](stems.md) for timings and how it works.

## 3. Controllers

Plug a controller in before starting Dubplate, then open **Preferences**.
If it is a known model it is mapped automatically; otherwise import a controller mapping from another DJ program (see
[midi/README.md](midi/README.md)), or press **Map…** next to the controller and teach Dubplate each control by moving it (MIDI learn).
Linux and macOS need no driver for class-compliant controllers; on Windows most controllers appear as
standard MIDI devices, but some makers need their own driver installed first.

## 4. Exporting to a USB stick for CDJ / XDJ players

Right-click a reel → **Export to USB…**, choose the drive, and Dubplate copies the tracks and writes the
database, beat grids, waveforms and cues that CDJ and XDJ players read. Format the drive FAT32 (or exFAT for
newer players). **This has not been tried on a real player yet** — please report how it goes.

## Where Dubplate keeps things

| | Linux | Windows | macOS |
|---|---|---|---|
| Settings, MIDI profiles | `~/.config/dubplate` | `%APPDATA%\Dubplate` | `~/Library/Application Support/Dubplate` |
| Library, models, runtime | `~/.local/share/dubplate` | `%LOCALAPPDATA%\Dubplate` | `~/Library/Application Support/Dubplate` |
| Analysis and stem caches | `~/.cache/dubplate` | `%LOCALAPPDATA%\Dubplate\cache` | `~/Library/Caches/Dubplate` |

If you used this program under its earlier name, Ferrite, move the three old folders (`ferrite` to `dubplate`, or `Ferrite` to `Dubplate`) to keep your library, cues, caches and models.

To uninstall, delete the app and those three folders. Your music is never moved or modified.
