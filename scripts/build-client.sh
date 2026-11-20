#!/bin/zsh
# Builds cranvr-client.apk: cranvr's Cranpose client for headsets (see client/Cargo.toml). The
# Cranpose Gradle plugin builds the Rust library with cargo-ndk, packs it with the OpenXR loader
# and adds the manifest entries of the `xr` service. Gradle takes the plugin and the Rust crates
# from the Cranpose git checkout that client/Cargo.toml names, so the first build needs the network.
#
# Usage: scripts/build-client.sh [--phone] [--scripted-hand] [--xr-store STORE] [--install [SERIAL]]
#   --phone    builds cranvr-phone.apk instead: the same crate as a flat app for an Android phone or
#              tablet, with no OpenXR. The screen shows the picture and the motion sensors are the
#              head. It has the application id dev.cranvr.client.phone, so it installs beside the
#              headset's APK.
#   --scripted-hand  builds cranvr-client-scripted.apk with a hand that types into the address field
#              (client/src/scripted.rs), for a headset on a desk. It has the id of the plain app and installs
#              over it. Install the plain build after the test.
#   --xr-store STORE  the store whose features the manifest carries: meta (the default of the Cranpose
#              plugin), android-xr or open. The APK is named cranvr-client-STORE.apk, so a build
#              for the Android XR emulator keeps the Quest's APK.
#   --install  installs the APK with adb
#
# Environment:
#   ANDROID_HOME      the Android SDK (default ~/Library/Android/sdk)
#   ANDROID_NDK_HOME  the NDK (default: the newest 28.x in the SDK, else the newest)
#
# The APK carries the application id dev.cranvr.client (dev.cranvr.client.phone with --phone).
# The Android debug key signs it, so later builds install over it.

set -euo pipefail

root=${0:A:h:h}
install=0 serial="" phone=0 store="" scripted=0
while (( $# )); do
    case $1 in
        --phone) phone=1; shift ;;
        --scripted-hand) scripted=1; shift ;;
        --xr-store) store=$2; shift 2 ;;
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
if (( phone )); then
    name=cranvr-phone gradle_args=(-PcranvrPhone=true) app_id=dev.cranvr.client.phone
else
    name=cranvr-client gradle_args=() app_id=dev.cranvr.client
fi
if (( scripted )); then
    gradle_args+=(-PcranvrScriptedHand=true)
    name=${name}-scripted
fi
if [[ -n ${store} ]]; then
    gradle_args+=("-PcranposeXrStore=${store}")
    name=${name}-${store}
fi
apk=${work}/${name}.apk
mkdir -p "${work}"

print "Building ${name} with Gradle (log: ${work}/${name}.log)"
if ! (cd "${root}/client/android" && ANDROID_HOME=${sdk} ANDROID_NDK_HOME=${ndk} ./gradlew --no-daemon \
    "${gradle_args[@]}" :app:assembleRelease > "${work}/${name}.log" 2>&1); then
    tail -30 "${work}/${name}.log" >&2
    exit 1
fi
built=$(print -l "${root}"/client/android/app/build/outputs/apk/release/*.apk(N) | head -1)
if [[ -z ${built} ]]; then
    print -u2 "Gradle made no APK. See ${work}/${name}.log"
    exit 1
fi
cp "${built}" "${apk}"
print "Built ${apk}"

if (( install )); then
    adb_cmd=(adb)
    [[ -n ${serial} ]] && adb_cmd+=(-s "${serial}")
    "${adb_cmd[@]}" install -r "${apk}"
    if (( phone )); then
        print "Installed ${app_id}. Start \"cranvr phone\" from the launcher."
    else
        print "Installed ${app_id}. Start it in the headset from the app library (Unknown Sources)."
    fi
fi
