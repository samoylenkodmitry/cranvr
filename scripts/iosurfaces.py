#!/usr/bin/env python3
"""Counts the live global IOSurfaces of one height, in every process of this user.

Usage: python3 -I scripts/iosurfaces.py HEIGHT
Prints one line: the number of surfaces and their bytes.

A game's swapchain images under cranvr's DXMT fork are global IOSurfaces: any process looks them up by
their ID. macOS hands out IDs from the low numbers and reuses them, so the script tries the IDs from 1
until it has missed 8192 in a row after the last one it found, and at least up to 16384. That takes
about 30 ms. A test that gives its images a height that no game uses (scripts/e2e.sh --swapchain-churn
takes 333) finds its own surfaces this way, also those that launchd or another process still holds after
the test program ended.
"""

import ctypes
import sys

if len(sys.argv) != 2:
    sys.exit(__doc__)
height = int(sys.argv[1])

iosurface = ctypes.cdll.LoadLibrary("/System/Library/Frameworks/IOSurface.framework/IOSurface")
corefoundation = ctypes.cdll.LoadLibrary("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
iosurface.IOSurfaceLookup.restype = ctypes.c_void_p
iosurface.IOSurfaceLookup.argtypes = [ctypes.c_uint32]
iosurface.IOSurfaceGetHeight.restype = ctypes.c_size_t
iosurface.IOSurfaceGetHeight.argtypes = [ctypes.c_void_p]
iosurface.IOSurfaceGetAllocSize.restype = ctypes.c_size_t
iosurface.IOSurfaceGetAllocSize.argtypes = [ctypes.c_void_p]
corefoundation.CFRelease.argtypes = [ctypes.c_void_p]

count = 0
size = 0
last = 0
surface_id = 1
while surface_id <= max(16384, last + 8192):
    surface = iosurface.IOSurfaceLookup(surface_id)
    if surface:
        last = surface_id
        if iosurface.IOSurfaceGetHeight(surface) == height:
            count += 1
            size += iosurface.IOSurfaceGetAllocSize(surface)
        corefoundation.CFRelease(surface)
    surface_id += 1
print(count, size)
