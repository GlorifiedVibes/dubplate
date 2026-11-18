#!/usr/bin/env bash
# setup_model.sh — download and export the HTDemucs TorchScript model for Dubplate.
#
# Run once on a new machine, or to update to a newer demucs version.
# The model is saved to: ~/.local/share/dubplate/models/htdemucs.pt (~169 MB)
#
# Usage:
#   ./scripts/setup_model.sh [venv_dir]
#
# Arguments:
#   venv_dir  Where to create the Python venv (default: /tmp/dubplate-export-env)
#             Pass a permanent path to reuse the venv on future runs.
#
# Requirements:
#   - Python 3.9+ on PATH
#   - Internet access (downloads demucs + htdemucs weights on first run)
#   - ~2 GB free disk space during download (weights are cached by demucs)

set -euo pipefail

VENV_DIR="${1:-/tmp/dubplate-export-env}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODEL_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/dubplate/models"
MODEL_PATH="$MODEL_DIR/htdemucs.pt"

echo "╔══════════════════════════════════════════════╗"
echo "║   Dubplate — HTDemucs model setup             ║"
echo "╚══════════════════════════════════════════════╝"
echo
echo "  Venv:   $VENV_DIR"
echo "  Model:  $MODEL_PATH"
echo

# ── 1. Python check ───────────────────────────────────────────────────────────
if ! command -v python3 &>/dev/null; then
    echo "ERROR: python3 not found. Install Python 3.9+ and retry."
    exit 1
fi

PYTHON_VER=$(python3 -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")
echo "[1/4] Python $PYTHON_VER found"

# ── 2. Create / activate venv ─────────────────────────────────────────────────
if [ ! -f "$VENV_DIR/bin/activate" ]; then
    echo "[2/4] Creating Python virtual environment at $VENV_DIR..."
    python3 -m venv "$VENV_DIR"
else
    echo "[2/4] Reusing existing venv at $VENV_DIR"
fi

# shellcheck source=/dev/null
source "$VENV_DIR/bin/activate"

# ── 3. Install demucs ─────────────────────────────────────────────────────────
echo "[3/4] Installing demucs (this may take a few minutes on first run)..."
pip install --quiet --upgrade pip
pip install --quiet demucs

echo
echo "[4/4] Exporting HTDemucs TorchScript model..."
echo "      (downloads ~1.5 GB of model weights on first run via demucs)"
echo

python "$SCRIPT_DIR/export_htdemucs_onnx.py"

# ── Verify ────────────────────────────────────────────────────────────────────
echo
if [ -f "$MODEL_PATH" ]; then
    SIZE=$(du -h "$MODEL_PATH" | cut -f1)
    echo "✓ Model ready — $MODEL_PATH  ($SIZE)"
else
    echo "✗ Model not found at expected path: $MODEL_PATH"
    deactivate 2>/dev/null || true
    exit 1
fi

deactivate 2>/dev/null || true

echo
echo "All done! To build Dubplate with stem separation:"
echo
echo "  export LIBTORCH=~/.local/lib/libtorch"
echo "  export LD_LIBRARY_PATH=~/.local/lib/libtorch/lib:\$LD_LIBRARY_PATH"
echo "  cargo build --release"
echo
echo "libtorch 2.5.1 CPU tarball (required for tch 0.18):"
echo "  https://download.pytorch.org/libtorch/cpu/libtorch-cxx11-abi-shared-with-deps-2.5.1%2Bcpu.zip"
echo
echo "For GPU inference (RTX 3060 / CUDA 12.4):"
echo "  https://download.pytorch.org/libtorch/cu124/libtorch-cxx11-abi-shared-with-deps-2.5.1%2Bcu124.zip"
echo "  After extracting, run: scripts/fix_libtorch_cuda_symlinks.sh <libtorch_dir>"
