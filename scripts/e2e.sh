#!/bin/zsh
# End-to-end test of the stream from this Mac to a client. It runs cranvr-host, a client and an
# optional game, then writes a JSON report: every 5-second stats line of the host (fps, render and
# encode time, the client's latency split by stage) and the median and worst of each.
#
# Usage: scripts/e2e.sh [--seconds N] [--fps HZ] [--game | --wine PROGRAM [--wine-arg ARG]...] [--desktop]
#                       [--quest [SERIAL] | --native-tcp] [--native-drop PERCENT] [--native-port PORT]
#                       [--host-arg ARG]... [--report FILE]
#                       [--tracking [FEATURES] | --hand-controllers | --tracking-wine | --openvr-wine | --openvr-actions MODE]
#                       [--face-source visual|audio|unknown]
#
#   --seconds N   how long the stream runs (default 20)
#   --fps HZ      the stream's refresh rate (default 120, as the host). The probe also offers 144,
#                 165, 180 and 240 Hz, which measure the room left above a Quest 3's 120 Hz
#   --game        a fake game sends the frames through the BasaltVR runtime protocol. With the
#                 probe it also checks the controller input of the probe's script.
#   --wine PROGRAM a Windows OpenXR program runs in cranvr's Wine stack without sound, through
#                 scripts/run-game.sh: a Windows path such as 'C:\basalt\runtime\d3d11_runtime_smoke.exe'.
#                 The probe then counts every frame of the right size, since the program draws no
#                 eye badges. --wine-arg passes one argument to the program.
#   --tracking [FEATURES]  hands, body, face and eyes: the fake game uses these features (a comma
#                 list of hands, body, full_body, face and eyes; default all), the probe offers
#                 them and plays the script of probe/src/tracked.rs, and the fake game checks what
#                 reaches it. The run also checks that the host asked the headset only after the
#                 game created its trackers. Implies --game.
#   --hand-controllers  a game without hand code: the probe tracks hands and no controller, and the
#                 fake game checks that the host made controllers of the hands (a pinch is the
#                 trigger, the aim pose points from the shoulder through the fingers). Implies --game.
#   --tracking-wine  the same through the real runtime: C:\basalt\runtime\tracking_smoke.exe runs in
#                 Wine, creates a hand tracker for each hand, a body tracker for each joint set, a
#                 face tracker and an eye gaze action, and compares what it reads with the script.
#                 Set CRANVR_HOME to a stack that scripts/setup-wine.sh built from the runtime to
#                 test. The run lasts at least 40 seconds.
#   --openvr-wine  an OpenVR program through OpenComposite and the real runtime: C:\opencomposite\openvr_smoke.exe
#                 runs in Wine. It finds the runtime through the prefix's OpenVR path file, which
#                 scripts/run-game.sh writes when CRANVR_OPENCOMPOSITE is 1 (this option sets it). It
#                 makes a Direct3D 11 device, submits both eyes in every frame and reads the head pose
#                 and the legacy state of the scripted controllers (a pull of the left grip, the right
#                 trigger at 0.75, the A button). It prints "openvr_smoke: PASS" or "FAIL ...". Set
#                 CRANVR_HOME to a stack that scripts/setup-wine.sh built with OpenComposite.
#   --openvr-actions touch|vive|both  the same program with IVRInput: it writes an action manifest with default
#                 bindings (for Touch controllers, the Vive wand or both, as a game that ships a manifest does),
#                 sets it, and reads the actions that the scripted controllers drive. The run also fails
#                 when the program's haptic pulse does not reach the probe. Implies --openvr-wine.
#   --face-source SOURCE  with --tracking and --tracking-wine: where the probe's face comes from.
#                 audio (the default) is a Quest 3's microphone face, visual is a face from cameras,
#                 and unknown is an offer that names no source, as an older headset app sends. The
#                 offer, the host's file for the runtime and each sample carry the source. The
#                 Windows program checks that the support structure says it, that a face tracker
#                 which asks for the other source is refused, and each sample's dataSource.
#   --desktop     the host shows this Mac's screen. The probe asks for the desktop stream, plays a
#                 pointer script (the host only logs its mouse events: CRANVR_POINTER_DRY_RUN) and
#                 counts the Mac's sound. It counts every frame of the right size, since the
#                 desktop scene draws no eye badges. The run fails when the probe gets the sound
#                 for less than 80 percent of the stream time. --host-arg --virtual-display
#                 --host-arg 1920x1080 shows a virtual display instead of the main one.
#   --quest       the client is cranvr's Cranpose client (dev.cranvr.client, see
#                 scripts/build-client.sh) on a Quest attached by USB. The Quest and the Mac must
#                 share a network. The host runs with --trust-all on --native-port. The app gets this
#                 Mac's address and the room `scene` as launch extras: with the Room setting at
#                 Passthrough the client shows no stream and reports no frame timing, so the host's
#                 latency block would stay empty. The run keeps the host's key in
#                 target/e2e/host-identity.txt: a headset refuses a known address and port with
#                 a new key. Give a --native-port that the headset has not used with another host
#                 at this Mac's address, such as 9961.
#                 Without --quest the client is cranvr-probe on this Mac, over loopback with UDP
#                 media and parity packets. It simulates a headset's display: it reports the
#                 frame timing and the loss, and it checks the eye badges of the host's scene.
#   --native-tcp  with the probe: the media goes over TCP, as it does over USB with `adb reverse`
#   --native-drop PERCENT  with the probe: it drops this share of the media packets, so the
#                 parity has loss to recover
#   --native-port PORT  the host's TCP port (default 9950)
#   --host-arg ARG one more cranvr-host argument, for an A/B run of a setting:
#                 --host-arg --no-display-lock
#   --report FILE where the report goes (default: target/e2e/run-<time>/report.json)
#
# The options --native and --cranvr-client are accepted and change nothing: cranvr's protocol and
# client are the only ones.
#
# The script exits with 0 when the median fps reached 90 percent of the target and the client saw
# the right picture. It exits with 1 otherwise.

