#!/usr/bin/env python3
"""Collect ferraus adoption numbers from every channel that publishes them.

    python3 tools/metrics/collect.py                       # print today's snapshot
    python3 tools/metrics/collect.py --history metrics/history.jsonl --summary metrics/README.md

Sources (no audio, no personal data; only public counters and the repository's own traffic):

* GitHub: stars, forks, watchers; release downloads per file, grouped into installs (binary
  archives, MSI, .deb, .rpm, per OS and app), installer scripts, and update checks
  (``dist-manifest.json`` is what ``ferraus update`` reads); traffic: views, clones,
  referrers, and popular pages for the last 14 days.
* crates.io: total and recent (90-day) downloads.
* ghcr.io: total container downloads (read from the package page).
* Directories: the MCP Registry's latest listed version, the Glama grade, skills.sh installs,
  and the state of the listing pull requests.

Release download counts include the release pipeline's own checks (about one download per
file per release) and are a ceiling, not a head count. The apt/dnf repository on GitHub Pages
and Homebrew's third-party taps publish no download counts.

A token comes from GITHUB_TOKEN or ``gh auth token``. Traffic needs push access to the
repository (a fine-grained token with "Administration: read"); without it those fields are
null and the rest still works. GitHub keeps traffic for 14 days only, so run this at least
weekly (``.github/workflows/metrics.yml`` runs it daily) to keep a history.
"""

import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request

REPO = "djynnius/ferraus"
APPS = ("ferraus-plughost-vst3", "ferraus-engrave", "ferraus")  # longest prefix first
UA = "ferraus-metrics (https://github.com/djynnius/ferraus)"
LISTING_PRS = {
    "awesome-mcp-servers": ("punkpeye/awesome-mcp-servers", 18742),
    "hol-guard": ("hashgraph-online/hol-guard", 7331),
}


