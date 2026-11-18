#!/bin/zsh
# End-to-end test of the stream from this Mac to a client. It runs cranvr-host, a client and an
# optional game, then writes a JSON report: every 5-second stats line of the host (fps, render and
# encode time, the client's latency split by stage) and the median and worst of each.
#
# Usage: scripts/e2e.sh [--seconds N] [--fps HZ] [--game | --wine PROGRAM [--wine-arg ARG]...] [--desktop]
#                       [--android [SERIAL] [--wifi] [--pause-tailscale] | --quest [SERIAL] [--cranvr-client]
#                        | --native [--native-tcp] [--native-drop PERCENT] [--native-port PORT]]
#                       [--host-arg ARG]... [--report FILE]
#
#   --seconds N   how long the stream runs (default 20)
#   --fps HZ      the stream's refresh rate (default 120, as the host). The probe also offers 144,
#                 165, 180 and 240 Hz, which measure the room left above a Quest 3's 120 Hz
#   --game        a fake game sends the frames through the BasaltVR runtime protocol
#   --wine PROGRAM a Windows OpenXR program runs in cranvr's Wine stack without sound, through
#                 scripts/run-game.sh: a Windows path such as 'C:\basalt\runtime\d3d11_runtime_smoke.exe'.
#                 The probe then counts every frame of the right size, since the program draws no
#                 eye badges. --wine-arg passes one argument to the program.
#   --desktop     the host shows this Mac's screen. The probe then counts every frame of the right
#                 size, since the desktop scene draws no eye badges. --host-arg --virtual-display
#                 --host-arg 1920x1080 shows a virtual display instead of the main one.
#   --android     the client is cranvr-probe on an Android phone attached by USB: ALVR's client
#                 core with its MediaCodec decoder and a display loop at the refresh
#                 rate. The host reaches it through `adb forward` over USB. The serial comes from
#                 `adb devices`; without one the first device on Android 10 or newer is used.
#   --wifi        with --android: the host reaches the phone over Wi-Fi with a UDP stream, as a
#                 Quest connects. The phone and the Mac must share a network; adb only starts
#                 the probe.
#   --pause-tailscale with --android: disconnects Tailscale on the device for the run and connects
#                 it again at the end. A device that accepts a Tailscale subnet route for its own
#                 network sends its UDP through the subnet router, which changes the source
#                 address: ALVR then drops every pose. An app can be excluded from Tailscale; the
#                 probe runs as the shell user and cannot.
#   --quest       the client is the ALVR app on a Quest attached by USB. The Quest and the Mac
#                 must share a network.
#   --cranvr-client with --quest: the client is cranvr's Cranpose client (dev.cranvr.client2, see
#                 scripts/build-client.sh) instead of the store ALVR app. The host then runs with
#                 --protocol native --trust-all on --native-port. The app gets this Mac's address
#                 as a launch extra. The run keeps the host's key in
#                 target/e2e/host-identity.txt: a headset refuses a known address and port with
#                 a new key. Give a --native-port that the headset has not used with another
#                 host at this Mac's address, such as 9961.
#                 Without --quest or --android the client is cranvr-probe on this Mac, over
#                 loopback TCP.
#   --native      cranvr's own protocol instead of ALVR's: the host runs with --protocol native
#                 and cranvr-probe connects to it over loopback, with UDP media and parity packets.
#                 The report's latency block comes from the headset's frame timing reports. It adds
#                 the loss, the parity and the clock round trip. It cannot combine with --android
#                 or --quest. It has no game sound and no desktop layer yet.
#   --native-tcp  with --native: the media goes over TCP, as it does over USB with `adb reverse`
#   --native-drop PERCENT  with --native: the probe drops this share of the media packets, so the
#                 parity has loss to recover
#   --native-port PORT  with --native: the host's TCP port (default 9950)
#   --host-arg ARG one more cranvr-host argument, for an A/B run of a setting:
#                 --host-arg --buffering-frames --host-arg 1
#   --report FILE where the report goes (default: target/e2e/run-<time>/report.json)
#
# A phone run holds the device lock of Cranpose's device scripts, so it never shares the phone
# with a Cranpose robot or benchmark run. The script exits with 0 when the median fps reached
# 90 percent of the target and the client saw the right picture. It exits with 1 otherwise.

set -euo pipefail

