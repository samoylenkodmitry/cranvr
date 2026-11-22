#!/bin/zsh
# Builds cranvr's driver for SteamVR (steamvr/driver_cranvr.cpp) with the MinGW toolchain of the game support and
# lays out the folder that SteamVR loads: driver.vrdrivermanifest, bin/win64/driver_cranvr.dll and
# resources/settings/default.vrsettings.
#
# Usage: scripts/build-steamvr-driver.sh [OUT_DIR]   (default: target/steamvr-driver/cranvr)
#
# Needs: brew install mingw-w64. scripts/package-game-support.sh takes the folder into the archive, and
# scripts/setup-wine.sh installs it as C:\cranvr\steamvr in the prefix.

set -euo pipefail

cranvr=${0:A:h:h}
out=${1:-${cranvr}/target/steamvr-driver/cranvr}
out=${out:A}
src=${cranvr}/steamvr

if ! command -v x86_64-w64-mingw32-g++ >/dev/null; then
    print -u2 "Missing x86_64-w64-mingw32-g++: brew install mingw-w64"
    exit 1
fi

mkdir -p "${out}/bin/win64" "${out}/resources/settings"
# A static C++ runtime: vrserver.exe has no MinGW DLLs next to it
x86_64-w64-mingw32-g++ -std=c++17 -O2 -Wall -Wextra -Wno-unused-parameter -Werror -shared \
    -static -static-libgcc -static-libstdc++ -I"${src}/openvr" \
    -o "${out}/bin/win64/driver_cranvr.dll" "${src}/driver_cranvr.cpp" -ld3d11 -ldxgi -lws2_32
cp "${src}/driver.vrdrivermanifest" "${out}/driver.vrdrivermanifest"
cp "${src}/resources/settings/default.vrsettings" "${out}/resources/settings/default.vrsettings"
print "SteamVR driver: ${out}"
