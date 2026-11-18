# Stem separation

Dubplate splits a track into drums, bass, other and vocals with HTDemucs (the 4-stem model).
Separation is optional: the app plays, mixes and analyses without it.

## Install

```bash
./scripts/setup_onnx.sh           # model + ONNX Runtime; uses the CUDA build if an NVIDIA GPU is found
./scripts/setup_onnx.sh --cpu     # model + CPU-only runtime (about 30 MB instead of 2 GB)
```

Files go to `${XDG_DATA_HOME:-~/.local/share}/dubplate/`:

| Path | What | Size |
|---|---|---|
| `models/htdemucs-dml.onnx` | HTDemucs 4-stem (compact export, runs on DirectML), checked against a pinned SHA-256 | 372 MB |
| `runtime/` | `libonnxruntime`, plus CUDA 13 and cuDNN 9 libraries for the GPU build | 30 MB CPU / 2.1 GB GPU |

The GPU build needs an NVIDIA driver that supports CUDA 13. Without one Dubplate falls back to the CPU
and says so in the log (`ONNX model ready on CPU`). `DUBPLATE_DEVICE=cpu` forces the CPU.

On Windows run `scripts\setup_onnx.ps1` (DirectML, any DirectX 12 GPU); on macOS `./scripts/setup_onnx.sh`
(CoreML). Both are written but have not been run on those systems. See [install.md](install.md).

## How it works

* `stem_pipeline.rs` cuts the track into 7.8 s segments with 25 % overlap, normalises the audio the
  way Demucs does (mean and standard deviation of the mono mix, once for the whole track), and blends
  the segments back with triangular weights. Engines only turn one segment into four stems.
* `ort_stems.rs` runs the segments through ONNX Runtime, which is opened at run time (no link-time
  dependency). The session stays loaded between tracks; opening it takes about 5 s.
* Results are cached in `~/.cache/dubplate/*.stems`. The cache carries a version; changing the maths
  means bumping `STEMS_VERSION` in `stems.rs` so stale stems are recomputed.

Engine order: ONNX Runtime, then libtorch (if built in), then the `demucs` command-line tool.

### libtorch (opt-in)

`cargo build --no-default-features --features torch-inference` (with `LIBTORCH` set) builds the older
TorchScript engine. Do not combine it with ONNX in one binary: libtorch's cuDNN is loaded first and the
ONNX CUDA provider then fails to start. See `scripts/setup_model.sh`.

## What was measured

Against the original PyTorch Demucs 4.1.0 and against instrumentals supplied with the sample tracks
(SDR of drums + bass + other against the supplied instrumental, three tracks):

| | SDR |
|---|---|
| plain mix (no separation) | 5.9 dB |
| previous TorchScript engine | 24.3 dB |
| ONNX engine | 24.3 dB |
| original PyTorch Demucs | 24.0 dB |

On a loud electronic track the previous TorchScript model produced a bass stem about 60× too loud
(rms 0.060 against 0.001 from the original and from ONNX, with most of its energy above 2 kHz).
The ONNX engine matched the original there.

## Licence of the model

Dubplate does not ship the model: the setup scripts download it on the user's machine
from a Hugging Face repo of the project's own ([ferrite-stems/htdemucs-onnx-dml](https://huggingface.co/ferrite-stems/htdemucs-onnx-dml),
a format conversion of the original weights, with the licence caveat below on its model card) and check it
against a fixed SHA-256. `scripts/export_htdemucs_onnx_dml.py` rebuilds the same file from the original weights.
`DUBPLATE_MODEL_URL=<address>` makes setup try a mirror of your own first.

The terms of the *weights* are unclear. The Demucs code is MIT. In 2022-23 its author said the pretrained
weights are not covered by that licence and are "provided for research purposes" (they were trained on
MUSDB18); some Hugging Face copies now carry an MIT tag, which looks inherited from the code repository
([discussion](https://huggingface.co/adefossez/HTDemucs/discussions/1)). Until that is settled:

* personal use on your own machine is the intended use;
* do not bundle the model into a package or installer, or sell a product built around it;
* anyone redistributing Dubplate commercially should ask the Demucs authors first.
