#!/bin/zsh
# Packs cranvr's game support into one archive that another Mac can install without building:
# Wine 11.10 with cranvr's DXMT fork and cranvr's patched Wine files (wined3d.dll, wininet.dll, wtsapi32.dll,
# user32.dll, winemac.so, opengl32.so, ntdll.so and winecoreaudio.so), BasaltVR's OpenXR runtime with cranvr's patches, OpenComposite, which runs
# OpenVR games on that runtime, and cranvr's SteamVR driver, which runs OpenVR games in SteamVR for Windows.
#
# Usage: scripts/package-game-support.sh [OUT_DIR]
#
# It takes the files from a built tree (scripts/build-basaltvr.sh), from the OpenComposite build
# (scripts/build-opencomposite.sh), from the Wine build (scripts/build-wine.sh) and from the driver's build
# (scripts/build-steamvr-driver.sh) and writes
# cranvr-game-support-macos.tar.xz, its .sha256, cranvr-opencomposite-source.tar.xz and
# cranvr-wine-source.tar.xz into OUT_DIR (default target/game-support). The last two are the sources of
# the OpenComposite and of the patched Wine files in the archive, which the GPL 3 and the LGPL 2.1 ask
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
#   engine/      Wine 11.10 (Gcenx's macOS build), with DXMT's DLLs and the patched Wine files in place
#   dxmt/        DXMT's DLLs again, for the Wine prefix's system32
#   runtime/     the OpenXR runtime, its manifest, its checks (hand_interaction_smoke.exe among them) and the Steam web
#                helper shim
#   opencomposite/  OpenComposite's vrclient_x64.dll, its empty version.txt and openvr_smoke.exe, cranvr's
#                OpenVR test program
#   steamvr-driver/  cranvr's SteamVR driver as SteamVR loads it: driver.vrdrivermanifest, bin/win64/driver_cranvr.dll
#                and resources/settings/default.vrsettings
#   wine-patched/  Wine's wined3d.dll, wininet.dll, wtsapi32.dll, user32.dll, winemac.so, opengl32.so, ntdll.so and
#                winecoreaudio.so with cranvr's patches,
#                hlsl_compile.exe (cranvr's test program for the HLSL compiler) with the shader that it compiles,
#                gl_compat_check.exe (cranvr's test program for an OpenGL program like Google Earth VR),
#                far_jump_check.exe (cranvr's test program for the far jump of MonoMod's detours),
#                stack_sample_check.exe (cranvr's test program for a sampling profiler's stack copy),
#                session_check.exe (cranvr's test program for the session's state, as SteamVR asks for it) and
#                microphone_check.exe (cranvr's test program for the microphone that games hear)
#   SOURCE/      opencomposite-source.tar.xz and wine-source.tar.xz: the sources of those files (see below)
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
driver_build=${CRANVR_STEAMVR_DRIVER_DIR:-${cranvr}/target/steamvr-driver/cranvr}

for f in "${wine_app}/bin/wine" "${fork}/x86_64-windows/d3d11.dll" "${fork}/x86_64-unix/winemetal.so" \
         "${runtime_bin}/basalt_openxr.dll" "${runtime_bin}/steamwebhelper_compat.exe" "${runtime_bin}/hand_interaction_smoke.exe"; do
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

wine_files=(wined3d.dll wininet.dll wtsapi32.dll user32.dll winemac.so opengl32.so ntdll.so winecoreaudio.so hlsl_compile.exe
    isnan.hlsl gl_compat_check.exe far_jump_check.exe stack_sample_check.exe session_check.exe microphone_check.exe)
for f in "${wine_files[@]/#/${wine_build}/bin/}" "${wine_src}/.git"; do
    if [[ ! -e ${f} ]]; then
        print -u2 "Missing ${f}. Run scripts/build-wine.sh first."
        exit 1
    fi
