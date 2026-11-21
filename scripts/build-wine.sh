#!/bin/zsh
# Builds the files of Wine 11.10 that cranvr's game support replaces in Gcenx's macOS build, with cranvr's patches
# from third_party/wine/patches. Wine's license is LGPL 2.1.
#   - wined3d.dll holds vkd3d-shader, the HLSL compiler that Wine's d3dcompiler_47.dll calls. A game that compiles
#     HLSL at run time (most Direct3D 11 games do) goes through it, so a missing intrinsic there makes the game's
#     shader fail.
#   - winemac.so, the Mac side of Wine's Mac driver: it makes a core context of OpenGL when a program asks for the
#     compatibility profile of 3.2 or newer, which macOS does not have.
#   - opengl32.so, the Mac side of Wine's opengl32: it compiles the legacy GLSL of such a program in a core context.
#   - wininet.dll: it closes the handles of a program with requests on several threads without a race.
#   - ntdll.so, the Mac side of ntdll: it gives a program with high-entropy addresses its memory above 4 GB, as
#     Windows does. The detours of MonoMod (Harmony in Unity mods) need free pages near their code. Thread stacks
#     stay below 4 GB, under the stacks of Wine's Mac side.
#
# Usage: scripts/build-wine.sh [DIR]   (default: third_party/wine/src)
# Then:  scripts/package-game-support.sh
#
# It clones Wine at the tag wine-11.10 (commit pinned below) into DIR, applies cranvr's patches from
# third_party/wine/patches and builds the five files. Windows files are 64-bit PE files from the MinGW-w64 cross
# compiler that builds BasaltVR's runtime. Mac files are x86_64 libraries for Rosetta 2 with macOS 10.15 as the
# oldest system, as Gcenx's build has. Wine's build needs a 64-bit Intel host to configure, so the script runs
# configure and make under Rosetta 2 (arch -x86_64). The build goes to the build folder beside DIR. The results are
# in build/bin: wined3d.dll and wininet.dll (stripped), winemac.so, opengl32.so, ntdll.so, and cranvr's test programs
# hlsl_compile.exe (the HLSL compiler), gl_compat_check.exe (an OpenGL program like Google Earth VR),
# far_jump_check.exe (MonoMod's far jump) and stack_sample_check.exe (a sampling profiler's stack copy).
#
# Needs: brew install mingw-w64 bison (Apple's bison is too old; Homebrew's is keg-only and found here),
# flex, Xcode's command line tools and Rosetta 2 (softwareupdate --install-rosetta).

set -euo pipefail

readonly repo=https://gitlab.winehq.org/wine/wine.git
readonly tag=wine-11.10
readonly commit=2cac6ccf33c0807f374dc96f5a20e35a2da86157
cranvr=${0:A:h:h}
dir=${1:-${cranvr}/third_party/wine/src}
dir=${dir:A}
build=${dir:h}/build
support=${cranvr}/third_party/wine

for tool in git make flex x86_64-w64-mingw32-gcc x86_64-w64-mingw32-g++ x86_64-w64-mingw32-strip; do
    if ! command -v ${tool} >/dev/null; then
        print -u2 "Missing ${tool}. Install it with Homebrew (brew install mingw-w64) and try again."
        exit 1
    fi
done
bison_dir=/opt/homebrew/opt/bison/bin
if [[ ! -x ${bison_dir}/bison ]]; then
    print -u2 "Missing Homebrew's bison: brew install bison"
    exit 1
fi
if ! arch -x86_64 /usr/bin/true 2>/dev/null; then
    print -u2 "Rosetta 2 is missing: softwareupdate --install-rosetta"
    exit 1
fi

if [[ ! -d ${dir}/.git ]]; then
    git clone --depth 1 --branch "${tag}" "${repo}" "${dir}"
fi
if [[ $(git -C "${dir}" rev-parse HEAD) != ${commit} ]]; then
    if ! git -C "${dir}" diff --quiet; then
        print -u2 "${dir} has local changes and is not at the pinned commit; refusing to touch it."
        exit 1
    fi
    git -C "${dir}" fetch --depth 1 origin "${commit}"
    git -C "${dir}" checkout --quiet "${commit}"
fi

