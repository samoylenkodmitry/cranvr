"""Sums GPU time from an xctrace export of metal-gpu-intervals: per process and stage, and per
pass label for one process. Usage: gpu-by-pass.py EXPORT.xml SECONDS FRAMES [PROCESS-SUBSTRING]"""
import sys
import xml.etree.ElementTree as ET
from collections import defaultdict

path, seconds, frames = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
focus = sys.argv[4] if len(sys.argv) > 4 else "Beat Saber"
ids = {}


def resolve(e):
    if e is None:
        return None
    if "ref" in e.attrib:
        return ids.get(e.attrib["ref"])
    if "id" in e.attrib:
        ids[e.attrib["id"]] = e
    for child in e:
        resolve(child)
    return e


def fmt(e):
    if e is None or e.tag == "sentinel":
        return ""
    return e.attrib.get("fmt") or (e.text or "")


root = ET.parse(path).getroot()
schema = root.find(".//schema")
cols = [c.find("mnemonic").text for c in schema.findall("col")]
by_process = defaultdict(lambda: defaultdict(float))
by_label = defaultdict(lambda: [0.0, 0])
focus_frames = set()
stage_of_label = {}
for row in root.iter("row"):
    cells = dict(zip(cols, [resolve(c) for c in row]))
    duration = float(fmt(cells["duration"]) and cells["duration"].text or 0) / 1e6  # ms
    channel = fmt(cells.get("channel-name"))
    process = fmt(cells.get("process"))
    by_process[process][channel] += duration
    if focus in process:
        focus_frames.add(fmt(cells.get("frame-number")))
        label = fmt(cells.get("event-label")) or "(no label)"
        key = (channel, label[:70])
        by_label[key][0] += duration
        by_label[key][1] += 1

if frames <= 0:
    frames = max(len(focus_frames), 1)
print(f"GPU time in {seconds:.0f} s, ms per frame at {frames:.0f} frames:")
for process, channels in sorted(by_process.items(), key=lambda p: -sum(p[1].values()))[:8]:
    total = sum(channels.values())
    parts = ", ".join(f"{c} {v / frames:.2f}" for c, v in sorted(channels.items(), key=lambda x: -x[1]))
    print(f"  {process[:40]:40s} {total / seconds / 10:5.1f}% of wall time  per frame: {parts}")
if frames <= 0:
    frames = max(len(focus_frames), 1)
    print(f"({focus}: {frames:.0f} frames in the trace)")
print(f"\nPasses of '{focus}' by GPU time (ms per frame, count per frame):")
total = sum(v[0] for v in by_label.values())
for (channel, label), (ms, n) in sorted(by_label.items(), key=lambda x: -x[1][0])[:30]:
    print(f"  {ms / frames:6.3f}  {100 * ms / total:5.1f}%  {n / frames:5.2f}  {channel:9s} {label}")
