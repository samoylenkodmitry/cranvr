#!/usr/bin/env python3
"""Checks the warp and the head pose of cranvr's window display from two screenshots.

Usage: flat-warp-check.py STRAIGHT.png TURNED.png YAW_DEG [--world]

STRAIGHT is a screenshot of the test scene as it is. The host draws a red badge in the left eye
and a blue one in the right eye at fixed view angles, and a yellow line fixed in the world.

Without --world, TURNED has debug.cranvr.warp_yaw_deg set to YAW_DEG: only the warp turns the
display left, while the host draws for the true pose. Each badge must move right by YAW_DEG.

With --world, TURNED has debug.cranvr.world_yaw_deg set to YAW_DEG: the head pose turns left in
the world, for the host and for the warp. The badges must stay. The yellow line must move right
by about YAW_DEG: it runs along the floor toward the horizon, so its average place moves by the
yaw only roughly (LINE_TOLERANCE). A mirrored picture moves the line left; the first check cannot
see that.

The script turns pixel places into angles with the eye's field of view (the one in
quest/src/flat/mod.rs) and passes when every move is within MAX_ERROR_DEG of the expected one.
A phone with a camera cutout gives the window less than the screen's width: the screenshot then
has black columns beside the window. The script finds the window in STRAIGHT, where the picture
fills both eyes, and uses that place in both screenshots.
"""

import math
import struct
import sys
import zlib

# quest/src/flat/mod.rs: the tangent of half the vertical field of view of each eye
TAN_HALF_HEIGHT = 0.9
MAX_ERROR_DEG = 1.5
# The share of the turn by which the yellow line may miss it
LINE_TOLERANCE = 0.5


def read_png(path):
    data = open(path, "rb").read()
    pos, idat = 8, b""
    while pos < len(data):
        length = struct.unpack(">I", data[pos:pos + 4])[0]
        kind, chunk = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, _, color = struct.unpack(">IIBB", chunk[:10])
        elif kind == b"IDAT":
            idat += chunk
    raw = zlib.decompress(idat)
    bpp = 4 if color == 6 else 3
    stride = width * bpp
    rows, previous, i = [], bytearray(stride), 0
    for _ in range(height):
        kind, line = raw[i], bytearray(raw[i + 1:i + 1 + stride])
        i += 1 + stride
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b = previous[x]
            c = previous[x - bpp] if x >= bpp else 0
            if kind == 1:
                line[x] = (line[x] + a) & 255
            elif kind == 2:
                line[x] = (line[x] + b) & 255
            elif kind == 3:
                line[x] = (line[x] + ((a + b) >> 1)) & 255
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(bytes(line))
        previous = line
    return width, height, bpp, rows


def window_columns(image):
    """The first column of the window and the column after its last one: the columns at the edges
    with only black pixels belong to the screen around the window."""
    width, height, bpp, rows = image

    def black(x):
        return all(max(rows[y][x * bpp:x * bpp + 3]) < 10 for y in range(0, height, 4))

    left, right = 0, width
    while left < right and black(left):
        left += 1
    while right > left and black(right - 1):
        right -= 1
    return left, right


def badge_x(image, window, test, eye):
    """The badge's average x in its eye, in pixels from the eye's left edge."""
    _, height, bpp, rows = image
    left, right = window
    half = (right - left) // 2
    start = left + eye * half
    xs = [
        x
        for y in range(0, height, 2)
        for x in range(start, start + half, 2)
        if test(rows[y][x * bpp], rows[y][x * bpp + 1], rows[y][x * bpp + 2])
    ]
    return sum(xs) / len(xs) - start if xs else None


def angle(x, half, tan_half_width):
    return math.degrees(math.atan((x / half * 2 - 1) * tan_half_width))


def main():
    if len(sys.argv) not in (4, 5) or (len(sys.argv) == 5 and sys.argv[4] != "--world"):
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    straight, turned, yaw = read_png(sys.argv[1]), read_png(sys.argv[2]), float(sys.argv[3])
    world = len(sys.argv) == 5
    window = window_columns(straight)
    half = (window[1] - window[0]) // 2
    tan_half_width = TAN_HALF_HEIGHT * half / straight[1]
    if window != (0, straight[0]):
        print(f"window: columns {window[0]} to {window[1]} of {straight[0]}")
    badge = 0.0 if world else yaw
    marks = [
        ("red badge", 0, lambda r, g, b: r > 200 and g < 60 and b < 60, badge),
        ("blue badge", 1, lambda r, g, b: b > 200 and r < 60 and g < 80, badge),
    ]
    if world:
        marks.append(("yellow line", 0, lambda r, g, b: r > 170 and g > 150 and b < 110, yaw))
    ok = True
    for name, eye, test, expected in marks:
        allowed = LINE_TOLERANCE * abs(yaw) if name == "yellow line" else MAX_ERROR_DEG
        before, after = badge_x(straight, window, test, eye), badge_x(turned, window, test, eye)
        if before is None or after is None:
            print(f"{name}: not found")
            ok = False
            continue
        moved = angle(after, half, tan_half_width) - angle(before, half, tan_half_width)
        error = moved - expected
        ok &= abs(error) <= allowed
        print(f"{name}: x {before:.0f} -> {after:.0f} px, moved {moved:.2f} deg, expected {expected}, error {error:+.2f} deg")
    print("PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
