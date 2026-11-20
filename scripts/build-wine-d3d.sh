#!/bin/zsh
# Builds Wine's Direct3D shader code for cranvr's game support: wined3d.dll of Wine 11.10 with cranvr's
# patches. wined3d.dll holds vkd3d-shader, the HLSL compiler that Wine's d3dcompiler_47.dll calls. A
# game that compiles HLSL at run time (most Direct3D 11 games do) goes through it, so a missing
# intrinsic there makes the game's shader fail. Wine's license is LGPL 2.1.
#
# Usage: scripts/build-wine-d3d.sh [DIR]   (default: third_party/wine/src)
# Then:  scripts/package-game-support.sh
#
# It clones Wine at the tag wine-11.10 (commit pinned below) into DIR, applies cranvr's patches from
# third_party/wine/patches and builds wined3d.dll as a 64-bit PE file with the
# MinGW-w64 cross compiler that builds BasaltVR's runtime. Wine's build needs a 64-bit Intel host
# to configure, so the script runs configure and make under Rosetta 2 (arch -x86_64). The build goes
# to the build folder beside DIR. The results are build/bin/wined3d.dll (stripped) and build/bin/hlsl_compile.exe,
# cranvr's test program for the HLSL compiler.
#
# Needs: brew install mingw-w64 bison (Apple's bison is too old; Homebrew's is keg-only and found here),
# flex and Rosetta 2 (softwareupdate --install-rosetta).

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

mkdir -p "${build}/wine"
export PATH=${bison_dir}:${PATH}
if [[ ! -f ${build}/wine/Makefile ]]; then
    print "Configuring Wine (PE files only)"
    # The Mac side of this Wine is never used: the game support keeps Gcenx's Wine and takes only the PE
    # files from this build. So every optional library is off.
    (cd "${build}/wine" && CC="clang -arch x86_64" CXX="clang++ -arch x86_64" arch -x86_64 "${dir}/configure" \
        --enable-win64 --with-mingw --without-x --without-freetype --without-gnutls --without-sdl \
        --without-pulse --without-alsa --without-cups --without-gstreamer --without-vulkan --without-opencl \
        --without-pcap --without-usb --without-v4l2 --without-gphoto --without-sane --without-krb5 \
        --without-unwind --disable-tests > configure.log 2>&1) || { tail -20 "${build}/wine/configure.log" >&2; exit 1; }
fi

print "Building wined3d.dll (MinGW, under Rosetta 2)"
(cd "${build}/wine" && arch -x86_64 make -j"$(sysctl -n hw.ncpu)" dlls/wined3d/all > make.log 2>&1) \
    || { tail -30 "${build}/wine/make.log" >&2; exit 1; }

mkdir -p "${build}/bin"
x86_64-w64-mingw32-strip -o "${build}/bin/wined3d.dll" "${build}/wine/dlls/wined3d/x86_64-windows/wined3d.dll"

# cranvr's test program for the HLSL compiler
print "Building hlsl_compile.exe"
x86_64-w64-mingw32-g++ -std=c++17 -O2 -Wall -s -static -static-libgcc -static-libstdc++ \
    -o "${build}/bin/hlsl_compile.exe" "${support}/tests/hlsl_compile.cpp"

cp "${support}/tests/isnan.hlsl" "${build}/bin/"

print "Done: ${build}/bin/wined3d.dll and hlsl_compile.exe"
print "Next: scripts/package-game-support.sh"
