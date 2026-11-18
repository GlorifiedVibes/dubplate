#!/usr/bin/env bash
# setup_onnx.sh — install what Dubplate needs for stem separation (Linux and macOS; Windows has
# setup_onnx.ps1).
#
#   ./scripts/setup_onnx.sh            model + ONNX Runtime (CUDA build if an NVIDIA GPU is found)
#   ./scripts/setup_onnx.sh --cpu      model + CPU-only ONNX Runtime (about 30 MB, no GPU libraries)
#   ./scripts/setup_onnx.sh --no-model runtime only
#   ./scripts/setup_onnx.sh --no-runtime  model only
#
# Everything lands in ${XDG_DATA_HOME:-~/.local/share}/dubplate/ (Linux) or
# ~/Library/Application Support/Dubplate/ (macOS):
#   models/htdemucs-dml.onnx   HTDemucs 4-stem model (372 MB), checked against a pinned SHA-256
#   runtime/               libonnxruntime, plus the CUDA 13 / cuDNN 9 libraries for GPU use
#
# Linux needs curl and python3 with pip (only to fetch the runtime wheels — nothing is installed into Python).
# The GPU build wants an NVIDIA driver that supports CUDA 13; without one Dubplate falls back to the CPU.
# macOS needs only curl and tar; the runtime already includes Apple's GPU / Neural Engine support (CoreML)
# and works on Apple silicon.
set -euo pipefail

MODEL_URL="https://huggingface.co/ferrite-stems/htdemucs-onnx-dml/resolve/main/htdemucs-dml.onnx"
MODEL_SHA256="9e06fef2b23e0f772c6a0cedc4da3aeacb066fa2fa7b6bf70ba08bb1db6220ab"
ORT_VERSION="1.31.0"

OS="$(uname -s)"
if [ "$OS" = Darwin ]; then
  DATA="$HOME/Library/Application Support/Dubplate"
else
  DATA="${XDG_DATA_HOME:-$HOME/.local/share}/dubplate"
fi
MODELS="$DATA/models"
RUNTIME="$DATA/runtime"

want_model=1; want_runtime=1; gpu=auto
for a in "$@"; do
  case "$a" in
    --cpu) gpu=0 ;;
    --gpu) gpu=1 ;;
    --no-model) want_model=0 ;;
    --no-runtime) want_runtime=0 ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done
if [ "$gpu" = auto ]; then
  if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L >/dev/null 2>&1; then gpu=1; else gpu=0; fi
fi

case "$(uname -m)" in
  x86_64)  plat="x86_64" ;;
  aarch64|arm64) plat="aarch64" ;;
  *) echo "unsupported CPU: $(uname -m)" >&2; exit 1 ;;
esac
case "$OS" in
  Linux) ;;
  Darwin) [ "$plat" = aarch64 ] || { echo "macOS builds of ONNX Runtime are Apple silicon only" >&2; exit 1; }; gpu=0 ;;
  *) echo "this script is for Linux and macOS; on Windows run setup_onnx.ps1" >&2; exit 1 ;;
esac

sha256_ok() { # sha256_ok <hash> <file>
  if command -v sha256sum >/dev/null 2>&1; then echo "$1  $2" | sha256sum -c --status
  else [ "$(shasum -a 256 "$2" | cut -d' ' -f1)" = "$1" ]; fi
}

mkdir -p "$MODELS" "$RUNTIME"

# ── model ─────────────────────────────────────────────────────────────────────
if [ "$want_model" = 1 ]; then
  dest="$MODELS/htdemucs-dml.onnx"
  if [ -f "$dest" ] && sha256_ok "$MODEL_SHA256" "$dest" 2>/dev/null; then
    echo "model: already installed and verified"
  else
    echo "model: downloading 372 MB …"
    if [ -t 1 ]; then bar=(--progress-bar); else bar=(-sS); fi
    curl -L --fail "${bar[@]}" -C - -o "$dest.part" "$MODEL_URL"
    if ! sha256_ok "$MODEL_SHA256" "$dest.part"; then
      rm -f "$dest.part"
      echo "model: checksum mismatch — download discarded" >&2
      exit 1
    fi
    mv "$dest.part" "$dest"
    echo "model: installed → $dest"
  fi
fi