set -euo pipefail

root=${0:A:h:h}
seconds=20 fps=120 fps_given=0 game=0 desktop=0 client=mac serial="" report=""
tracking=0 tracking_features=hands,body,full_body,face,eyes hand_controllers=0 tracking_wine=0 openvr_wine=0 openvr_actions="" face_source=audio
wine="" wine_args=()
native_tcp=0 native_drop=0 native_port=9950
more_host_args=()
while (( $# )); do
    case $1 in
        --seconds) seconds=$2; shift 2 ;;
        --fps) fps=$2; fps_given=1; shift 2 ;;
        --game) game=1; shift ;;
        --tracking)
            game=1 tracking=1; shift
            if (( $# )) && [[ $1 != --* ]]; then tracking_features=$1; shift; fi ;;
        --hand-controllers) game=1 hand_controllers=1; shift ;;
        --tracking-wine) tracking_wine=1; shift ;;
        --openvr-wine) openvr_wine=1; shift ;;
        --openvr-actions)
            openvr_wine=1 openvr_actions=$2; shift 2
            [[ ${openvr_actions} == (touch|vive|both) ]] || { print -u2 "--openvr-actions takes touch, vive or both"; exit 2; } ;;
        --face-source)
            face_source=$2; shift 2
            [[ ${face_source} == (visual|audio|unknown) ]] || { print -u2 "--face-source takes visual, audio or unknown"; exit 2; } ;;
        --desktop) desktop=1; shift ;;
        --wine) wine=$2; shift 2 ;;
        --wine-arg) wine_args+=("$2"); shift 2 ;;
        --cranvr-client|--native) shift ;;
        --native-tcp) native_tcp=1; shift ;;
        --native-drop) native_drop=$2; shift 2 ;;
        --native-port) native_port=$2; shift 2 ;;
        --quest)
            client=quest; shift
            if (( $# )) && [[ $1 != --* ]]; then serial=$1; shift; fi ;;
        --host-arg) more_host_args+=("$2"); shift 2 ;;
        --report) report=$2; shift 2 ;;
        *) print -u2 "unknown option $1"; exit 2 ;;
    esac
done

if [[ ${client} == quest ]] && (( tracking || hand_controllers || tracking_wine || openvr_wine )); then
    print -u2 "--tracking, --hand-controllers, --tracking-wine and --openvr-wine belong to the probe: they cannot combine with --quest."
    exit 2
