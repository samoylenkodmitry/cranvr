"""Checks where the headset's model of each controller stands against the real controller.

usage: scripts/desk-check.py LOG

LOG is a logcat of a build from `scripts/build-client.sh --scripted-hand` started with
`--ez log_poses true` ("scripted poses:" lines, see scripts/desk-check.sh). Both controllers must be
awake and still. The script prints, for each controller of the last line with both, the aim pose in
the frame of the grip pose and its gap to the node `laser_begin` of the runtime's model. The model
of `XR_FB_render_model` has its origin at the grip pose, and `laser_begin` marks where its ray
starts. A model at the right size and pose has `laser_begin` where the runtime puts the aim, so the
gap is a few millimeters at most.
"""
import math
import re
import sys

# `laser_begin` in the frame of the model's root, in meters, as the Quest 3's runtime 210 hands out
# "Meta Quest Touch Plus Controller Left" and "Right"
LASER_BEGIN = {"left": [0.0001, -0.0122, -0.0415], "right": [-0.0002, -0.0122, -0.0415]}


def qmul(a, b):
    ax, ay, az, aw = a
    bx, by, bz, bw = b
    return [aw * bx + ax * bw + ay * bz - az * by, aw * by - ax * bz + ay * bw + az * bx,
            aw * bz + ax * by - ay * bx + az * bw, aw * bw - ax * bx - ay * by - az * bz]


def rot(q, v):
    conj = [-q[0], -q[1], -q[2], q[3]]
    return qmul(qmul(q, [v[0], v[1], v[2], 0.0]), conj)[:3]


NUM = r"[-+0-9.eE]+"
VEC = rf"\[({NUM}), ({NUM}), ({NUM})\] \[({NUM}), ({NUM}), ({NUM}), ({NUM})\]"
line_re = re.compile(rf"left grip {VEC}; left aim {VEC}; right grip {VEC}; right aim {VEC}")
samples = []
with open(sys.argv[1], errors="replace") as log:
    for line in log:
        m = line_re.search(line)
        if m:
            v = [float(x) for x in m.groups()]
            samples.append([(v[i:i + 3], v[i + 3:i + 7]) for i in range(0, 28, 7)])
if not samples:
    sys.exit("no 'scripted poses:' line with both controllers: wake them, lay them still and run again")
print(f"{len(samples)} lines with both controllers; the last one counts")
left_grip, left_aim, right_grip, right_aim = samples[-1]
for name, (grip_at, grip_turn), (aim_at, aim_turn) in (
    ("left", left_grip, left_aim),
    ("right", right_grip, right_aim),
):
    back = [-grip_turn[0], -grip_turn[1], -grip_turn[2], grip_turn[3]]
    at = rot(back, [a - b for a, b in zip(aim_at, grip_at)])
    forward = rot(qmul(back, aim_turn), [0.0, 0.0, -1.0])
    gap = math.dist(at, LASER_BEGIN[name]) * 1000
    print(f"{name}: aim at {[round(x * 1000, 1) for x in at]} mm in the grip's frame, pointing "
          f"{[round(x, 3) for x in forward]}; laser_begin at {[round(x * 1000, 1) for x in LASER_BEGIN[name]]} mm, "
          f"the gap {gap:.1f} mm")
