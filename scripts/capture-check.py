#!/usr/bin/env python3
"""Checks a screen capture of the headset before anyone sees it, then keeps or deletes it.

A capture may show the user's room through the headset's cameras. A capture of the app's virtual room
(`--es room scene`) is mostly exact black around the panels. The rule: keep a picture only when at
least 40% of its pixels are exact black and at most 15% are other unsaturated pixels (grey, as the
cameras' picture is). The script prints the two shares and the verdict. It moves a picture that
passes into KEEP_DIR when one is given and deletes every other picture at once.

Usage: scripts/capture-check.py CAPTURE.png [KEEP_DIR]

macOS's `sips` turns the PNG into a BMP, which the script reads. It looks at every third pixel of
every third row, which gives the shares to a tenth of a percent.
"""
import os
import pathlib
import struct
import subprocess
import sys
import tempfile

BLACK_AT_LEAST = 0.40
GREY_AT_MOST = 0.15
# A pixel whose channels differ by less than this share of its brightest channel counts as grey
GREY_SATURATION = 0.2
STEP = 3


def shares(png: pathlib.Path) -> tuple[float, float]:
    with tempfile.TemporaryDirectory() as folder:
        bmp = pathlib.Path(folder) / "capture.bmp"
        subprocess.run(["sips", "-s", "format", "bmp", str(png), "--out", str(bmp)], check=True, capture_output=True)
        data = bmp.read_bytes()
    offset = struct.unpack_from("<I", data, 10)[0]
    width, height = struct.unpack_from("<ii", data, 18)
    bits = struct.unpack_from("<H", data, 28)[0]
    size = bits // 8
    stride = (width * size + 3) // 4 * 4
    black = grey = seen = 0
    for row in range(0, abs(height), STEP):
        start = offset + row * stride
        line = data[start:start + width * size]
        for at in range(0, len(line) - size + 1, size * STEP):
            b, g, r = line[at], line[at + 1], line[at + 2]
            seen += 1
            top = max(r, g, b)
            if top == 0:
                black += 1
            elif top - min(r, g, b) < GREY_SATURATION * top:
                grey += 1
    return black / seen, grey / seen


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    png = pathlib.Path(sys.argv[1])
    keep = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else None
    try:
        black, grey = shares(png)
    except (subprocess.CalledProcessError, struct.error, ZeroDivisionError) as error:
        print(f"{png.name}: unreadable ({error}), deleted")
        os.remove(png)
        return 1
    passes = black >= BLACK_AT_LEAST and grey <= GREY_AT_MOST
    print(f"{png.name}: {black:.1%} exact black, {grey:.1%} other unsaturated: {'kept' if passes and keep else 'deleted'}")
    if passes and keep:
        keep.mkdir(parents=True, exist_ok=True)
        png.replace(keep / png.name)
    else:
        os.remove(png)
    return 0 if passes else 1


if __name__ == "__main__":
    sys.exit(main())
