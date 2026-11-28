#!/usr/bin/env python3
"""Measures the message of a game's loading screen in a screen capture of the headset (scripts/screen-check.sh).

Usage: scripts/screen-color.py CAPTURE.png NAME   prints "NAME: N message pixels at X,Y": the pixels of the
                                                 message's color (the client's WARN_COLOR, orange) in the
                                                 left eye and their middle
       scripts/screen-color.py --verdict COLORS.txt
                                                 reads the lines of the captures none, shown, turned and
                                                 hidden and exits 1 unless the message shows in shown,
                                                 stands more to the left in turned and is gone in none
                                                 and hidden

macOS's `sips` turns the PNG into a BMP, which the script reads. It looks at every second pixel of every
second row.
"""
import pathlib
import re
import struct
import subprocess
import sys
import tempfile

STEP = 2
# The message shows when this many pixels of its color are in the left eye
SHOWN_AT_LEAST = 200


def message_pixels(png):
    with tempfile.TemporaryDirectory() as folder:
        bmp = pathlib.Path(folder) / "capture.bmp"
        subprocess.run(["sips", "-s", "format", "bmp", str(png), "--out", str(bmp)], check=True, capture_output=True)
        data = bmp.read_bytes()
    offset = struct.unpack_from("<I", data, 10)[0]
    width, height = struct.unpack_from("<ii", data, 18)
    size = struct.unpack_from("<H", data, 28)[0] // 8
    stride = (width * size + 3) // 4 * 4
    bottom_up = height > 0
    height = abs(height)
    count, xs, ys = 0, 0, 0
    for y in range(0, height, STEP):
        row = height - 1 - y if bottom_up else y
        for x in range(0, width // 2, STEP):
            start = offset + row * stride + x * size
            b, g, r = data[start], data[start + 1], data[start + 2]
            # The message's orange: a strong red, a middle green and little blue
            if r > 150 and r - b > 90 and 0.45 * r < g < 0.95 * r:
                count += 1
                xs += x
                ys += y
    return count, (xs // count, ys // count) if count else None


def verdict(path):
    seen = {}
    for line in pathlib.Path(path).read_text().splitlines():
        found = re.match(r"(\w+): (\d+) message pixels(?: at (\d+),(\d+))?", line)
        if found:
            seen[found.group(1)] = (int(found.group(2)), int(found.group(3)) if found.group(3) else None)
    missing = [name for name in ("none", "shown", "turned", "hidden") if name not in seen]
    if missing:
        print("verdict: no capture of " + ", ".join(missing))
        return 1
    shown, turned = seen["shown"], seen["turned"]
    checks = {
        "the message shows": shown[0] >= SHOWN_AT_LEAST,
        "no message before the screen": seen["none"][0] < shown[0] // 10,
        "no message after the screen": seen["hidden"][0] < shown[0] // 10,
        "the turned message stands more to the left": turned[0] >= SHOWN_AT_LEAST // 4 and turned[1] is not None and turned[1] < shown[1],
    }
    for name, passed in checks.items():
        print("verdict: %s: %s" % (name, "yes" if passed else "NO"))
    return 0 if all(checks.values()) else 1


def main():
    if sys.argv[1] == "--verdict":
        sys.exit(verdict(sys.argv[2]))
    count, middle = message_pixels(sys.argv[1])
    print("%s: %d message pixels%s" % (sys.argv[2], count, " at %d,%d" % middle if middle else ""))


if __name__ == "__main__":
    main()
