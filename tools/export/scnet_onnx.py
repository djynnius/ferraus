"""Export an SCNet checkpoint (Music-Source-Separation-Training implementation) to ONNX.

Usage::

    git clone https://github.com/ZFTurbo/Music-Source-Separation-Training msst
    pip install torch onnx onnxruntime pyyaml einops
    python tools/export/scnet_onnx.py --msst msst \
        --checkpoint model_scnet_ep_54_sdr_9.8051.ckpt --config config_musdb18_scnet_xl.yaml \
        --name separate/scnet-xl --version ep54 --out build/scnet_xl
    python tools/export/parity.py scnet --msst msst --checkpoint ... --config ... --onnx build/scnet_xl/model.onnx

SCNet ("Sparse Compression Network", arXiv 2401.13276) runs on a complex spectrogram
stacked as channels. ``SCNetONNX`` re-implements ``SCNet.forward`` with the STFT/iSTFT
from ``common`` and reuses the encoder (``sd_layers``), the dual-path RNN blocks
(``dualpath_blocks``) and the decoder (``su_layers``) unchanged. The dual-path blocks use
LSTMs, which export as ONNX ``LSTM`` nodes.

The SCNet XL checkpoints were trained on MUSDB18-HQ and are released without a separate
weight license, so the registry keeps them out of default tiers (``commercial_use: null``).
"""

from __future__ import annotations

import argparse
import sys
from collections import deque

import torch
import torch.nn.functional as F
from torch import nn

from common import RealISTFT, RealSTFT, Spec, export_waveform_model, load_state_dict, load_yaml


class SCNetONNX(nn.Module):
    def __init__(self, model: nn.Module):
        super().__init__()
        self.model = model
        cfg = dict(model.stft_config)
        self.n_fft = cfg["n_fft"]
        self.hop = cfg.get("hop_length", model.hop_length)
        win = torch.hann_window(cfg.get("win_length", self.n_fft))
        normalized = bool(cfg.get("normalized", False))
        self.stft = RealSTFT(self.n_fft, self.hop, normalized=normalized, window=win)
        self.istft = RealISTFT(self.n_fft, self.hop, normalized=normalized, window=win)
        self.channels = model.audio_channels
        self.num_sources = len(model.sources)

    def forward(self, x):
        m = self.model
        b, ch, length = x.shape
        hop = self.hop
        # same padding rule as SCNet.forward (static for a fixed export length)
        padding = hop - length % hop
        if (length + padding) // hop % 2 == 0:
            padding += hop
        x = F.pad(x, (0, padding))
        padded = x.shape[-1]
        z = self.stft(x.reshape(b * ch, padded))  # [b*ch, F, T, 2]
        fr, t = z.shape[1], z.shape[2]
        # view_as_real → permute(0, 3, 1, 2) → [b, ch*2, F, T]
        z = z.permute(0, 3, 1, 2).reshape(b, ch * 2, fr, t)
        mean = z.mean(dim=(1, 2, 3), keepdim=True)
        std = z.std(dim=(1, 2, 3), keepdim=True)
        h = (z - mean) / (1e-5 + std)
        skips = deque()
        for sd in m.sd_layers:
            h, skip, lengths, original_lengths = sd(h)
            skips.append((skip, lengths, original_lengths))
        mid = getattr(m, "dualpath_blocks", None) or getattr(m, "separation_net")
        h = mid(h)
        for su in m.su_layers:
            skip, lengths, original_lengths = skips.pop()
            h = su(h, skip, lengths, original_lengths)
        h = h * std + mean
        n = m.dims[0]
        h = h.view(b, n, -1, fr, t)
        h = h.reshape(-1, 2, fr, t).permute(0, 2, 3, 1)  # [b*S*ch, F, T, 2]
        y = self.istft(h, padded)
        y = y.reshape(b, self.num_sources, self.channels, padded)
        return y[..., :length]


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--msst", required=True, help="path to a Music-Source-Separation-Training checkout")
    p.add_argument("--checkpoint", required=True)
    p.add_argument("--config", required=True)
    p.add_argument("--name", required=True)
    p.add_argument("--version", required=True)
    p.add_argument("--segment", type=int, help="segment in samples (default: audio.chunk_size)")
    p.add_argument("--fp16-weights", action="store_true")
    p.add_argument("--out", required=True)
    args = p.parse_args()

    sys.path.insert(0, args.msst)
    from models.scnet.scnet import SCNet  # noqa: E402  (MSST checkout)

    cfg = load_yaml(args.config)
    model = SCNet(**cfg["model"])
    model.load_state_dict(load_state_dict(args.checkpoint))
    model.eval()
    wrapper = SCNetONNX(model)
    sr = int(cfg.get("audio", {}).get("sample_rate", 44100))
    segment = args.segment or int(cfg["audio"]["chunk_size"])
    spec = Spec(
        name=args.name,
        version=args.version,
        sample_rate=sr,
        license={"spdx": None, "url": "https://github.com/ZFTurbo/Music-Source-Separation-Training", "commercial_use": None},
        params={"arch": "scnet", "n_fft": wrapper.n_fft, "hop": wrapper.hop},
    )
    export_waveform_model(wrapper, segment, list(model.sources), args.out, spec, channels=wrapper.channels, fp16_weights=args.fp16_weights)


if __name__ == "__main__":
    main()
