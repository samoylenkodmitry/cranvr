#!/bin/zsh
# Packs cranvr's game support into one archive that another Mac can install without building:
# Wine 11.10 with cranvr's DXMT fork and cranvr's wined3d.dll (the HLSL compiler of Wine), BasaltVR's OpenXR
# runtime with cranvr's patches and OpenComposite, which runs OpenVR games on that runtime.
#
# Usage: scripts/package-game-support.sh [OUT_DIR]
#
# It takes the files from a built tree (scripts/build-basaltvr.sh), from the OpenComposite build
# (scripts/build-opencomposite.sh) and from the Wine build (scripts/build-wine-d3d.sh) and writes
# cranvr-game-support-macos.tar.xz, its .sha256, cranvr-opencomposite-source.tar.xz and
# cranvr-wine-source.tar.xz into OUT_DIR (default target/game-support). The last two are the sources of
# the OpenComposite and of the patched wined3d.dll in the archive, which the GPL 3 and the LGPL 2.1 ask
# for: publish them as release files too.
# scripts/install-game-support.sh installs such an archive. The built tree is
# third_party/basaltvr/src, or $CRANVR_BASALTVR_DIR when the build lives in another checkout.
#
# The version: scripts/game-support-version.txt names it (a date, YYYY.MM.DD, with a fourth number
# for a second release on one day). The archive carries it as VERSION, and setup-wine.sh stamps it
# into the installed stack. The app compares that stamp with the same file, which it holds from
# its build, and offers "Update game support" when the stack is older. So a change of a runtime or
# DXMT patch needs a new date in that file, a new archive and a release tagged v<version>.
#
# The archive holds:
#   engine/      Wine 11.10 (Gcenx's macOS build), with DXMT's DLLs in place
#   dxmt/        DXMT's DLLs again, for the Wine prefix's system32
#   runtime/     the OpenXR runtime, its manifest, its checks and the Steam web helper shim
#   opencomposite/  OpenComposite's vrclient_x64.dll, its empty version.txt and openvr_smoke.exe, cranvr's
#                OpenVR test program
#   wine-d3d/    Wine's wined3d.dll with cranvr's patches, hlsl_compile.exe (cranvr's test program for the
#                HLSL compiler) and the shader that it compiles
#   SOURCE/      opencomposite-source.tar.xz and wine-source.tar.xz: the sources of those DLLs (see below)
#   LICENSES/    the licenses of Wine, DXMT, BasaltVR, the OpenXR SDK and OpenComposite with its parts
#   SOURCES.md   where each part comes from and which patches it carries
#   VERSION      the version of this game support

set -euo pipefail

cranvr=${0:A:h:h}
out=${1:-${cranvr}/target/game-support}
basalt=${CRANVR_BASALTVR_DIR:-${cranvr}/third_party/basaltvr/src}
runtime=${basalt}/runtime
wine_app="${runtime}/build/dxmt/toolchain/wine-11.10/Wine Devel.app/Contents/Resources/wine"
fork=${runtime}/build/dxmt/fork-install
runtime_bin=${runtime}/build/bin
oc_src=${CRANVR_OPENCOMPOSITE_SRC:-${cranvr}/third_party/opencomposite/src}
oc_build=${CRANVR_OPENCOMPOSITE_BUILD:-${cranvr}/third_party/opencomposite/build}
oc_support=${cranvr}/third_party/opencomposite
wine_src=${CRANVR_WINE_D3D_SRC:-${cranvr}/third_party/wine/src}
wine_build=${CRANVR_WINE_D3D_BUILD:-${cranvr}/third_party/wine/build}
wine_support=${cranvr}/third_party/wine

for f in "${wine_app}/bin/wine" "${fork}/x86_64-windows/d3d11.dll" "${fork}/x86_64-unix/winemetal.so" \
         "${runtime_bin}/basalt_openxr.dll" "${runtime_bin}/steamwebhelper_compat.exe"; do
    if [[ ! -e ${f} ]]; then
        print -u2 "Missing ${f}. Run scripts/build-basaltvr.sh first."
        exit 1
    fi
done

