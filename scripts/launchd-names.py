#!/usr/bin/env python3
"""Counts the names that processes of this user registered with launchd, with the IOSurfaces behind DXMT's names.

Usage: python3 -I scripts/launchd-names.py [--pids PID,...] [--save FILE] [--since FILE] [--list]

launchd keeps a name that a process registered (bootstrap_register) until the port under it dies. DXMT registered
the port of each shared texture and each shared event under a name "DXMT_shared_resource_..." (or
"..._keyed_mutex"). Those ports belong to the kernel and never die. launchd kept them, with the memory of the
textures, until the user logged out. Since cranvr's DXMT patch 0020 the port under such a name belongs to the
process that serves the object. The name goes with the object or the process.

`launchctl print gui/UID` lists the names under "unmanaged processes", one entry for each process, alive or not,
that still has a name. The script looks each DXMT name up and asks IOSurface whether the port is a surface's.

It prints one line: "names N dxmt D dead D2 surfaces S bytes B order O" for all names, or for the processes of --pids:
  names     the names of those processes ("dynamic endpoints")
  dxmt      the names of DXMT among them
  dead      the names of processes that have ended
  surfaces  the DXMT names whose port is an IOSurface, then the bytes of those surfaces
  order     the DXMT names "DXMT_order_..." among them: since cranvr's DXMT patch 0022 each command queue that holds a
            shared texture serves its two events under such names while it lives (dxmt_shared_order.hpp)
--save FILE keeps the list of (process, name). --since FILE also prints "new N gone G": the names that came and went
since that list. --list prints each name with its process and its surface's bytes.
"""

import ctypes
import json
import os
import re
import subprocess
import sys


def parse(argv):
    opts = {"pids": None, "save": None, "since": None, "list": False}
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--pids" and i + 1 < len(argv):
            opts["pids"] = {int(p) for p in argv[i + 1].split(",") if p}
            i += 2
        elif arg in ("--save", "--since") and i + 1 < len(argv):
            opts[arg[2:]] = argv[i + 1]
            i += 2
        elif arg == "--list":
            opts["list"] = True
            i += 1
        else:
            sys.exit(__doc__)
    return opts


def names_of_launchd():
    """(pid, process name, endpoint name) of each dynamic endpoint of an unmanaged process"""
    out = subprocess.run(["launchctl", "print", "gui/%d" % os.getuid()], capture_output=True, text=True).stdout
    found = []
    process = None
    section = None
    for line in out.splitlines():
        m = re.match(r"^\t\tcom\.apple\.xpc\.launchd\.unmanaged\.(.+)\.(\d+) = \{$", line)
        if m:
            process = (int(m.group(2)), m.group(1))
            section = None
            continue
        if process is None:
            continue
        if re.match(r"^\t\t\}$", line):
            process = None
        elif re.match(r"^\t\t\t[a-z -]*endpoints = \{$", line):
            section = line.strip().split(" =")[0]
        elif section == "dynamic endpoints":
            m = re.match(r'^\t\t\t\t"(.+)" = \{$', line)
            if m:
                found.append((process[0], process[1], m.group(1)))
    return found


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass
    return True


class Surfaces:
    def __init__(self):
        system = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
        self.iosurface = ctypes.CDLL("/System/Library/Frameworks/IOSurface.framework/IOSurface")
        self.cf = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
        self.bootstrap_port = ctypes.c_uint.in_dll(system, "bootstrap_port")
        self.task = ctypes.c_uint.in_dll(system, "mach_task_self_")
        self.look_up = system.bootstrap_look_up
        self.look_up.argtypes = [ctypes.c_uint, ctypes.c_char_p, ctypes.POINTER(ctypes.c_uint)]
        self.look_up.restype = ctypes.c_int
        self.deallocate = system.mach_port_deallocate
        self.deallocate.argtypes = [ctypes.c_uint, ctypes.c_uint]
        self.iosurface.IOSurfaceLookupFromMachPort.argtypes = [ctypes.c_uint]
        self.iosurface.IOSurfaceLookupFromMachPort.restype = ctypes.c_void_p
        self.iosurface.IOSurfaceGetAllocSize.argtypes = [ctypes.c_void_p]
        self.iosurface.IOSurfaceGetAllocSize.restype = ctypes.c_size_t
        self.cf.CFRelease.argtypes = [ctypes.c_void_p]

    def bytes_of(self, name):
        """The bytes of the IOSurface under the name, or None for another kind of port"""
        port = ctypes.c_uint(0)
        if self.look_up(self.bootstrap_port.value, name.encode(), ctypes.byref(port)) != 0:
            return None
        try:
            surface = self.iosurface.IOSurfaceLookupFromMachPort(port.value)
            if not surface:
                return None
            size = self.iosurface.IOSurfaceGetAllocSize(surface)
            self.cf.CFRelease(surface)
            return size
        finally:
            self.deallocate(self.task.value, port.value)


def main():
    opts = parse(sys.argv[1:])
    entries = names_of_launchd()
    if opts["pids"] is not None:
        entries = [e for e in entries if e[0] in opts["pids"]]
    surfaces = Surfaces()
    count = len(entries)
    dxmt = dead = surface_count = surface_bytes = order = 0
    lines = []
    for pid, process, name in entries:
        is_dxmt = name.startswith("DXMT_")
        dxmt += is_dxmt
        order += name.startswith("DXMT_order_")
        dead += not alive(pid)
        size = surfaces.bytes_of(name) if is_dxmt else None
        if size is not None:
            surface_count += 1
            surface_bytes += size
        if opts["list"]:
            lines.append("%d %s %s %s %s" % (pid, process, "alive" if alive(pid) else "dead", name,
                                             "-" if size is None else size))
    result = "names %d dxmt %d dead %d surfaces %d bytes %d order %d" % (count, dxmt, dead, surface_count, surface_bytes,
                                                                         order)
    if opts["since"]:
        with open(opts["since"]) as f:
            before = {(p, n) for p, n in json.load(f)}
        now = {(p, n) for p, _, n in entries}
        result += " new %d gone %d" % (len(now - before), len(before - now))
    if opts["save"]:
        with open(opts["save"], "w") as f:
            json.dump(sorted((p, n) for p, _, n in entries), f)
    print(result)
    for line in lines:
        print(line)


main()
