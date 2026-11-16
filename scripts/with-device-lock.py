#!/usr/bin/env python3
"""Runs a command while it holds the lock of one Android device.

Usage: with-device-lock.py SERIAL COMMAND [ARG]...

The lock file is the one Cranpose's device scripts use (scripts/android_robot_device.py), so a
cranvr run and a Cranpose robot or benchmark run never share a phone. The script waits for the
lock, runs the command and exits with its exit code.
"""

import fcntl
import hashlib
import subprocess
import sys
import tempfile
from pathlib import Path


def main():
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    serial, command = sys.argv[1], sys.argv[2:]
    key = hashlib.sha256(serial.encode()).hexdigest()[:16]
    path = Path(tempfile.gettempdir()) / ("cranpose-device-" + key + ".lock")
    with path.open("a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        sys.exit(subprocess.call(command))


if __name__ == "__main__":
    main()