root=${0:A:h:h}
seconds=20 fps=120 game=0 desktop=0 client=mac serial="" report="" wifi=0 pause_tailscale=0
wine="" wine_args=() cranvr_client=0
native=0 native_tcp=0 native_drop=0 native_port=9950
more_host_args=()
# The arguments other than the client, for the run under a device lock
other_args=()
while (( $# )); do
    case $1 in
        --seconds) seconds=$2; other_args+=("$1" "$2"); shift 2 ;;
        --fps) fps=$2; other_args+=("$1" "$2"); shift 2 ;;
        --game) game=1; other_args+=("$1"); shift ;;
        --desktop) desktop=1; other_args+=("$1"); shift ;;
        --wine) wine=$2; other_args+=("$1" "$2"); shift 2 ;;
        --wine-arg) wine_args+=("$2"); other_args+=("$1" "$2"); shift 2 ;;
        --cranvr-client) cranvr_client=1; other_args+=("$1"); shift ;;
        --native) native=1; other_args+=("$1"); shift ;;
        --native-tcp) native_tcp=1; other_args+=("$1"); shift ;;
        --native-drop) native_drop=$2; other_args+=("$1" "$2"); shift 2 ;;
        --native-port) native_port=$2; other_args+=("$1" "$2"); shift 2 ;;
        --wifi) wifi=1; other_args+=("$1"); shift ;;
        --pause-tailscale) pause_tailscale=1; other_args+=("$1"); shift ;;
        --android|--quest)
            client=${1#--}; shift
            if (( $# )) && [[ $1 != --* ]]; then serial=$1; shift; fi ;;
        --host-arg) more_host_args+=("$2"); other_args+=("$1" "$2"); shift 2 ;;
        --report) report=$2; other_args+=("$1" "$2"); shift 2 ;;
        *) print -u2 "unknown option $1"; exit 2 ;;
    esac
done

if (( native )) && [[ ${client} != mac ]]; then
    print -u2 "--native runs the probe on this Mac: it cannot combine with --android or --quest."
    exit 2
fi

# A device: its serial from the arguments, or the first that passes `fits`
pick_device() {
    local s
    for s in $(adb devices | awk 'NR > 1 && $2 == "device" { print $1 }'); do
        if "$1" "${s}"; then
            print -r -- "${s}"
            return
        fi
    done
}
is_quest() { adb -s "$1" shell getprop ro.product.model | grep -qi quest }
is_recent_android() { (( $(adb -s "$1" shell getprop ro.build.version.sdk | tr -d '\r') >= 29 )) }

if [[ ${client} == android && -z ${CRANVR_DEVICE_LOCKED:-} ]]; then
    serial=${serial:-$(pick_device is_recent_android)}
    if [[ -z ${serial} ]]; then
        print -u2 "No Android 10 or newer device in adb devices."
        exit 1
    fi
    print "Waiting for the lock of ${serial}"
    CRANVR_DEVICE_LOCKED=1 exec python3 -I "${root}/scripts/with-device-lock.py" "${serial}" \
        "$0" --android "${serial}" "${other_args[@]}"
fi

bin=${root}/target/release
work=${root}/target/e2e/run-$(date +%Y%m%d-%H%M%S)
mkdir -p "${work}"
report=${report:-${work}/report.json}
print "Building the host and the probe (log: ${work}/build.log)"
if ! (cd "${root}" && cargo build --release -p cranvr-host -p cranvr-probe > "${work}/build.log" 2>&1); then
    tail -20 "${work}/build.log" >&2
    exit 1
fi

host_args=(--status --config-dir "${work}/config" --fps "${fps}")
case ${client} in
    mac)
        if (( native )); then
            # The host listens on this Mac only, so it needs no trust option
            host_args+=(--protocol native --native-port "${native_port}")
            (( native_tcp )) && host_args+=(--tcp)
        else
            host_args+=(--trust-all --tcp)
        fi
        ;;
    android) ;;
    quest)
        host_args+=(--trust-all)
        if (( cranvr_client )); then
            host_args+=(--protocol native --native-port "${native_port}")
        fi
        ;;
esac
(( desktop )) && host_args+=(--desktop)
host_args+=("${more_host_args[@]}")

