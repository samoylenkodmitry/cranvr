#!/bin/zsh
# Builds the Windows side of cranvr's game support from BasaltVR (MIT), with cranvr's patches:
#   - the OpenXR runtime that games load under Wine
#   - the DXMT fork that shares game images with cranvr-host as IOSurfaces
#
# Usage: scripts/build-basaltvr.sh [DIR]   (default: third_party/basaltvr/src)
# Then:  scripts/setup-wine.sh DIR
#
# Needs: Xcode with the Metal toolchain (xcodebuild -downloadComponent MetalToolchain), Rosetta 2,
# and brew install cmake meson ninja mingw-w64. The first run downloads Wine 11.10, DXMT 0.80 and
# LLVM 15 sources (pinned and checked by BasaltVR's scripts) and builds LLVM: about an hour.

set -euo pipefail

readonly repo=https://github.com/Stooovie/BasaltVR.git
readonly commit=6116e59124ef0f11ba157571b6f44f901eb6ba59
cranvr=${0:A:h:h}
dir=${1:-${cranvr}/third_party/basaltvr/src}
dir=${dir:A}
patches=${cranvr}/third_party/basaltvr/patches

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

# Runtime patches: apply once (a second run finds them applied). The series builds on itself: a
# later patch may change lines that an earlier one added, and the earlier one then no longer
# reverses on its own. The newest patch that reverses cleanly is applied, and so are all before it.
patch_files=("${patches}"/runtime/*.patch)
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
# DXMT patches join BasaltVR's own; its fork script applies the whole set
cp "${patches}"/dxmt/*.patch "${dir}/runtime/dxmt-fork/patches/"

cd "${dir}"
# Meson wraps the Mac compiler in sccache when it finds one, and sccache drops -ObjC from DXMT's
# Mac sources. Name the compilers, keep sccache out of PATH, and drop a build dir that cached it.
export PATH=$(print -r -- "${PATH}" | tr ':' '\n' | grep -v '\.cargo/bin' | paste -sd: -)
export CC_FOR_BUILD=cc CXX_FOR_BUILD=c++
meson_dir=${dir}/runtime/build/dxmt/src/build
if grep -qs sccache "${meson_dir}/meson-logs/meson-log.txt"; then
    rm -rf "${meson_dir}"
fi

print "Building the runtime (MinGW)"
cmake -S runtime --preset mingw-release >/dev/null
cmake --build runtime/build --parallel

print "Downloading Wine 11.10 and DXMT 0.80, checking them"
runtime/scripts/test-dxmt.zsh

print "Building the DXMT fork"
runtime/scripts/build-dxmt-fork.zsh

print "Done. Next: scripts/setup-wine.sh ${dir}"
