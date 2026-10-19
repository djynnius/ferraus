"""Numerical parity between the reference PyTorch models and their ONNX exports.

Acceptance criterion (PRD, stem separation): the ONNX port matches the PyTorch reference
within 1e-4 maximum absolute error on a fixed clip. Exit status is 0 on success, 1 on a
parity failure.

Usage::

    python tools/export/parity.py stft                                   # RealSTFT/RealISTFT vs torch
    python tools/export/parity.py demucs --model htdemucs --onnx build/htdemucs/model.onnx
    python tools/export/parity.py demucs --checkpoint model_drumsep.th --onnx build/drumsep/model.onnx
    python tools/export/parity.py roformer --kind mel --checkpoint MelBandRoformer.ckpt \
        --config config_vocals_mel_band_roformer_kj.yaml --onnx build/mel_roformer_kj/model.onnx [--complement]
    python tools/export/parity.py scnet --msst msst --checkpoint ... --config ... --onnx build/scnet_xl/model.onnx

The fixed clip is ``--clip <wav>`` (first segment, resampled by the caller to the model rate)
or, by default, a deterministic synthetic stereo signal (seeded tones, chirp, noise bursts)
of exactly one model segment. Three comparisons are printed:

1. original PyTorch module vs the real-valued wrapper (isolates the STFT re-implementation),
2. wrapper vs ONNX Runtime (isolates the exporter),
3. original vs ONNX Runtime (the acceptance number).
"""

from __future__ import annotations

import argparse
import math
import sys

import numpy as np
import torch

TOL = 1e-4


def synthetic_clip(length: int, sr: int, channels: int = 2, seed: int = 1234) -> torch.Tensor:
    g = torch.Generator().manual_seed(seed)
    t = torch.arange(length, dtype=torch.float64) / sr
    sig = 0.3 * torch.sin(2 * math.pi * 110 * t) + 0.2 * torch.sin(2 * math.pi * (440 + 220 * t) * t)
    bursts = (torch.rand(length, generator=g, dtype=torch.float64) * 2 - 1) * (torch.sin(2 * math.pi * 2 * t) > 0.95)
    left = sig + 0.3 * bursts
    right = 0.8 * sig - 0.2 * bursts + 0.05 * torch.sin(2 * math.pi * 3000 * t)
    x = torch.stack([left, right][:channels]).float()
    return x[None]


def load_clip(path: str, length: int) -> torch.Tensor:
    import soundfile as sf

    audio, _ = sf.read(path, dtype="float32", always_2d=True)
    audio = torch.from_numpy(audio.T.copy())
    if audio.shape[0] == 1:
        audio = audio.repeat(2, 1)
    audio = audio[:, :length]
    if audio.shape[1] < length:
        audio = torch.nn.functional.pad(audio, (0, length - audio.shape[1]))
    return audio[None]


def run_onnx(path: str, x: torch.Tensor) -> np.ndarray:
    import onnxruntime as ort

    opts = ort.SessionOptions()
    opts.intra_op_num_threads = 1
    sess = ort.InferenceSession(path, opts, providers=["CPUExecutionProvider"])
    name = sess.get_inputs()[0].name
    return sess.run(None, {name: x.numpy()})[0]


def report(label: str, a, b) -> float:
    a = np.asarray(a, dtype=np.float64)
    b = np.asarray(b, dtype=np.float64)
    if a.shape != b.shape:
        print(f"{label}: shape mismatch {a.shape} vs {b.shape}")
        return float("inf")
    err = float(np.max(np.abs(a - b)))
    print(f"{label}: max abs error {err:.3e} ({'ok' if err < TOL else 'FAIL'})")
    return err


def check_stft() -> float:
    from common import RealISTFT, RealSTFT

    worst = 0.0
    x = synthetic_clip(44100, 44100)[0]
    for n_fft, hop, normalized in [(4096, 1024, True), (2048, 512, False), (2048, 441, False), (7680, 1024, False)]:
        w = torch.hann_window(n_fft)
        ref = torch.view_as_real(torch.stft(x, n_fft, hop, window=w, center=True, normalized=normalized, return_complex=True))
        ours = RealSTFT(n_fft, hop, normalized=normalized)(x)
        # unnormalized STFT coefficients reach ~n_fft x amplitude, so compare relative to the
        # largest coefficient (float32 rounding); the waveform round trip below is absolute
        scale = float(ref.abs().max())
        worst = max(worst, report(f"stft n_fft={n_fft} hop={hop} (relative)", ref / scale, ours / scale))
        back_ref = torch.istft(torch.view_as_complex(ref.contiguous()), n_fft, hop, window=w, center=True, normalized=normalized, length=x.shape[-1])
        back = RealISTFT(n_fft, hop, normalized=normalized)(ours, x.shape[-1])
        worst = max(worst, report(f"istft n_fft={n_fft} hop={hop}", back_ref, back))
    return worst


