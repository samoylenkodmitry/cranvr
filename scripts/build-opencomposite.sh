#!/bin/zsh
# Builds OpenComposite (GPLv3) for cranvr's game support: the program that turns a game's OpenVR calls
# into OpenXR calls to the BasaltVR runtime. The result is vrclient_x64.dll, a Windows x64 DLL for Wine.
#
# Usage: scripts/build-opencomposite.sh [DIR]   (default: third_party/opencomposite/src)
# Then:  scripts/package-game-support.sh
#
# It clones OpenComposite (https://gitlab.com/znixian/OpenOVR, branch openxr) into DIR at the pinned
# commit, takes the two submodules that a Windows build needs (the OpenXR SDK and glm), applies
# cranvr's patches from third_party/opencomposite/patches and builds with the MinGW-w64 cross
# toolchain that builds BasaltVR's runtime (brew install cmake ninja mingw-w64). The build goes to
# the build folder beside DIR. The clone is the only thing that this script runs code from: the build
# runs two of its Python scripts, which split the OpenVR headers and generate the interface stubs.

set -euo pipefail

readonly repo=https://gitlab.com/znixian/OpenOVR.git
readonly commit=cff07db75c4823afe93ed7027b03d5f7bc86f164
cranvr=${0:A:h:h}
dir=${1:-${cranvr}/third_party/opencomposite/src}
dir=${dir:A}
build=${dir:h}/build
support=${cranvr}/third_party/opencomposite

if [[ ! -d ${dir}/.git ]]; then
    git clone "${repo}" "${dir}"
fi
if [[ $(git -C "${dir}" rev-parse HEAD) != ${commit} ]]; then
    if ! git -C "${dir}" diff --quiet; then
        print -u2 "${dir} has local changes and is not at the pinned commit; refusing to touch it."
        exit 1
    fi
    git -C "${dir}" fetch origin
    git -C "${dir}" checkout --quiet "${commit}"
fi
# Only the OpenXR SDK and glm: libunwind is for Linux backtraces, and the Vulkan SDK is not used
git -C "${dir}" submodule update --init --depth 1 libs/openxr-sdk libs/glm

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

# The static libraries make the DLL stand alone: Wine has no libstdc++ or libgcc to give it
print "Building OpenComposite (MinGW, Release)"
cmake -S "${dir}" -B "${build}" -G Ninja \
    -DCMAKE_TOOLCHAIN_FILE="${support}/mingw-x86_64.cmake" \
    -DCMAKE_BUILD_TYPE=Release \
    -DOC_D3D_ONLY=ON \
    -DOC_VERSION="${commit[1,7]}-cranvr" \
    -DCMAKE_SHARED_LINKER_FLAGS="-static -static-libgcc -static-libstdc++" >/dev/null
cmake --build "${build}" --parallel

# The OpenVR test program of cranvr: it needs only OpenComposite's copy of Valve's OpenVR headers
print "Building openvr_smoke.exe"
x86_64-w64-mingw32-g++ -std=c++20 -O2 -Wall -s -static -static-libgcc -static-libstdc++ \
    -I"${dir}/OpenVRHeaders" -o "${build}/bin/openvr_smoke.exe" "${support}/tests/openvr_smoke.cpp" -ld3d11 -ldxgi

print "Done: ${build}/bin/vrclient_x64.dll and openvr_smoke.exe"
print "Next: scripts/package-game-support.sh"
