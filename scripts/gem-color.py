#!/usr/bin/env python3
"""Measures the gem that records in a screen capture of the headset (scripts/gem-check.sh).

Usage: scripts/gem-color.py --find CAPTURE.png [BEFORE.png]
                                              prints X,Y: the middle of the green pixels in the left
                                              eye, where a pointed gem stands. With BEFORE, a capture
                                              of the gem before the ray pointed at it, only the
                                              pixels that turned green count.
       scripts/gem-color.py CAPTURE.png [X,Y]     prints the mean color of the lit pixels within 30
                                                  pixels of X,Y, its light (luma) and what it looks
                                                  like

       scripts/gem-color.py --verdict COLORS.txt THEME
                                              reads the lines that the second form printed for the
                                              captures of scripts/gem-looks.sh and exits 1 unless
                                              the idle gem has the theme's hue (violet for amber,
                                              blue for cyan), the pointed gem is green, the
                                              recording gem red and the blocked gem the theme's hue
                                              at no more than 60% of the idle gem's light

macOS's `sips` turns the PNG into a BMP, which the script reads.
"""
import pathlib
import re
import struct
import subprocess
import sys
import tempfile

BOX = 30


def pixels(png):
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

    def at(x, y):
        row = height - 1 - y if bottom_up else y
        start = offset + row * stride + x * size
        b, g, r = data[start], data[start + 1], data[start + 2]
        return r, g, b

    return width, height, at


def green(r, g, b):
    return g > 150 and g - r > 70 and g - b > 40


def find(png, before=None):
    width, height, at = pixels(png)
    was = pixels(before)[2] if before else None
    xs, ys = [], []
    for y in range(0, height, 2):
        for x in range(0, width // 2, 2):
            if green(*at(x, y)) and not (was and green(*was(x, y))):
                xs.append(x)
                ys.append(y)
    if len(xs) < 20:
        return None
    xs.sort()
    ys.sort()
    return xs[len(xs) // 2], ys[len(ys) // 2]


def look(r, g, b):
    """What the mean color of the gem's lit pixels looks like. The bright core of a gem clips to
    white, so the mean says more of its hue than its brightest pixels."""
    if r > g + 50 and r > b + 50:
        return "red"
    if g > r + 40 and g > b + 10:
        return "green"
    if b > g and r > g:
        return "violet"
    if b > r:
        return "blue"
    return "other"


# The blocked gem's light may reach this share of the idle gem's light at most
DIM_AT_MOST = 0.6


def verdict(path, theme):
    glass = {"amber": "violet", "cyan": "blue"}[theme]
    looks = {}
    for line in pathlib.Path(path).read_text().splitlines():
        found = re.match(r"(gem-[a-z-]+)\.png: .* \(light (\d+)\), .*: (\w+)$", line.strip())
        if found:
            looks[found[1]] = (found[3], int(found[2]))
    wants = [
        ("gem-idle", glass),
        ("gem-pointed", "green"),
        ("gem-recording", "red"),
        ("gem-recording-later", "red"),
        ("gem-blocked", glass),
        ("gem-blocked-press", glass),
    ]
    fails = []
    for name, hue in wants:
        seen = looks.get(name, ("missing", 0))[0]
        if seen != hue:
            fails.append(f"{name} is {seen}, not {hue}")
    idle = looks.get("gem-idle", (None, 0))[1]
    for name in ("gem-blocked", "gem-blocked-press"):
        light = looks.get(name, (None, 0))[1]
        if idle and light > DIM_AT_MOST * idle:
            fails.append(f"{name} has {light / idle:.0%} of the idle gem's light, more than {DIM_AT_MOST:.0%}")
        elif idle:
            print(f"{name} has {light / idle:.0%} of the idle gem's light")
    pulse = [looks.get(name, (None, 0))[1] for name in ("gem-recording", "gem-recording-later")]
    if all(pulse):
        print(f"the recording gem's light in two captures about 0.8 s apart: {pulse[0]} and {pulse[1]}")
    for fail in fails:
        print(f"the looks fail: {fail}")
    if not fails:
        print("the looks pass")
    return 1 if fails else 0


def main():
    if sys.argv[1] == "--verdict":
        sys.exit(verdict(sys.argv[2], sys.argv[3]))
    if sys.argv[1] == "--find":
        place = find(sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
        if place:
            print(f"{place[0]},{place[1]}")
        return
    png = sys.argv[1]
    if len(sys.argv) < 3 or "," not in sys.argv[2]:
        print(f"{pathlib.Path(png).name}: no place of the gem")
        return
    cx, cy = (int(v) for v in sys.argv[2].split(","))
    width, height, at = pixels(png)
    lit = []
    for y in range(max(0, cy - BOX), min(height, cy + BOX)):
        for x in range(max(0, cx - BOX), min(width, cx + BOX)):
            r, g, b = at(x, y)
            if max(r, g, b) > 24:
                lit.append((r, g, b))
    if not lit:
        print(f"{pathlib.Path(png).name}: nothing lit at {cx},{cy}")
        return
    mean = [sum(p[i] for p in lit) / len(lit) for i in range(3)]
    brightest = sorted(lit, key=lambda p: -max(p))[: max(1, len(lit) // 5)]
    top = [sum(p[i] for p in brightest) / len(brightest) for i in range(3)]
    light = 0.2126 * mean[0] + 0.7152 * mean[1] + 0.0722 * mean[2]
    print(f"{pathlib.Path(png).name}: {len(lit)} lit pixels at {cx},{cy}, mean rgb {mean[0]:.0f},{mean[1]:.0f},{mean[2]:.0f} (light {light:.0f}), brightest fifth {top[0]:.0f},{top[1]:.0f},{top[2]:.0f}: {look(*mean)}")


if __name__ == "__main__":
    main()
