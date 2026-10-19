"""Export Demucs v4 (HTDemucs) and v3 (HDemucs) models to ONNX for ferraus.

Usage::

    pip install torch demucs onnx onnxruntime
    python tools/export/demucs_htdemucs.py --model htdemucs --out build/htdemucs
    python tools/export/demucs_htdemucs.py --model htdemucs_ft --out build/htdemucs_ft   # one ONNX per bag member
    python tools/export/demucs_htdemucs.py --model htdemucs_6s --out build/htdemucs_6s
    python tools/export/demucs_htdemucs.py --checkpoint model_drumsep.th --hdemucs --out build/drumsep \
        --stems kick,snare,cymbals,toms
    python tools/export/parity.py demucs --model htdemucs --onnx build/htdemucs/model.onnx

Weights: Meta's pretrained Demucs checkpoints (MIT) are fetched by ``demucs.pretrained``
from dl.fbaipublicfiles.com (htdemucs = 955717e8-8726e21a.th; htdemucs_ft = f7e0c4bc,
d12395a8, 92cfc3b6, 04573f0d; htdemucs_6s = 5c90dfd2-34c22ccb).

How the STFT gets into the graph: HTDemucs/HDemucs route every complex tensor through four
methods — ``_spec`` (STFT), ``_magnitude`` (complex-as-channels), ``_mask`` and ``_ispec``
(iSTFT). This script swaps those four methods on the instance for real-valued versions that
represent complex numbers as a trailing dimension of size 2 and compute the transforms with
``common.RealSTFT`` / ``common.RealISTFT`` (fixed convolutions). ``forward`` itself is
untouched, so the exported graph is the original network. Demucs' ``spectro`` uses
``normalized=True``; the real transforms reproduce that scaling.

The exported model takes exactly one training segment (7.8 s = 343 980 samples at 44.1 kHz
for v4) and ferraus does the overlap-add chunking, test-time shifts, and track
normalization (``params.normalize = "track"``) on the host, like ``demucs.apply``.
"""

from __future__ import annotations

import argparse
import math
import types

import torch
import torch.nn.functional as F
from torch import nn

from common import RealISTFT, RealSTFT, Spec, export_waveform_model


def patch_spectral_methods(model: nn.Module) -> nn.Module:
    """Replace the complex-valued helpers of an (H)TDemucs instance with real-valued ones."""
    nfft = model.nfft
    hl = model.hop_length
    stft = RealSTFT(nfft, hl, normalized=True)
    istft = RealISTFT(nfft, hl, normalized=True)
    model.add_module("_ferraus_stft", stft)
    model.add_module("_ferraus_istft", istft)
    cac = getattr(model, "cac", True)
    if not cac:
        raise SystemExit("only complex-as-channels (cac=True) Demucs models are supported")

    def _spec(self, x):
        # demucs.htdemucs.HTDemucs._spec with real tensors [..., F, T, 2]
        assert hl == nfft // 4
        le = int(math.ceil(x.shape[-1] / hl))
        pad = hl // 2 * 3
        x = F.pad(x, (pad, pad + le * hl - x.shape[-1]), mode="reflect")
        z = self._ferraus_stft(x)  # [B, C, F, T, 2]
        z = z[..., :-1, :, :]
        z = z[..., 2 : 2 + le, :]
        return z

    def _magnitude(self, z):
        b, c, fr, t, _ = z.shape
        m = z.permute(0, 1, 4, 2, 3)
        return m.reshape(b, c * 2, fr, t)

    def _mask(self, z, m):
        b, s, c, fr, t = m.shape
        return m.view(b, s, -1, 2, fr, t).permute(0, 1, 2, 4, 5, 3)

    def _ispec(self, z, length=None, scale=0):
        hop = hl // (4**scale)
        z = F.pad(z, (0, 0, 0, 0, 0, 1))  # one zero frequency bin
        z = F.pad(z, (0, 0, 2, 2))  # two frames each side
        pad = hop // 2 * 3
        le = hop * int(math.ceil(length / hop)) + 2 * pad
        x = self._ferraus_istft(z, le)
        return x[..., pad : pad + length]

    model._spec = types.MethodType(_spec, model)
    model._magnitude = types.MethodType(_magnitude, model)
    model._mask = types.MethodType(_mask, model)
    model._ispec = types.MethodType(_ispec, model)
    return model


class Wrapper(nn.Module):
    """``mix [1, 2, T] -> stems [1, S, 2, T]``."""

    def __init__(self, model: nn.Module):
        super().__init__()
        self.model = model

    def forward(self, mix):
        return self.model(mix)


def load_models(args) -> list[tuple[str, nn.Module]]:
    if args.checkpoint:
        from demucs.states import load_model

        pkg = torch.load(args.checkpoint, map_location="cpu", weights_only=False)
        return [(args.name or "model", load_model(pkg).eval())]
    from demucs.apply import BagOfModels
    from demucs.pretrained import get_model

    m = get_model(args.model)
    if isinstance(m, BagOfModels) and len(m.models) == 1:
        # most pretrained names are a "bag" of one model
        return [(args.model, m.models[0].eval())]
    if isinstance(m, BagOfModels):
        names = ["drums", "bass", "other", "vocals"] if args.model == "htdemucs_ft" else [str(i) for i in range(len(m.models))]
        return list(zip(names, [x.eval() for x in m.models]))
    return [(args.model, m.eval())]


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--model", default="htdemucs", help="pretrained name (htdemucs, htdemucs_ft, htdemucs_6s, hdemucs_mmi)")
    p.add_argument("--checkpoint", help="a Demucs .th package instead of a pretrained name (e.g. DrumSep)")
    p.add_argument("--hdemucs", action="store_true", help="the checkpoint is a v3 HDemucs (no transformer)")
    p.add_argument("--name", help="registry name for --checkpoint exports")
    p.add_argument("--stems", help="comma-separated stem names overriding model.sources")
    p.add_argument("--segment", type=float, help="segment seconds (default: the model's training segment)")
    p.add_argument("--out", required=True)
    p.add_argument("--version", default="v4-onnx")
    p.add_argument("--fp16-weights", action="store_true", help="store weights as fp16 (compute stays fp32)")
    args = p.parse_args()

    for member, model in load_models(args):
        model = patch_spectral_methods(model)
        sr = int(model.samplerate)
        seg_s = args.segment or float(getattr(model, "segment", 7.8))
        segment = int(seg_s * sr)
        if hasattr(model, "use_train_segment"):
            model.use_train_segment = False  # the host always feeds exactly one segment
        stems = args.stems.split(",") if args.stems else list(model.sources)
        out = args.out if member == args.model or args.checkpoint else f"{args.out}/{member}"
        name = f"separate/{args.name or args.model.replace('_', '-')}" + ("" if out == args.out else f"-{member}")
        spec = Spec(
            name=name,
            version=args.version,
            sample_rate=sr,
            license={"spdx": "MIT", "url": "https://github.com/facebookresearch/demucs/blob/main/LICENSE", "commercial_use": True},
            params={"normalize": "track"},
        )
        export_waveform_model(Wrapper(model), segment, stems, out, spec, channels=int(model.audio_channels), fp16_weights=args.fp16_weights)


if __name__ == "__main__":
    main()
