#!/bin/zsh
# Builds the Wine stack that runs Windows OpenXR games for cranvr-host:
#   - Wine 11.10 (Gcenx build), cloned from a BasaltVR checkout
#   - the BasaltVR DXMT fork, which shares each game image with the host as an IOSurface
#   - the BasaltVR OpenXR runtime (C:\basalt\runtime), registered as the active runtime
#   - OpenComposite (C:\opencomposite), which turns a game's OpenVR calls into OpenXR calls to that
#     runtime, and cranvr's SteamVR driver (C:\cranvr\steamvr, scripts/build-steamvr-driver.sh), which runs
#     a game in SteamVR for Windows from the prefix's Steam library. A game finds either one through the
#     prefix's OpenVR path file, which its launch options choose (see scripts/run-game.sh); this script
#     writes no such file.
#   - Wine's files with cranvr's patches (scripts/build-wine.sh): wined3d.dll, whose HLSL compiler
#     (vkd3d-shader) a game that compiles its shaders at run time uses through d3dcompiler_47.dll;
#     winemac.so and opengl32.so, which give an OpenGL game that asks for the compatibility profile a core
#     context and compile its legacy GLSL; wininet.dll, whose handles a game with requests on several
#     threads closed in a race; ntdll.so, which gives a program with high-entropy addresses its memory above
#     4 GB as Windows does, so MonoMod's detours in Unity mods find free pages near their code; wtsapi32.dll,
#     which reports the session as unlocked, so SteamVR's compositor shows the picture; user32.dll, which exports
#     the function that SteamVR's dashboard needs to start
#
# The BasaltVR checkout must have run, in this order:
#   runtime/scripts/test-dxmt.zsh          (downloads Wine 11.10 and stock DXMT)
#   runtime/scripts/build-dxmt-fork.zsh    (builds the fork)
#   cmake -S runtime --preset mingw-release && cmake --build runtime/build
#
# The OpenComposite build (scripts/build-opencomposite.sh) is optional for BASALTVR_DIR: without it the
# stack has no OpenVR support. $CRANVR_OPENCOMPOSITE_DIR names another build folder than
# third_party/opencomposite/build. The Wine build (scripts/build-wine.sh) is optional the same way:
# without it Wine's HLSL compiler lacks isnan() (a game whose shader calls it shows a black picture) and
# an OpenGL game that asks for the compatibility profile gets no context.
# $CRANVR_WINE_DIR names another build folder than third_party/wine/build.
#
# Usage: scripts/setup-wine.sh BASALTVR_DIR
#        scripts/setup-wine.sh GAME_SUPPORT_DIR   (an unpacked archive from package-game-support.sh)
# The stack goes to $CRANVR_HOME/wine (default: ~/Library/Application Support/cranvr/wine).
# Run it again to update the stack; the prefix and its installed games stay.
#
# At the end it stamps the stack's version into $CRANVR_HOME/wine/game-support-version.txt. The
# version comes from the archive's VERSION file, or from scripts/game-support-version.txt when the
# stack is built from a BasaltVR checkout. An archive without VERSION (a release before the stamp)
# removes the stamp, so the app sees an old stack. The cranvr app compares the stamp with the
# version that it expects.

set -euo pipefail

