#!/bin/zsh
# Builds the OpenXR API layer of scripts/perf/xr_frame_timing.c with MinGW-w64 into target/perf/xr-frame-timing,
# registers it in the Wine prefix as an explicit layer and prints the variable that turns it on for one game
# run. An explicit layer loads only in a process whose XR_ENABLE_API_LAYERS names it, so other runs do not
# see it. Each 600 frames the layer writes one line to the game's stderr, which scripts/e2e.sh keeps in
# game.log of its run:
#
#   eval "$(scripts/perf/xr-frame-timing.sh)"
#   scripts/e2e.sh --native-port 9961 --wine 'C:\...\Game.exe'
#   grep 'cranvr frame timing' target/e2e/run-*/game.log
#   scripts/perf/xr-frame-timing.sh --remove
#
# The loader under Wine ignores XR_API_LAYER_PATH: Wine's processes run at a high integrity level, and the
# OpenXR loader then reads no path from the environment. So the layer goes into the prefix's registry
# (HKLM\Software\Khronos\OpenXR\1\ApiLayers\Explicit). --remove takes it out again.
#
# Environment: CRANVR_HOME (default ~/Library/Application Support/cranvr) holds the Wine stack.
# Needs: brew install mingw-w64

set -euo pipefail

root=${0:A:h:h:h}
home=${CRANVR_HOME:-$HOME/Library/Application Support/cranvr}
export WINEPREFIX=${home}/wine/prefix WINEDEBUG=-all
wine=${home}/wine/engine/bin/wine
out=${root}/target/perf/xr-frame-timing
manifest="Z:${out//\//\\}\\cranvr_frame_timing.json"
key='HKLM\Software\Khronos\OpenXR\1\ApiLayers\Explicit'

if [[ ${1:-} == --remove ]]; then
    "${wine}" reg delete "${key}" /v "${manifest}" /f /reg:64 >/dev/null 2>&1 || true
    # The keys that the layer's entry made go too, when nothing else is in them
    for k in "${key}" "${key%\\*}"; do
        if [[ $("${wine}" reg query "${k}" /s /reg:64 2>/dev/null | tr -d '\r' | grep -cv '^$') == 1 ]]; then
            "${wine}" reg delete "${k}" /f /reg:64 >/dev/null 2>&1 || true
        fi
    done
    print -u2 "The layer is out of the registry of ${WINEPREFIX}"
    exit 0
fi

mkdir -p "${out}"
x86_64-w64-mingw32-gcc -O2 -shared -static-libgcc -o "${out}/cranvr_frame_timing.dll" "${root}/scripts/perf/xr_frame_timing.c"
# A library path without a folder goes to Windows' own search for DLLs, so the manifest names the whole path
library="Z:${out//\//\\\\}\\\\cranvr_frame_timing.dll"
cat > "${out}/cranvr_frame_timing.json" <<JSON
{
    "file_format_version": "1.0.0",
    "api_layer": {
        "name": "XR_APILAYER_CRANVR_frame_timing",
        "library_path": "${library}",
        "api_version": "1.0",
        "implementation_version": "1",
        "description": "Times xrWaitFrame, xrBeginFrame and xrEndFrame of a game for cranvr's tests"
    }
}
JSON
"${wine}" reg add "${key}" /v "${manifest}" /t REG_DWORD /d 0 /f /reg:64 >/dev/null 2>&1
print -u2 "The layer is in the registry of ${WINEPREFIX}; scripts/perf/xr-frame-timing.sh --remove takes it out"
print -r -- "export XR_ENABLE_API_LAYERS=XR_APILAYER_CRANVR_frame_timing"
