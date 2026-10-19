"""Shared helpers for exporting separation models from PyTorch to ONNX for ferraus.

ferraus runs every separation model as *waveform in, waveform out*:

    mix   float32 [1, 2, T]        (stereo, model sample rate, T = the trained segment)
    stems float32 [1, S, 2, T]

so the STFT and inverse STFT must live inside the ONNX graph. ``torch.stft`` /
``torch.istft`` with complex tensors do not export portably (ONNX has no complex dtype and
``istft`` has no exporter), so this module re-implements both as fixed convolutions:

* ``RealSTFT``  — reflect padding + ``conv1d`` with windowed DFT basis vectors. Output is a
  real tensor ``[..., F, frames, 2]`` (real, imaginary), i.e. exactly
  ``torch.view_as_real(torch.stft(...))``.
* ``RealISTFT`` — inverse real DFT basis + ``conv_transpose1d`` overlap-add, divided by the
  window-square envelope, centre padding removed; matches ``torch.istft(center=True)``.

Both match torch within ~1e-6 in float32 (``python parity.py --stft-only`` checks this).

Every exporter writes ``<out>/model.onnx`` and ``<out>/spec.json`` (the ``ModelSpec`` that
ferraus stores next to cached models) and prints a registry entry with the SHA-256 so the
manifest in ``crates/ferraus-models/registry/registry.json`` can be updated. Exported files are release assets of
the ferraus-models repository; they are never committed to git.

Requirements: ``torch>=2.2``, ``onnx>=1.16``, ``onnxruntime>=1.18`` (for parity), ``numpy``.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
from dataclasses import asdict, dataclass, field
from typing import Optional

import torch
import torch.nn.functional as F
from torch import nn

OPSET = 17


def hann(n_fft: int, device=None, dtype=torch.float32) -> torch.Tensor:
    """Periodic Hann window, identical to ``torch.hann_window(n_fft)``."""
    return torch.hann_window(n_fft, periodic=True, device=device, dtype=dtype)


class RealSTFT(nn.Module):
    """``view_as_real(torch.stft(x, n_fft, hop, window=hann, center=True, pad_mode='reflect'))``.

    Input ``[..., T]``; output ``[..., n_fft // 2 + 1, frames, 2]``.
    """

    def __init__(self, n_fft: int, hop: int, normalized: bool = False, window: Optional[torch.Tensor] = None):
        super().__init__()
        self.n_fft = n_fft
        self.hop = hop
        self.normalized = normalized
        win = hann(n_fft) if window is None else window.float()
        bins = n_fft // 2 + 1
        n = torch.arange(n_fft, dtype=torch.float64)
        k = torch.arange(bins, dtype=torch.float64)[:, None]
        ang = 2.0 * math.pi * k * n / n_fft
        scale = 1.0 / math.sqrt(n_fft) if normalized else 1.0
        cos = (torch.cos(ang) * win.double() * scale).float()
        sin = (-torch.sin(ang) * win.double() * scale).float()
        # [2 * bins, 1, n_fft]: first the real parts, then the imaginary parts
        self.register_buffer("basis", torch.cat([cos, sin], 0)[:, None, :], persistent=False)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        shape = x.shape[:-1]
        x = x.reshape(-1, 1, x.shape[-1])
        pad = self.n_fft // 2
        x = F.pad(x, (pad, pad), mode="reflect")
        y = F.conv1d(x, self.basis, stride=self.hop)  # [B, 2 * bins, frames]
        bins = self.n_fft // 2 + 1
        y = y.reshape(y.shape[0], 2, bins, y.shape[-1]).permute(0, 2, 3, 1)
        return y.reshape(*shape, bins, y.shape[-2], 2)


class RealISTFT(nn.Module):
    """``torch.istft(view_as_complex(z), n_fft, hop, window=hann, center=True, length=length)``.

    Input ``[..., n_fft // 2 + 1, frames, 2]``; output ``[..., length]``.
    """

    def __init__(self, n_fft: int, hop: int, normalized: bool = False, window: Optional[torch.Tensor] = None):
        super().__init__()
        self.n_fft = n_fft
        self.hop = hop
        win = hann(n_fft) if window is None else window.float()
        bins = n_fft // 2 + 1
        n = torch.arange(n_fft, dtype=torch.float64)
        k = torch.arange(bins, dtype=torch.float64)[:, None]
        ang = 2.0 * math.pi * k * n / n_fft
        coef = torch.full((bins, 1), 2.0, dtype=torch.float64)
        coef[0] = 1.0
        if n_fft % 2 == 0:
            coef[-1] = 1.0
        scale = math.sqrt(n_fft) if normalized else 1.0
        cos = coef * torch.cos(ang) / n_fft * win.double() * scale
        sin = -coef * torch.sin(ang) / n_fft * win.double() * scale
        self.register_buffer("basis", torch.cat([cos, sin], 0).float()[:, None, :], persistent=False)
        self.register_buffer("win_sq", (win.double() ** 2).float()[None, None, :], persistent=False)

    def forward(self, z: torch.Tensor, length: int) -> torch.Tensor:
        shape = z.shape[:-3]
        bins, frames = z.shape[-3], z.shape[-2]
        z = z.reshape(-1, bins, frames, 2).permute(0, 3, 1, 2).reshape(-1, 2 * bins, frames)
        y = F.conv_transpose1d(z, self.basis, stride=self.hop)  # [B, 1, (frames-1)*hop + n_fft]
        ones = torch.ones(1, 1, frames, dtype=z.dtype, device=z.device)
        env = F.conv_transpose1d(ones, self.win_sq, stride=self.hop)
        y = y / torch.clamp(env, min=1e-11)
        pad = self.n_fft // 2
        y = y[:, 0, pad : pad + length]
        return y.reshape(*shape, length)


def complex_mul(a: torch.Tensor, b: torch.Tensor) -> torch.Tensor:
    """Multiply complex numbers stored as ``[..., 2]`` real tensors."""
    ar, ai = a[..., 0], a[..., 1]
    br, bi = b[..., 0], b[..., 1]
    return torch.stack([ar * br - ai * bi, ar * bi + ai * br], dim=-1)


@dataclass
class Spec:
    """Mirror of ferraus' ``ModelSpec`` (crates/ferraus-models/src/spec.rs)."""

    name: str
    version: str
    task: str = "separate"
    sha256: Optional[str] = None
    sample_rate: int = 44100
    stems: list = field(default_factory=list)
    arch: str = "waveform"
    inputs: list = field(default_factory=list)
    outputs: list = field(default_factory=list)
    chunk_seconds: Optional[float] = None
    chunk_samples: Optional[int] = None
    overlap: float = 0.25
    context_samples: int = 0
    provider_hints: list = field(default_factory=lambda: ["cuda", "coreml", "cpu"])
    license: dict = field(default_factory=dict)
    params: dict = field(default_factory=dict)


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def export_waveform_model(model: nn.Module, segment: int, stems: list, out_dir: str, spec: Spec,
                          channels: int = 2, opset: int = OPSET, fp16_weights: bool = False) -> str:
    """Export ``model(mix[1, C, T]) -> stems[1, S, C, T]`` with a fixed segment length.

    Writes ``model.onnx`` and ``spec.json`` into ``out_dir`` and returns the ONNX path.
    """
    os.makedirs(out_dir, exist_ok=True)
    path = os.path.join(out_dir, "model.onnx")
    model = model.eval().float()
    dummy = torch.zeros(1, channels, segment)
    with torch.no_grad():
        out = model(dummy)
    if tuple(out.shape) != (1, len(stems), channels, segment):
        raise SystemExit(f"model output shape {tuple(out.shape)} != (1, {len(stems)}, {channels}, {segment})")
    torch.onnx.export(
        model,
        (dummy,),
        path,
        input_names=["mix"],
        output_names=["stems"],
        opset_version=opset,
        do_constant_folding=True,
        dynamo=False,
    )
    if fp16_weights:
        convert_initializers_to_fp16(path)
    import onnx

    onnx.checker.check_model(path)
    spec.inputs = [{"name": "mix", "shape": [1, channels, segment]}]
    spec.outputs = [{"name": "stems", "shape": [1, len(stems), channels, segment]}]
    spec.stems = list(stems)
    spec.chunk_samples = segment
    spec.chunk_seconds = round(segment / spec.sample_rate, 4)
    spec.sha256 = sha256_file(path)
    with open(os.path.join(out_dir, "spec.json"), "w") as f:
        json.dump(asdict(spec), f, indent=2)
    print_registry_entry(path, spec)
    return path


def convert_initializers_to_fp16(path: str) -> None:
    """Store large float initializers as fp16 with a Cast back to fp32 (halves the download,
    keeps fp32 compute so CPU results stay within ~1e-4 of the fp32 export)."""
    import numpy as np
    import onnx
    from onnx import helper, numpy_helper

    m = onnx.load(path)
    g = m.graph
    new_inits, casts = [], []
    for init in g.initializer:
        arr = numpy_helper.to_array(init)
        if arr.dtype == np.float32 and arr.size >= 1024:
            half = numpy_helper.from_array(arr.astype(np.float16), init.name + "__fp16")
            new_inits.append(half)
            casts.append(helper.make_node("Cast", [half.name], [init.name], to=onnx.TensorProto.FLOAT))
        else:
            new_inits.append(init)
    del g.initializer[:]
    g.initializer.extend(new_inits)
    nodes = list(g.node)
    del g.node[:]
    g.node.extend(casts + nodes)
    onnx.save(m, path)


def print_registry_entry(path: str, spec: Spec) -> None:
    entry = {
        "name": spec.name,
        "version": spec.version,
        "task": spec.task,
        "stems": spec.stems,
        "format": "onnx",
        "size_bytes": os.path.getsize(path),
        "sha256": spec.sha256,
        "urls": ["<release asset URL>", "<mirror URL>"],
        "sample_rate": spec.sample_rate,
        "chunk_seconds": spec.chunk_seconds,
        "overlap": spec.overlap,
        "license": spec.license,
        "status": "verified",
        "arch": "waveform",
        "io": {
            "inputs": spec.inputs,
            "outputs": spec.outputs,
            "chunk_samples": spec.chunk_samples,
            "context_samples": spec.context_samples,
            "params": spec.params,
        },
    }
    print(json.dumps(entry, indent=2))


def load_yaml(path: str) -> dict:
    import yaml

    with open(path) as f:
        return yaml.safe_load(f)


def load_state_dict(path: str) -> dict:
    sd = torch.load(path, map_location="cpu", weights_only=False)
    for key in ("state_dict", "state", "model"):
        if isinstance(sd, dict) and key in sd and isinstance(sd[key], dict):
            sd = sd[key]
    # Lightning / DataParallel prefixes
    return {k.removeprefix("model.").removeprefix("module."): v for k, v in sd.items()}
