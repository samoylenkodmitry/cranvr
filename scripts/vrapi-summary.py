#!/usr/bin/env python3
"""Reads a Quest's VrApi lines (one a second, from `adb logcat`) on stdin and prints the medians of
what a frame costs: the frame rate, the stale frames, the compositor's time (TW), the app's GPU time
(App), the CPU and GPU time of the frame (CPU&GPU), the GPU's busy share and the layer count."""
import re
import statistics
import sys

rows = []
for line in sys.stdin:
    fields = dict(re.findall(r"([A-Za-z&%0-9/]+)=([^,]+)", line))
    rows.append(fields)


def number(text):
    found = re.match(r"[-0-9.]+", text or "")
    return float(found.group()) if found else None


def median(key):
    values = [number(row.get(key)) for row in rows]
    values = [value for value in values if value is not None]
    return statistics.median(values) if values else float("nan")


if rows:
    fps = sorted({row.get("FPS", "?") for row in rows})
    layers = sorted({row.get("LCnt", "?").split("(")[0] for row in rows})
    stale = sum(number(row.get("Stale")) or 0 for row in rows)
    print(f"  {len(rows)} s: FPS {','.join(fps)}, stale {stale:.0f}, TW {median('TW'):.2f} ms, App {median('App'):.2f} ms, CPU&GPU {median('CPU&GPU'):.2f} ms, GPU {median('GPU%') * 100:.0f}%, LCnt {','.join(layers)}")
else:
    print("  no VrApi lines")
