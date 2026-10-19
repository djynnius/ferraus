#!/usr/bin/env python3
"""A minimal MCP client for `ferraus mcp serve`, using only the Python standard library.

It starts the server over stdio, then walks through what an AI client does: initialize, list
the tools, call ferraus_info and an effect tool, list the prompts, start a long job and poll
it, and show that the sandbox refuses a path outside the allowed directories.

    python3 examples/mcp/client.py [--samples DIR] [--out DIR]

With --samples (a folder made by examples/make-samples.sh) it uses samples/beat.wav;
without it, it asks the server to generate a test tone first. Everything is written inside
--out (default: a new temp dir). The binary is $FERRAUS, or `ferraus` on PATH.

The protocol is JSON-RPC 2.0, one message per line on stdin/stdout. Every tool result carries
the same envelope `ferraus <command> --json` prints, in `structuredContent`.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time


class McpClient:
    def __init__(self, argv):
        # The server logs to stderr; keep it out of our output but not lost.
        self.log = tempfile.TemporaryFile()
        self.proc = subprocess.Popen(
            argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, text=True, bufsize=1
        )
        self.next_id = 0

    def request(self, method, params=None):
        self.next_id += 1
        msg = {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params or {}}
        self._send(msg)
        while True:
            line = self.proc.stdout.readline()
            if not line:
                self.log.seek(0)
                sys.exit(f"server exited: {self.log.read().decode(errors='replace')}")
            reply = json.loads(line)
            # Notifications (progress, logging) can arrive before the reply; skip them.
            if reply.get("id") == self.next_id:
                if "error" in reply:
                    sys.exit(f"{method} failed: {reply['error']}")
                return reply["result"]

    def notify(self, method, params=None):
        self._send({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def tool(self, name, arguments):
        """Call a tool; returns (envelope, is_error)."""
        result = self.request("tools/call", {"name": name, "arguments": arguments})
        envelope = result.get("structuredContent")
        if envelope is None:  # clients that only read text get the same JSON there
            envelope = json.loads(result["content"][0]["text"])
        return envelope, bool(result.get("isError"))

    def close(self):
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()

    def _send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()


def section(title):
    print(f"\n== {title}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--samples", help="folder made by examples/make-samples.sh")
    ap.add_argument("--out", help="where outputs go (default: a new temp dir)")
    args = ap.parse_args()

    out = os.path.abspath(args.out or tempfile.mkdtemp(prefix="ferraus-mcp."))
    os.makedirs(out, exist_ok=True)
    ferraus = os.environ.get("FERRAUS", "ferraus")
    # Every read and write must be inside an --allow directory. The first one is where
    # relative paths resolve, so the output folder goes first; samples are only read.
    argv = [ferraus, "mcp", "serve", "--allow", out]
    if args.samples:
        argv += ["--allow", os.path.abspath(args.samples)]
    print(f"   server: {' '.join(argv)}")

    mcp = McpClient(argv)
    failures = 0
    try:
        section("initialize")
        init = mcp.request(
            "initialize",
            {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "ferraus-example", "version": "1"}},
        )
        mcp.notify("notifications/initialized")
        info = init["serverInfo"]
        print(f"   {info['title']} {info['version']}, protocol {init['protocolVersion']}")
        print(f"   capabilities: {', '.join(sorted(init['capabilities']))}")

        section("tools/list")
        tools = mcp.request("tools/list")["tools"]
        names = [t["name"] for t in tools]
        jobs = [n for n in names if n.startswith("ferraus_job_")]
        print(f"   {len(tools)} tools: one per command (ferraus_denoise, ferraus_analyze_key, ...) plus {', '.join(jobs)}")
        info_tool = next(t for t in tools if t["name"] == "ferraus_info")
        # inputSchema is the same JSON Schema as `ferraus schema info`.
        props = ", ".join(sorted(info_tool["inputSchema"]["properties"]))
        print(f"   ferraus_info takes: {props} (required: {info_tool['inputSchema'].get('required')})")

        if args.samples:
            source = os.path.join(os.path.abspath(args.samples), "beat.wav")
        else:
            section("tools/call ferraus_generate_tone (no samples given)")
            env, err = mcp.tool("ferraus_generate_tone", {"freq": "220Hz", "dur": "4s", "output": "tone.wav", "overwrite": True})
            source = env["outputs"][0]["path"]
            print(f"   wrote {source}")

        section("tools/call ferraus_info")
        env, err = mcp.tool("ferraus_info", {"inputs": [source], "loudness": True})
        r = env["result"]
        print(f"   ok={env['ok']} {r['duration_s']} s, {r['sample_rate']} Hz, {r['channels']} ch, "
              f"{r['loudness']['integrated_lufs']} LUFS")

        section("tools/call ferraus_fx (an effect chain in one call)")
        # Arguments are the command's flags with _ for -; a repeatable flag takes a list.
        env, err = mcp.tool(
            "ferraus_fx",
            {"inputs": [source], "fx": ["highpass:freq=60Hz", "compress:preset=bus", "normalize:lufs=-16"],
             "output": "beat.fx.wav", "overwrite": True},
        )
        failures += err
        print(f"   ok={env['ok']} -> {env['outputs'][0]['path']}")
        print(f"   params: {json.dumps(env['params'])[:160]}")

        section("prompts/list")
        for p in mcp.request("prompts/list")["prompts"]:
            print(f"   {p['name']}: {p.get('description', '')[:90]}")

        section("a long job: ferraus_job_start, then poll ferraus_job_status")
        job, err = mcp.tool(
            "ferraus_job_start",
            {"tool": "ferraus_master", "arguments": {"inputs": ["beat.fx.wav"], "target": "streaming",
                                                       "output": "beat.master.wav", "overwrite": True}},
        )
        job_id = job["job_id"]
        print(f"   started {job_id} ({job['state']})")
        deadline = time.time() + 300
        while True:
            status, _ = mcp.tool("ferraus_job_status", {"job_id": job_id})
            if status["state"] != "running" or time.time() > deadline:
                break
            time.sleep(0.25)
        print(f"   state: {status['state']}")
        result, err = mcp.tool("ferraus_job_result", {"job_id": job_id})
        failures += err or status["state"] != "succeeded"
        m = result.get("metrics", {})
        print(f"   master: {m.get('integrated_lufs')} LUFS, true peak {m.get('true_peak_dbtp')} dBTP, "
              f"target reached: {m.get('target_reached')}")

        section("the sandbox")
        outside = "/etc/hosts" if os.name != "nt" else "C:\\Windows\\win.ini"
        env, err = mcp.tool("ferraus_info", {"inputs": [outside]})
        print(f"   ferraus_info {outside}: isError={err}, {env['error']['code']}: {env['error']['message']}")
        failures += not err
    finally:
        mcp.close()

    section("Done" if not failures else f"{failures} check(s) failed")
    print(f"   outputs in {out}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
