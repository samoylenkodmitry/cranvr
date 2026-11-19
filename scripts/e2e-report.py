#!/usr/bin/env python3
"""Turns the status lines of one cranvr-host run into a JSON report.

Usage: e2e-report.py HOST_STDOUT TARGET_FPS REPORT [KEY=VALUE]...

The script writes the report to REPORT and prints its summary. The report keeps every stats line
of the run, so a slow second stays visible. Its summary holds the median and the worst value of
each number over the lines with a connected headset. KEY=VALUE pairs go into the report as they
are: the mode, the client's exit code, temperatures.

Exit code 0 means the stream ran at 90 percent of TARGET_FPS or more on median, over the lines
after the first, and the client showed frames at that rate too. With the probe as the client it
must also show 95 percent of the frames that reached its decoder. When the run passes
audio_seconds, the probe must get the Mac's sound for 80 percent of the stream time. The first line holds the time
before the stream started. Exit code 1 means it ran slower, or no headset connected.
"""

import json
import statistics
import sys

# The share of the frames that reached the client's decoder that it must show
MIN_SHOWN_RATIO = 0.95
# The share of the stream time for which the client must get the Mac's sound. The stream and the
# sound start a moment apart.
MIN_AUDIO_SHARE = 0.8


def stat_lines(path):
    connected = False
    lines = []
    with open(path, encoding="utf-8") as f:
        for raw in f:
            if not raw.startswith("@cranvr "):
                continue
            line = json.loads(raw[len("@cranvr "):])
            if line.get("type") == "headset":
                connected = line.get("connected", False)
            elif line.get("type") == "stats" and connected:
                lines.append(line)
    return lines


def summary(lines):
    numbers = {}
    for line in lines:
        for key in ("fps", "render_ms", "encode_ms"):
            numbers.setdefault(key, []).append(line[key])
        for key, value in (line.get("latency") or {}).items():
            if key != "frames":
                numbers.setdefault(f"latency.{key}", []).append(value)
    worst = {"fps": min, "latency.client_fps": min}
    return {
        key: {
            "median": round(statistics.median(values), 2),
            "worst": round(worst.get(key, max)(values), 2),
        }
        for key, values in numbers.items()
    }


def main():
    if len(sys.argv) < 4:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    path, target, out = sys.argv[1], float(sys.argv[2]), sys.argv[3]
    lines = stat_lines(path)
    report = dict(arg.split("=", 1) for arg in sys.argv[4:])
    report["target_fps"] = target
    report["summary"] = summary(lines) if lines else {}
    report["stats"] = lines
    steady = lines[1:] or lines
    report["steady_fps"] = round(statistics.median(line["fps"] for line in steady), 2) if steady else 0.0
    # The client may drop frames that the host sent: its own rate counts too. The probe's count
    # of shown frames decides where it exists.
    client_fps = report["summary"].get("latency.client_fps", {}).get("median")
    if "received" in report and "shown" in report:
        received, shown = int(report["received"]), int(report["shown"])
        report["shown_ratio"] = round(shown / received, 3) if received else 0.0
    shown_ratio = report.get("shown_ratio")
    # The probe gets the Mac's sound for about as long as it streams
    audio_ok = True
    if "audio_seconds" in report and "seconds" in report:
        audio_ok = float(report["audio_seconds"]) >= MIN_AUDIO_SHARE * float(report["seconds"])
        report["audio_ok"] = audio_ok
    report["pass"] = (
        bool(lines)
        and report["steady_fps"] >= 0.9 * target
        and (client_fps is None or client_fps >= 0.9 * target)
        and (shown_ratio is None or shown_ratio >= MIN_SHOWN_RATIO)
        and audio_ok
    )
    with open(out, "w", encoding="utf-8") as f:
        json.dump(report, f, indent=2)
    for key, value in report["summary"].items():
        print(f"  {key:28} median {value['median']:8}  worst {value['worst']:8}")
    shown = f", {report['shown_ratio']:.1%} of frames shown" if "shown_ratio" in report else ""
    audio = f", {report['audio_seconds']} s of sound" if "audio_seconds" in report else ""
    print(f"{len(lines)} stats lines, {report['steady_fps']} fps after the first{shown}{audio}, " + ("PASS" if report["pass"] else "FAIL"))
    sys.exit(0 if report["pass"] else 1)


if __name__ == "__main__":
    main()
