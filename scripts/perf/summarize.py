"""Prints one line per run: the mean stream rate, the game's frame rate and the latency stages of the
last three 5-second stats windows after "Game connected".

Usage: summarize.py perf-LABEL.log LABEL"""
import json
import re
import sys

lines = open(sys.argv[1]).read().splitlines()
start = next((i for i, l in enumerate(lines) if "Game connected" in l), 0) + 1
fps, lat, game = [], [], []
for line in lines[start:]:
    m = re.search(r"\] ([\d.]+) fps \((\d+) game frames\)", line)
    if m:
        fps.append(float(m.group(1)))
        game.append(int(m.group(2)) / 5)
    if "Headset latency" in line:
        lat.append(json.loads(line[line.index("{"):]))
fps, lat, game = fps[-3:], lat[-3:], game[-3:]
keys = ["total_ms", "game_ms", "encode_ms", "network_ms", "decode_ms", "decode_queue_ms", "vsync_wait_ms", "client_fps"]


def avg(xs):
    return sum(xs) / len(xs) if xs else float("nan")


stages = " ".join("%s %.1f" % (k.replace("_ms", ""), avg([x[k] for x in lat])) for k in keys)
print(sys.argv[2], "fps %.0f game %.0f min %.0f" % (avg(fps), avg(game), min(game, default=0)), stages)
