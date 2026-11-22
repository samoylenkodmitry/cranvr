"""Reads the client's log of a hand menu check (scripts/menu-check.sh) and prints numbers.

usage: scripts/menu-check.py LOGCAT [HOST_LOG]

For each line the time counts from the client's line "scripted menu check" (or from its first line
of the menu for a run without a script). The script prints:
- each move of the menu between the hands and the view, with the time;
- each opening and closing of the menu and what asked for it;
- each ray that entered the menu or its ball, with the point that it hit and its distance in mm
  from the middle that the script aimed at (a dp is a millimeter);
- each change of the line under the ball;
- the panels that the app composed, and the host's lines about the menu.
"""
import re
import sys
from datetime import datetime

TIME = re.compile(r"^(\d\d-\d\d \d\d:\d\d:\d\d\.\d\d\d)")
CHECK = re.compile(r"scripted menu check with (\w+): a menu (\d+) dp tall")
ENTER = re.compile(r"(the left|the right|a) pointer enters the hand (menu|ball) at (-?\d+), (-?\d+) dp")
MOVE = re.compile(r"the hand menu moves (.*?): the left controller is (\w+), the right one (\w+)")
OPEN = re.compile(r"(a button (opens|closes) the hand menu|the hand menu (opens|closes)\b.*|the Mac opens the hand menu.*|the Mac asks to open the hand menu|the hand menu closes: .*)")
HINT = re.compile(r"(the menu hint (shows|goes).*)")
LAYERS = re.compile(r"layers composed by the app: (\d+) panels and (\d+) display surfaces")
# The menu is 470 dp wide, and the ball 50 dp
MENU_WIDTH, BALL = 470, 50


def stamp(line):
    m = TIME.match(line)
    return datetime.strptime("2026-" + m.group(1), "%Y-%m-%d %H:%M:%S.%f") if m else None


events = []
height = None
start = None
with open(sys.argv[1], errors="replace") as log:
    for line in log:
        at = stamp(line)
        if at is None:
            continue
        m = CHECK.search(line)
        if m:
            start, height = at, int(m.group(2))
            events.append((at, f"script: {m.group(1)}, a menu {height} dp tall"))
            continue
        m = ENTER.search(line)
        if m:
            x, y = int(m.group(3)), int(m.group(4))
            if m.group(2) == "menu" and height:
                aim = (MENU_WIDTH / 2, height / 2)
            else:
                aim = (BALL / 2, BALL / 2)
            off = ((x - aim[0]) ** 2 + (y - aim[1]) ** 2) ** 0.5
            events.append((at, f"{m.group(1)} ray enters the {m.group(2)} at {x}, {y} dp: {off:.0f} mm from its aimed middle"))
            continue
        for pattern in (MOVE, OPEN, HINT, LAYERS):
            m = pattern.search(line)
            if m:
                events.append((at, m.group(0)))
                if start is None and pattern is not LAYERS:
                    start = at
                break
if start is None and events:
    start = events[0][0]
for at, text in events:
    print(f"{(at - start).total_seconds():7.2f} s  {text}")
if len(sys.argv) > 2:
    with open(sys.argv[2], errors="replace") as host:
        for line in host:
            if "hand menu" in line or "Headset connected" in line or "connected, " in line:
                print("host: " + line.strip()[:200])
