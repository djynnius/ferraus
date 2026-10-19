"""ONNX-exportable forward passes for BS-RoFormer and Mel-Band RoFormer.

Both architectures (lucidrains/BS-RoFormer, also vendored in ZFTurbo's
Music-Source-Separation-Training as ``models/bs_roformer``) call ``torch.stft`` with complex
outputs, multiply complex masks, and call ``torch.istft``. The wrappers below re-implement
``forward`` with identical tensor algebra but complex numbers stored as a trailing ``[..., 2]``
real dimension, using ``common.RealSTFT`` / ``RealISTFT``; all learned submodules
(``band_split``, ``layers``, ``final_norm``, ``mask_estimators``) are reused unchanged.

Tracked upstream: lucidrains ``bs-roformer`` 0.4–0.6 and MSST ``v1.0.x`` (same structure).
Set ``flash_attn=False`` (the config loader below forces it) so attention exports as plain
MatMul/Softmax. ``python parity.py roformer ...`` compares the wrapper against the original
module on a fixed clip before export.
"""

from __future__ import annotations

import torch
from torch import nn

from common import RealISTFT, RealSTFT, complex_mul


def _run_layers(model: nn.Module, x: torch.Tensor) -> torch.Tensor:
    """The axial transformer stack: x [b, t, f, d]."""
    for block in model.layers:
        if len(block) == 3:
            linear_transformer, time_transformer, freq_transformer = block
            b, t, f, d = x.shape
            x = linear_transformer(x.reshape(b, t * f, d)).reshape(b, t, f, d)
        else:
            time_transformer, freq_transformer = block
        b, t, f, d = x.shape
        x = x.permute(0, 2, 1, 3).reshape(b * f, t, d)
        x = time_transformer(x)
        x = x.reshape(b, f, t, d).permute(0, 2, 1, 3)
        x = x.reshape(b * t, f, d)
        x = freq_transformer(x)
        x = x.reshape(b, t, f, d)
    final_norm = getattr(model, "final_norm", None)
    if final_norm is not None:
        x = final_norm(x)
    return x


class _Base(nn.Module):
    def __init__(self, model: nn.Module):
        super().__init__()
        self.model = model
        kw = dict(model.stft_kwargs)
        self.n_fft = kw["n_fft"]
        self.hop = kw.get("hop_length", self.n_fft // 4)
        win_len = kw.get("win_length", self.n_fft)
        window = model.stft_window_fn(win_len) if hasattr(model, "stft_window_fn") else torch.hann_window(win_len)
        if win_len < self.n_fft:  # torch centres short windows
            left = (self.n_fft - win_len) // 2
            window = torch.nn.functional.pad(window, (left, self.n_fft - win_len - left))
        normalized = bool(kw.get("normalized", False))
        self.stft = RealSTFT(self.n_fft, self.hop, normalized=normalized, window=window)
        self.istft = RealISTFT(self.n_fft, self.hop, normalized=normalized, window=window)
        self.channels = getattr(model, "audio_channels", 2)
        self.num_stems = getattr(model, "num_stems", 1)

    def spectrogram(self, raw):
        """raw [b, s, t] → stft [b, f*s, frames, 2] (stereo interleaved into frequency)."""
        b, s, t = raw.shape
        z = self.stft(raw.reshape(b * s, t))  # [b*s, f, frames, 2]
        fr, frames = z.shape[1], z.shape[2]
        z = z.reshape(b, s, fr, frames, 2).permute(0, 2, 1, 3, 4).reshape(b, fr * s, frames, 2)
        return z

    def reconstruct(self, masked, length):
        """masked [b, n, f*s, frames, 2] → audio [b, n, s, length]."""
        b, n, fs, frames, _ = masked.shape
        s = self.channels
        fr = fs // s
        z = masked.reshape(b, n, fr, s, frames, 2).permute(0, 1, 3, 2, 4, 5).reshape(b * n * s, fr, frames, 2)
        audio = self.istft(z, length)
        return audio.reshape(b, n, s, length)


class BSRoformerONNX(_Base):
    """``mix [1, 2, T] -> stems [1, num_stems, 2, T]`` for a lucidrains BSRoformer."""

    def forward(self, raw):
        length = raw.shape[-1]
        stft_repr = self.spectrogram(raw)  # [b, F, frames, 2]
        b, fs, frames, _ = stft_repr.shape
        x = stft_repr.permute(0, 2, 1, 3).reshape(b, frames, fs * 2)  # 'b f t c -> b t (f c)'
        x = self.model.band_split(x)
        x = _run_layers(self.model, x)
        mask = torch.stack([fn(x) for fn in self.model.mask_estimators], dim=1)  # [b, n, t, f*2]
        n = mask.shape[1]
        mask = mask.reshape(b, n, frames, fs, 2).permute(0, 1, 3, 2, 4)  # [b, n, f, t, 2]
        masked = complex_mul(stft_repr[:, None], mask)
        return self.reconstruct(masked, length)


class MelBandRoformerONNX(_Base):
    """``mix [1, 2, T] -> stems [1, num_stems, 2, T]`` for a lucidrains MelBandRoformer."""

    def forward(self, raw):
        length = raw.shape[-1]
        m = self.model
        stft_repr = self.spectrogram(raw)  # [b, F*s, frames, 2]
        b, fs, frames, _ = stft_repr.shape
        idx = m.freq_indices  # mel-band gather indices into the interleaved frequency axis
        x = stft_repr[:, idx]  # [b, sum(band sizes), frames, 2]
        x = x.permute(0, 2, 1, 3).reshape(b, frames, -1)
        x = m.band_split(x)
        x = _run_layers(m, x)
        masks = torch.stack([fn(x) for fn in m.mask_estimators], dim=1)  # [b, n, t, bands*2]
        n = masks.shape[1]
        masks = masks.reshape(b, n, frames, -1, 2).permute(0, 1, 3, 2, 4)  # [b, n, sel, t, 2]
        # average overlapping mel bands back onto linear frequencies (scatter-add / count)
        sel = idx.shape[0]
        scatter = idx.view(1, 1, sel, 1, 1).expand(b, n, sel, frames, 2)
        summed = torch.zeros(b, n, fs, frames, 2, dtype=masks.dtype).scatter_add(2, scatter, masks)
        denom = m.num_bands_per_freq.repeat_interleave(self.channels).view(1, 1, fs, 1, 1)
        averaged = summed / denom.clamp(min=1e-8)
        masked = complex_mul(stft_repr[:, None], averaged)
        return self.reconstruct(masked, length)


def build_from_config(kind: str, config: dict, state_dict: dict) -> nn.Module:
    """Instantiate a lucidrains model from an MSST-style YAML config and load weights."""
    model_cfg = dict(config["model"])
    model_cfg["flash_attn"] = False
    if kind == "bs":
        from bs_roformer import BSRoformer as Cls
    else:
        from bs_roformer import MelBandRoformer as Cls
    allowed = Cls.__init__.__code__.co_varnames
    model = Cls(**{k: v for k, v in model_cfg.items() if k in allowed})
    missing, unexpected = model.load_state_dict(state_dict, strict=False)
    if missing:
        raise SystemExit(f"checkpoint is missing {len(missing)} tensors, e.g. {missing[:3]}")
    if unexpected:
        print(f"warning: ignoring {len(unexpected)} unexpected tensors, e.g. {unexpected[:3]}")
    return model.eval()