pids=()
tailscale_paused=0
adb_cmd=()
package="" guardian_before=""
forwards=()
cleanup() {
    for pid in "${pids[@]}"; do
        kill "${pid}" 2>/dev/null || true
    done
    # Wine runs the program in its own process: end it by the program's file name
    if [[ -n ${wine} ]]; then
        pkill -f "${wine##*\\}" 2>/dev/null || true
    fi
    for port in "${forwards[@]}"; do
        "${adb_cmd[@]}" forward --remove "tcp:${port}" 2>/dev/null || true
    done
    if [[ ${client} == android ]]; then
        "${adb_cmd[@]}" shell pkill -f cranvr-probe >/dev/null 2>&1 || true
    fi
    if (( tailscale_paused )); then
        tailscale_vpn CONNECT_VPN
        if ! wait_vpn 1; then
            print -u2 "WARNING: Tailscale did not connect again on ${serial}. Open the Tailscale app there."
        fi
    fi
    if [[ -n ${package} ]]; then
        "${adb_cmd[@]}" shell am force-stop "${package}" >/dev/null 2>&1 || true
    fi
    if [[ ${client} == quest ]]; then
        "${adb_cmd[@]}" shell setprop debug.oculus.guardian_pause "${guardian_before:-0}" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

# Tailscale's own intents: its app keeps its settings, and connecting again restores the tunnel
tailscale_vpn() {
    "${adb_cmd[@]}" shell am broadcast -a "com.tailscale.ipn.$1" -n com.tailscale.ipn/.IPNReceiver >/dev/null
}
# Waits until the device has a VPN network (1) or none (0)
wait_vpn() {
    local i count
    for i in {1..40}; do
        count=$("${adb_cmd[@]}" shell dumpsys connectivity | grep -c "VPN CONNECTED" || true)
        if (( $1 ? count > 0 : count == 0 )); then
            return 0
        fi
        sleep 0.5
    done
    return 1
}

# The host's status line for a connected headset
headset_connected() {
    grep '"connected":true' "${work}/host.out" | grep -q '"type":"headset"'
}

# Warnings about the device's network: a VPN over this Mac's address, slow Wi-Fi. The check also
# finds this Mac's address on the device's network (mac_ip), which the Cranpose client needs.
device_warnings=()
mac_ip=""
check_device() {
    local device_ip interface line
    device_ip=$("${adb_cmd[@]}" shell ip -4 addr show wlan0 | awk '/inet / { split($2, a, "/"); print a[1] }')
    [[ -n ${device_ip} ]] || return 0
    interface=$(route -n get "${device_ip}" 2>/dev/null | awk '/interface:/ { print $2 }')
    mac_ip=$(ipconfig getifaddr "${interface:-en0}" 2>/dev/null || true)
    [[ -n ${mac_ip} ]] || return 0
    "${adb_cmd[@]}" shell dumpsys connectivity > "${work}/connectivity.txt"
    while IFS= read -r line; do
        device_warnings+=("${line}")
        print -u2 "WARNING: ${line}"
    done < <(python3 -I "${root}/scripts/device-check.py" "${work}/connectivity.txt" "${mac_ip}")
}

celsius() {
    "${adb_cmd[@]}" shell dumpsys battery | awk '/temperature/ { print $2 / 10; exit }'
}

device_dir=/data/local/tmp/cranvr-probe
case ${client} in
    android)
        adb_cmd=(adb -s "${serial}")
        print "Building cranvr-probe for Android (log: ${work}/build-android.log)"
        sdk=${ANDROID_HOME:-${HOME}/Library/Android/sdk}
        if ! (cd "${root}" && ANDROID_HOME=${sdk} cargo ndk -t arm64-v8a --platform 29 build --release \
            -p cranvr-probe --bin cranvr-probe > "${work}/build-android.log" 2>&1); then
            tail -20 "${work}/build-android.log" >&2
            exit 1
        fi
        "${adb_cmd[@]}" shell mkdir -p "${device_dir}"
        "${adb_cmd[@]}" push -q "${root}/target/aarch64-linux-android/release/cranvr-probe" "${device_dir}/"
        if (( wifi )); then
            client_ip=$("${adb_cmd[@]}" shell ip -4 addr show wlan0 | awk '/inet / { split($2, a, "/"); print a[1] }')
            if [[ -z ${client_ip} ]]; then
                print -u2 "${serial} has no Wi-Fi address."
                exit 1
            fi
            host_args+=(--client-ip "${client_ip}")
        else
            # The host connects to the client: control on TCP 9943, the stream on TCP 9944
            for port in 9943 9944; do
                "${adb_cmd[@]}" forward "tcp:${port}" "tcp:${port}" >/dev/null
                forwards+=(${port})
            done
            host_args+=(--client-ip 127.0.0.1 --tcp)
        fi
        if (( pause_tailscale )) && "${adb_cmd[@]}" shell dumpsys connectivity | grep -q "VPN CONNECTED"; then
            print "Pausing Tailscale on ${serial}"
            tailscale_paused=1
            tailscale_vpn DISCONNECT_VPN
            if ! wait_vpn 0; then
                print -u2 "Tailscale did not disconnect on ${serial}."
                exit 1
            fi
        fi
        # Over USB the stream does not use the device's network
        if (( wifi )); then
            check_device
        fi
        celsius_before=$(celsius)
        "${adb_cmd[@]}" shell input keyevent KEYCODE_WAKEUP
        ;;
    quest)
        adb_cmd=(adb -s "${serial:-$(pick_device is_quest)}")
        serial=${adb_cmd[3]}
        if [[ -z ${serial} ]]; then
            print -u2 "No Quest in adb devices. Connect it by USB and allow USB debugging in the headset."
            exit 1
        fi
        if (( cranvr_client )); then
            package=$("${adb_cmd[@]}" shell pm list packages dev.cranvr.client2 | tr -d '\r' | sed -n 's/^package:\(dev\.cranvr\.client2\)$/\1/p')
            if [[ -z ${package} ]]; then
                print -u2 "cranvr's Cranpose client is not on the Quest. Build and install it:"
                print -u2 "  scripts/build-client.sh --install ${serial}"
                exit 1
            fi
        else
            package=$("${adb_cmd[@]}" shell pm list packages | sed -n 's/^package:\(alvr\.client.*\)/\1/p' | head -1 | tr -d '\r')
        fi
        if [[ -z ${package} ]]; then
            print -u2 "The ALVR client is not on the Quest. Install ALVR from the Meta store, or alvr_client_android.apk of ALVR v20.14.1:"
            print -u2 "  adb -s ${serial} install alvr_client_android.apk"
            exit 1
        fi
        check_device
        if (( cranvr_client )) && [[ -z ${mac_ip} ]]; then
            print -u2 "Cannot find this Mac's address on the Quest's network. The Cranpose client needs it."
            exit 1
        fi
        celsius_before=$(celsius)
        "${adb_cmd[@]}" shell input keyevent KEYCODE_WAKEUP
        # Keeps the display on without a face in the headset
        "${adb_cmd[@]}" shell am broadcast -a com.oculus.vrpowermanager.prox_close >/dev/null
        # A headset that lies still loses its place in the room. The boundary then covers the app
        # with a dialog. The pause lasts for the run; cleanup sets the old value again.
        guardian_before=$("${adb_cmd[@]}" shell getprop debug.oculus.guardian_pause | tr -d '\r')
        "${adb_cmd[@]}" shell setprop debug.oculus.guardian_pause 1
        ;;
