"""Prints the stream rate, the game's frame rate and the latency stages from the host's log.

Usage: summarize.py LOG LABEL [--all]
  LOG     perf-LABEL.log from game-run.sh, or host.log (or its tail) from a played session
  --all   one line per 5-second stats window after "Game connected", not only the mean of the
          last three"""
import json
import re
import sys

args = [a for a in sys.argv[1:] if a != "--all"]
every = "--all" in sys.argv
lines = open(args[0]).read().splitlines()
start = next((i for i, l in enumerate(lines) if "Game connected" in l), 0) + 1
keys = ["total_ms", "game_ms", "encode_ms", "network_ms", "decode_ms", "decode_queue_ms", "vsync_wait_ms", "client_fps"]
windows = []  # (time, stream fps, game fps, latency stats or None)
for line in lines[start:]:
    m = re.search(r"(\d\d:\d\d:\d\d)\.\d+ .*\] ([\d.]+) fps \((\d+) game frames\)", line)
    if m:
        windows.append([m.group(1), float(m.group(2)), int(m.group(3)) / 5, None])
    if "Headset latency" in line and windows:
        windows[-1][3] = json.loads(line[line.index("{"):])


def avg(xs):
    return sum(xs) / len(xs) if xs else float("nan")


def stages(lat):
    return " ".join("%s %.1f" % (k.replace("_ms", ""), avg([x[k] for x in lat])) for k in keys)


if every:
    for t, fps, game, lat in windows:
        print(t, "fps %.0f game %.0f" % (fps, game), stages([lat]) if lat else "")
    low = [w for w in windows if w[2] < 0.95 * max((w[1] for w in windows), default=0)]
    print("%d windows, %d with the game below 95%% of the stream's best rate" % (len(windows), len(low)))
else:
    last = windows[-3:]
    fps, game = [w[1] for w in last], [w[2] for w in last]
    lat = [w[3] for w in last if w[3]]
    print(args[1], "fps %.0f game %.0f min %.0f" % (avg(fps), avg(game), min(game, default=0)), stages(lat))
