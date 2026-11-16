#!/bin/zsh
# End-to-end run of cranvr's own Android app in its window display (quest/src/flat) on an
# Android device without an OpenXR runtime, such as a Galaxy Tab. The device's motion sensors
# give the head pose, and its screen shows the stream. The app logs two latency numbers from
# real display times every 2 seconds: the round trip (pose sample, to the Mac, to that frame on
# the display) and motion to display (the newest sensor sample, which warps the frame, to the
# display). This script prints their medians and writes them to a JSON report.
#
# Usage: scripts/e2e-flat.sh [--seconds N] [--serial SERIAL] [--host-arg ARG]... [--no-build]
#                            [--keep-tailscale] [--usb] [--warp-check] [--report FILE]
#
#   --seconds N       how long the stream runs (default 30)
#   --host-arg ARG    one more cranvr-host argument
#   --no-build        use the APK and the host from the last build
#   --usb             the host reaches the device through `adb forward` over USB, with a TCP
#                     stream, instead of over Wi-Fi
#   --warp-check      after the run, start the app twice more and check the picture with
#                     scripts/flat-warp-check.py: once with only the warp turned 10 degrees
#                     (debug.cranvr.warp_yaw_deg), once with the head turned 10 degrees in the
#                     world (debug.cranvr.world_yaw_deg)
#   --keep-tailscale  leave Tailscale connected on the device; the host then streams over TCP.
#                     Without it the script disconnects Tailscale for the run: a Tailscale subnet
#                     route for the local network sends the device's UDP through another device.
#
# The device must be awake and unlocked. The run holds the device lock of Cranpose's device
# scripts.

set -euo pipefail

root=${0:A:h:h}
seconds=30 serial="" build=1 keep_tailscale=0 usb=0 warp_check=0 report=""
host_args=()
other_args=()
while (( $# )); do
    case $1 in
        --seconds) seconds=$2; other_args+=("$1" "$2"); shift 2 ;;
        --serial) serial=$2; shift 2 ;;
        --host-arg) host_args+=("$2"); other_args+=("$1" "$2"); shift 2 ;;
        --no-build) build=0; other_args+=("$1"); shift ;;
        --keep-tailscale) keep_tailscale=1; other_args+=("$1"); shift ;;
        --usb) usb=1; keep_tailscale=1; other_args+=("$1"); shift ;;
        --warp-check) warp_check=1; other_args+=("$1"); shift ;;
        --report) report=$2; other_args+=("$1" "$2"); shift 2 ;;
        *) print -u2 "unknown option $1"; exit 2 ;;
    esac
done
serial=${serial:-$(adb devices | awk 'NR > 1 && $2 == "device" { print $1; exit }')}
if [[ -z ${CRANVR_DEVICE_LOCKED:-} ]]; then
    print "Waiting for the lock of ${serial}"
    CRANVR_DEVICE_LOCKED=1 exec python3 -I "${root}/scripts/with-device-lock.py" "${serial}" \
        "$0" --serial "${serial}" "${other_args[@]}"
fi

adb_cmd=(adb -s "${serial}")
work=${root}/target/e2e/flat-$(date +%Y%m%d-%H%M%S)
mkdir -p "${work}"
report=${report:-${work}/report.json}
if (( build )); then
    print "Building the host and the app (log: ${work}/build.log)"
    (cd "${root}" && cargo build --release -p cranvr-host > "${work}/build.log" 2>&1)
    "${root}/scripts/build-quest.sh" >> "${work}/build.log" 2>&1
fi
"${adb_cmd[@]}" install -r --no-incremental "${root}/target/quest-client/cranvr-quest.apk" > /dev/null
# ALVR asks for the microphone at start; a grant ahead of time skips the dialog
"${adb_cmd[@]}" shell pm grant dev.cranvr.client android.permission.RECORD_AUDIO

