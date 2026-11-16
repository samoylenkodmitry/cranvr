#!/usr/bin/env python3
"""Turns the log of one run of cranvr's window display into a JSON report.

Usage: e2e-flat-report.py LOGCAT HOST_STDOUT REPORT

The app logs a line `cranvr-flat: {...}` every 2 seconds with the 10th, 50th and 90th percentile
of each latency number over those seconds. The report keeps every line. Its summary holds the
median over the lines of each percentile, without the first two lines, which hold the start.
The host's stats lines give the host's frame rate and encode time.
"""

import json
import re
import statistics
import sys

NUMBERS = ["round_trip_ms", "motion_to_display_ms", "swap_to_display_ms", "render_ms", "pose_age_ms"]
RATES = ["displayed_fps", "new_frame_fps"]


def app_lines(path):
    lines = []
    for line in open(path, encoding="utf-8", errors="replace"):
        match = re.search(r"cranvr-flat: (\{.*\})", line)
        if match:
            lines.append(json.loads(match.group(1)))
    return lines


def host_lines(path):
    lines = []
    for line in open(path, encoding="utf-8", errors="replace"):
        if line.startswith("@cranvr "):
            value = json.loads(line[len("@cranvr "):])
            if value.get("type") == "stats":
                lines.append(value)
    return lines


def median(values):
    values = [v for v in values if v is not None]
    return round(statistics.median(values), 2) if values else None


def main():
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    lines = app_lines(sys.argv[1])
    host = host_lines(sys.argv[2])
    steady = [line for line in lines[2:] if line.get("round_trip_ms")]
    summary = {}
    for name in NUMBERS:
        summary[name] = {p: median([(line.get(name) or {}).get(p) for line in steady]) for p in ["p10", "p50", "p90"]}
    for name in RATES:
        summary[name] = median([line.get(name) for line in steady])
    summary["host_fps"] = median([line["fps"] for line in host[1:]])
    summary["host_encode_ms"] = median([line["encode_ms"] for line in host[1:]])
    report = {"summary": summary, "app": lines, "host": host}
    with open(sys.argv[3], "w", encoding="utf-8") as f:
        json.dump(report, f, indent=2)
    for name in NUMBERS:
        s = summary[name]
        print(f"  {name:22} p10 {s['p10']}  p50 {s['p50']}  p90 {s['p90']}")
    for name in RATES + ["host_fps", "host_encode_ms"]:
        print(f"  {name:22} {summary[name]}")
    print(f"{len(steady)} steady lines of {len(lines)}; report: {sys.argv[3]}")
    sys.exit(0 if steady else 1)


if __name__ == "__main__":
    main()
