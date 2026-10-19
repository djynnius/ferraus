#!/usr/bin/env python3
"""Run ferraus commands with --json and handle the envelope the way an agent should.

    python3 examples/agents/parse_envelope.py [--samples DIR] [--out DIR]
    python3 examples/agents/parse_envelope.py -- analyze key song.wav     # any one command

The demo runs a few commands that between them show every case: a success with outputs and
their roles, warnings, a missing input (exit 3), a bad argument (exit 2), and a model that is
not cached (exit 5, whose error.hint is the command that fixes it). Python 3 standard library
only; the binary is $FERRAUS, or `ferraus` on PATH. Outputs go to --out (default: a temp dir).
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field

# From AGENTS.md. The exit code and error.exit_code always agree.
EXIT_CODES = {
    0: "ok",
    1: "processing failed",
    2: "usage or parameter error",
    3: "input missing or unreadable",
    4: "unsupported format",
    5: "model not cached (run error.hint, or retry with -y)",
    6: "download or verification failed",
    7: "plugin error",
    8: "cancelled or timed out",
    9: "output verification failed",
    10: "network error",
    70: "internal error (a bug: please report it)",
}


@dataclass
class Result:
    exit_code: int
    envelope: dict
    stderr: str = ""
    outputs: dict = field(default_factory=dict)  # role -> path

    @property
    def ok(self):
        return self.envelope.get("ok") is True


def run_ferraus(*args):
    """Run `ferraus <args> --json` and return the parsed envelope.

    stdout holds exactly one JSON document, even on failure; logs and progress go to stderr.
    """
    exe = os.environ.get("FERRAUS", "ferraus")
    proc = subprocess.run([exe, *args, "--json"], capture_output=True, text=True)
    try:
        envelope = json.loads(proc.stdout)
    except json.JSONDecodeError:
        # Only a crash before ferraus could print its envelope ends up here.
        envelope = {"ok": False, "error": {"code": "no_envelope", "message": proc.stderr.strip()[-500:],
                                           "exit_code": proc.returncode}}
    outputs = {}
    for i, o in enumerate(envelope.get("outputs", [])):
        # Several outputs (stems, MIDI + notes, ...) are told apart by role, never by position;
        # the main output of a command may have none.
        role = o.get("role") or "main"
        outputs[role if role not in outputs else f"{role}{i}"] = o["path"]
    return Result(proc.returncode, envelope, proc.stderr, outputs)


def report(title, r):
    """Print what an agent should look at, in the order it should look."""
    env = r.envelope
    print(f"\n== {title}")
    print(f"   exit {r.exit_code} ({EXIT_CODES.get(r.exit_code, 'unknown')}), ok={env.get('ok')}, "
          f"command={env.get('command')!r}")
    if not r.ok:
        err = env.get("error", {})
        print(f"   error.code:    {err.get('code')}")
        print(f"   error.message: {err.get('message')}")
        if err.get("hint"):
            # Usually a runnable command; for exit 5 it is the `ferraus models pull ...` to run.
            print(f"   error.hint:    {err['hint']}")
        if r.exit_code == 5:
            print("   -> an agent runs the hint (or asks the user before a large download), then retries")
        return
    for w in env.get("warnings", []):
        # Warnings never fail a command, but they often say a result is a guess.
        print(f"   warning {w.get('code')}: {w.get('message')}")
    for role, path in r.outputs.items():
        print(f"   output [{role}]: {path}")
    if env.get("metrics"):
        print(f"   metrics: {json.dumps(env['metrics'])[:200]}")
    batch = env.get("result")
    if isinstance(batch, list):
        # Several inputs: one entry per input, each with its own ok/error.
        for item in batch:
            print(f"   item {os.path.basename(str(item.get('input')))}: ok={item.get('ok', True)}")


def demo(samples, out):
    checks = []

    r = run_ferraus("transcribe", os.path.join(samples, "hum.wav"), "-o", os.path.join(out, "hum.mid"), "--overwrite")
    report("success with two outputs told apart by role (transcribe)", r)
    checks.append(r.ok and "notes" in r.outputs)

    r = run_ferraus("compare", os.path.join(samples, "vocal.dry.wav"), os.path.join(samples, "podcast", "ep01.wav"))
    report("success with warnings (compare files of different length)", r)
    checks.append(r.ok and len(r.envelope.get("warnings", [])) > 0)

    r = run_ferraus("analyze", "loudness", os.path.join(samples, "podcast", "*.wav"))
    report("a batch: a quoted glob gives one result per input", r)
    checks.append(r.ok and isinstance(r.envelope.get("result"), list))

    r = run_ferraus("info", os.path.join(out, "missing.wav"))
    report("input missing", r)
    checks.append(r.exit_code == 3 and r.envelope["error"]["exit_code"] == 3)

    r = run_ferraus("gain", os.path.join(samples, "tone440.wav"), "--by", "loud", "-o", os.path.join(out, "x.wav"))
    report("bad argument", r)
    checks.append(r.exit_code == 2)

    # The DeepFilterNet speech model is not bundled. Without -y ferraus never downloads it: exit 5.
    # (If an earlier run already cached it, the command simply succeeds.)
    r = run_ferraus("denoise", os.path.join(samples, "vocal.wav"), "--mode", "ml", "--model", "dfn3",
                    "-o", os.path.join(out, "vocal.ml.wav"), "--overwrite")
    report("model not cached", r)
    checks.append(r.exit_code in (0, 5) and (r.ok or r.envelope["error"].get("hint", "").startswith("ferraus models pull")))

    failed = checks.count(False)
    print(f"\n== {len(checks) - failed}/{len(checks)} cases behaved as expected")
    return 1 if failed else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--samples", help="folder made by examples/make-samples.sh (default: make one)")
    ap.add_argument("--out", help="where outputs go (default: a new temp dir)")
    ap.add_argument("command", nargs="*", help="run just this ferraus command (put it after --)")
    args = ap.parse_args()

    if args.command:
        r = run_ferraus(*args.command)
        report("ferraus " + " ".join(args.command), r)
        return r.exit_code

    out = os.path.abspath(args.out or tempfile.mkdtemp(prefix="ferraus-envelope."))
    os.makedirs(out, exist_ok=True)
    samples = args.samples
    if not samples:
        samples = os.path.join(out, "samples")
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "make-samples.sh")
        subprocess.run(["bash", script, samples], check=True, stdout=subprocess.DEVNULL)
    return demo(os.path.abspath(samples), out)


if __name__ == "__main__":
    sys.exit(main())