fi
if (( openvr_wine )); then
    # The program ends a few seconds before the probe, so its verdict is in the log when the probe is done
    (( seconds >= 20 )) || seconds=20
    wine='C:\opencomposite\openvr_smoke.exe'
    wine_args=(--seconds $(( seconds - 6 )) --expect-input)
    [[ -z ${openvr_actions} ]] || wine_args+=(--actions "${openvr_actions}")
    export CRANVR_OPENCOMPOSITE=1
    # The run tests the path of an OpenVR program, not the rate. On a Mac that other builds keep busy (a
    # load average above 20) the program loses frames to the scheduler: the report of its second-by-second
    # frame counts shows 120 in the quiet seconds and 60 to 100 in the busy ones. The run therefore asks the
    # host for 72 Hz unless --fps is given.
    (( fps_given )) || fps=72
    # What the runtime tells the game about the display must be the rate that the host runs at
    wine_args+=(--expect-hz "${fps}")
fi
if (( tracking_wine )); then
    # The Windows program waits for the headset's features, starts a session and reads the data
    wine='C:\basalt\runtime\tracking_smoke.exe'
    wine_args=(--seconds 25 --face-source "${face_source}")
    (( seconds >= 40 )) || seconds=40
fi

if [[ ${client} == quest ]] && (( native_tcp || native_drop )); then
    print -u2 "--native-tcp and --native-drop belong to the probe: they cannot combine with --quest."
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

bin=${root}/target/release
work=${root}/target/e2e/run-$(date +%Y%m%d-%H%M%S)
mkdir -p "${work}"
report=${report:-${work}/report.json}
print "Building the host and the probe (log: ${work}/build.log)"
if ! (cd "${root}" && cargo build --release -p cranvr-host -p cranvr-probe > "${work}/build.log" 2>&1); then
    tail -20 "${work}/build.log" >&2
    exit 1
fi

host_args=(--status --config-dir "${work}/config" --fps "${fps}" --native-port "${native_port}")
case ${client} in
    mac)
        # The host listens on this Mac only for the probe: a client on this Mac needs no pairing
        (( native_tcp )) && host_args+=(--tcp)
        ;;
    quest)
        host_args+=(--trust-all)
        ;;
esac
if (( desktop )); then
    host_args+=(--desktop)
    # The probe's pointer script then moves no real mouse
    [[ ${client} == mac ]] && export CRANVR_POINTER_DRY_RUN=1
fi
host_args+=("${more_host_args[@]}")

