#!/usr/bin/env python3
"""Write crates/ferraus-cli/THIRD-PARTY-LICENSES.md: every third-party crate compiled into the
`ferraus` binary with its version, SPDX license, and source. Release CI regenerates the file
with full license texts using cargo-about (about.toml / about.hbs)."""
import json, subprocess, sys

meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", sys.argv[1] if len(sys.argv) > 1 else "aarch64-apple-darwin"]))
pkgs = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
root = next(p["id"] for p in meta["packages"] if p["name"] == "ferraus")
seen, stack = set(), [root]
while stack:
    i = stack.pop()
    if i in seen:
        continue
    seen.add(i)
    for d in nodes[i]["deps"]:
        if any(k["kind"] in (None, "build") for k in d["dep_kinds"]):
            stack.append(d["pkg"])
rows = sorted((pkgs[i]["name"], pkgs[i]["version"], pkgs[i].get("license") or "see repository", pkgs[i].get("repository") or "")
              for i in seen if not pkgs[i]["name"].startswith("ferraus"))
out = ["# Third-party licenses", "",
       "ferraus is licensed under Apache-2.0 OR MIT. The `ferraus` binary includes the crates below.",
       "FFmpeg, LAME, libopus, CUDA, plugins, sound libraries, and model weights are not compiled in; their",
       "licenses are shown when they are downloaded or loaded. Release builds regenerate this file with",
       "full license texts (cargo-about).", "",
       f"{len(rows)} crates.", "", "| Crate | Version | License | Source |", "| --- | --- | --- | --- |"]
out += [f"| {n} | {v} | {l} | {r} |" for n, v, l, r in rows]
open("crates/ferraus-cli/THIRD-PARTY-LICENSES.md", "w").write("\n".join(out) + "\n")
print(len(rows), "crates")
