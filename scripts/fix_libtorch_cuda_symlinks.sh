#!/usr/bin/env bash
# fix_libtorch_cuda_symlinks.sh — create versioned symlinks libtorch 2.5.1 cu124 needs.
#
# The libtorch CUDA tarball bundles NVRTC libs with hash-names or no version
# suffix, but the runtime linker looks for specific versioned names.
# This script creates the missing symlinks.
#
# Usage:
#   ./scripts/fix_libtorch_cuda_symlinks.sh [libtorch_dir]
#
# Default libtorch_dir: ~/.local/lib/libtorch-cuda/libtorch

set -euo pipefail

LIB_DIR="${1:-$HOME/.local/lib/libtorch-cuda/libtorch}/lib"

if [ ! -d "$LIB_DIR" ]; then
    echo "ERROR: lib directory not found: $LIB_DIR"
    echo "Pass the libtorch root as an argument, e.g.:"
    echo "  $0 ~/.local/lib/libtorch-cuda/libtorch"
    exit 1
fi

echo "Fixing NVRTC symlinks in $LIB_DIR ..."

cd "$LIB_DIR"

# libnvrtc.so.12 — needed by the CUDA JIT fuser
if [ ! -e libnvrtc.so.12 ]; then
    HASH_SO=$(ls libnvrtc-*.so.12 2>/dev/null | head -1)
    if [ -n "$HASH_SO" ]; then
        ln -s "$HASH_SO" libnvrtc.so.12
        echo "  libnvrtc.so.12 -> $HASH_SO"
    else
        echo "  WARN: libnvrtc-<hash>.so.12 not found, skipping"
    fi
else
    echo "  libnvrtc.so.12 already exists"
fi

# libnvrtc-builtins.so.12.4 — needed by nvrtc kernel compilation (cu124 build)
if [ ! -e libnvrtc-builtins.so.12.4 ]; then
    if [ -e libnvrtc-builtins.so ]; then
        ln -s libnvrtc-builtins.so libnvrtc-builtins.so.12.4
        echo "  libnvrtc-builtins.so.12.4 -> libnvrtc-builtins.so"
    else
        echo "  WARN: libnvrtc-builtins.so not found, skipping"
    fi
else
    echo "  libnvrtc-builtins.so.12.4 already exists"
fi

echo "Done."
