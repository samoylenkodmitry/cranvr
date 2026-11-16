#!/bin/zsh
# Builds cranvr-quest.apk: cranvr's Quest client (see quest/Cargo.toml). Everything comes from
# source on this Mac: the client from quest/, the Khronos OpenXR loader from the OpenXR SDK
# source in BasaltVR's build tree. The script downloads nothing that is not in the Cargo cache.
#
# Usage: scripts/build-quest.sh [--install [SERIAL]]
#   --install  installs the APK with adb on a Quest attached by USB, beside the store ALVR app
#
# Environment:
#   ANDROID_HOME  the Android SDK (default ~/Library/Android/sdk)
#   OPENXR_SDK    the OpenXR SDK source (default: the one in BasaltVR's runtime build)
#
# The APK is signed with the Android debug key in ~/.android/debug.keystore, so later builds
# install over it.

set -euo pipefail

root=${0:A:h:h}
install=0 serial=""
while (( $# )); do
    case $1 in
        --install)
            install=1; shift
            if (( $# )) && [[ $1 != --* ]]; then serial=$1; shift; fi ;;
        *) print -u2 "unknown option $1"; exit 2 ;;
    esac
done

sdk=${ANDROID_HOME:-${HOME}/Library/Android/sdk}
ndk=$(print -l "${sdk}"/ndk/*(/N) | sort -V | tail -1)
tools=$(print -l "${sdk}"/build-tools/*(/N) | sort -V | tail -1)
platform=$(print -l "${sdk}"/platforms/android-<34->(/N) | sort -V | head -1)
openxr_sdk=${OPENXR_SDK:-${root}/third_party/basaltvr/src/runtime/build/_deps/openxr_sdk-src}
work=${root}/target/quest-client
apk=${work}/cranvr-quest.apk
keystore=${HOME}/.android/debug.keystore

for need in "${ndk}" "${tools}/aapt2" "${platform}/android.jar" "${openxr_sdk}/src/loader" "${keystore}"; do
    if [[ ! -e ${need} ]]; then
        print -u2 "Missing ${need}"
        exit 1
    fi
done
mkdir -p "${work}"

print "Building the OpenXR loader from ${openxr_sdk}"
loader_build=${work}/openxr-loader
if [[ ! -f ${loader_build}/build.ninja ]]; then
    cmake -S "${openxr_sdk}" -B "${loader_build}" -G Ninja \
        -DCMAKE_TOOLCHAIN_FILE="${ndk}/build/cmake/android.toolchain.cmake" \
        -DANDROID_ABI=arm64-v8a -DANDROID_PLATFORM=29 -DCMAKE_BUILD_TYPE=Release \
        -DBUILD_TESTS=OFF -DBUILD_API_LAYERS=OFF -DBUILD_CONFORMANCE_TESTS=OFF > "${work}/loader-cmake.log"
fi
ninja -C "${loader_build}" openxr_loader > "${work}/loader-build.log"

print "Building the client (log: ${work}/build.log)"
if ! (cd "${root}/quest" && ANDROID_HOME=${sdk} ANDROID_NDK_HOME=${ndk} cargo ndk -t arm64-v8a --platform 29 \
    build --release --offline > "${work}/build.log" 2>&1); then
    tail -20 "${work}/build.log" >&2
    exit 1
fi

print "Packaging ${apk}"
stage=${work}/stage
rm -rf "${stage}"
mkdir -p "${stage}/lib/arm64-v8a"
cp "${root}/quest/target/aarch64-linux-android/release/libcranvr_quest.so" "${stage}/lib/arm64-v8a/"
cp "${loader_build}/src/loader/libopenxr_loader.so" "${stage}/lib/arm64-v8a/"
"${ndk}/toolchains/llvm/prebuilt/darwin-x86_64/bin/llvm-strip" "${stage}/lib/arm64-v8a/libopenxr_loader.so"
"${tools}/aapt2" compile --dir "${root}/quest/resources" -o "${work}/resources.zip"
"${tools}/aapt2" link -o "${work}/unaligned.apk" -I "${platform}/android.jar" \
    --manifest "${root}/quest/AndroidManifest.xml" \
    --min-sdk-version 26 --target-sdk-version 32 --version-code 20140100 --version-name 20.14.1 \
    "${work}/resources.zip"
(cd "${stage}" && zip -q -r "${work}/unaligned.apk" lib)
"${tools}/zipalign" -f -P 16 4 "${work}/unaligned.apk" "${work}/aligned.apk"
"${tools}/apksigner" sign --ks "${keystore}" --ks-pass pass:android --ks-key-alias androiddebugkey \
    --out "${apk}" "${work}/aligned.apk"
"${tools}/apksigner" verify "${apk}"
print "Built ${apk}"

if (( install )); then
    adb_cmd=(adb)
    [[ -n ${serial} ]] && adb_cmd+=(-s "${serial}")
    "${adb_cmd[@]}" install -r "${apk}"
    print "Installed dev.cranvr.client. Start it in the headset from the app library (Unknown Sources)."
fi
