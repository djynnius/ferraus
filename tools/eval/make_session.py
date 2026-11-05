#!/usr/bin/env python3
"""Generate test sessions for the companion-skill evals (skills/*/evals) with ferraus itself.

No audio is committed to the repository: every eval scaffolds its inputs with this script.
The session is a 32-bar pop arrangement (A minor, 100 BPM, Am-F-C-G) written as MIDI and
rendered part by part through the installed General MIDI soundfont (``gm-basic``; ferraus
falls back to its built-in synth without it), then shaped like a raw DAW export: numbered
file names with spaces, mono files for mono sources, uneven levels.

Usage (``ferraus`` must be on PATH)::

    python3 tools/eval/make_session.py stems  --out .          # stems/01 Kick.wav ... 10 Synth Lead.wav
    python3 tools/eval/make_session.py mix    --out .          # mix.wav: an unmastered rough mix (and stems/)
    python3 tools/eval/make_session.py limited --out .         # loud_master.wav: a brickwall-limited master (and mix.wav)
    python3 tools/eval/make_session.py vocal  --out .          # vocal_take.wav: a noisy, humming, out-of-tune take, plus beat.wav
    python3 tools/eval/make_session.py export-check --out .    # stems/ with one stem shifted by 250 ms, plus bounce.wav

Everything is deterministic (fixed seed). Intermediate files go to a temporary directory.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile

BPM = 100
NOTE = {n: i for i, n in enumerate(["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"])}
CHORDS = [("A", ["A", "C", "E"]), ("F", ["F", "A", "C"]), ("C", ["C", "E", "G"]), ("G", ["G", "B", "D"])]
SECTIONS = [("intro", 0, 4), ("verse", 4, 12), ("chorus", 12, 20), ("break", 20, 24), ("chorus", 24, 32)]
# part, file name, instrument, export gain, mono?
STEMS = [
    ("kick", "01 Kick", "drums", "12dB", True),
    ("snare", "02 Snare", "drums", "9dB", True),
    ("hats", "03 Hats", "drums", "17dB", False),
    ("bass", "04 Bass", "gm:33", "13dB", True),
    ("keys", "05 Keys", "gm:4", "15dB", False),
    ("pad", "06 Pad", "gm:89", "9dB", False),
    ("guitar", "07 Gtr Arp", "gm:27", "15dB", False),
    ("lead_vox", "08 Lead Vox", "gm:52", "14dB", True),
    ("bv_oohs", "09 BV Oohs", "gm:53", "12dB", False),
    ("synth_lead", "10 Synth Lead", "gm:81", "13dB", False),
]
ROUGH_GAINS = "-2dB,-2dB,-6dB,-3dB,-8dB,-10dB,-9dB,0dB,-8dB,-9dB"
ROUGH_PANS = "0,0,30,0,-25,0,40,0,-50,-35"


def run(*args: str) -> dict:
    p = subprocess.run(["ferraus", *args, "--json"], capture_output=True, text=True)
    try:
        env = json.loads(p.stdout)
    except json.JSONDecodeError:
        sys.exit(f"ferraus {' '.join(args)} failed:\n{p.stderr}")
    if not env.get("ok"):
        sys.exit(f"ferraus {' '.join(args)} failed: {env.get('error')}")
    return env


def p(name: str, octave: int) -> int:
    return 12 * (octave + 1) + NOTE[name]


def section(bar: int) -> str:
    for name, a, b in SECTIONS:
        if a <= bar < b:
            return name
    return "end"


def parts() -> dict[str, list[dict]]:
    random.seed(7)
    out: dict[str, list[dict]] = {k: [] for k, *_ in STEMS}

    def add(part, beat, dur, pitch, vel, ch=1):
        jitter = random.uniform(-0.006, 0.006) if ch == 10 else random.uniform(-0.012, 0.012)
        out[part].append({"beat": round(max(0.0, beat + jitter), 4), "duration_beats": dur, "pitch": pitch,
                          "velocity": max(1, min(127, vel + random.randint(-6, 6))), "channel": ch})

    for bar in range(32):
        s = section(bar)
        b0 = bar * 4
        root, tones = CHORDS[bar % 4]
        full = s in ("verse", "chorus", "break")
        if full:
            for k in ([0, 1.5, 2.5] if s == "chorus" else [0, 2.5]):
                add("kick", b0 + k, 0.25, 36, 110, 10)
            if s != "break":
                for sn in (1, 3):
                    add("snare", b0 + sn, 0.25, 38, 105, 10)
                    if s == "chorus":
                        add("snare", b0 + sn, 0.25, 39, 80, 10)
        if s != "end":
            for e in range(8):
                add("hats", b0 + e * 0.5, 0.25, 46 if (e == 7 and s == "chorus") else 42, 92 if e % 2 == 0 else 70, 10)
        if full:
            for bt, d in ((0, 1.0), (1.5, 0.5), (2, 0.75), (3, 0.5), (3.5, 0.5)):
                add("bass", b0 + bt, d, p(root, 2) if bt != 3.5 else p(tones[2], 2), 100)
        for bt in ((0, 1.5, 2.5) if s != "intro" else (0, 2)):
            for t in tones:
                add("keys", b0 + bt, 0.9 if s == "intro" else 0.4, p(t, 4), 82)
        if s in ("intro", "chorus"):
            for i, t in enumerate(tones):
                add("pad", b0, 4.0, p(t, 3 if i == 0 else 4), 70)
        if s in ("verse", "chorus"):
            for e, t in enumerate([tones[0], tones[1], tones[2], tones[1]] * 2):
                add("guitar", b0 + e * 0.5, 0.45, p(t, 4 if e % 4 else 3), 78)
        if s in ("verse", "break"):
            phrase = ([(0, 1.0, "E", 4), (1, 0.5, "C", 4), (1.5, 0.5, "D", 4), (2, 1.5, "E", 4)] if bar % 2 == 0
                      else [(0, 0.5, "G", 4), (0.5, 0.5, "E", 4), (1, 1.0, "D", 4), (2, 1.5, "C", 4)])
            for bt, d, n, o in phrase:
                add("lead_vox", b0 + bt, d, p(n, o), 96)
        if s == "chorus":
            phrase = ([(0, 1.0, "A", 4), (1, 0.5, "C", 5), (1.5, 1.0, "E", 5), (2.5, 1.5, "D", 5)] if bar % 2 == 0
                      else [(0, 1.0, "C", 5), (1, 1.0, "B", 4), (2, 2.0, "G", 4)])
            for bt, d, n, o in phrase:
                add("lead_vox", b0 + bt, d, p(n, o), 104)
            for t in tones[1:]:
                add("bv_oohs", b0, 3.8, p(t, 4), 70)
            for i, bt in enumerate([0, 0.75, 1.5, 2, 2.75, 3.5]):
                add("synth_lead", b0 + bt, 0.4, p(tones[i % 3], 5), 88)
    end = 32 * 4
    add("kick", end, 0.5, 36, 115, 10)
    add("bass", end, 2.0, p("A", 2), 105)
    for t in CHORDS[0][1]:
        add("keys", end, 2.0, p(t, 4), 90)
        add("pad", end, 2.0, p(t, 4), 75)
    return out


def midi_doc(name: str, notes: list[dict]) -> dict:
    return {"ppq": 480, "tempo_map": [{"beat": 0, "bpm": BPM}],
            "time_signatures": [{"beat": 0, "numerator": 4, "denominator": 4}],
            "tracks": [{"name": name, "notes": sorted(notes, key=lambda n: n["beat"])}]}


def render_part(tmp: str, name: str, notes: list[dict], instrument: str) -> str:
    js, mid, wav = (os.path.join(tmp, f"{name}.{e}") for e in ("json", "mid", "wav"))
    with open(js, "w") as f:
        json.dump(midi_doc(name, notes), f)
    run("midi", "from-json", js, "-o", mid, "--overwrite")
    if instrument == "drums":
        run("render", mid, "--map", "1=drums", "-o", wav, "--sample-rate", "48000", "--overwrite")
    else:
        run("render", mid, "--instrument", instrument, "-o", wav, "--sample-rate", "48000", "--overwrite")
    return wav


def make_stems(out: str, tmp: str, shift_stem: str | None = None) -> list[str]:
    stems_dir = os.path.join(out, "stems")
    os.makedirs(stems_dir, exist_ok=True)
    pad = os.path.join(tmp, "_len.wav")
    run("generate", "silence", "--dur", "80s", "--sample-rate", "48000", "--channels", "2", "-o", pad, "--overwrite")
    notes = parts()
    files = []
    for part, fname, inst, gain, mono in STEMS:
        wav = render_part(tmp, part, notes[part], inst)
        padded = os.path.join(tmp, f"{part}.p.wav")
        offsets = "250ms,0s" if part == shift_stem else "0s,0s"
        run("mix", wav, pad, "--gains", f"{gain},0dB", "--offsets", offsets, "-o", padded, "--overwrite")
        dest = os.path.join(stems_dir, f"{fname}.wav")
        if mono:
            run("channels", padded, "--to", "mono", "-o", dest, "--bit-depth", "24", "--overwrite", "--no-sidecar")
        else:
            run("convert", padded, "-o", dest, "--bit-depth", "24", "--overwrite", "--no-sidecar")
        files.append(dest)
    return files


def rough_mix(files: list[str], dest: str) -> None:
    run("mix", *files, "--gains", ROUGH_GAINS, "--pans", ROUGH_PANS, "-o", dest, "--bit-depth", "24", "--overwrite", "--no-sidecar")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("kind", choices=["stems", "mix", "limited", "vocal", "export-check"])
    ap.add_argument("--out", default=".")
    a = ap.parse_args()
    if shutil.which("ferraus") is None:
        sys.exit("ferraus is not on PATH")
    os.makedirs(a.out, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ferraus-session-") as tmp:
        if a.kind == "export-check":
            # the bounce is made from the aligned stems, then one stem is re-exported late
            aligned = make_stems(os.path.join(tmp, "aligned"), tmp)
            run("mix", *aligned, "-o", os.path.join(a.out, "bounce.wav"), "--bit-depth", "24", "--overwrite", "--no-sidecar")
            make_stems(a.out, tmp, shift_stem="keys")
            return
        files = make_stems(a.out, tmp)
        if a.kind in ("mix", "limited"):
            rough_mix(files, os.path.join(a.out, "mix.wav"))
        if a.kind == "limited":
            run("fx", os.path.join(a.out, "mix.wav"), "-o", os.path.join(a.out, "loud_master.wav"),
                "--fx", "gain:by=+12dB", "--fx", "limit:ceiling=-0.1dBTP,release=30ms", "--overwrite", "--no-sidecar")
        if a.kind == "vocal":
            notes = parts()["lead_vox"]
            # every other note sung sharp, the rest flat
            even = render_part(tmp, "vox_even", notes[0::2], "gm:52")
            odd = render_part(tmp, "vox_odd", notes[1::2], "gm:52")
            run("pitch", even, "--cents", "35", "-o", os.path.join(tmp, "even_sharp.wav"), "--overwrite")
            run("pitch", odd, "--cents", "-30", "-o", os.path.join(tmp, "odd_flat.wav"), "--overwrite")
            run("generate", "noise", "--color", "pink", "--dur", "80s", "--amplitude", "-45dB", "--sample-rate", "48000", "-o", os.path.join(tmp, "noise.wav"), "--overwrite")
            run("generate", "tone", "--freq", "60Hz", "--dur", "80s", "--amplitude", "-38dB", "--sample-rate", "48000", "-o", os.path.join(tmp, "hum.wav"), "--overwrite")
            take = os.path.join(tmp, "take.wav")
            run("mix", os.path.join(tmp, "even_sharp.wav"), os.path.join(tmp, "odd_flat.wav"), os.path.join(tmp, "noise.wav"),
                os.path.join(tmp, "hum.wav"), "--gains", "14dB,14dB,0dB,0dB", "-o", take, "--overwrite")
            run("channels", take, "--to", "mono", "-o", os.path.join(a.out, "vocal_take.wav"), "--bit-depth", "24", "--overwrite", "--no-sidecar")
            beat = [f for f in files if "Lead Vox" not in f and "BV" not in f]
            run("mix", *beat, "--gains", "-2dB,-2dB,-6dB,-3dB,-8dB,-10dB,-9dB,-9dB", "--pans", "0,0,30,0,-25,0,40,-35",
                "-o", os.path.join(a.out, "beat.wav"), "--bit-depth", "24", "--overwrite", "--no-sidecar")
            shutil.rmtree(os.path.join(a.out, "stems"))


if __name__ == "__main__":
    main()