pids=()
adb_cmd=()
package="" guardian_before=""
cleanup() {
    for pid in "${pids[@]}"; do
        kill "${pid}" 2>/dev/null || true
    done
    # Wine runs the program in its own process: end it by the program's file name
    if [[ -n ${wine} ]]; then
        pkill -f "${wine##*\\}" 2>/dev/null || true
    fi
    if [[ -n ${package} ]]; then
        "${adb_cmd[@]}" shell am force-stop "${package}" >/dev/null 2>&1 || true
    fi
    if [[ ${client} == quest ]]; then
        "${adb_cmd[@]}" shell setprop debug.oculus.guardian_pause "${guardian_before:-0}" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

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

if [[ ${client} == quest ]]; then
    adb_cmd=(adb -s "${serial:-$(pick_device is_quest)}")
    serial=${adb_cmd[3]}
    if [[ -z ${serial} ]]; then
        print -u2 "No Quest in adb devices. Connect it by USB and allow USB debugging in the headset."
        exit 1
    fi
    package=$("${adb_cmd[@]}" shell pm list packages dev.cranvr.client | tr -d '\r' | sed -n 's/^package:\(dev\.cranvr\.client\)$/\1/p')
    if [[ -z ${package} ]]; then
        print -u2 "cranvr's Cranpose client is not on the Quest. Build and install it:"
        print -u2 "  scripts/build-client.sh --install ${serial}"
        exit 1
    fi
    check_device
    if [[ -z ${mac_ip} ]]; then
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
fi

# A fresh config folder gives the host a new key. A headset refuses a known address with a new
# key. A run with a headset therefore keeps the key of its first run.
identity=${root}/target/e2e/host-identity.txt
if [[ ${client} == quest && -f ${identity} ]]; then
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
if [[ ${client} == quest && ! -f ${identity} && -f ${work}/config/host-identity.txt ]]; then
    cp "${work}/config/host-identity.txt" "${identity}"
fi

game_pid=""
if (( game )); then
    # With the probe the fake game also checks the controller input of the probe's script (see
    # cranvr-fake-game) and ends a few seconds after the probe, so its exit code counts. A
    # headset on a desk sends no controller input.
    if [[ ${client} == mac ]]; then
        game_args=(--expect-input)
        (( tracking )) && game_args=(--use-tracking "${tracking_features}" --release-tracking --expect-tracking --face-source "${face_source}")
        (( hand_controllers )) && game_args=(--expect-hand-controllers)
        "${bin}/cranvr-fake-game" --control "${work}/config/runtime-control.txt" --fps "${fps}" \
            --seconds $(( seconds + 4 )) "${game_args[@]}" > "${work}/game.log" 2>&1 &
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

probe_args=(--seconds "${seconds}" --connect "127.0.0.1:${native_port}" --drop-percent "${native_drop}")
(( native_tcp )) && probe_args+=(--tcp)
(( game )) && probe_args+=(--game)
# A real program and the desktop scene draw no eye badges
if [[ -n ${wine} ]] || (( desktop )); then
    probe_args+=(--any-picture)
fi
# The probe asks for the desktop stream, plays a pointer script and counts the Mac's sound
(( desktop )) && probe_args+=(--desktop)
# The probe offers hands, body, face and eyes and plays the script of tracked.rs for what the host asks
(( tracking || tracking_wine )) && probe_args+=(--tracking --face-source "${face_source}")
(( hand_controllers )) && probe_args+=(--tracking --no-controllers)
# A program that compares its controller poses with the probe's script needs the head still: its room is then the probe's room
[[ -z ${openvr_actions} ]] || probe_args+=(--still-head)
client_exit=0
case ${client} in
    mac)
        print "Running cranvr-probe for ${seconds} seconds"
        # HOME keeps the probe out of the user's Library folder
        HOME=${work}/probe-home "${bin}/cranvr-probe" "${probe_args[@]}" > "${work}/probe.log" 2>&1 || client_exit=$?
        ;;
    quest)
        print "Starting ${package} on ${serial}"
        # The Cranpose client takes the host's address, the start of the connection and the room
        # from the extras of its launch
        "${adb_cmd[@]}" shell am force-stop "${package}"
        "${adb_cmd[@]}" shell am start -n "${package}/dev.cranpose.android.CranposeActivity" \
            --es host "${mac_ip}:${native_port}" --ez connect true --es room scene >/dev/null
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
tracking_exit=0
openvr_exit=0
if (( openvr_wine )); then
    for i in {1..60}; do
        grep -qE "openvr_smoke: (PASS|FAIL)" "${work}/game.log" && break
        sleep 0.5
    done
    grep -h "openvr_smoke:" "${work}/game.log" || true
    grep -q "openvr_smoke: PASS" "${work}/game.log" || { print -u2 "The OpenVR program did not pass; see ${work}/game.log"; openvr_exit=1; }
    if [[ -n ${openvr_actions} ]]; then
        # The program pulses the left haptic output when its grab action fires: the pulse crosses the runtime and the
        # host to the probe
        if [[ $(grep -hoE '[0-9]+ haptic pulses' "${work}/probe.log" | grep -oE '^[0-9]+' | head -1) -ge 1 ]]; then
            grep -h "probe: first haptic pulse" "${work}/probe.log" || true
        else
            print -u2 "The program's haptic pulse did not reach the probe; see ${work}/probe.log"
            openvr_exit=1
        fi
    fi
    # OpenComposite's own log, for a failure
    opencomposite_log=$(print -rl -- "${CRANVR_HOME:-$HOME/Library/Application Support/cranvr}"/wine/prefix/drive_c/users/*/AppData/Local/OpenComposite/logs/opencomposite.log(N) | head -1)
    if [[ -n ${opencomposite_log} ]]; then
        cp "${opencomposite_log}" "${work}/opencomposite.log"
        (( openvr_exit == 0 )) || tail -30 "${work}/opencomposite.log" >&2
    fi
fi
if (( tracking_wine )); then
    # The probe's time is up. The Windows program ends on its own: wait for its verdict.
    for i in {1..60}; do
        grep -qE "tracking_smoke: (PASS|FAIL)" "${work}/game.log" && break
        sleep 0.5
    done
    grep -h "tracking_smoke:" "${work}/game.log" || true
    grep -q "tracking_smoke: PASS" "${work}/game.log" || { print -u2 "The tracking program did not pass; see ${work}/game.log"; tracking_exit=1; }
fi
if (( tracking || tracking_wine )); then
    # The host asks the headset for tracking only after the game created its trackers
    python3 -I - "${work}/probe.log" "${work}/game.log" <<'PYTHON' || tracking_exit=1
import re, sys
probe, game = (open(path, errors="replace").read() for path in sys.argv[1:3])
asks = [(int(ms), what) for what, ms in re.findall(r"probe: the host asks for (.*?) at unix (\d+) ms", probe)]
made = re.findall(r"(?:fake-game: tracking requested: .*?|tracking_smoke: creating the trackers) at unix (\d+) ms", game)
if not made or not asks:
    print("tracking: no asks (%d) or no trackers (%d)" % (len(asks), len(made)), file=sys.stderr)
    sys.exit(1)
created = int(made[0])
first = next(ms for ms, what in asks if what != "no tracking")
early = [ms for ms, what in asks if what != "no tracking" and ms < created - 20]
print("tracking: the first ask came %d ms after the trackers were created, the last ask was %s" % (first - created, asks[-1][1]))
if early:
    print("tracking: the host asked %d ms before the game created a tracker" % (created - early[0]), file=sys.stderr)
    sys.exit(1)
if first - created > 3000 or asks[-1][1] != "no tracking":
    print("tracking: the first ask is late or the last ask is not the end of tracking", file=sys.stderr)
    sys.exit(1)
PYTHON
    grep -h "probe: tracking sent" "${work}/probe.log" || true
fi
if [[ ${client} == mac ]] && (( game )); then
    wait ${game_pid} || game_exit=$?
    grep -hE "fake-game: (input|tracking|controllers made|hand controller)" "${work}/game.log" || true
    (( game_exit == 0 )) || print -u2 "The fake game failed or saw wrong input; see ${work}/game.log"
fi
# The host prints a stats line every 5 seconds: wait for the last one
sleep 5.5

protocol=udp
if (( ${host_args[(Ie)--tcp]} )); then
    protocol=tcp
fi
extra=(mode=${client} protocol=${protocol} native_drop=${native_drop} "warnings=${(j: | :)device_warnings}" client_exit=${client_exit} game=${game} "wine=${wine}" desktop=${desktop} seconds=${seconds} \
    work=${work} "host_args=${more_host_args[*]}")
# The probe's count of frames that reached its decoder, and of frames it showed with the right picture
done_line=$(grep -h "probe: done" "${work}/probe.log" 2>/dev/null || true)
if [[ ${done_line} =~ '([0-9]+) received, [0-9]+ decoded \(([0-9]+) good' ]]; then
    extra+=(received=${match[1]} shown=${match[2]})
fi
# The seconds of the Mac's sound that reached the probe. The capture sends silence too, so
# the sound arrives without a sound playing.
audio_line=$(grep -h "probe: audio:" "${work}/probe.log" 2>/dev/null || true)
if [[ ${audio_line} =~ '([0-9.]+) s of sound in' ]] && (( ! ${host_args[(Ie)--no-game-audio]} )); then
    extra+=(audio_seconds=${match[1]})
fi
if [[ ${client} == quest ]]; then
    model=$("${adb_cmd[@]}" shell getprop ro.product.model | tr -d '\r')
    extra+=(serial=${serial} "model=${model}" device_celsius_before=${celsius_before} device_celsius_after=$(celsius))
fi
result=0
python3 -I "${root}/scripts/e2e-report.py" "${work}/host.out" "${fps}" "${report}" "${extra[@]}" || result=$?
(( client_exit == 0 )) || result=1
(( game_exit == 0 )) || result=1
(( tracking_exit == 0 )) || result=1
(( openvr_exit == 0 )) || result=1
grep -hE "probe: (done|frame arrival|native|desktop|audio)" "${work}/probe.log" 2>/dev/null || true
print "Report: ${report}"
exit ${result}
