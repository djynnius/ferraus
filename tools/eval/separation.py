"""Evaluate `ferraus separate` on MUSDB18-HQ: SDR per stem, per track, and aggregated.

MUSDB18-HQ is licensed for research only and is never redistributed by ferraus; pass the path
to your own copy (the decoded "HQ" WAV layout)::

    <root>/test/<track name>/{mixture,vocals,drums,bass,other}.wav

Usage::

    python tools/eval/separation.py /data/musdb18hq --tier fast
    python tools/eval/separation.py /data/musdb18hq --tier quality --subset test --json results.json
    python tools/eval/separation.py /data/musdb18hq --model separate/htdemucs --tracks 5 --museval
    python tools/eval/separation.py /data/musdb18hq --tier max --ferraus target/release/ferraus

Metrics:

* ``sdr`` (default) — the "utterance SDR" used by the Sound Demixing Challenge and the
  MVSEP leaderboards: ``10 log10(sum(s^2) / sum((s - s_hat)^2))`` over the whole track,
  per stem, averaged over channels. These are the numbers the PRD tier targets compare to.
* ``--museval`` — additionally BSSEval v4 median-of-frames SDR (``museval`` package), the
  metric of the MUSDB18 papers (Demucs v4 reports 9.0 dB average for htdemucs).

The script shells out to the ferraus binary (``--ferraus``, default ``ferraus`` on PATH) so
it measures exactly what users run, writes stems to a temporary directory per track, and
reports wall time per track. Models must already be cached (``ferraus models pull
--all-for separate --tier <tier>``); the script passes ``--offline`` so a missing model fails
fast with exit code 5 instead of downloading.

Output: a table on stdout, and with ``--json`` a file with per-track and aggregate numbers
plus the ferraus version and the exact command, suitable for the per-stem validation runs
recorded in the registry ``benchmarks`` field.
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time

import numpy as np

STEMS = ["vocals", "drums", "bass", "other"]


def read_wav(path: str) -> tuple[np.ndarray, int]:
    import soundfile as sf

    audio, sr = sf.read(path, dtype="float64", always_2d=True)
    return audio, sr


def usdr(reference: np.ndarray, estimate: np.ndarray, eps: float = 1e-10) -> float:
    """Utterance-level SDR in dB, averaged over channels (SDX / MVSEP convention)."""
    n = min(len(reference), len(estimate))
    ref, est = reference[:n], estimate[:n]
    num = np.sum(ref**2, axis=0) + eps
    den = np.sum((ref - est) ** 2, axis=0) + eps
    return float(np.mean(10 * np.log10(num / den)))


def bsseval_sdr(references: dict, estimates: dict, sr: int) -> dict:
    import museval

    names = [s for s in STEMS if s in estimates]
    ref = np.stack([references[s] for s in names])
    n = min(ref.shape[1], min(estimates[s].shape[0] for s in names))
    est = np.stack([estimates[s][:n] for s in names])
    sdr, _isr, _sir, _sar = museval.evaluate(ref[:, :n], est, win=sr, hop=sr)
    return {s: float(np.nanmedian(sdr[i])) for i, s in enumerate(names)}


def run_ferraus(binary: str, mixture: str, outdir: str, args) -> tuple[float, dict]:
    cmd = [binary, "separate", mixture, "--outdir", outdir, "-o", "{name}.{stem}.wav", "--stems", "4",
           "--offline", "--json", "--overwrite", "--no-sidecar"]
    if args.model:
        cmd += ["--model", args.model]
    elif args.ensemble:
        cmd += ["--ensemble", args.ensemble]
        if args.weights:
            cmd += ["--weights", args.weights]
    else:
        cmd += ["--tier", args.tier]
    if args.shifts:
        cmd += ["--shifts", str(args.shifts)]
    if args.device:
        cmd += ["--device", args.device]
    start = time.time()
    proc = subprocess.run(cmd, capture_output=True, text=True)
    elapsed = time.time() - start
    try:
        envelope = json.loads(proc.stdout)
    except json.JSONDecodeError:
        envelope = {"ok": False, "error": {"message": proc.stderr.strip() or proc.stdout.strip()}}
    if proc.returncode != 0 or not envelope.get("ok"):
        err = envelope.get("error", {})
        hint = err.get("hint")
        sys.exit(f"ferraus failed (exit {proc.returncode}): {err.get('message')}" + (f"\nhint: {hint}" if hint else ""))
    return elapsed, envelope


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("root", help="MUSDB18-HQ root (contains train/ and test/)")
    p.add_argument("--subset", default="test", choices=["test", "train"])
    p.add_argument("--tier", default="quality", choices=["fast", "balanced", "quality", "max"])
    p.add_argument("--model", help="evaluate one registry model or ONNX file instead of a tier")
    p.add_argument("--ensemble", help="comma-separated models for a custom ensemble")
    p.add_argument("--weights", help="ensemble weights")
    p.add_argument("--shifts", type=int)
    p.add_argument("--device", help="auto, cpu, cuda, coreml, directml")
    p.add_argument("--tracks", type=int, help="only the first N tracks (quick runs)")
    p.add_argument("--ferraus", default="ferraus", help="path to the ferraus binary")
    p.add_argument("--museval", action="store_true", help="also compute BSSEval v4 SDR (slow)")
    p.add_argument("--json", help="write results to this file")
    args = p.parse_args()

    subset_dir = os.path.join(args.root, args.subset)
    if not os.path.isdir(subset_dir):
        sys.exit(f"{subset_dir} not found; pass the MUSDB18-HQ root directory")
    tracks = sorted(d for d in os.listdir(subset_dir) if os.path.isfile(os.path.join(subset_dir, d, "mixture.wav")))
    if args.tracks:
        tracks = tracks[: args.tracks]
    if not tracks:
        sys.exit(f"no tracks with mixture.wav under {subset_dir}")

    version = subprocess.run([args.ferraus, "--version"], capture_output=True, text=True).stdout.strip()
    per_track = []
    print(f"{'track':40s} " + " ".join(f"{s:>8s}" for s in STEMS) + f" {'avg':>8s} {'sec':>7s}")
    for name in tracks:
        tdir = os.path.join(subset_dir, name)
        refs = {}
        sr = 44100
        for s in STEMS:
            refs[s], sr = read_wav(os.path.join(tdir, f"{s}.wav"))
        with tempfile.TemporaryDirectory() as out:
            elapsed, envelope = run_ferraus(args.ferraus, os.path.join(tdir, "mixture.wav"), out, args)
            ests = {s: read_wav(os.path.join(out, f"mixture.{s}.wav"))[0] for s in STEMS}
        scores = {s: usdr(refs[s], ests[s]) for s in STEMS}
        row = {"track": name, "sdr": scores, "avg": statistics.mean(scores.values()), "seconds": elapsed,
               "quality": {r["stem"]: r.get("quality") for r in envelope.get("result", {}).get("stems", [])}}
        if args.museval:
            row["bsseval_sdr"] = bsseval_sdr(refs, ests, sr)
        per_track.append(row)
        print(f"{name[:40]:40s} " + " ".join(f"{scores[s]:8.2f}" for s in STEMS) + f" {row['avg']:8.2f} {elapsed:7.1f}")

    agg = {s: statistics.mean(r["sdr"][s] for r in per_track) for s in STEMS}
    med = {s: statistics.median(r["sdr"][s] for r in per_track) for s in STEMS}
    print(f"{'mean':40s} " + " ".join(f"{agg[s]:8.2f}" for s in STEMS) + f" {statistics.mean(agg.values()):8.2f}")
    print(f"{'median':40s} " + " ".join(f"{med[s]:8.2f}" for s in STEMS) + f" {statistics.mean(med.values()):8.2f}")
    result = {
        "ferraus_version": version,
        "dataset": f"MUSDB18-HQ/{args.subset}",
        "selection": {"tier": None if (args.model or args.ensemble) else args.tier, "model": args.model,
                      "ensemble": args.ensemble, "weights": args.weights, "shifts": args.shifts},
        "metric": "utterance SDR (dB), channel-averaged",
        "mean": agg,
        "median": med,
        "average": statistics.mean(agg.values()),
        "tracks": per_track,
    }
    if args.museval:
        result["bsseval_median_of_medians"] = {
            s: statistics.median(r["bsseval_sdr"][s] for r in per_track) for s in STEMS
        }
    if args.json:
        with open(args.json, "w") as f:
            json.dump(result, f, indent=2)
        print(f"wrote {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