# ── runtime ───────────────────────────────────────────────────────────────────
if [ "$want_runtime" = 1 ] && [ "$OS" = Darwin ]; then
  echo "runtime: fetching ONNX Runtime $ORT_VERSION for Apple silicon …"
  work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
  if [ -t 1 ]; then bar=(--progress-bar); else bar=(-sS); fi
  curl -L --fail "${bar[@]}" -o "$work/ort.tgz" \
      "https://github.com/microsoft/onnxruntime/releases/download/v$ORT_VERSION/onnxruntime-osx-arm64-$ORT_VERSION.tgz"
  tar -xzf "$work/ort.tgz" -C "$work"
  lib="$(ls "$work"/onnxruntime-*/lib/libonnxruntime.*.dylib 2>/dev/null | head -1 || true)"
  [ -n "$lib" ] || { echo "runtime: no libonnxruntime found in the download" >&2; exit 1; }
  cp "$lib" "$RUNTIME/"
  ln -sf "$(basename "$lib")" "$RUNTIME/libonnxruntime.dylib"
  echo "runtime: installed → $RUNTIME"
elif [ "$want_runtime" = 1 ]; then
  command -v python3 >/dev/null || { echo "python3 is needed to fetch the runtime wheels" >&2; exit 1; }
  work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
  platforms=()
  for t in manylinux_2_28 manylinux_2_27 manylinux_2_17 manylinux2014; do platforms+=(--platform "${t}_${plat}"); done

  if [ "$gpu" = 1 ]; then
    echo "runtime: fetching ONNX Runtime $ORT_VERSION with CUDA 13 / cuDNN 9 (about 2 GB) …"
    spec="onnxruntime-gpu[cuda,cudnn]==$ORT_VERSION"
  else
    echo "runtime: fetching CPU-only ONNX Runtime $ORT_VERSION …"
    spec="onnxruntime==$ORT_VERSION"
  fi
  # Wheels are only archives of libraries here, so the interpreter version doesn't matter.
  python3 -m pip download --quiet --only-binary=:all: --python-version 3.12 --implementation cp \
      "${platforms[@]}" -d "$work/wheels" "$spec"

  python3 - "$work/wheels" "$RUNTIME" "$gpu" <<'PY'
import os, shutil, sys, zipfile, glob, re
wheels, dest, gpu = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
# Only what ONNX Runtime's CUDA provider loads (cuFFT, NVBLAS and friends stay behind).
cuda_keep = re.compile(r"^lib(cudart|cublas|cublasLt|curand|nvrtc|nvrtc-builtins|nvJitLink|cudnn.*)\.so")
n = 0
for whl in sorted(glob.glob(os.path.join(wheels, "*.whl"))):
    with zipfile.ZipFile(whl) as z:
        for info in z.infolist():
            base = os.path.basename(info.filename)
            if not base.startswith("lib") or ".so" not in base: continue
            ours = "onnxruntime/capi/" in info.filename and (
                base.startswith("libonnxruntime.so") or
                (gpu and base in ("libonnxruntime_providers_shared.so", "libonnxruntime_providers_cuda.so")))
            theirs = info.filename.startswith("nvidia/") and gpu and cuda_keep.match(base)
            if not (ours or theirs): continue
            with z.open(info) as src, open(os.path.join(dest, base), "wb") as out:
                shutil.copyfileobj(src, out)
            os.chmod(os.path.join(dest, base), 0o755)
            n += 1
if not os.path.exists(dest) or not any(f.startswith("libonnxruntime.so.") for f in os.listdir(dest)):
    sys.exit("runtime: no libonnxruntime found in the downloaded wheels")
print(f"runtime: {n} libraries → {dest}")
PY
  # libonnxruntime.so.1.31.0 → libonnxruntime.so
  ort="$(ls "$RUNTIME"/libonnxruntime.so.* 2>/dev/null | sort | tail -1 || true)"
  [ -n "$ort" ] && ln -sf "$(basename "$ort")" "$RUNTIME/libonnxruntime.so"
  echo "runtime: $(du -sh "$RUNTIME" | cut -f1) installed"
fi

echo
if [ "$OS" = Darwin ]; then
  echo "Done. Stems will use the GPU / Neural Engine through CoreML when you analyse a deck."
else
  echo "Done. Stems will use the $( [ "$gpu" = 1 ] && echo GPU || echo CPU ) when you analyse a deck."
fi