def check(original, wrapper, onnx_path: str, x: torch.Tensor) -> float:
    with torch.no_grad():
        ref = original(x)
        ours = wrapper(x)
    report("original vs wrapper", ref, ours)
    out = run_onnx(onnx_path, x)
    report("wrapper vs onnx", ours, out)
    return report("original vs onnx (acceptance)", ref, out)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("stft")
    d = sub.add_parser("demucs")
    d.add_argument("--model", default="htdemucs")
    d.add_argument("--checkpoint")
    d.add_argument("--member", type=int, default=0, help="bag member index (htdemucs_ft)")
    d.add_argument("--onnx", required=True)
    d.add_argument("--clip")
    r = sub.add_parser("roformer")
    r.add_argument("--kind", choices=["bs", "mel"], required=True)
    r.add_argument("--checkpoint", required=True)
    r.add_argument("--config", required=True)
    r.add_argument("--complement", action="store_true", help="the export appended mix - target")
    r.add_argument("--onnx", required=True)
    r.add_argument("--clip")
    s = sub.add_parser("scnet")
    s.add_argument("--msst", required=True)
    s.add_argument("--checkpoint", required=True)
    s.add_argument("--config", required=True)
    s.add_argument("--onnx", required=True)
    s.add_argument("--clip")
    args = p.parse_args()
    torch.manual_seed(0)

    if args.cmd == "stft":
        return 0 if check_stft() < TOL else 1

    if args.cmd == "demucs":
        import copy

        from demucs_htdemucs import patch_spectral_methods

        if args.checkpoint:
            from demucs.states import load_model

            original = load_model(torch.load(args.checkpoint, map_location="cpu", weights_only=False)).eval()
        else:
            from demucs.apply import BagOfModels
            from demucs.pretrained import get_model

            original = get_model(args.model)
            if isinstance(original, BagOfModels):
                original = original.models[args.member]
            original = original.eval()
        if hasattr(original, "use_train_segment"):
            original.use_train_segment = False
        wrapper = patch_spectral_methods(copy.deepcopy(original))
        length = int(float(getattr(original, "segment", 7.8)) * original.samplerate)
        x = load_clip(args.clip, length) if args.clip else synthetic_clip(length, original.samplerate)
        err = check(original, wrapper, args.onnx, x)

    elif args.cmd == "roformer":
        from common import load_state_dict, load_yaml
        from roformer_common import BSRoformerONNX, MelBandRoformerONNX, build_from_config

        cfg = load_yaml(args.config)
        original = build_from_config(args.kind, cfg, load_state_dict(args.checkpoint))
        wrapper = BSRoformerONNX(original) if args.kind == "bs" else MelBandRoformerONNX(original)
        length = int(cfg.get("audio", {}).get("chunk_size", 352800))
        sr = int(cfg.get("audio", {}).get("sample_rate", 44100))
        x = load_clip(args.clip, length) if args.clip else synthetic_clip(length, sr)

        def reference(mix):
            y = original(mix)
            if y.dim() == 3:  # single-stem models return [b, s, t]
                y = y[:, None]
            if args.complement:
                y = torch.cat([y, mix[:, None] - y], dim=1)
            return y

        def ours(mix):
            y = wrapper(mix)
            if args.complement:
                y = torch.cat([y, mix[:, None] - y], dim=1)
            return y

        err = check(reference, ours, args.onnx, x)

    else:
        sys.path.insert(0, args.msst)
        from models.scnet.scnet import SCNet  # noqa: E402

        from common import load_state_dict, load_yaml
        from scnet_onnx import SCNetONNX

        cfg = load_yaml(args.config)
        original = SCNet(**cfg["model"])
        original.load_state_dict(load_state_dict(args.checkpoint))
        original.eval()
        wrapper = SCNetONNX(original)
        length = int(cfg["audio"]["chunk_size"])
        x = load_clip(args.clip, length) if args.clip else synthetic_clip(length, int(cfg["audio"].get("sample_rate", 44100)))
        err = check(original, wrapper, args.onnx, x)

    return 0 if err < TOL else 1


if __name__ == "__main__":
    sys.exit(main())