if (( $# != 1 )); then
    print -u2 "usage: scripts/setup-wine.sh BASALTVR_DIR|GAME_SUPPORT_DIR"
    exit 2
fi
home=${CRANVR_HOME:-$HOME/Library/Application Support/cranvr}
stack=${home}/wine
if [[ -x ${1:A}/engine/bin/wine && -f ${1:A}/runtime/basalt_openxr.dll ]]; then
    # An unpacked game support archive: Wine with DXMT in place, DXMT's DLLs, the runtime
    source_wine=${1:A}/engine
    fork=${1:A}/dxmt
    runtime_bin=${1:A}/runtime
    opencomposite_bin=${1:A}/opencomposite
    steamvr_driver=${1:A}/steamvr-driver
    wine_bin=${1:A}/wine-patched
    version_file=${1:A}/VERSION
else
    basalt=${1:A}/runtime
    source_wine="${basalt}/build/dxmt/toolchain/wine-11.10/Wine Devel.app/Contents/Resources/wine"
    fork=${basalt}/build/dxmt/fork-install
    runtime_bin=${basalt}/build/bin
    opencomposite_bin=${CRANVR_OPENCOMPOSITE_DIR:-${0:A:h:h}/third_party/opencomposite/build}/bin
    steamvr_driver=${CRANVR_STEAMVR_DRIVER_DIR:-${0:A:h:h}/target/steamvr-driver/cranvr}
    wine_bin=${CRANVR_WINE_DIR:-${0:A:h:h}/third_party/wine/build}/bin
    version_file=${0:A:h}/game-support-version.txt
fi

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
# An update that stops halfway leaves no stamp, so the app offers the update again
rm -f "${stack}/game-support-version.txt"
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

# Wine's files with cranvr's patches (third_party/wine). The prefix keeps its own copy of each Windows DLL in
# system32, and the loader takes that one. An engine that exists already is not cloned again, so each file
# is copied in by name.
if [[ -f ${wine_bin}/wined3d.dll ]]; then
    for dll in wined3d.dll wininet.dll wtsapi32.dll user32.dll; do
        cp "${wine_bin}/${dll}" "${wine_root}/lib/wine/x86_64-windows/${dll}"
        cp "${wine_bin}/${dll}" "${prefix}/drive_c/windows/system32/${dll}"
    done
    for lib in winemac.so opengl32.so ntdll.so; do
        cp "${wine_bin}/${lib}" "${wine_root}/lib/wine/x86_64-unix/${lib}"
    done
    mkdir -p "${prefix}/drive_c/wine-patched"
    cp "${wine_bin}/hlsl_compile.exe" "${wine_bin}/isnan.hlsl" "${wine_bin}/gl_compat_check.exe" \
        "${wine_bin}/far_jump_check.exe" "${wine_bin}/stack_sample_check.exe" "${wine_bin}/session_check.exe" \
        "${prefix}/drive_c/wine-patched/"
else
    print "No patched Wine files in ${wine_bin}: Wine's HLSL compiler lacks isnan, OpenGL games get no compatibility profile, and MonoMod's far detours can miss."
fi

runtime_dir=${prefix}/drive_c/basalt/runtime
mkdir -p "${runtime_dir}"
cp "${runtime_bin}/basalt_openxr.dll" "${runtime_bin}/basalt_openxr.json" "${runtime_bin}/loader_smoke.exe" "${runtime_dir}/"
for extra in d3d11_runtime_smoke.exe tracking_smoke.exe hand_interaction_smoke.exe steamwebhelper_compat.exe; do
    if [[ -e ${runtime_bin}/${extra} ]]; then
        cp "${runtime_bin}/${extra}" "${runtime_dir}/"
    fi
done
"${wine_root}/bin/wine" reg.exe add 'HKLM\Software\Khronos\OpenXR\1' \
    /v ActiveRuntime /t REG_SZ /d 'C:\basalt\runtime\basalt_openxr.json' /f >/dev/null

# OpenComposite: vrclient_x64.dll in bin, where an OpenVR loader looks for it, and its test next to it.
# bin/version.txt keeps Steam from rewriting the OpenVR path file of a runtime that has none.
oc_dir=${prefix}/drive_c/opencomposite
if [[ -f ${opencomposite_bin}/vrclient_x64.dll ]]; then
    mkdir -p "${oc_dir}/bin"
    cp "${opencomposite_bin}/vrclient_x64.dll" "${opencomposite_bin}/version.txt" "${oc_dir}/bin/"
    cp "${opencomposite_bin}/openvr_smoke.exe" "${oc_dir}/"
else
    print "No OpenComposite in ${opencomposite_bin}: this stack runs OpenXR games only."
fi

# cranvr's SteamVR driver, in the folder that the OpenVR path file names for SteamVR (app/src/openvr.rs). The
# folder is replaced as a whole, so no file of an older driver stays.
driver_dir=${prefix}/drive_c/cranvr/steamvr
if [[ -f ${steamvr_driver}/bin/win64/driver_cranvr.dll ]]; then
    rm -rf "${driver_dir}"
    mkdir -p "${driver_dir:h}"
    cp -R "${steamvr_driver}" "${driver_dir}"
else
    print "No SteamVR driver in ${steamvr_driver}: games run without SteamVR (scripts/build-steamvr-driver.sh builds it)."
fi

# A crash should end the program, not wait in Wine's debugger
"${wine_root}/bin/wine" reg.exe add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion\AeDebug' \
    /v Debugger /t REG_SZ /d '' /f >/dev/null

print "Checking the OpenXR runtime through the Khronos loader"
(cd "${runtime_dir}" && DXMT_BASALT_IOSURFACE=1 BASALT_EXPECT_D3D11_EXTENSION=1 "${wine_root}/bin/wine" ./loader_smoke.exe >/dev/null 2>&1) || { print -u2 "FAIL: loader_smoke.exe (run it by hand for details)"; exit 1; }
if [[ -f ${oc_dir}/bin/vrclient_x64.dll ]]; then
    print "Checking that OpenComposite loads"
    (cd "${oc_dir}" && DXMT_BASALT_IOSURFACE=1 "${wine_root}/bin/wine" ./openvr_smoke.exe --load-only --runtime 'C:\opencomposite' >/dev/null 2>&1) || { print -u2 "FAIL: openvr_smoke.exe --load-only (run it by hand for details)"; exit 1; }
fi
if [[ -f ${prefix}/drive_c/wine-patched/hlsl_compile.exe ]]; then
    print "Checking that Wine's HLSL compiler knows isnan"
    hlsl_out=$(cd "${prefix}/drive_c/wine-patched" && "${wine_root}/bin/wine" ./hlsl_compile.exe isnan.hlsl 2>&1 || true)
    if [[ ${hlsl_out} != *"hlsl_compile: PASS"* ]]; then
        print -u2 "FAIL: hlsl_compile.exe isnan.hlsl (run it by hand for details):"
        print -u2 -r -- "${hlsl_out}"
        exit 1
    fi
fi
if [[ -f ${prefix}/drive_c/wine-patched/gl_compat_check.exe ]]; then
    print "Checking that an OpenGL program that asks for the compatibility profile draws with GLSL 1.10"
    gl_out=$(cd "${prefix}/drive_c/wine-patched" && MVK_CONFIG_LOG_LEVEL=0 "${wine_root}/bin/wine" ./gl_compat_check.exe 2>&1 || true)
    if [[ ${gl_out} != *"gl_compat_check: PASS"* ]]; then
        print -u2 "FAIL: gl_compat_check.exe (run it by hand for details):"
        print -u2 -r -- "${gl_out}"
        exit 1
    fi
fi
if [[ -f ${prefix}/drive_c/wine-patched/far_jump_check.exe ]]; then
    print "Checking that a program with high-entropy addresses gets them above 4 GB (MonoMod's far jump)"
    far_out=$(cd "${prefix}/drive_c/wine-patched" && "${wine_root}/bin/wine" ./far_jump_check.exe 2>&1 || true)
    if [[ ${far_out} != *"far_jump_check: PASS"* ]]; then
        print -u2 "FAIL: far_jump_check.exe (run it by hand for details):"
        print -u2 -r -- "${far_out}"
        exit 1
    fi
fi
if [[ -f ${prefix}/drive_c/wine-patched/stack_sample_check.exe ]]; then
    print "Checking that a profiler's copy of a suspended thread's stack stays readable"
    sample_out=$(cd "${prefix}/drive_c/wine-patched" && "${wine_root}/bin/wine" ./stack_sample_check.exe 2>&1 || true)
    if [[ ${sample_out} != *"stack_sample_check: PASS"* ]]; then
        print -u2 "FAIL: stack_sample_check.exe (run it by hand for details):"
        print -u2 -r -- "${sample_out}"
        exit 1
    fi
fi
if [[ -f ${prefix}/drive_c/wine-patched/session_check.exe ]]; then
    print "Checking that the session counts as unlocked, as SteamVR asks"
    session_out=$(cd "${prefix}/drive_c/wine-patched" && "${wine_root}/bin/wine" ./session_check.exe 2>&1 || true)
    if [[ ${session_out} != *"session_check: PASS"* ]]; then
        print -u2 "FAIL: session_check.exe (run it by hand for details):"
        print -u2 -r -- "${session_out}"
        exit 1
    fi
fi
# The stamp comes last: a stack that failed a step above has none
if [[ -f ${version_file} ]]; then
    tr -d '[:space:]' < "${version_file}" > "${stack}/game-support-version.txt"
    print "Game support version $(<"${stack}/game-support-version.txt")"
fi
print "Wine stack ready: ${stack}"
print "Install games into the prefix, then start them with scripts/run-game.sh"
