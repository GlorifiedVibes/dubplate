#!/usr/bin/env python3
"""
Export HTDemucs to a compact ONNX model that DirectML (and everything else) can run.

The ONNX model people usually download (adowu/htdemucs-onnx) has its inverse FFT unrolled into a chain of
680 ScatterND operations, each writing a full copy of the output. The CPU reuses the buffer in place, but
DirectML keeps every copy alive: on a 12 GB RTX 3060 the card fills in half a second and the run fails
("not enough memory resources", then "the GPU will not respond"). 16,000 of the model's 24,700 operations
are that one block.

Here the STFT is one Conv1d against the windowed DFT basis, and the inverse is one MatMul against the
windowed inverse basis followed by four shifted additions (the 4096-sample frame is four hops of 1024).
Everything has a fixed shape. The result is about 8,000 operations, no ScatterND, same numbers.

Usage (in a throw-away virtualenv):
    pip install torch demucs onnx onnxscript onnxruntime numpy
    python scripts/export_htdemucs_onnx_dml.py [out.onnx]

It checks the result against the original model before writing, and says how far apart they are.
"""

import math
import sys
from pathlib import Path

import numpy as np
import torch
import torch.nn.functional as F


def build(model_cls):
    """HTDemucs with the complex-number and loop parts replaced by real, fixed-shape ones."""

    class RealHTDemucs(model_cls):
        def _bases(self, device):
            n = self.nfft                      # 4096
            win = torch.hann_window(n, periodic=True, device=device, dtype=torch.float64)
            k = torch.arange(n // 2 + 1, device=device, dtype=torch.float64)[:, None]
            t = torch.arange(n, device=device, dtype=torch.float64)[None, :]
            ang = 2.0 * math.pi * k * t / n
            scale = 1.0 / math.sqrt(n)          # normalized=True
            # forward: X[k] = sum_t w[t] x[t] (cos - i sin) / sqrt(n)
            fwd = torch.cat([win * torch.cos(ang), -win * torch.sin(ang)], dim=0) * scale   # [2*(n/2+1), n]
            # inverse: x[t] = w[t] * sum_k c_k (Re cos - Im sin) / sqrt(n), c = 1 at the ends, 2 inside
            c = torch.full((n // 2 + 1, 1), 2.0, device=device, dtype=torch.float64)
            c[0, 0] = 1.0
            c[-1, 0] = 1.0
            inv = torch.cat([c * torch.cos(ang), -c * torch.sin(ang)], dim=0) * win * scale  # [2*(n/2+1), n]
            return fwd.float(), inv.float(), win

        def _spec(self, x):
            """[B, C, T] -> [B, C, Fr, frames, 2] (real and imaginary parts), like the original _spec."""
            hl, nfft = self.hop_length, self.nfft
            T = x.shape[-1]
            le = int(math.ceil(T / hl))
            pad = hl // 2 * 3
            x = F.pad(x, (pad, pad + le * hl - T), mode="reflect")
            B, C, L = x.shape
            x = x.reshape(B * C, 1, L)
            x = F.pad(x, (nfft // 2, nfft // 2), mode="reflect")        # torch.stft's own centring
            fwd, _, _ = self._bases(x.device)
            y = F.conv1d(x, fwd[:, None, :], stride=hl)                  # [B*C, 2*(nfft/2+1), frames]
            nb = nfft // 2 + 1
            re, im = y[:, :nb - 1], y[:, nb:2 * nb - 1]                  # drop the top bin, as the original does
            z = torch.stack([re, im], dim=-1)                            # [B*C, Fr, frames, 2]
            z = z[:, :, 2:2 + le]
            return z.reshape(B, C, nb - 1, le, 2)

        def _magnitude(self, z):
            B, C, Fr, T, _ = z.shape
            return z.permute(0, 1, 4, 2, 3).reshape(B, C * 2, Fr, T)

        def _mask(self, z, m):
            B, S, C, Fr, T = m.shape
            return m.view(B, S, -1, 2, Fr, T).permute(0, 1, 2, 4, 5, 3)   # [B, S, ch, Fr, T, 2]

        def _ispec(self, z, length=None, scale=0):
            hl, nfft = self.hop_length, self.nfft
            B, S, C, Fr, T, _ = z.shape
            re, im = z[..., 0], z[..., 1]
            re = F.pad(re, (2, 2, 0, 1))                                  # a zero top bin, two zero frames each end
            im = F.pad(im, (2, 2, 0, 1))
            nb = Fr + 1
            frames_n = T + 4
            _, inv, win = self._bases(z.device)
            spec = torch.cat([re, im], dim=3).reshape(B * S * C, 2 * nb, frames_n).transpose(1, 2)   # [N, frames, 2*nb]
            fr = torch.matmul(spec, inv)                                  # [N, frames, nfft]
            # overlap-add: a frame is nfft/hl hops; piece i of frame j lands at hop j + i
            q = nfft // hl
            pieces = fr.reshape(B * S * C, frames_n, q, hl)
            out = None
            for i in range(q):
                p = F.pad(pieces[:, :, i, :], (0, 0, i, q - 1 - i))        # shift by i frames
                out = p if out is None else out + p
            total = (frames_n + q - 1) * hl
            out = out.reshape(B * S * C, total)
            # the window's squared overlap, the same for every call: divide it out as torch.istft does
            # (computed with numpy, outside the traced graph: written as torch operations it becomes a chain of
            # hundreds of ScatterND nodes in the exported model)
            w2 = win.cpu().numpy() ** 2
            env_np = np.zeros(total, dtype=np.float64)
            for j in range(frames_n):
                env_np[j * hl: j * hl + nfft] += w2
            env = torch.from_numpy(env_np)
            le = hl * int(math.ceil(length / hl)) + 2 * (hl // 2 * 3)
            lo = nfft // 2
            out = out[:, lo: lo + le] / env[lo: lo + le].float().clamp_min(1e-11)
            pad = hl // 2 * 3
            out = out[:, pad: pad + length]
            return out.reshape(B, S, C, length)

    return RealHTDemucs


def main():
    out_path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("htdemucs-dml.onnx")
    from demucs.pretrained import get_model
    from demucs.htdemucs import HTDemucs

    print("Loading htdemucs …")
    orig = get_model("htdemucs").models[0].eval()
    Real = build(HTDemucs)
    fast = Real.__new__(Real)
    fast.__dict__.update(orig.__dict__)         # same weights and settings, other methods
    fast.eval()

    seg = int(orig.segment * orig.samplerate)    # 343980
    torch.manual_seed(0)
    t = torch.arange(seg) / orig.samplerate
    mix = torch.stack([
        0.4 * torch.sin(2 * math.pi * 220 * t) + 0.2 * torch.randn(seg) * (torch.sin(2 * math.pi * 3 * t) > 0),
        0.3 * torch.sin(2 * math.pi * 330 * t) + 0.2 * torch.randn(seg) * (torch.sin(2 * math.pi * 2 * t) > 0),
    ])[None]
    if len(sys.argv) > 2:                        # a real clip: a .npy of shape [2, >= seg] at 44.1 kHz
        a = np.load(sys.argv[2])
        mix = torch.from_numpy(a[:, :seg].astype(np.float32))[None]

    with torch.no_grad():
        ref = orig(mix)
        got = fast(mix)
    print(f"torch, new vs original: max |diff| = {(ref - got).abs().max().item():.2e}  (signal peak {ref.abs().max().item():.2f})")

    print("Exporting …")
    torch.onnx.export(fast, (mix,), str(out_path), input_names=["mix"], output_names=["stems"],
                      opset_version=17, do_constant_folding=True, dynamo=False)

    import onnx
    m = onnx.load(str(out_path))
    ops = {}
    for n in m.graph.node:
        ops[n.op_type] = ops.get(n.op_type, 0) + 1
    print(f"{len(m.graph.node)} operations, ScatterND: {ops.get('ScatterND', 0)}; {out_path.stat().st_size / 1e6:.0f} MB")

    import onnxruntime as ort
    sess = ort.InferenceSession(str(out_path), providers=["CPUExecutionProvider"])
    import time
    t0 = time.time()
    onnx_out = sess.run(None, {"mix": mix.numpy()})[0]
    print(f"onnxruntime CPU: {time.time() - t0:.1f} s; vs original torch: max |diff| = {np.abs(onnx_out - ref.numpy()).max():.2e}")


if __name__ == "__main__":
    main()
