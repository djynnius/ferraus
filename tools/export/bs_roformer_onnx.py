"""Export a BS-RoFormer checkpoint (lucidrains architecture) to ONNX for ferraus.

Usage::

    pip install torch "bs-roformer>=0.4" onnx onnxruntime pyyaml
    python tools/export/bs_roformer_onnx.py \
        --checkpoint model_bs_roformer_ep_317_sdr_12.9755.ckpt \
        --config model_bs_roformer_ep_317_sdr_12.9755.yaml \
        --name separate/bs-roformer-viperx-1297 --version ep317 \
        --stems vocals,other --license-spdx UNKNOWN --out build/bs_roformer
    python tools/export/parity.py roformer --kind bs --checkpoint ... --config ... --onnx build/bs_roformer/model.onnx

Configs are the Music-Source-Separation-Training YAML files distributed next to each
checkpoint (``model:`` block = constructor arguments, ``audio.chunk_size`` = training
segment, ``training.instruments`` / ``training.target_instrument`` = stems).

Single-target checkpoints (``target_instrument: vocals``) predict one stem; pass
``--complement other`` to add ``mix - target`` as a second output so ferraus gets a full
2-stem model. The STFT/iSTFT are inside the graph (see ``roformer_common.py``).

License: record the checkpoint's license in ``--license-*``. The registry keeps checkpoints
without a clear license out of every default tier.
"""

from __future__ import annotations

import argparse

import torch
from torch import nn

from common import Spec, export_waveform_model, load_state_dict, load_yaml
from roformer_common import BSRoformerONNX, MelBandRoformerONNX, build_from_config


class WithComplement(nn.Module):
    def __init__(self, inner: nn.Module):
        super().__init__()
        self.inner = inner

    def forward(self, mix):
        target = self.inner(mix)  # [1, 1, 2, T]
        return torch.cat([target, mix[:, None] - target], dim=1)


def export(kind: str, argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--checkpoint", required=True)
    p.add_argument("--config", required=True)
    p.add_argument("--name", required=True)
    p.add_argument("--version", required=True)
    p.add_argument("--stems", help="comma-separated output names (default: from the config)")
    p.add_argument("--complement", help="add mix - target as a stem with this name (single-target models)")
    p.add_argument("--segment", type=int, help="segment in samples (default: audio.chunk_size)")
    p.add_argument("--license-spdx", default=None)
    p.add_argument("--license-url", default=None)
    p.add_argument("--commercial-use", choices=["true", "false", "unknown"], default="unknown")
    p.add_argument("--fp16-weights", action="store_true")
    p.add_argument("--out", required=True)
    args = p.parse_args(argv)

    cfg = load_yaml(args.config)
    model = build_from_config(kind, cfg, load_state_dict(args.checkpoint))
    wrapper = BSRoformerONNX(model) if kind == "bs" else MelBandRoformerONNX(model)
    training = cfg.get("training", {})
    target = training.get("target_instrument")
    stems = args.stems.split(",") if args.stems else ([target] if target else list(training.get("instruments", [])))
    net: nn.Module = wrapper
    if args.complement:
        net = WithComplement(wrapper)
        stems = stems[:1] + [args.complement]
    sr = int(cfg.get("audio", {}).get("sample_rate", 44100))
    segment = args.segment or int(cfg.get("audio", {}).get("chunk_size", 352800))
    commercial = {"true": True, "false": False, "unknown": None}[args.commercial_use]
    spec = Spec(
        name=args.name,
        version=args.version,
        sample_rate=sr,
        license={"spdx": args.license_spdx, "url": args.license_url, "commercial_use": commercial},
        params={"arch": f"{kind}-roformer", "n_fft": wrapper.n_fft, "hop": wrapper.hop},
    )
    export_waveform_model(net, segment, stems, args.out, spec, channels=wrapper.channels, fp16_weights=args.fp16_weights)


if __name__ == "__main__":
    export("bs")