done
# A file older than the patches, the build or its test program was built before the last change
for file in wined3d.dll wininet.dll wtsapi32.dll user32.dll winemac.so opengl32.so ntdll.so winecoreaudio.so; do
    for source in "${wine_support}"/patches/*.patch "${cranvr}/scripts/build-wine.sh"; do
        if [[ ${source} -nt ${wine_build}/bin/${file} ]]; then
            print -u2 "${source:t} is newer than ${file}. Run scripts/build-wine.sh first."
            exit 1
        fi
    done
done
for pair in hlsl_compile.cpp:hlsl_compile.exe isnan.hlsl:hlsl_compile.exe gl_compat_check.cpp:gl_compat_check.exe \
    far_jump_check.cpp:far_jump_check.exe stack_sample_check.cpp:stack_sample_check.exe session_check.cpp:session_check.exe \
    microphone_check.cpp:microphone_check.exe; do
    if [[ ${wine_support}/tests/${pair%%:*} -nt ${wine_build}/bin/${pair##*:} ]]; then
        print -u2 "${pair%%:*} is newer than ${pair##*:}. Run scripts/build-wine.sh first."
        exit 1
    fi
done

# The SteamVR driver, built after the last change of its source
if [[ ! -f ${driver_build}/bin/win64/driver_cranvr.dll ]]; then
    print -u2 "Missing ${driver_build}/bin/win64/driver_cranvr.dll. Run scripts/build-steamvr-driver.sh first."
    exit 1
fi
for source in "${cranvr}"/steamvr/driver_cranvr.cpp "${cranvr}"/steamvr/driver.vrdrivermanifest \
    "${cranvr}"/steamvr/resources/settings/default.vrsettings "${cranvr}/scripts/build-steamvr-driver.sh"; do
    if [[ ${source} -nt ${driver_build}/bin/win64/driver_cranvr.dll ]]; then
        print -u2 "${source:t} is newer than driver_cranvr.dll. Run scripts/build-steamvr-driver.sh first."
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
mkdir -p "${root}/dxmt/x86_64-windows" "${root}/dxmt/x86_64-unix" "${root}/runtime" "${root}/opencomposite" "${root}/wine-patched" "${root}/SOURCE" "${root}/LICENSES"

print "Copying Wine 11.10"
cp -cR "${wine_app}" "${root}/engine"
for dll in d3d10core.dll d3d11.dll dxgi.dll winemetal.dll; do
    cp "${fork}/x86_64-windows/${dll}" "${root}/engine/lib/wine/x86_64-windows/${dll}"
    cp "${fork}/x86_64-windows/${dll}" "${root}/dxmt/x86_64-windows/${dll}"
done
cp "${fork}/x86_64-unix/winemetal.so" "${root}/engine/lib/wine/x86_64-unix/winemetal.so"
cp "${fork}/x86_64-unix/winemetal.so" "${root}/dxmt/x86_64-unix/winemetal.so"

print "Copying the OpenXR runtime"
for f in basalt_openxr.dll basalt_openxr.json loader_smoke.exe d3d11_runtime_smoke.exe tracking_smoke.exe hand_interaction_smoke.exe steamwebhelper_compat.exe; do
    cp "${runtime_bin}/${f}" "${root}/runtime/${f}"
done

print "Copying Wine's patched files and packing their source"
cp "${wine_files[@]/#/${wine_build}/bin/}" "${root}/wine-patched/"
# The engine's own copies: the prefix takes its copies of the Windows DLLs from wine-patched/ at setup
cp "${wine_build}/bin/wined3d.dll" "${wine_build}/bin/wininet.dll" "${wine_build}/bin/wtsapi32.dll" "${wine_build}/bin/user32.dll" \
    "${root}/engine/lib/wine/x86_64-windows/"
cp "${wine_build}/bin/winemac.so" "${wine_build}/bin/opengl32.so" "${wine_build}/bin/ntdll.so" "${wine_build}/bin/winecoreaudio.so" \
    "${root}/engine/lib/wine/x86_64-unix/"
# The patched files are Wine's (LGPL 2.1 or later; wined3d.dll holds vkd3d-shader). Their source goes with them:
# the pinned tag's tree as it is on winehq.org, cranvr's patches and the steps of the build.
wine_commit=$(git -C "${wine_src}" rev-parse HEAD)
wine_name=wine-${wine_commit[1,12]}-source
wine_tree=${stage}/${wine_name}
mkdir -p "${out}" "${wine_tree}/Wine" "${wine_tree}/cranvr/tests" "${wine_tree}/cranvr/scripts"
git -C "${wine_src}" archive --format=tar HEAD | tar -x -C "${wine_tree}/Wine"
cp -R "${wine_support}/patches" "${wine_tree}/cranvr/"
cp "${wine_support}/tests/hlsl_compile.cpp" "${wine_support}/tests/isnan.hlsl" "${wine_support}/tests/gl_compat_check.cpp" \
    "${wine_support}/tests/far_jump_check.cpp" "${wine_support}/tests/stack_sample_check.cpp" \
    "${wine_support}/tests/session_check.cpp" "${wine_support}/tests/microphone_check.cpp" "${wine_tree}/cranvr/tests/"
cp "${cranvr}/scripts/build-wine.sh" "${wine_tree}/cranvr/scripts/"
{
    print "Source of wined3d.dll, wininet.dll, wtsapi32.dll, user32.dll, winemac.so, opengl32.so, ntdll.so and winecoreaudio.so"
    print "in cranvr's game support"
    print "(Wine 11.10, LGPL 2.1 or later, see Wine/COPYING.LIB and Wine/libs/vkd3d/COPYING)"
    print
    print "Wine/            https://gitlab.winehq.org/wine/wine at the tag wine-11.10, commit ${wine_commit}, as it is"
    print "cranvr/patches   cranvr's changes to it, to apply in this order with git apply from Wine/:"
    print "                 0001 adds the HLSL intrinsic isnan() to vkd3d-shader (libs/vkd3d/libs/vkd3d-shader/hlsl.y),"
    print "                 0002 makes winemac create a core context for a request of the compatibility profile of OpenGL"
    print "                 3.2 to 4.1, or of such a context without the forward-compatible flag, which it refused,"
    print "                 0003 makes opengl32 compile GLSL 1.10, 1.20 and ES 1.00 in a core context as GLSL 1.50 and"
    print "                 GLSL ES 3.00 as GLSL 3.30, and log a shader that does not compile or a program that does not link,"
    print "                 0004 changes the list of a wininet handle's children under the lock: a program that closed"
    print "                 handles on several threads crashed in invalidate_handle,"
    print "                 0005 makes ntdll on macOS give a 64-bit program with high-entropy addresses its memory at no"
    print "                 fixed address above 4 GB, as Windows does (dlls/ntdll/unix/virtual.c): MonoMod's detours walk"
    print "                 down to page 0 from code in the low 2 GB and wrapped their far jumps. Thread stacks stay below"
    print "                 4 GB, so a profiler's copy of a suspended thread's stack does not run into a thread stack,"
    print "                 0006 makes wtsapi32 answer WTSSessionInfoEx (the console's session, unlocked) and WTSIsRemoteSession"
    print "                 (a local session): SteamVR's compositor took a session without an answer as locked and hid the picture,"
    print "                 0007 makes user32 export DwmGetDxSharedSurface, which fails: SteamVR's dashboard did not start"
    print "                 without the import (vrdesktop.dll),"
    print "                 0008 gives winecoreaudio the capture device \"cranvr microphone\", the default one, while cranvr's"
    print "                 microphone.txt names a source: it records the headset's microphone from a ring in shared memory"
    print "                 that cranvr's host writes, a Mac input device, or silence (dlls/winecoreaudio.drv/coreaudio.c)."
    print "cranvr/tests     hlsl_compile.cpp, gl_compat_check.cpp, far_jump_check.cpp, stack_sample_check.cpp, session_check.cpp and"
    print "                 microphone_check.cpp, the test"
    print "                 programs that are next to the files (they are not part of Wine). isnan.hlsl is the shader that"
    print "                 hlsl_compile.exe compiles."
    print "cranvr/scripts/build-wine.sh  the build: git apply, configure (under Rosetta 2 on an Apple Silicon Mac, with"
    print "                 MinGW-w64 for the Windows files and macOS 10.15 as the oldest system), then make for the eight"
    print "                 files. They appear under dlls/ in the build folder. It needs flex, bison 3 or later and mingw-w64"
    print "                 (Homebrew), and Xcode's command line tools."
} > "${wine_tree}/README.txt"
tar -C "${stage}" -cf - "${wine_name}" | xz -T0 -9 > "${root}/SOURCE/wine-source.tar.xz"
cp "${root}/SOURCE/wine-source.tar.xz" "${out}/cranvr-wine-source.tar.xz"

print "Copying the SteamVR driver"
cp -R "${driver_build}" "${root}/steamvr-driver"
cp "${cranvr}/steamvr/openvr/LICENSE" "${root}/LICENSES/SteamVR-driver-OpenVR-headers-BSD-3-Clause.txt"

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
    print "                 one mip level and lets a skybox override do nothing (the runtime takes no quad layer),"
    print "                 0006 logs where a controller's pose comes from and keeps a controller where it was for up to a second"
    print "                 when the runtime has no pose of it for a moment (a game drew it on the floor),"
    print "                 0007 gives the headset and each controller a serial number of its own: every device said <unknown>,"
    print "                 and a game that tells its controllers apart by serial number took two controllers for one,"
    print "                 0008 leaves overlays and the keyboard out of the frame (the runtime takes no quad layer, so the frame"
    print "                 failed and the headset got none while a game kept an overlay up, as a loading screen does),"
    print "                 0009 runs an OpenGL game: the session stays on a Direct3D 11 device of OpenComposite's own, and each"
    print "                 OpenGL texture of the game is copied through the CPU into the runtime's Direct3D 11 image; it also"
    print "                 answers GetFrameTimeRemaining, which Google Earth VR calls (it stopped the game),"
    print "                 0010 sends the button events of the legacy controller state (VREvent_ButtonPress and its kin) to a"
    print "                 game that reads that state: the OpenXR port sent none, and Google Earth VR saw no press,"
    print "                 0011 refuses every application type but a scene with an init error and one log line, never a message"
    print "                 box (Steam's VR watchdog got a box that stopped Steam), and answers the symbols of its init errors."
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
    print -r -- "- Wine 11.10: Gcenx's macOS build, https://github.com/Gcenx/macOS_Wine_builds/releases/tag/11.10 (LGPL 2.1 or later; source at https://gitlab.winehq.org/wine/wine/-/tags/wine-11.10). Unchanged, apart from DXMT's DLLs and the patched Wine files (next line)."
    print -r -- "- wined3d.dll, wininet.dll, wtsapi32.dll, user32.dll, winemac.so, opengl32.so, ntdll.so and winecoreaudio.so of Wine 11.10 (LGPL 2.1 or later), built from https://gitlab.winehq.org/wine/wine at the tag wine-11.10, commit ${wine_commit}, with cranvr's patches 0001 (the HLSL intrinsic isnan in vkd3d-shader), 0002 (a core OpenGL context for a request of the compatibility profile), 0003 (legacy GLSL in a core context), 0004 (wininet's handles closed on several threads), 0005 (memory above 4 GB for a program with high-entropy addresses on macOS), 0006 (an unlocked local session for SteamVR), 0007 (DwmGetDxSharedSurface for SteamVR's dashboard) and 0008 (the capture device \"cranvr microphone\", which records the headset's microphone, a Mac input device or silence). They replace the files in the engine, and the DLLs also in the Wine prefix. Their source, the patches and the build are in SOURCE/wine-source.tar.xz of this archive and in the file cranvr-wine-source.tar.xz of the release. wine-patched/hlsl_compile.exe, wine-patched/gl_compat_check.exe, wine-patched/far_jump_check.exe, wine-patched/stack_sample_check.exe, wine-patched/session_check.exe and wine-patched/microphone_check.exe are cranvr's own test programs (MIT)."
    print -r -- "- DXMT v0.80 (MIT), https://github.com/3Shain/dxmt, with BasaltVR's patches 0001 to 0004 and cranvr's patches 0005 to 0018. Tree commit ${dxmt_commit}."
    print -r -- "- steamvr-driver/: cranvr's SteamVR driver (MIT), built from steamvr/driver_cranvr.cpp of cranvr with MinGW-w64. It includes Valve's openvr_driver.h of OpenVR 2.15.6 (BSD 3-Clause, LICENSES/SteamVR-driver-OpenVR-headers-BSD-3-Clause.txt). SteamVR itself is not part of this archive: it comes from the user's Steam library."
    print -r -- "- BasaltVR (MIT), https://github.com/Stooovie/BasaltVR at ${basalt_commit}, with cranvr's runtime patches."
    print -r -- "- The OpenXR SDK loader in the runtime's checks (Apache 2.0)."
    print -r -- "- OpenComposite (GPL 3), https://gitlab.com/znixian/OpenOVR at ${oc_commit}, with cranvr's patches 0001 to 0011. Its source, the patches and the build are in SOURCE/opencomposite-source.tar.xz of this archive and in the file cranvr-opencomposite-source.tar.xz of the release. It is a separate program: games load it as an OpenVR runtime (opencomposite/vrclient_x64.dll) and it calls the OpenXR runtime above. It contains the OpenXR SDK loader (Apache 2.0), jsoncpp (MIT), glm (MIT), inih (BSD), Valve's OpenVR headers (BSD 3-Clause) and hand models of Oculus (CC BY 4.0): their licenses are in LICENSES/."
    print
    print "cranvr's DXMT patches add a limit on MSAA samples, an MSAA resolve without a sample store, an"
    print "optional skip of the Mac window's present, a silent Discard hint (patch 0009) and an optional report of render-target"
    print "array indexing from any shader (patch 0010, for a Unity game with the OpenVR provider), the shaders of a failed pipeline in the log"
    print "(patch 0011) and a pipeline with the rasterizer off for a vertex shader that declares no output (patch 0012: Metal refused such a"
    print "pipeline and a game's draw then ran with the pipeline of the draw before it). Patches 0013 to 0018 run SteamVR's processes:"
    print "buffers and textures that one process shares with another (0013, 0018), a keyed mutex for Chromium's shared textures (0014),"
    print "a resource's DXGI usage from its bind flags (0015), a desktop duplication that delivers no frame (0016) and a swapchain"
    print "for a window of another process (0017). cranvr's runtime patches add the headset's refresh rate,"
    print "a view size per game, an optional limit of one slice per swapchain, a separate aim pose for each"
    print "controller and the tracking of hands, body, face and eyes (XR_EXT_hand_tracking, XR_FB_body_tracking,"
    print "XR_META_body_tracking_full_body, XR_FB_face_tracking2 and XR_EXT_eye_gaze_interaction). Patch 0006"
    print "makes the face's data source the headset's own: the microphone on a Quest 3. Patch 0007 offers the STAGE"
    print "reference space, which is the LOCAL space that cranvr-host already puts the floor at height 0 in."
    print "Patch 0008 lets an action set attach to a session after the session that held it was destroyed: an OpenVR"
    print "game with an action manifest (IVRInput) reaches OpenComposite, which does exactly that at the game's first frame."
    print "Patch 0009 offers XR_FB_display_refresh_rate with the headset's rate as the only rate. Patch 0010 logs the"
    print "bound actions of a game and the first input that reaches each of them. Patch 0011 keeps the controllers' poses"
    print "fresh under load: a reader of a pose that found it newer than its own clock reading counted it as stale, and a"
    print "pose is fresh for 1.5 seconds, not 0.5. It also writes the input notes to game-input.log beside the control file."
    print "Patch 0012 offers XR_EXT_hand_interaction: the hand interaction profile with its four poses and three gestures,"
    print "made by cranvr-host from a tracked hand's joints or mapped from a Touch controller, and the pinch and poke poses on"
    print "every profile of the hands. Each hand takes the profile of what the user holds."
} > "${root}/SOURCES.md"

print -r -- "${version}" > "${root}/VERSION"

archive=${out}/cranvr-game-support-macos.tar.xz
print "Packing ${archive:t} (this takes a few minutes)"
tar -C "${stage}" -cf - cranvr-game-support | xz -T0 -6 > "${archive}"
(cd "${out}" && shasum -a 256 "${archive:t}" > "${archive:t}.sha256")
print "Wrote ${archive} (version ${version}, $(du -h "${archive}" | cut -f1))"
cat "${archive}.sha256"