# A runtime DLL older than its sources was built before the last patch: the archive would lack it
for source in "${runtime}"/pe/*.{cpp,hpp,h}(N) "${runtime}"/CMakeLists.txt; do
    if [[ ${source} -nt ${runtime_bin}/basalt_openxr.dll ]]; then
        print -u2 "${source:t} is newer than basalt_openxr.dll. Run scripts/build-basaltvr.sh first."
        exit 1
    fi
done

for f in "${oc_build}/bin/vrclient_x64.dll" "${oc_build}/bin/openvr_smoke.exe" "${oc_build}/bin/version.txt" "${oc_src}/.git"; do
    if [[ ! -e ${f} ]]; then
        print -u2 "Missing ${f}. Run scripts/build-opencomposite.sh first."
        exit 1
    fi
done
# A DLL older than the patches or the test program was built before the last change
for source in "${oc_support}"/patches/*.patch "${oc_support}"/shim/* "${oc_support}/mingw-x86_64.cmake" "${cranvr}/scripts/build-opencomposite.sh"; do
    if [[ ${source} -nt ${oc_build}/bin/vrclient_x64.dll ]]; then
        print -u2 "${source:t} is newer than vrclient_x64.dll. Run scripts/build-opencomposite.sh first."
        exit 1
    fi
done
if [[ ${oc_support}/tests/openvr_smoke.cpp -nt ${oc_build}/bin/openvr_smoke.exe ]]; then
    print -u2 "openvr_smoke.cpp is newer than openvr_smoke.exe. Run scripts/build-opencomposite.sh first."
    exit 1
fi

for f in "${wine_build}/bin/wined3d.dll" "${wine_build}/bin/hlsl_compile.exe" "${wine_build}/bin/isnan.hlsl" "${wine_src}/.git"; do
    if [[ ! -e ${f} ]]; then
        print -u2 "Missing ${f}. Run scripts/build-wine-d3d.sh first."
        exit 1
    fi
done
# A DLL older than the patches or the test program was built before the last change
for source in "${wine_support}"/patches/*.patch "${cranvr}/scripts/build-wine-d3d.sh"; do
    if [[ ${source} -nt ${wine_build}/bin/wined3d.dll ]]; then
        print -u2 "${source:t} is newer than wined3d.dll. Run scripts/build-wine-d3d.sh first."
        exit 1
    fi
done
for source in "${wine_support}/tests/hlsl_compile.cpp" "${wine_support}/tests/isnan.hlsl"; do
    if [[ ${source} -nt ${wine_build}/bin/hlsl_compile.exe ]]; then
        print -u2 "${source:t} is newer than hlsl_compile.exe. Run scripts/build-wine-d3d.sh first."
        exit 1
    fi
done

version=$(tr -d '[:space:]' < "${cranvr}/scripts/game-support-version.txt")
if [[ ! ${version} =~ '^[0-9]{4}\.[0-9]{2}\.[0-9]{2}(\.[0-9]+)?$' ]]; then
    print -u2 "scripts/game-support-version.txt holds '${version}', not YYYY.MM.DD."
    exit 1
fi

stage=$(mktemp -d)
trap 'rm -rf "${stage}"' EXIT
root=${stage}/cranvr-game-support
mkdir -p "${root}/dxmt/x86_64-windows" "${root}/dxmt/x86_64-unix" "${root}/runtime" "${root}/opencomposite" "${root}/wine-d3d" "${root}/SOURCE" "${root}/LICENSES"

print "Copying Wine 11.10"
cp -cR "${wine_app}" "${root}/engine"
for dll in d3d10core.dll d3d11.dll dxgi.dll winemetal.dll; do
    cp "${fork}/x86_64-windows/${dll}" "${root}/engine/lib/wine/x86_64-windows/${dll}"
    cp "${fork}/x86_64-windows/${dll}" "${root}/dxmt/x86_64-windows/${dll}"
done
cp "${fork}/x86_64-unix/winemetal.so" "${root}/engine/lib/wine/x86_64-unix/winemetal.so"
cp "${fork}/x86_64-unix/winemetal.so" "${root}/dxmt/x86_64-unix/winemetal.so"

print "Copying the OpenXR runtime"
for f in basalt_openxr.dll basalt_openxr.json loader_smoke.exe d3d11_runtime_smoke.exe tracking_smoke.exe steamwebhelper_compat.exe; do
    cp "${runtime_bin}/${f}" "${root}/runtime/${f}"
done

print "Copying Wine's patched wined3d.dll and packing its source"
cp "${wine_build}/bin/wined3d.dll" "${wine_build}/bin/hlsl_compile.exe" "${wine_build}/bin/isnan.hlsl" "${root}/wine-d3d/"
# The engine's own copy: the prefix takes its copy from wine-d3d/ at setup
cp "${wine_build}/bin/wined3d.dll" "${root}/engine/lib/wine/x86_64-windows/wined3d.dll"
# Wine's wined3d.dll holds vkd3d-shader (LGPL 2.1 or later). Its source goes with the DLL: the pinned tag's
# tree as it is on winehq.org, cranvr's patches and the steps of the build.
wine_commit=$(git -C "${wine_src}" rev-parse HEAD)
wine_name=wine-${wine_commit[1,12]}-source
wine_tree=${stage}/${wine_name}
mkdir -p "${out}" "${wine_tree}/Wine" "${wine_tree}/cranvr/tests" "${wine_tree}/cranvr/scripts"
git -C "${wine_src}" archive --format=tar HEAD | tar -x -C "${wine_tree}/Wine"
cp -R "${wine_support}/patches" "${wine_tree}/cranvr/"
cp "${wine_support}/tests/hlsl_compile.cpp" "${wine_support}/tests/isnan.hlsl" "${wine_tree}/cranvr/tests/"
cp "${cranvr}/scripts/build-wine-d3d.sh" "${wine_tree}/cranvr/scripts/"
{
    print "Source of wined3d.dll in cranvr's game support (Wine 11.10, LGPL 2.1 or later, see Wine/COPYING.LIB and Wine/libs/vkd3d/COPYING)"
    print
    print "Wine/            https://gitlab.winehq.org/wine/wine at the tag wine-11.10, commit ${wine_commit}, as it is"
    print "cranvr/patches   cranvr's changes to it, to apply in this order with git apply from Wine/:"
    print "                 0001 adds the HLSL intrinsic isnan() to vkd3d-shader (libs/vkd3d/libs/vkd3d-shader/hlsl.y)."
    print "cranvr/tests     hlsl_compile.cpp, the test program that is next to the DLL (it is not part of the DLL)."
    print "                 isnan.hlsl is the shader that it compiles."
    print "cranvr/scripts/build-wine-d3d.sh  the build: git apply, configure for 64-bit Windows with MinGW-w64"
    print "                 (under Rosetta 2 on an Apple Silicon Mac), then make dlls/wined3d/all. The DLL appears at"
    print "                 dlls/wined3d/x86_64-windows/wined3d.dll in the build folder. It needs flex, bison 3 or later and"
    print "                 mingw-w64 (Homebrew)."
} > "${wine_tree}/README.txt"
tar -C "${stage}" -cf - "${wine_name}" | xz -T0 -9 > "${root}/SOURCE/wine-source.tar.xz"
cp "${root}/SOURCE/wine-source.tar.xz" "${out}/cranvr-wine-source.tar.xz"

print "Copying OpenComposite and packing its source"
cp "${oc_build}/bin/vrclient_x64.dll" "${oc_build}/bin/version.txt" "${oc_build}/bin/openvr_smoke.exe" "${root}/opencomposite/"

# OpenComposite is GPL 3 software. Its source goes with the DLL: the pinned commit with its two libraries,
# cranvr's patches and everything that the build uses, and the steps of the build.
oc_commit=$(git -C "${oc_src}" rev-parse HEAD)
oc_glm=$(git -C "${oc_src}/libs/glm" rev-parse HEAD)
oc_openxr=$(git -C "${oc_src}/libs/openxr-sdk" rev-parse HEAD)
oc_name=opencomposite-${oc_commit[1,12]}-source
oc_tree=${stage}/${oc_name}
mkdir -p "${oc_tree}/OpenComposite/libs/glm" "${oc_tree}/OpenComposite/libs/openxr-sdk" "${oc_tree}/cranvr/tests" "${oc_tree}/cranvr/scripts"
git -C "${oc_src}" archive --format=tar HEAD | tar -x -C "${oc_tree}/OpenComposite"
git -C "${oc_src}/libs/glm" archive --format=tar HEAD | tar -x -C "${oc_tree}/OpenComposite/libs/glm"
git -C "${oc_src}/libs/openxr-sdk" archive --format=tar HEAD | tar -x -C "${oc_tree}/OpenComposite/libs/openxr-sdk"
cp -R "${oc_support}/patches" "${oc_support}/shim" "${oc_tree}/cranvr/"
cp "${oc_support}/mingw-x86_64.cmake" "${oc_tree}/cranvr/"
cp "${oc_support}/tests/openvr_smoke.cpp" "${oc_tree}/cranvr/tests/"
cp "${cranvr}/scripts/build-opencomposite.sh" "${oc_tree}/cranvr/scripts/"
{
    print "Source of vrclient_x64.dll in cranvr's game support (OpenComposite, GPL 3, see OpenComposite/LICENSE.txt)"
    print
    print "OpenComposite/   https://gitlab.com/znixian/OpenOVR (branch openxr) at commit ${oc_commit}, as it is"
    print "                 on GitLab, with the two submodules that a Windows build needs:"
    print "                 libs/glm (https://github.com/g-truc/glm) at ${oc_glm}"
    print "                 libs/openxr-sdk (https://github.com/KhronosGroup/OpenXR-SDK) at ${oc_openxr}"
    print "cranvr/patches   cranvr's changes to it, to apply in this order with git apply from OpenComposite/:"
    print "                 0001 builds for Windows with MinGW-w64 and with Direct3D only,"
    print "                 0002 fixes the code for MinGW-w64, makes the log work during static initialisation,"
    print "                 passes returned structs the way MSVC's callers expect, and copies a game's"
    print "                 R8G8B8A8 image into the B8G8R8A8 image that cranvr's OpenXR runtime offers,"
    print "                 0003 looks for the session's state every 2 ms instead of every 250 ms (two session"
    print "                 restarts at the start of a game stalled its first frames) and maps the Vive wand's"
    print "                 trackpad to the Touch controller's thumbstick for a game that ships no Touch bindings,"
    print "                 0004 answers the display frequency from the runtime (XR_FB_display_refresh_rate), not 90 Hz,"
    print "                 0005 draws the converting copy right side up (it turned every picture over), gives the swapchain"
    print "                 one mip level and lets a skybox override do nothing (the runtime takes no quad layer)."
    print "cranvr/shim      atlbase.h: the part of ATL that OpenComposite uses, for MinGW-w64."
    print "cranvr/mingw-x86_64.cmake  the CMake toolchain file of the build."
    print "cranvr/tests/openvr_smoke.cpp  the test program that is next to the DLL (it is not part of the DLL)."
    print "cranvr/scripts/build-opencomposite.sh  the build: git apply, then cmake and ninja with"
    print "                 -DOC_D3D_ONLY=ON and the toolchain file. It needs cmake, ninja, python 3 and mingw-w64"
    print "                 (Homebrew). The DLL appears at build/bin/vrclient_x64.dll."
} > "${oc_tree}/README.txt"
tar -C "${stage}" -cf - "${oc_name}" | xz -T0 -9 > "${root}/SOURCE/opencomposite-source.tar.xz"
mkdir -p "${out}"
cp "${root}/SOURCE/opencomposite-source.tar.xz" "${out}/cranvr-opencomposite-source.tar.xz"

cp "${oc_src}/LICENSE.txt" "${root}/LICENSES/OpenComposite-GPL-3.0.txt"
cp "${oc_src}/LICENSE_OPENVR.txt" "${root}/LICENSES/OpenComposite-OpenVR-headers-BSD-3-Clause.txt"
cp "${oc_src}/LICENSE_INIH.txt" "${root}/LICENSES/OpenComposite-inih-BSD.txt"
cp "${oc_src}/LICENCE_BUILD_UTIL.txt" "${root}/LICENSES/OpenComposite-build-util-BSD.txt"
cp "${oc_src}/LICENCE_DEBUG_BREAK.txt" "${root}/LICENSES/OpenComposite-debugbreak-BSD.txt"
cp "${oc_src}/assets/LICENSE.txt" "${root}/LICENSES/OpenComposite-hand-models-CC-BY-4.0.txt"
cp "${oc_src}/libs/glm/copying.txt" "${root}/LICENSES/OpenComposite-glm-MIT.txt"
cp "${oc_src}/libs/openxr-sdk/LICENSE" "${root}/LICENSES/OpenComposite-OpenXR-SDK-Apache-2.0.txt"
cp "${oc_src}/libs/openxr-sdk/src/external/jsoncpp/LICENSE" "${root}/LICENSES/OpenComposite-jsoncpp-MIT.txt"

cp "${basalt}/LICENSE" "${root}/LICENSES/BasaltVR-MIT.txt"
cp "${basalt}/LICENSES/"* "${root}/LICENSES/"
cp "${runtime}/build/dxmt/src/LICENSE" "${root}/LICENSES/DXMT-fork-MIT.txt"
cp "${cranvr}/third_party/licenses/LGPL-2.1.txt" "${root}/LICENSES/Wine-LGPL-2.1.txt"
cp "${wine_src}/LICENSE" "${root}/LICENSES/Wine-LICENSE.txt"
cp "${wine_src}/libs/vkd3d/COPYING" "${root}/LICENSES/Wine-vkd3d-LGPL-2.1.txt"
cp "${wine_src}/libs/vkd3d/AUTHORS" "${root}/LICENSES/Wine-vkd3d-AUTHORS.txt"

basalt_commit=$(git -C "${basalt}" rev-parse HEAD)
dxmt_commit=$(git -C "${runtime}/build/dxmt/src" rev-parse HEAD)
{
    print "# Sources of cranvr's game support"
    print
    print -r -- "- Wine 11.10: Gcenx's macOS build, https://github.com/Gcenx/macOS_Wine_builds/releases/tag/11.10 (LGPL 2.1 or later; source at https://gitlab.winehq.org/wine/wine/-/tags/wine-11.10). Unchanged, apart from DXMT's DLLs and wined3d.dll (next line)."
    print -r -- "- wined3d.dll of Wine 11.10 (LGPL 2.1 or later), built from https://gitlab.winehq.org/wine/wine at the tag wine-11.10, commit ${wine_commit}, with cranvr's patch 0001 (the HLSL intrinsic isnan in vkd3d-shader). It replaces the file in the engine and in the Wine prefix. Its source, the patch and the build are in SOURCE/wine-source.tar.xz of this archive and in the file cranvr-wine-source.tar.xz of the release. wine-d3d/hlsl_compile.exe is cranvr's own test program (MIT)."
    print -r -- "- DXMT v0.80 (MIT), https://github.com/3Shain/dxmt, with BasaltVR's patches 0001 to 0004 and cranvr's patches 0005 to 0010. Tree commit ${dxmt_commit}."
    print -r -- "- BasaltVR (MIT), https://github.com/Stooovie/BasaltVR at ${basalt_commit}, with cranvr's runtime patches."
    print -r -- "- The OpenXR SDK loader in the runtime's checks (Apache 2.0)."
    print -r -- "- OpenComposite (GPL 3), https://gitlab.com/znixian/OpenOVR at ${oc_commit}, with cranvr's patches 0001 to 0005. Its source, the patches and the build are in SOURCE/opencomposite-source.tar.xz of this archive and in the file cranvr-opencomposite-source.tar.xz of the release. It is a separate program: games load it as an OpenVR runtime (opencomposite/vrclient_x64.dll) and it calls the OpenXR runtime above. It contains the OpenXR SDK loader (Apache 2.0), jsoncpp (MIT), glm (MIT), inih (BSD), Valve's OpenVR headers (BSD 3-Clause) and hand models of Oculus (CC BY 4.0): their licenses are in LICENSES/."
    print
    print "cranvr's DXMT patches add a limit on MSAA samples, an MSAA resolve without a sample store, an"
    print "optional skip of the Mac window's present, a silent Discard hint (patch 0009) and an optional report of render-target"
    print "array indexing from any shader (patch 0010, for a Unity game with the OpenVR provider). cranvr's runtime patches add the headset's refresh rate,"
    print "a view size per game, an optional limit of one slice per swapchain, a separate aim pose for each"
    print "controller and the tracking of hands, body, face and eyes (XR_EXT_hand_tracking, XR_FB_body_tracking,"
    print "XR_META_body_tracking_full_body, XR_FB_face_tracking2 and XR_EXT_eye_gaze_interaction). Patch 0006"
    print "makes the face's data source the headset's own: the microphone on a Quest 3. Patch 0007 offers the STAGE"
    print "reference space, which is the LOCAL space that cranvr-host already puts the floor at height 0 in."
    print "Patch 0008 lets an action set attach to a session after the session that held it was destroyed: an OpenVR"
    print "game with an action manifest (IVRInput) reaches OpenComposite, which does exactly that at the game's first frame."
    print "Patch 0009 offers XR_FB_display_refresh_rate with the headset's rate as the only rate. Patch 0010 logs the"
    print "bound actions of a game and the first input that reaches each of them."
} > "${root}/SOURCES.md"

print -r -- "${version}" > "${root}/VERSION"

archive=${out}/cranvr-game-support-macos.tar.xz
print "Packing ${archive:t} (this takes a few minutes)"
tar -C "${stage}" -cf - cranvr-game-support | xz -T0 -6 > "${archive}"
(cd "${out}" && shasum -a 256 "${archive:t}" > "${archive:t}.sha256")
print "Wrote ${archive} (version ${version}, $(du -h "${archive}" | cut -f1))"
cat "${archive}.sha256"