def token():
    t = os.environ.get("GITHUB_TOKEN")
    if t:
        return t
    try:
        return subprocess.run(["gh", "auth", "token"], capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


TOKEN = None


def get(url, accept="application/json", auth=False):
    headers = {"User-Agent": UA, "Accept": accept}
    if auth and TOKEN:
        headers["Authorization"] = f"Bearer {TOKEN}"
    req = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.read().decode("utf-8", "replace")
    except (urllib.error.URLError, TimeoutError) as e:
        print(f"warning: {url}: {e}", file=sys.stderr)
        return None


def gh(path):
    text = get(f"https://api.github.com/{path}", "application/vnd.github+json", auth=True)
    return json.loads(text) if text else None


def classify(name):
    """(kind, app, os) for a release asset."""
    if name.endswith((".sha256", ".sig", ".json.sha256")) or name in ("sha256.sum",):
        return ("checksum", None, None)
    if name == "dist-manifest.json":
        return ("update_check", None, None)
    app = next((a for a in APPS if name.startswith(a)), "other")
    if name.endswith(("-installer.sh", "-installer.ps1")):
        return ("installer_script", app, "windows" if name.endswith(".ps1") else "unix")
    if name.endswith((".tar.xz", ".tar.gz", ".zip", ".msi", ".deb", ".rpm")):
        if "apple-darwin" in name:
            os_ = "macos"
        elif "windows" in name or name.endswith(".msi"):
            os_ = "windows"
        else:
            os_ = "linux"
        return ("install", app, os_)
    return ("other", app, None)


def github():
    repo = gh(f"repos/{REPO}") or {}
    out = {
        "stars": repo.get("stargazers_count"),
        "forks": repo.get("forks_count"),
        "watchers": repo.get("subscribers_count"),
        "open_issues": repo.get("open_issues_count"),
    }
    releases = gh(f"repos/{REPO}/releases?per_page=100") or []
    installs = {"total": 0, "by_os": {}, "by_app": {}, "by_release": {}}
    scripts = {"unix": 0, "windows": 0}
    update_checks = {"total": 0, "latest_release": 0}
    for i, rel in enumerate(releases):
        tag = rel.get("tag_name")
        rel_installs = 0
        for a in rel.get("assets", []):
            kind, app, os_ = classify(a["name"])
            n = a.get("download_count", 0)
            if kind == "install":
                installs["total"] += n
                rel_installs += n
                installs["by_os"][os_] = installs["by_os"].get(os_, 0) + n
                installs["by_app"][app] = installs["by_app"].get(app, 0) + n
            elif kind == "installer_script":
                scripts[os_] += n
            elif kind == "update_check":
                update_checks["total"] += n
                if i == 0:
                    update_checks["latest_release"] = n
        installs["by_release"][tag] = rel_installs
    out["release_installs"] = installs
    out["installer_scripts"] = scripts
    out["update_checks"] = update_checks
    out["latest_release"] = releases[0]["tag_name"] if releases else None

    views = gh(f"repos/{REPO}/traffic/views")
    clones = gh(f"repos/{REPO}/traffic/clones")
    refs = gh(f"repos/{REPO}/traffic/popular/referrers")
    paths = gh(f"repos/{REPO}/traffic/popular/paths")
    if views is None:
        out["traffic"] = None
    else:
        day = lambda xs: {x["timestamp"][:10]: [x["count"], x["uniques"]] for x in xs}  # noqa: E731
        out["traffic"] = {
            "views_14d": [views.get("count"), views.get("uniques")],
            "clones_14d": [clones.get("count"), clones.get("uniques")] if clones else None,
            "views_daily": day(views.get("views", [])),
            "clones_daily": day(clones.get("clones", [])) if clones else {},
            "referrers": [[r["referrer"], r["count"], r["uniques"]] for r in (refs or [])][:10],
            "paths": [[p["path"], p["count"], p["uniques"]] for p in (paths or [])][:10],
        }
    return out


def crates():
    text = get("https://crates.io/api/v1/crates/ferraus")
    if not text:
        return None
    c = json.loads(text).get("crate", {})
    return {"downloads": c.get("downloads"), "recent_90d": c.get("recent_downloads"), "version": c.get("max_version")}


def number_after(text, label):
    if not text:
        return None
    plain = re.sub(r"<[^>]+>", " ", text)
    m = re.search(re.escape(label) + r"\s+([\d,]+)", plain)
    return int(m.group(1).replace(",", "")) if m else None


def ghcr():
    page = get(f"https://github.com/{REPO}/pkgs/container/ferraus", "text/html")
    return {"total_downloads": number_after(page, "Total downloads")}


def directories():
    out = {}
    text = get("https://registry.modelcontextprotocol.io/v0/servers?search=io.github.djynnius/ferraus")
    if text:
        latest = None
        for s in json.loads(text).get("servers", []):
            meta = s.get("_meta", {}).get("io.modelcontextprotocol.registry/official", {})
            if meta.get("isLatest"):
                latest = s.get("server", s).get("version")
        out["mcp_registry_latest"] = latest
    badge = get("https://glama.ai/mcp/servers/djynnius/ferraus/badges/score.svg", "image/svg+xml")
    m = re.search(r"rated (\S+) on Glama", badge or "")
    out["glama_grade"] = m.group(1) if m else None
    page = get("https://www.skills.sh/djynnius/ferraus", "text/html")
    m = re.search(r"([\d,]+)\s*total installs", re.sub(r"<[^>]+>", " ", page or ""))
    out["skills_sh_installs"] = int(m.group(1).replace(",", "")) if m else None
    prs = {}
    for name, (repo, num) in LISTING_PRS.items():
        pr = gh(f"repos/{repo}/pulls/{num}")
        prs[name] = None if not pr else ("merged" if pr.get("merged_at") else pr.get("state"))
    out["listing_prs"] = prs
    return out


def snapshot():
    return {
        "date": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d"),
        "github": github(),
        "crates_io": crates(),
        "ghcr": ghcr(),
        "directories": directories(),
    }


# ------------------------------------------------------------------------------- summary

HEADLINE = [
    ("GitHub stars", lambda s: s["github"]["stars"]),
    ("Release installs (archives, MSI, deb, rpm)", lambda s: s["github"]["release_installs"]["total"]),
    ("  macOS / Linux / Windows", lambda s: "{} / {} / {}".format(*(s["github"]["release_installs"]["by_os"].get(o, 0) for o in ("macos", "linux", "windows")))),
    ("Installer scripts run (sh / ps1)", lambda s: "{} / {}".format(s["github"]["installer_scripts"]["unix"], s["github"]["installer_scripts"]["windows"])),
    ("Update checks (dist-manifest.json)", lambda s: s["github"]["update_checks"]["total"]),
    ("crates.io downloads", lambda s: (s.get("crates_io") or {}).get("downloads")),
    ("Container pulls (ghcr.io)", lambda s: (s.get("ghcr") or {}).get("total_downloads")),
    ("skills.sh installs", lambda s: s["directories"].get("skills_sh_installs")),
    ("Repo views, 14 days (unique)", lambda s: "{} ({})".format(*s["github"]["traffic"]["views_14d"]) if s["github"].get("traffic") else None),
    ("MCP Registry latest", lambda s: s["directories"].get("mcp_registry_latest")),
    ("Glama grade", lambda s: s["directories"].get("glama_grade")),
]


def value(fn, snap):
    try:
        return fn(snap)
    except (KeyError, TypeError, IndexError):
        return None


def delta(now, then):
    if isinstance(now, int) and isinstance(then, int):
        d = now - then
        return f"{d:+d}" if d else "0"
    return ""


def summary(history):
    now = history[-1]
    today = dt.date.fromisoformat(now["date"])
    week = next((h for h in reversed(history) if dt.date.fromisoformat(h["date"]) <= today - dt.timedelta(days=7)), None)
    first = history[0]
    lines = [
        "# ferraus adoption",
        "",
        f"Updated {now['date']} by `tools/metrics/collect.py` ({len(history)} snapshot(s) since {first['date']}).",
        "Release counts include the release pipeline's own downloads (about one per file per release).",
        "",
        "| Metric | Now | 7 days | Since start |",
        "|---|---|---|---|",
    ]
    for label, fn in HEADLINE:
        v = value(fn, now)
        lines.append(f"| {label} | {v if v is not None else 'n/a'} | {delta(v, value(fn, week)) if week else ''} | {delta(v, value(fn, first))} |")
    # daily views across all snapshots (GitHub only keeps 14 days, so merge them)
    days = {}
    for h in history:
        t = (h.get("github") or {}).get("traffic") or {}
        for d, (c, u) in (t.get("views_daily") or {}).items():
            days[d] = max(days.get(d, [0, 0]), [c, u])
    if days:
        lines += ["", "## Repository views per day (unique)", "", "| Day | Views | Unique |", "|---|---|---|"]
        lines += [f"| {d} | {c} | {u} |" for d, (c, u) in sorted(days.items())[-30:]]
    t = now["github"].get("traffic") or {}
    if t.get("referrers"):
        lines += ["", "## Where visitors came from (14 days)", "", "| Referrer | Views | Unique |", "|---|---|---|"]
        lines += [f"| {r} | {c} | {u} |" for r, c, u in t["referrers"]]
    prs = now["directories"].get("listing_prs") or {}
    if prs:
        lines += ["", "## Listings", ""] + [f"- {k}: {v}" for k, v in prs.items()]
    return "\n".join(lines) + "\n"


def main():
    global TOKEN
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--history", help="append the snapshot to this JSON Lines file")
    ap.add_argument("--summary", help="write a Markdown summary of the history here")
    args = ap.parse_args()
    TOKEN = token()
    snap = snapshot()
    history = []
    if args.history:
        os.makedirs(os.path.dirname(os.path.abspath(args.history)), exist_ok=True)
        if os.path.exists(args.history):
            with open(args.history) as f:
                history = [json.loads(line) for line in f if line.strip()]
        history = [h for h in history if h.get("date") != snap["date"]] + [snap]
        with open(args.history, "w") as f:
            f.writelines(json.dumps(h, sort_keys=True) + "\n" for h in history)
    if args.summary:
        with open(args.summary, "w") as f:
            f.write(summary(history or [snap]))
    json.dump(snap, sys.stdout, indent=2, sort_keys=True)
    print()


if __name__ == "__main__":
    main()
