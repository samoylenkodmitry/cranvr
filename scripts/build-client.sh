#!/bin/zsh
# Builds cranvr-client.apk: cranvr's Cranpose client for headsets (see client/Cargo.toml). The
# Cranpose Gradle plugin builds the Rust library with cargo-ndk, packs it with the OpenXR loader
# and adds the manifest entries of the `xr` service. Gradle takes the plugin and the Rust crates
# from the Cranpose git checkout that client/Cargo.toml names, so the first build needs the network.
#
# Usage: scripts/build-client.sh [--install [SERIAL]]
#   --install  installs the APK with adb, beside the old client (dev.cranvr.client) and the ALVR app
#
# Environment:
#   ANDROID_HOME      the Android SDK (default ~/Library/Android/sdk)
#   ANDROID_NDK_HOME  the NDK (default: the newest 28.x in the SDK, else the newest)
#
# The APK carries the application id dev.cranvr.client2. The Android debug key signs it, so later
# builds install over it.

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
ndk=${ANDROID_NDK_HOME:-$(print -l "${sdk}"/ndk/28.*(/N) | sort -V | tail -1)}
[[ -z ${ndk} ]] && ndk=$(print -l "${sdk}"/ndk/*(/N) | sort -V | tail -1)
for need in "${sdk}" "${ndk}"; do
    if [[ -z ${need} || ! -e ${need} ]]; then
        print -u2 "Missing the Android SDK or NDK (${need:-none})"
        exit 1
    fi
done
work=${root}/target/client
apk=${work}/cranvr-client.apk
mkdir -p "${work}"

print "Building the client with Gradle (log: ${work}/build.log)"
if ! (cd "${root}/client/android" && ANDROID_HOME=${sdk} ANDROID_NDK_HOME=${ndk} ./gradlew --no-daemon \
    :app:assembleRelease > "${work}/build.log" 2>&1); then
    tail -30 "${work}/build.log" >&2
    exit 1
fi
built=$(print -l "${root}"/client/android/app/build/outputs/apk/release/*.apk(N) | head -1)
if [[ -z ${built} ]]; then
    print -u2 "Gradle made no APK. See ${work}/build.log"
    exit 1
fi
cp "${built}" "${apk}"
print "Built ${apk}"

if (( install )); then
    adb_cmd=(adb)
    [[ -n ${serial} ]] && adb_cmd+=(-s "${serial}")
    "${adb_cmd[@]}" install -r "${apk}"
    print "Installed dev.cranvr.client2. Start it in the headset from the app library (Unknown Sources)."
fi