tailscale_paused=0
host_pid=""
cleanup() {
    "${adb_cmd[@]}" shell am force-stop dev.cranvr.client 2>/dev/null || true
    [[ -n ${host_pid} ]] && kill "${host_pid}" 2>/dev/null || true
    if (( usb )); then
        "${adb_cmd[@]}" forward --remove tcp:9943 2>/dev/null || true
        "${adb_cmd[@]}" forward --remove tcp:9944 2>/dev/null || true
    fi
    if (( tailscale_paused )); then
        "${adb_cmd[@]}" shell am broadcast -a com.tailscale.ipn.CONNECT_VPN -n com.tailscale.ipn/.IPNReceiver > /dev/null || true
        print "Tailscale on ${serial} connects again"
    fi
}
trap cleanup EXIT INT TERM

if (( usb )); then
    # The host connects to the app: control on TCP 9943, the stream on TCP 9944
    for port in 9943 9944; do
        "${adb_cmd[@]}" forward "tcp:${port}" "tcp:${port}" > /dev/null
    done
    host_args+=(--client-ip 127.0.0.1 --tcp)
elif (( keep_tailscale )); then
    host_args+=(--tcp)
elif "${adb_cmd[@]}" shell dumpsys connectivity | grep -q "VPN CONNECTED"; then
    print "Pausing Tailscale on ${serial}"
    tailscale_paused=1
    "${adb_cmd[@]}" shell am broadcast -a com.tailscale.ipn.DISCONNECT_VPN -n com.tailscale.ipn/.IPNReceiver > /dev/null
    for i in {1..20}; do
        "${adb_cmd[@]}" shell dumpsys connectivity | grep -q "VPN CONNECTED" || break
        sleep 0.5
    done
fi

# The host's eye width defaults to a Quest 3's. The window shows each eye in half the screen:
# 1400 pixels on a Galaxy Tab S7+.
if (( ! ${host_args[(Ie)--eye-width]} )); then
    host_args+=(--eye-width 1400)
fi
print "Starting the host (log: ${work}/host.log)"
# No game sound: the device would play the Mac's sound on its speakers
"${root}/target/release/cranvr-host" --status --fps 120 --trust-all --no-game-audio --config-dir "${work}/config" "${host_args[@]}" \
    > "${work}/host.out" 2> "${work}/host.log" &
host_pid=$!
sleep 2
"${adb_cmd[@]}" logcat -c
"${adb_cmd[@]}" shell input keyevent KEYCODE_WAKEUP
# Full screen, also when Samsung DeX would open the app in a window of its own
"${adb_cmd[@]}" shell am start --windowingMode 1 -n dev.cranvr.client/android.app.NativeActivity > /dev/null
print "Streaming for ${seconds} seconds"
sleep "${seconds}"
"${adb_cmd[@]}" exec-out screencap -p > "${work}/screen.png" || true
"${adb_cmd[@]}" logcat -d > "${work}/logcat.txt"
warp_result=0
# One more start of the app with a property at 10 degrees, and a screenshot
turned_screen() {
    "${adb_cmd[@]}" shell setprop "debug.cranvr.$1" 10
    "${adb_cmd[@]}" shell am force-stop dev.cranvr.client
    "${adb_cmd[@]}" shell am start --windowingMode 1 -n dev.cranvr.client/android.app.NativeActivity > /dev/null
    sleep 15
    "${adb_cmd[@]}" exec-out screencap -p > "${work}/screen-$1.png" || true
    "${adb_cmd[@]}" shell setprop "debug.cranvr.$1" 0
}
if (( warp_check )); then
    print "Checking the warp: only the display turns 10 degrees"
    turned_screen warp_yaw_deg
    python3 -I "${root}/scripts/flat-warp-check.py" "${work}/screen.png" "${work}/screen-warp_yaw_deg.png" 10 || warp_result=1
    print "Checking the head pose: the head turns 10 degrees in the world"
    turned_screen world_yaw_deg
    python3 -I "${root}/scripts/flat-warp-check.py" "${work}/screen.png" "${work}/screen-world_yaw_deg.png" 10 --world || warp_result=1
fi
cleanup
trap - EXIT INT TERM
python3 -I "${root}/scripts/e2e-flat-report.py" "${work}/logcat.txt" "${work}/host.out" "${report}" || exit 1
exit ${warp_result}
