#!/bin/zsh
# Packs cranvr's game support into one archive that another Mac can install without building:
# Wine 11.10 with cranvr's DXMT fork, and BasaltVR's OpenXR runtime with cranvr's patches.
#
# Usage: scripts/package-game-support.sh [OUT_DIR]
#
# It takes the files from a built tree (scripts/build-basaltvr.sh) and writes
# cranvr-game-support-macos.tar.xz and its .sha256 into OUT_DIR (default target/game-support).
# scripts/install-game-support.sh installs such an archive.
#
# The archive holds:
#   engine/      Wine 11.10 (Gcenx's macOS build), with DXMT's DLLs in place
#   dxmt/        DXMT's DLLs again, for the Wine prefix's system32
#   runtime/     the OpenXR runtime, its manifest, its checks and the Steam web helper shim
#   LICENSES/    the licenses of Wine, DXMT, BasaltVR and the OpenXR SDK
#   SOURCES.md   where each part comes from and which patches it carries

set -euo pipefail

cranvr=${0:A:h:h}
out=${1:-${cranvr}/target/game-support}
basalt=${cranvr}/third_party/basaltvr/src
runtime=${basalt}/runtime
wine_app="${runtime}/build/dxmt/toolchain/wine-11.10/Wine Devel.app/Contents/Resources/wine"
fork=${runtime}/build/dxmt/fork-install
runtime_bin=${runtime}/build/bin

for f in "${wine_app}/bin/wine" "${fork}/x86_64-windows/d3d11.dll" "${fork}/x86_64-unix/winemetal.so" \
         "${runtime_bin}/basalt_openxr.dll" "${runtime_bin}/steamwebhelper_compat.exe"; do
    if [[ ! -e ${f} ]]; then
        print -u2 "Missing ${f}. Run scripts/build-basaltvr.sh first."
        exit 1
    fi
done

stage=$(mktemp -d)
trap 'rm -rf "${stage}"' EXIT
root=${stage}/cranvr-game-support
mkdir -p "${root}/dxmt/x86_64-windows" "${root}/dxmt/x86_64-unix" "${root}/runtime" "${root}/LICENSES"

print "Copying Wine 11.10"
cp -cR "${wine_app}" "${root}/engine"
for dll in d3d10core.dll d3d11.dll dxgi.dll winemetal.dll; do
    cp "${fork}/x86_64-windows/${dll}" "${root}/engine/lib/wine/x86_64-windows/${dll}"
    cp "${fork}/x86_64-windows/${dll}" "${root}/dxmt/x86_64-windows/${dll}"
done
cp "${fork}/x86_64-unix/winemetal.so" "${root}/engine/lib/wine/x86_64-unix/winemetal.so"
cp "${fork}/x86_64-unix/winemetal.so" "${root}/dxmt/x86_64-unix/winemetal.so"

print "Copying the OpenXR runtime"
for f in basalt_openxr.dll basalt_openxr.json loader_smoke.exe d3d11_runtime_smoke.exe steamwebhelper_compat.exe; do
    cp "${runtime_bin}/${f}" "${root}/runtime/${f}"
done

cp "${basalt}/LICENSE" "${root}/LICENSES/BasaltVR-MIT.txt"
cp "${basalt}/LICENSES/"* "${root}/LICENSES/"
cp "${runtime}/build/dxmt/src/LICENSE" "${root}/LICENSES/DXMT-fork-MIT.txt"
cp "${cranvr}/third_party/licenses/LGPL-2.1.txt" "${root}/LICENSES/Wine-LGPL-2.1.txt"

basalt_commit=$(git -C "${basalt}" rev-parse HEAD)
dxmt_commit=$(git -C "${runtime}/build/dxmt/src" rev-parse HEAD)
{
    print "# Sources of cranvr's game support"
    print
    print -r -- "- Wine 11.10: Gcenx's macOS build, https://github.com/Gcenx/macOS_Wine_builds/releases/tag/11.10 (LGPL 2.1 or later; source at https://gitlab.winehq.org/wine/wine/-/tags/wine-11.10). Unchanged, apart from DXMT's DLLs."
    print -r -- "- DXMT v0.80 (MIT), https://github.com/3Shain/dxmt, with BasaltVR's patches 0001 to 0004 and cranvr's patches 0005 to 0008. Tree commit ${dxmt_commit}."
    print -r -- "- BasaltVR (MIT), https://github.com/Stooovie/BasaltVR at ${basalt_commit}, with cranvr's runtime patches."
    print -r -- "- The OpenXR SDK loader in the runtime's checks (Apache 2.0)."
    print
    print "cranvr's DXMT patches add a limit on MSAA samples, an MSAA resolve without a sample store and an"
    print "optional skip of the Mac window's present. cranvr's runtime patches add the headset's refresh rate,"
    print "a view size per game and an optional limit of one slice per swapchain."
} > "${root}/SOURCES.md"

mkdir -p "${out}"
archive=${out}/cranvr-game-support-macos.tar.xz
print "Packing ${archive:t} (this takes a few minutes)"
tar -C "${stage}" -cf - cranvr-game-support | xz -T0 -6 > "${archive}"
(cd "${out}" && shasum -a 256 "${archive:t}" > "${archive:t}.sha256")
print "Wrote ${archive} ($(du -h "${archive}" | cut -f1))"
cat "${archive}.sha256"
