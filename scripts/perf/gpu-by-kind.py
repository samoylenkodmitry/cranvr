"""GPU time per second by pass kind for one process, and per second for every process, from an
xctrace export of metal-gpu-intervals. Usage: gpu-by-kind.py EXPORT.xml [PROCESS-SUBSTRING]"""
import re
import sys
import xml.etree.ElementTree as ET
from collections import defaultdict

focus = sys.argv[2] if len(sys.argv) > 2 else "Beat Saber"
ids = {}


def res(e):
    if "ref" in e.attrib:
        return ids.get(e.attrib["ref"])
    if "id" in e.attrib:
        ids[e.attrib["id"]] = e
    for c in e:
        res(c)
    return e


def fmt(e):
    return "" if e is None else (e.attrib.get("fmt") or e.text or "")


root = ET.parse(sys.argv[1]).getroot()
cols = [c.find("mnemonic").text for c in root.find(".//schema").findall("col")]
kinds, procs = defaultdict(lambda: [0.0, 0]), defaultdict(float)
first, last = None, None
for row in root.iter("row"):
    c = dict(zip(cols, [res(x) for x in row]))
    start, ms = int(c["start"].text), int(c["duration"].text) / 1e6
    first = start if first is None else min(first, start)
    last = start if last is None else max(last, start)
    proc = re.sub(r" \(\d+\)$", "", fmt(c.get("process")))
    procs[proc] += ms
    if focus in proc:
        label = re.sub(r"\s*\(" + re.escape(focus) + r".*$", "", fmt(c.get("event-label")))
        label = re.sub(r"^Command Buffer \d+:", "", label)
        kind = fmt(c.get("channel-name")) + " " + re.sub(r" \d+$", "", label)
        kinds[kind][0] += ms
        kinds[kind][1] += 1
secs = (last - first) / 1e9
print(f"{secs:.1f} s traced; GPU stage time in ms per second:")
print("  " + ", ".join(f"{p[:24]} {ms / secs:.0f}" for p, ms in sorted(procs.items(), key=lambda x: -x[1])[:7]))
print(f"  {focus} by kind: " + ", ".join(f"{k} {v[0] / secs:.0f} ({v[1] / secs:.0f}/s)" for k, v in sorted(kinds.items(), key=lambda x: -x[1][0])[:6]))
