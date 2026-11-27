#!/usr/bin/env python3
"""Counts the pixels of screen captures of the headset for scripts/touch-check.sh.

Usage: scripts/touch-colors.py FIRST.png [LATER.png ...]

For each capture the script prints, in the left eye (the left half of the capture), the count of lit
pixels (luma above 40), of green pixels (a gem that a pointer rests on) and of red pixels (a gem
whose recording runs). For each later capture it also prints how many pixels of the left eye differ
from the first capture by more than 40 in a channel, such as the pixels of a menu that opened. It
looks at every second pixel of every second row.

macOS's `sips` turns the PNG into a BMP, which the script reads (scripts/gem-color.py).
"""
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from importlib import import_module  # noqa: E402

pixels = import_module("gem-color").pixels
green = import_module("gem-color").green

STEP = 2


def red(r, g, b):
    return r > 150 and r - g > 80 and r - b > 80


def lit(r, g, b):
    return 0.2126 * r + 0.7152 * g + 0.0722 * b > 40


def left_eye(png):
    width, height, at = pixels(png)
    return [at(x, y) for y in range(0, height, STEP) for x in range(0, width // 2, STEP)]


def main(paths):
    first = left_eye(paths[0])
    for path in paths:
        seen = first if path == paths[0] else left_eye(path)
        counts = {
            "lit": sum(lit(*p) for p in seen),
            "green": sum(green(*p) for p in seen),
            "red": sum(red(*p) for p in seen),
        }
        line = f"{pathlib.Path(path).stem}: " + ", ".join(f"{name} {count}" for name, count in counts.items())
        if path != paths[0] and len(seen) == len(first):
            changed = sum(max(abs(a - b) for a, b in zip(one, two)) > 40 for one, two in zip(first, seen))
            line += f", changed from {pathlib.Path(paths[0]).stem} {changed} of {len(seen)}"
        print(line)


if __name__ == "__main__":
    main(sys.argv[1:])
