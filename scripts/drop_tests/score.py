#!/usr/bin/env python3
"""Scores the beat grid, bar 1 and Drop sections against times a person heard by ear.

    cargo build --release -p dubplate-engine --example barcheck
    python3 scripts/drop_tests/score.py [samples/drop-tests]

The folder holds the audio and `labels.tsv` (tab-separated: file, first beat of bar 1, comma-separated drop times,
notes; times in seconds or m:ss.s). Both stay out of git. Fill the times in BEFORE looking at the app's grid, and
never move a label to fit the grid: labels snapped to a grid that was half a beat out hid that error for a while.

For every file: how far our bar 1 is from yours, and which of your drops have one of our Drop sections within 0.6 of a
beat. At the end: bar 1 right on N files, drops found, and how many of our Drop flags are drops you named.
"""
import os
import re
import subprocess
import sys

folder = sys.argv[1] if len(sys.argv) > 1 else "samples/drop-tests"
barcheck = "./target/release/examples/barcheck"


def secs(t):
    t = t.strip()
    if ":" in t:
        m, s = t.split(":")
        return int(m) * 60 + float(s)
    return float(t)


rows = []
for line in open(os.path.join(folder, "labels.tsv"), encoding="utf-8", errors="replace"):
    if line.startswith("#") or not line.strip():
        continue
    c = line.rstrip("\n").split("\t")
    if len(c) < 3 or not c[1].strip():
        continue
    rows.append((c[0], secs(c[1]), [secs(x) for x in c[2].split(",") if x.strip()]))

bar_ok = found = named = flags = 0
for name, bar1, drops in rows:
    out = subprocess.run([barcheck, os.path.join(folder, name)], capture_output=True, text=True).stdout
    bpm = re.search(r"([\d.]+) BPM", out)
    down = re.search(r"new downbeat (-?[\d.]+) s", out)
    if not (bpm and down):
        print(f"no verdict: {name}")
        continue
    beat = 60 / float(bpm.group(1))
    ours = [
        float(a)
        for a, k in re.findall(r"^\s+([\d.]+)-\s*[\d.]+ s\s+bar\s+-?\d+\s+\d+ beats\s+(\w+)", out, re.M)
        if k == "Drop"
    ]
    d = (bar1 - float(down.group(1))) / beat
    hit = [t for t in drops if any(abs(o - t) <= 0.6 * beat for o in ours)]
    bar_ok += abs(d) < 0.4
    found += len(hit)
    named += len(drops)
    flags += len(ours)
    missed = [round(t, 1) for t in drops if t not in hit]
    print(f"{name[:44]:44} bar 1 {d:+5.1f} beats from yours   drops {len(hit)}/{len(drops)}  our flags {len(ours)}"
          + (f"   missed {missed}" if missed else ""))
n = len(rows)
print(f"\nbar 1 within 0.4 beat of yours: {bar_ok}/{n}")
print(f"your drops found: {found}/{named}; our Drop flags: {flags} ({100 * found / max(flags, 1):.0f}% are drops you named)")