esac

# A fresh config folder gives the host a new key. A headset refuses a known address with a new
# key. A run with the Cranpose client therefore keeps the key of its first run.
identity=${root}/target/e2e/host-identity.txt
if [[ ${client} == quest ]] && (( cranvr_client )) && [[ -f ${identity} ]]; then
    mkdir -p "${work}/config"
    cp "${identity}" "${work}/config/host-identity.txt"
fi
print "Starting the host at ${fps} Hz (log: ${work}/host.log)"
"${bin}/cranvr-host" "${host_args[@]}" > "${work}/host.out" 2> "${work}/host.log" &
pids+=($!)
for i in {1..50}; do
    [[ -f ${work}/config/runtime-control.txt ]] && break
    sleep 0.2
done
if [[ ${client} == quest ]] && (( cranvr_client )) && [[ ! -f ${identity} && -f ${work}/config/host-identity.txt ]]; then
    cp "${work}/config/host-identity.txt" "${identity}"
fi

game_pid=""
if (( game )); then
    # With the native protocol the fake game also checks the controller input of the probe's script
    # (see cranvr-fake-game) and ends a few seconds after the probe, so its exit code counts.
    if (( native )); then
        "${bin}/cranvr-fake-game" --control "${work}/config/runtime-control.txt" --fps "${fps}" \
            --seconds $(( seconds + 4 )) --expect-input > "${work}/game.log" 2>&1 &
    else
        "${bin}/cranvr-fake-game" --control "${work}/config/runtime-control.txt" --fps "${fps}" \
            --seconds $(( seconds + 60 )) > "${work}/game.log" 2>&1 &
    fi
    game_pid=$!
    pids+=(${game_pid})
fi

if [[ -n ${wine} ]]; then
    print -r "Starting ${wine} in Wine (log: ${work}/game.log)"
    CRANVR_CONTROL=${work}/config/runtime-control.txt "${root}/scripts/run-game.sh" --mute "${wine}" "${wine_args[@]}" \
        > "${work}/game.log" 2>&1 &
    pids+=($!)
fi

probe_args=(--seconds "${seconds}")
if (( native )); then
    probe_args+=(--protocol native --connect "127.0.0.1:${native_port}" --drop-percent "${native_drop}")
    (( native_tcp )) && probe_args+=(--tcp)
