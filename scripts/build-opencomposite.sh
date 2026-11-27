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
#
# Vulkan games: the build takes Khronos' Vulkan headers (downloaded at a pinned commit and checked by
# their SHA-256) and a delay-load import library of Wine's vulkan-1.dll, which dlltool of MinGW-w64 makes
# from third_party/opencomposite/vulkan-1.def. The upstream build takes both from the Vulkan SDK for Windows.

set -euo pipefail

readonly repo=https://gitlab.com/znixian/OpenOVR.git
readonly commit=cff07db75c4823afe93ed7027b03d5f7bc86f164
# Khronos Vulkan-Headers v1.4.350, as BasaltVR's runtime and cranvr's Wine build take them
readonly vulkan_headers_url=https://github.com/KhronosGroup/Vulkan-Headers/archive/8864cdc896bbc2a9b6eb36b3218fc9ef57908d77.tar.gz
readonly vulkan_headers_sha256=35824117f649befc664a12622bc801eddbba036ac03d906db400ee1c1e7903e9
# Valve's OpenVR header of SDK 2.15.6 (tag v2.15.6, BSD 3-Clause). The pinned OpenComposite bundles the headers up to
# SDK 2.5.1. A game built with a newer SDK, such as Half-Life: Alyx, asks for IVRSystem_026 and other newer interfaces.
# Patch 0013 adds the SDK to OpenComposite's list of headers.
readonly openvr_header_url=https://raw.githubusercontent.com/ValveSoftware/openvr/0924064316de3effbcd1acf1e309182a2deb1c05/headers/openvr.h
readonly openvr_header_sha256=1e6ed57199896cc1f7c5484e50fa18955e97be15be690beb28d998c877ead7fd
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

openvr_header=${dir}/OpenVRHeaders/openvr-2.15.6.h
if [[ ! -f ${openvr_header} ]]; then
    curl -sSfL -o "${openvr_header}" "${openvr_header_url}"
fi
if [[ $(shasum -a 256 "${openvr_header}" | awk '{print $1}') != ${openvr_header_sha256} ]]; then
    print -u2 "FAIL: ${openvr_header} from ${openvr_header_url} has another SHA-256"
    exit 1
fi

# Patches: a second run finds them applied. The series builds on itself: a later patch may change lines that an
# earlier one added, and the earlier one then no longer reverses on its own. The newest patch that reverses
# cleanly is applied, and so are all before it.
patch_files=("${support}"/patches/*.patch)
applied=0
for (( i = ${#patch_files}; i >= 1; i-- )); do
    if git -C "${dir}" apply --reverse --check "${patch_files[i]}" 2>/dev/null; then
        applied=${i}
        break
    fi
done
for (( i = 1; i <= ${#patch_files}; i++ )); do
    patch=${patch_files[i]}
    if (( i <= applied )); then
        print "Already applied: ${patch:t}"
    elif git -C "${dir}" apply --check "${patch}" 2>/dev/null; then
        git -C "${dir}" apply "${patch}"
        print "Applied ${patch:t}"
    else
        print -u2 "FAIL: ${patch:t} does not apply to ${dir}"
        exit 1
    fi
done

# The Vulkan headers and the import library of vulkan-1.dll
vulkan_headers=${build}/vulkan-headers
if [[ ! -f ${vulkan_headers}/include/vulkan/vulkan.h ]]; then
    mkdir -p "${vulkan_headers}"
    curl -sSfL -o "${build}/vulkan-headers.tar.gz" "${vulkan_headers_url}"
    if [[ $(shasum -a 256 "${build}/vulkan-headers.tar.gz" | awk '{print $1}') != ${vulkan_headers_sha256} ]]; then
        print -u2 "FAIL: the Vulkan headers from ${vulkan_headers_url} have another SHA-256"
        exit 1
    fi
    tar -xzf "${build}/vulkan-headers.tar.gz" -C "${vulkan_headers}" --strip-components 1
fi
mkdir -p "${build}"
# A delay-load library: a Direct3D game never loads vulkan-1.dll, a Vulkan game loads it at its first call here
x86_64-w64-mingw32-dlltool -d "${support}/vulkan-1.def" -y "${build}/libvulkan-1.a" -D vulkan-1.dll

# The DLL's log and abort lines name their source files (__FILE__). The map writes them relative to the source and build
# folders, so the DLL holds no path of the Mac that built it. CMake reads these variables at the first configure of a folder.
export CFLAGS="-ffile-prefix-map=${dir}=OpenComposite -ffile-prefix-map=${build}=build"
export CXXFLAGS=${CFLAGS}
# The static libraries make the DLL stand alone: Wine has no libstdc++ or libgcc to give it
print "Building OpenComposite (MinGW, Release)"
cmake -S "${dir}" -B "${build}" -G Ninja \
    -DCMAKE_TOOLCHAIN_FILE="${support}/mingw-x86_64.cmake" \
    -DCMAKE_BUILD_TYPE=Release \
    -DOC_D3D_ONLY=ON \
    -DOC_VULKAN_INCLUDE_DIR="${vulkan_headers}/include" \
    -DOC_VULKAN_LIBRARY="${build}/libvulkan-1.a" \
    -DOC_VERSION="${commit[1,7]}-cranvr" \
    -DCMAKE_SHARED_LINKER_FLAGS="-static -static-libgcc -static-libstdc++" >/dev/null
cmake --build "${build}" --parallel

# The OpenVR test program of cranvr: it needs only OpenComposite's copy of Valve's OpenVR headers
print "Building openvr_smoke.exe"
x86_64-w64-mingw32-g++ -std=c++20 -O2 -Wall -s -static -static-libgcc -static-libstdc++ \
    -DVK_NO_PROTOTYPES -I"${vulkan_headers}/include" \
    -I"${dir}/OpenVRHeaders" -o "${build}/bin/openvr_smoke.exe" "${support}/tests/openvr_smoke.cpp" -ld3d11 -ldxgi -lopengl32 -lgdi32

print "Done: ${build}/bin/vrclient_x64.dll and openvr_smoke.exe"
print "Next: scripts/package-game-support.sh"
