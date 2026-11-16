#!/bin/zsh
# Builds the Wine stack that runs Windows OpenXR games for cranvr-host:
#   - Wine 11.10 (Gcenx build), cloned from a BasaltVR checkout
#   - the BasaltVR DXMT fork, which shares each game image with the host as an IOSurface
#   - the BasaltVR OpenXR runtime (C:\basalt\runtime), registered as the active runtime
#
# The BasaltVR checkout must have run, in this order:
#   runtime/scripts/test-dxmt.zsh          (downloads Wine 11.10 and stock DXMT)
#   runtime/scripts/build-dxmt-fork.zsh    (builds the fork)
#   cmake -S runtime --preset mingw-release && cmake --build runtime/build
#
# Usage: scripts/setup-wine.sh BASALTVR_DIR
# The stack goes to $CRANVR_HOME/wine (default: ~/Library/Application Support/cranvr/wine).
# Run it again to update the stack; the prefix and its installed games stay.

set -euo pipefail

if (( $# != 1 )); then
    print -u2 "usage: scripts/setup-wine.sh BASALTVR_DIR"
    exit 2
fi
basalt=${1:A}/runtime
home=${CRANVR_HOME:-$HOME/Library/Application Support/cranvr}
stack=${home}/wine
source_wine="${basalt}/build/dxmt/toolchain/wine-11.10/Wine Devel.app/Contents/Resources/wine"
fork=${basalt}/build/dxmt/fork-install
runtime_bin=${basalt}/build/bin

for f in "${source_wine}/bin/wine" "${fork}/x86_64-windows/d3d11.dll" "${fork}/x86_64-unix/winemetal.so" \
         "${runtime_bin}/basalt_openxr.dll" "${runtime_bin}/loader_smoke.exe"; do
    if [[ ! -e ${f} ]]; then
        print -u2 "Missing ${f}. Build BasaltVR first (see the top of this script)."
        exit 1
    fi
done
if ! arch -x86_64 /usr/bin/true 2>/dev/null; then
    print -u2 "Rosetta 2 is missing: softwareupdate --install-rosetta"
    exit 1
fi

mkdir -p "${stack}"
wine_root=${stack}/engine
prefix=${stack}/prefix

# The engine: an APFS clone of the pinned Wine, so it costs no disk space until changed
if [[ ! -x ${wine_root}/bin/wine ]]; then
    print "Cloning Wine 11.10 into ${wine_root}"
    cp -cR "${source_wine}" "${wine_root}"
fi
for dll in d3d10core.dll d3d11.dll dxgi.dll winemetal.dll; do
    cp "${fork}/x86_64-windows/${dll}" "${wine_root}/lib/wine/x86_64-windows/${dll}"
done
cp "${fork}/x86_64-unix/winemetal.so" "${wine_root}/lib/wine/x86_64-unix/winemetal.so"

export WINEPREFIX=${prefix} WINEARCH=win64 WINEDEBUG=-all
if [[ ! -d ${prefix}/drive_c/windows/system32 ]]; then
    print "Creating the Wine prefix ${prefix}"
    "${wine_root}/bin/wineboot" -u >/dev/null 2>&1
fi
# wineboot copies DLLs into system32 only when it creates the prefix; refresh them every run
for dll in d3d10core.dll d3d11.dll dxgi.dll winemetal.dll; do
    cp "${fork}/x86_64-windows/${dll}" "${prefix}/drive_c/windows/system32/${dll}"
done

runtime_dir=${prefix}/drive_c/basalt/runtime
mkdir -p "${runtime_dir}"
cp "${runtime_bin}/basalt_openxr.dll" "${runtime_bin}/basalt_openxr.json" "${runtime_bin}/loader_smoke.exe" "${runtime_dir}/"
for extra in d3d11_runtime_smoke.exe steamwebhelper_compat.exe; do
    if [[ -e ${runtime_bin}/${extra} ]]; then
        cp "${runtime_bin}/${extra}" "${runtime_dir}/"
    fi
done
"${wine_root}/bin/wine" reg.exe add 'HKLM\Software\Khronos\OpenXR\1' \
    /v ActiveRuntime /t REG_SZ /d 'C:\basalt\runtime\basalt_openxr.json' /f >/dev/null

# A crash should end the program, not wait in Wine's debugger
"${wine_root}/bin/wine" reg.exe add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion\AeDebug' \
    /v Debugger /t REG_SZ /d '' /f >/dev/null

print "Checking the OpenXR runtime through the Khronos loader"
(cd "${runtime_dir}" && DXMT_BASALT_IOSURFACE=1 BASALT_EXPECT_D3D11_EXTENSION=1 "${wine_root}/bin/wine" ./loader_smoke.exe >/dev/null 2>&1) || { print -u2 "FAIL: loader_smoke.exe (run it by hand for details)"; exit 1; }
print "Wine stack ready: ${stack}"
print "Install games into the prefix, then start them with scripts/run-game.sh"