# Patches: a second run finds them applied
for patch in "${support}"/patches/*.patch; do
    if git -C "${dir}" apply --reverse --check "${patch}" 2>/dev/null; then
        print "Already applied: ${patch:t}"
    elif git -C "${dir}" apply --check "${patch}" 2>/dev/null; then
        git -C "${dir}" apply "${patch}"
        print "Applied ${patch:t}"
    else
        print -u2 "FAIL: ${patch:t} does not apply to ${dir}"
        exit 1
    fi
done

# The Mac files run on macOS 10.15 and newer, as the rest of Gcenx's build does. A build folder configured with
# other options is configured again.
export MACOSX_DEPLOYMENT_TARGET=10.15
mkdir -p "${build}/wine"
export PATH=${bison_dir}:${PATH}
configure_args=(--enable-win64 --with-mingw --without-x --without-freetype --without-gnutls --without-sdl
    --without-pulse --without-alsa --without-cups --without-gstreamer --without-vulkan --without-opencl
    --without-pcap --without-usb --without-v4l2 --without-gphoto --without-sane --without-krb5
    --disable-tests)
configure_line="MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET} ${configure_args[*]}"
if [[ ! -f ${build}/wine/Makefile || $(cat "${build}/wine/configure-line.txt" 2>/dev/null) != "${configure_line}" ]]; then
    # Objects of another configuration would stay: start from an empty folder
    rm -rf "${build}/wine"
    mkdir -p "${build}/wine"
    print "Configuring Wine"
    # Only the five files of this build are used: the game support keeps the rest of Gcenx's Wine. So every
    # optional library is off. winemac.so and opengl32.so need none of them. ntdll.so unwinds with the system's
    # unwinder, as Gcenx's does, so configure keeps it.
    (cd "${build}/wine" && CC="clang -arch x86_64" CXX="clang++ -arch x86_64" arch -x86_64 "${dir}/configure" \
        "${configure_args[@]}" > configure.log 2>&1) || { tail -20 "${build}/wine/configure.log" >&2; exit 1; }
    print -r -- "${configure_line}" > "${build}/wine/configure-line.txt"
fi

print "Building wined3d.dll, wininet.dll, winemac.so, opengl32.so and ntdll.so (under Rosetta 2)"
(cd "${build}/wine" && arch -x86_64 make -j"$(sysctl -n hw.ncpu)" dlls/wined3d/all dlls/wininet/all \
    dlls/winemac.drv/all dlls/opengl32/all dlls/ntdll/all > make.log 2>&1) || { tail -30 "${build}/wine/make.log" >&2; exit 1; }

mkdir -p "${build}/bin"
x86_64-w64-mingw32-strip -o "${build}/bin/wined3d.dll" "${build}/wine/dlls/wined3d/x86_64-windows/wined3d.dll"
x86_64-w64-mingw32-strip -o "${build}/bin/wininet.dll" "${build}/wine/dlls/wininet/x86_64-windows/wininet.dll"
cp "${build}/wine/dlls/winemac.drv/winemac.so" "${build}/wine/dlls/opengl32/opengl32.so" "${build}/wine/dlls/ntdll/ntdll.so" \
    "${build}/bin/"
# ntdll.so unwinds through the Mac side with the system's unwinder, as the ntdll.so of Gcenx's build does
if ! nm -u "${build}/bin/ntdll.so" | grep -q _Unwind_Find_FDE; then
    print -u2 "FAIL: ntdll.so is built without the system's unwinder"
    exit 1
fi
for lib in winemac.so opengl32.so ntdll.so; do
    minos=$(otool -l "${build}/bin/${lib}" | awk '/LC_BUILD_VERSION/ { found = 1 } found && /minos/ { print $2; exit }')
    if [[ ${minos} != ${MACOSX_DEPLOYMENT_TARGET} ]]; then
        print -u2 "FAIL: ${lib} is built for macOS ${minos}, not ${MACOSX_DEPLOYMENT_TARGET}"
        exit 1
    fi
done

# cranvr's test programs for the HLSL compiler, for OpenGL, for MonoMod's far jump and for a profiler's stack copy
print "Building hlsl_compile.exe, gl_compat_check.exe, far_jump_check.exe and stack_sample_check.exe"
x86_64-w64-mingw32-g++ -std=c++17 -O2 -Wall -s -static -static-libgcc -static-libstdc++ \
    -o "${build}/bin/hlsl_compile.exe" "${support}/tests/hlsl_compile.cpp"
x86_64-w64-mingw32-g++ -std=c++17 -O2 -Wall -s -static -static-libgcc -static-libstdc++ \
    -o "${build}/bin/gl_compat_check.exe" "${support}/tests/gl_compat_check.cpp" -lopengl32 -lgdi32
# Programs with high-entropy addresses, as Unity games and Steam are: MinGW's linker marks them so by default
for check in far_jump_check stack_sample_check; do
    x86_64-w64-mingw32-g++ -std=c++17 -O2 -Wall -s -static -static-libgcc -static-libstdc++ -Wl,--dynamicbase \
        -Wl,--high-entropy-va -o "${build}/bin/${check}.exe" "${support}/tests/${check}.cpp"
done

cp "${support}/tests/isnan.hlsl" "${build}/bin/"

print "Done: ${build}/bin"
print "Next: scripts/package-game-support.sh"
