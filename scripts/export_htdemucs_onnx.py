#!/usr/bin/env python3
"""
Export HTDemucs v4 to ONNX for Dubplate's embedded stem inference.

The exported model is placed at:
    ~/.local/share/dubplate/models/htdemucs.onnx   (~90 MB)

Dubplate will automatically detect and use this model the next time you
click "Analyze" on a deck.

── Quick start (Arch / CachyOS / any managed-Python distro) ──────────────────

    python -m venv /tmp/dubplate-export-env
    source /tmp/dubplate-export-env/bin/activate
    pip install numpy demucs torch
    python scripts/export_htdemucs_onnx.py
    deactivate

── Quick start (other distros / venv already active) ─────────────────────────

    pip install numpy demucs torch
    python scripts/export_htdemucs_onnx.py

Requirements: Python ≥ 3.9, PyTorch ≥ 1.9.
"""

import os
import sys
from pathlib import Path


def export() -> None:
    try:
        import torch
        import torch.nn as nn
    except ImportError:
        sys.exit("Error: PyTorch not found.  Run: pip install torch")

    try:
        import numpy  # noqa: F401 — demucs requires numpy; fail early with a clear message
    except ImportError:
        sys.exit("Error: numpy not found.  Run: pip install numpy demucs torch")

    try:
        from demucs.pretrained import get_model
    except ImportError as e:
        sys.exit(f"Error: could not import demucs ({e}).  Run: pip install numpy demucs torch")

    print("Loading htdemucs weights …")
    bag = get_model("htdemucs")

    # get_model returns BagOfModels; unwrap to the first (and only) HTDemucs instance
    from demucs.htdemucs import HTDemucs
    if hasattr(bag, "models"):
        model = bag.models[0]
    else:
        model = bag   # already a single model
    model.eval()

    if not isinstance(model, HTDemucs):
        print(f"Warning: got {type(model).__name__}, expected HTDemucs — export may fail")

    sr          = model.samplerate          # 44100
    segment_len = int(model.segment * sr)   # typically 343980

    # ── Device selection ──────────────────────────────────────────────────────
    # The model MUST be traced on the same device it will run on at inference
    # time.  torch.jit.trace bakes the device of every tensor into the graph,
    # so a CPU-traced model fails on CUDA with "stft input and window must be
    # on the same device".  We trace on GPU when available and write a
    # companion .device file so Dubplate knows which device to use.
    device_str = "cuda" if torch.cuda.is_available() else "cpu"
    device     = torch.device(device_str)
    model      = model.to(device)

    if device_str == "cuda":
        # cuDNN 9.x modular sublibraries (libcudnn_engines_runtime_compiled
        # etc.) may not be present on all systems.  Disable cuDNN and use
        # raw CUDA kernels — identical numerical results, no extra deps.
        torch.backends.cudnn.enabled = False

    print(f"  sources    : {model.sources}")
    print(f"  sample rate: {sr} Hz")
    print(f"  segment    : {segment_len} samples  ({model.segment:.2f} s)")
    print(f"  device     : {device_str}")

    # ── Wrapper that exposes a clean (mix) → (stems) interface ───────────────
    class HTDemucsWrapper(nn.Module):
        def __init__(self, m):
            super().__init__()
            self.m = m

        def forward(self, mix: torch.Tensor) -> torch.Tensor:
            # mix:   [1, 2, T]
            # stems: [1, sources, 2, T]
            return self.m(mix)

    wrapped = HTDemucsWrapper(model)
    dummy   = torch.randn(1, 2, segment_len, device=device)

    # ── Export ────────────────────────────────────────────────────────────────
    out_dir = Path(
        os.environ.get("XDG_DATA_HOME",
                       Path.home() / ".local" / "share")
    ) / "dubplate" / "models"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path    = out_dir / "htdemucs.pt"
    device_path = out_dir / "htdemucs.device"

    print(f"\nExporting to {out_path} …")

    wrapped.eval()

    # HTDemucs uses complex STFT ops that neither ONNX exporter can handle.
    # Save as TorchScript instead; Dubplate loads it via tch-rs (libtorch).
    print("  Tracing with torch.jit.trace …", flush=True)
    with torch.no_grad():
        traced = torch.jit.trace(wrapped, (dummy,), strict=False)

    print("  Saving TorchScript model …", flush=True)
    traced.save(str(out_path))
    device_path.write_text(device_str)   # read by Dubplate at runtime

    size_mb = out_path.stat().st_size / 1_000_000
    print(f"\nDone!  {out_path}  ({size_mb:.1f} MB)")
    print(f"\nNote: SEGMENT_FRAMES in tch_stems.rs is set to 343_980.")
    print(f"      This model uses {segment_len}.")
    if segment_len != 343_980:
        print(f"\nWARNING: mismatch! Update SEGMENT_FRAMES in")
        print(f"         crates/core/src/tch_stems.rs to {segment_len}.")


if __name__ == "__main__":
    export()