fi
(( game )) && probe_args+=(--game)
# A real program and the desktop scene draw no eye badges
if [[ -n ${wine} ]] || (( desktop )); then
    probe_args+=(--any-picture)
fi
client_exit=0
case ${client} in
    mac)
        print "Running cranvr-probe for ${seconds} seconds"
        # HOME keeps the probe's ALVR client settings out of the user's Library folder
        HOME=${work}/probe-home "${bin}/cranvr-probe" "${probe_args[@]}" > "${work}/probe.log" 2>&1 || client_exit=$?
        ;;
    android)
        print "Running cranvr-probe on ${serial} for ${seconds} seconds"
        # The probe waits up to 60 seconds for the stream; the timeout ends a probe that hangs
        "${adb_cmd[@]}" shell "RUST_BACKTRACE=1 CRANVR_PROBE_HOME=${device_dir} timeout $(( seconds + 90 )) ${device_dir}/cranvr-probe ${probe_args[*]}" \
            > "${work}/probe.log" 2>&1 || client_exit=$?
        ;;
    quest)
        print "Starting ${package} on ${serial}"
        if (( cranvr_client )); then
            # The Cranpose client takes the host's address and the start of the connection from
            # the extras of its launch
            "${adb_cmd[@]}" shell am force-stop "${package}"
            "${adb_cmd[@]}" shell am start -n "${package}/dev.cranpose.android.CranposeActivity" \
                --es host "${mac_ip}:${native_port}" --ez connect true >/dev/null
        else
            "${adb_cmd[@]}" shell monkey -p "${package}" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
        fi
        for i in {1..120}; do
            headset_connected && break
            sleep 0.5
        done
        if ! headset_connected; then
            print -u2 "The Quest did not connect in 60 seconds. Check that it shares a network with this Mac."
            client_exit=1
        else
            print "Streaming for ${seconds} seconds"
            sleep "${seconds}"
            # What the headset shows, both eyes after its compositor
            "${adb_cmd[@]}" exec-out screencap -p > "${work}/quest.png" || true
        fi
        ;;
esac
(( client_exit == 0 )) || print -u2 "The client failed; see ${work}/probe.log"
game_exit=0
if (( native && game )); then
    wait ${game_pid} || game_exit=$?
    grep -h "fake-game: input" "${work}/game.log" || true
    (( game_exit == 0 )) || print -u2 "The fake game failed or saw wrong input; see ${work}/game.log"
fi
# The host prints a stats line every 5 seconds: wait for the last one
sleep 5.5

mode=${client}
if (( wifi )); then
    mode+=-wifi
fi
protocol=udp
if (( ${host_args[(Ie)--tcp]} )); then
    protocol=tcp
fi
(( native )) && mode+=-native
if [[ ${client} == quest ]] && (( cranvr_client )); then
    mode+=-native
fi
extra=(mode=${mode} protocol=${protocol} native_drop=${native_drop} "warnings=${(j: | :)device_warnings}" client_exit=${client_exit} game=${game} "wine=${wine}" desktop=${desktop} seconds=${seconds} \
    work=${work} "host_args=${more_host_args[*]}")
# The probe's count of frames that reached its decoder, and of frames it showed with the right picture
done_line=$(grep -h "probe: done" "${work}/probe.log" 2>/dev/null || true)
if [[ ${done_line} =~ '([0-9]+) received, [0-9]+ decoded \(([0-9]+) good' ]]; then
    extra+=(received=${match[1]} shown=${match[2]})
fi
# The seconds of the Mac's sound that reached the probe. The capture sends silence too, so
# the sound arrives without a sound playing.
audio_line=$(grep -h "probe: game audio" "${work}/probe.log" 2>/dev/null || true)
if [[ ${audio_line} =~ '([0-9.]+) s of stereo' ]] && (( ! ${host_args[(Ie)--no-game-audio]} )); then
    extra+=(audio_seconds=${match[1]})
fi
if [[ ${client} != mac ]]; then
    model=$("${adb_cmd[@]}" shell getprop ro.product.model | tr -d '\r')
    extra+=(serial=${serial} "model=${model}" device_celsius_before=${celsius_before} device_celsius_after=$(celsius))
fi
result=0
python3 -I "${root}/scripts/e2e-report.py" "${work}/host.out" "${fps}" "${report}" "${extra[@]}" || result=$?
(( client_exit == 0 )) || result=1
(( game_exit == 0 )) || result=1
grep -hE "probe: (done|frame arrival|native|game audio)" "${work}/probe.log" 2>/dev/null || true
print "Report: ${report}"
exit ${result}
