#!/bin/zsh
# End-to-end test of the stream from this Mac to a client. It runs cranvr-host, a client and an
# optional game, then writes a JSON report: every 5-second stats line of the host (fps, render and
# encode time, the client's latency split by stage) and the median and worst of each.
#
# Usage: scripts/e2e.sh [--seconds N] [--fps HZ] [--game | --wine PROGRAM [--wine-arg ARG]...] [--desktop]
#                       [--quest [SERIAL] | --native-tcp] [--native-drop PERCENT] [--native-port PORT]
#                       [--host-arg ARG]... [--probe-arg ARG]... [--report FILE]
#                       [--tracking [FEATURES] | --hand-controllers | --tracking-wine | --hand-interaction [hand|both]
#                        | --openvr-wine | --openvr-actions MODE | --openvr-overlay | --openvr-gl]
#                       [--displays N] [--hold] [--hand-hold] [--displays-in-game]
#                       [--game-stall AT:FOR]
#                       [--pause AT:FOR] [--left-trigger AT:FOR] [--right-trigger AT:FOR]
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
#   --hand-interaction [hand|both]  the hand interaction profile of XR_EXT_hand_interaction through the real runtime:
#                 C:\basalt\runtime\hand_interaction_smoke.exe runs in Wine and binds that profile alone (hand, the
#                 default) or with the Touch controller's profile (both). It creates no hand tracker. It drives the
#                 host's test hand (CRANVR_TEST_INPUT, host/src/test_input.rs) through a file of the run: the right
#                 hand becomes a tracked hand that pinches, closes into a fist, moves, rolls and leaves the cameras'
#                 view, and the program checks the values, the ready flags and the four poses that reach it. The
#                 left hand stays the probe's controller, which the runtime maps onto the profile. With both, the
#                 right hand must switch between the two profiles with an event each time. The probe's head stands
#                 still. Set CRANVR_HOME to a stack that scripts/setup-wine.sh built from the runtime to test.
#   --openvr-wine  an OpenVR program through OpenComposite and the real runtime: C:\opencomposite\openvr_smoke.exe
#                 runs in Wine. It finds the runtime through the prefix's OpenVR path file, which
#                 scripts/run-game.sh writes when CRANVR_OPENCOMPOSITE is 1 (this option sets it). It
#                 makes a Direct3D 11 device, submits both eyes in every frame and reads the head pose,
#                 the controller state of IVRSystem for the scripted controllers (a pull of the left grip, the right
#                 trigger at 0.75, the A button) and their poses in the array of WaitGetPoses (valid in
#                 90 percent of the frames, the left one where the probe holds it, the right one swinging;
#                 the probe's head stands still). It prints "openvr_smoke: PASS" or "FAIL ...". Set
#                 CRANVR_HOME to a stack that scripts/setup-wine.sh built with OpenComposite.
#   --openvr-actions touch|vive|both  the same program with IVRInput: it writes an action manifest with default
#                 bindings (for Touch controllers, the Vive wand or both, as a game that ships a manifest does),
#                 sets it, and reads the actions that the scripted controllers drive. The run also fails
#                 when the program's haptic pulse does not reach the probe. Implies --openvr-wine.
#   --openvr-overlay  the same program with an overlay up for the whole run (a loading screen's). The runtime takes
#                 no quad layer, so OpenComposite leaves the overlay out; the run fails when the host counts fewer
#                 game frames than half of the rate. Implies --openvr-wine.
#   --openvr-gl   the same program with OpenGL, as Google Earth VR draws: it asks Wine for an OpenGL 3.2 context with the
#                 compatibility profile, draws with a shader of GLSL 1.10 and submits one OpenGL texture with both eyes
#                 side by side. OpenComposite copies it into the runtime's Direct3D 11 images. The run checks the
#                 picture of each eye in the host the same way. Implies --openvr-wine.
#   --face-source SOURCE  with --tracking and --tracking-wine: where the probe's face comes from.
#                 audio (the default) is a Quest 3's microphone face, visual is a face from cameras,
#                 and unknown is an offer that names no source, as a headset's offer before its face gives data. The
#                 offer, the host's file for the runtime and each sample carry the source. The
#                 Windows program checks that the support structure says it, that a face tracker
#                 which asks for the other source is refused, and each sample's dataSource.
#   --game-stall AT:FOR  with the probe: the fake game sends no frame for FOR seconds (under 3) from AT seconds
#                 after its start, as a game does while it loads a level. The host must keep the game's last
#                 picture: the run fails when the probe decodes a frame of the host's own scene (wrong
#                 badges) between the game's last frame and its first frame after the stall. Implies --game.
#   --pause AT:FOR  with the probe: the app loses the input focus at AT seconds (2 or more) for FOR
#                 seconds, as under the headset's system menu. The probe tells the host, sends no tracking
#                 and no input and counts the frames that still arrive. The run fails when a frame
#                 arrives after the first 0.3 s of the pause, or when the stream does not go on after it.
#                 With --game the fake game keeps running through the pause.
#   --left-trigger AT:FOR, --right-trigger AT:FOR  with the probe: that trigger goes to 1 at AT seconds and
#                 stays for FOR seconds, once. The probe's cycle of presses is off then, so the pulls are
#                 the only presses and the head stands still. A game that shows one pointer at a time,
#                 such as a menu, then has its controller pressed for the first time at AT. A host
#                 started with CRANVR_EYE_DUMP=FOLDER writes what the game drew into FOLDER once a
#                 second, so the run shows which pointer was up before and after.
#   --desktop     the host shows this Mac's screen. The probe asks for the desktop stream, plays a
#                 pointer script (the host only logs its mouse events: CRANVR_POINTER_DRY_RUN) and
#                 counts the Mac's sound. The script presses the left, right and middle buttons and
#                 leans the thumbstick in both axes. A run of 14 seconds or more fails when the host's
#                 log shows no press of one button, no scroll in one axis or a press without its
#                 release. It counts every frame of the right size, since the desktop scene draws no
#                 eye badges. The run fails when the probe gets the sound for less than 80 percent of
#                 the stream time. --host-arg --virtual-display
#                 --host-arg 1920x1080 shows a virtual display instead of the main one.
#   --displays N  with --desktop's host: the probe shows N displays. It asks the host to add N-1 of them, wishes
#                 the video of each, decodes each stream, plays the pointer script on each and removes the last one
#                 three seconds before the end. Needs this Mac to make virtual displays (it takes the owner's
#                 displays through a short rearrangement: do not run it while the owner plays). Implies --desktop.
#   --display-test  with --displays: the host adds no real display. An added display shows the main display
#                 again (CRANVR_DISPLAY_TEST=main). The streams, wishes and pictures run as with a real one, and
#                 the Mac's displays stay as they are, which is safe while the owner plays.
#   --hold        the probe's right hand points at a panel in every other window of its script. The host must
#                 give the game rest values for those windows, and the fake game checks that none of the panel's
#                 presses reaches it, also not in the 0.1 s after the hold ends. Implies --game.
#   --hand-hold   like --hand-controllers, with the app holding both hands back from the game: the controllers
#                 that the host makes of the tracked hands must have rest values (a pinch at a panel is no
#                 trigger of the game). Implies --hand-controllers.
#   --displays-in-game  a game runs (the fake game). For 5 s the probe wishes no display, then it shows its
#                 displays for 5 s, then it hides them. The run checks the host's log and the probe's counts: no
#                 capture, no encoder and no picture sent or received while nothing is wished or after the
#                 wish ended, and pictures while it lasted. Add --displays N for more than one display.
#                 Implies --game.
#   --quest       the client is cranvr's Cranpose client (dev.cranvr.client, see
#                 scripts/build-client.sh) on a Quest attached by USB. The Quest and the Mac must
#                 share a network. The host runs with --trust-all on --native-port. The app gets this
#                 Mac's address and the room `scene` as launch extras: with the Room setting at
#                 Passthrough the client shows no stream and reports no frame timing, so the host's
#                 latency block would stay empty. The run keeps the host's key in
#                 target/e2e/host-identity.txt: a headset refuses a known address and port with
#                 a new key. Give a --native-port that the headset has not used with another host
#                 at this Mac's address, such as 9961. At the end the run gives the headset back to
#                 the cranvr app's host on port 9950: over the cable when the app linked that port
#                 with `adb reverse`, else at this Mac's address. On a port other than 9950 that
#                 launch also makes the headset forget the key that it keeps for the run's address.
#                 Without --quest the client is cranvr-probe on this Mac, over loopback with UDP
#                 media and parity packets. It simulates a headset's display: it reports the
#                 frame timing and the loss, and it checks the eye badges of the host's scene.
#   --native-tcp  with the probe: the media goes over TCP, as it does over USB with `adb reverse`
#   --native-drop PERCENT  with the probe: it drops this share of the media packets, so the
#                 parity has loss to recover
#   --native-port PORT  the host's TCP port (default 9950)
#   --host-arg ARG one more cranvr-host argument, for an A/B run of a setting:
#                 --host-arg --no-display-lock
#   --probe-arg ARG one more cranvr-probe argument, such as --still-head for a game whose room must stay the
#                 probe's room
#   --report FILE where the report goes (default: target/e2e/run-<time>/report.json)
#
# The options --native and --cranvr-client are accepted and change nothing: cranvr's protocol and
# client are the only ones.
#
# The script exits with 0 when the median fps reached 90 percent of the target and the client saw
# the right picture. It exits with 1 otherwise.

set -euo pipefail
# zsh lowers the priority of a background job by 5 ("nice") unless this option is off. The host and the game run as
# background jobs here. The cranvr app starts them at normal priority, so a test with the default option measured a
# game that lost the Mac's CPU time to every build that ran beside it.
unsetopt BG_NICE

root=${0:A:h:h}
seconds=20 fps=120 fps_given=0 game=0 desktop=0 client=mac serial="" report=""
displays=1 hold=0 hand_hold=0 displays_in_game=0 display_test=0
tracking=0 tracking_features=hands,body,full_body,face,eyes hand_controllers=0 tracking_wine=0 hand_interaction="" openvr_wine=0 openvr_actions="" openvr_overlay=0 openvr_gl=0 face_source=audio
game_stall=""
wine="" wine_args=()
native_tcp=0 native_drop=0 native_port=9950 pause="" left_trigger="" right_trigger=""
more_host_args=() more_probe_args=()
while (( $# )); do
    case $1 in
        --seconds) seconds=$2; shift 2 ;;
        --fps) fps=$2; fps_given=1; shift 2 ;;
        --game) game=1; shift ;;
        --tracking)
            game=1 tracking=1; shift
            if (( $# )) && [[ $1 != --* ]]; then tracking_features=$1; shift; fi ;;
        --hand-controllers) game=1 hand_controllers=1; shift ;;
        --game-stall) game=1 game_stall=$2; shift 2 ;;
        --tracking-wine) tracking_wine=1; shift ;;
        --hand-interaction)
            hand_interaction=hand; shift
            if (( $# )) && [[ $1 != --* ]]; then hand_interaction=$1; shift; fi
            [[ ${hand_interaction} == (hand|both) ]] || { print -u2 "--hand-interaction takes hand or both"; exit 2; } ;;
        --openvr-wine) openvr_wine=1; shift ;;
        --openvr-overlay) openvr_wine=1 openvr_overlay=1; shift ;;
        --openvr-gl) openvr_wine=1 openvr_gl=1; shift ;;
        --openvr-actions)
            openvr_wine=1 openvr_actions=$2; shift 2
            [[ ${openvr_actions} == (touch|vive|both) ]] || { print -u2 "--openvr-actions takes touch, vive or both"; exit 2; } ;;
        --face-source)
            face_source=$2; shift 2
            [[ ${face_source} == (visual|audio|unknown) ]] || { print -u2 "--face-source takes visual, audio or unknown"; exit 2; } ;;
        --desktop) desktop=1; shift ;;
        --displays) displays=$2; shift 2 ;;
        --display-test) display_test=1; shift ;;
        --hold) game=1 hold=1; shift ;;
        --hand-hold) game=1 hand_controllers=1 hand_hold=1; shift ;;
        --displays-in-game) game=1 displays_in_game=1; shift ;;
        --wine) wine=$2; shift 2 ;;
        --wine-arg) wine_args+=("$2"); shift 2 ;;
        --cranvr-client|--native) shift ;;
        --native-tcp) native_tcp=1; shift ;;
        --native-drop) native_drop=$2; shift 2 ;;
        --native-port) native_port=$2; shift 2 ;;
        --pause) pause=$2; shift 2 ;;
        --left-trigger) left_trigger=$2; shift 2 ;;
        --right-trigger) right_trigger=$2; shift 2 ;;
        --quest)
            client=quest; shift
            if (( $# )) && [[ $1 != --* ]]; then serial=$1; shift; fi ;;
        --host-arg) more_host_args+=("$2"); shift 2 ;;
        --probe-arg) more_probe_args+=("$2"); shift 2 ;;
        --report) report=$2; shift 2 ;;
        *) print -u2 "unknown option $1"; exit 2 ;;
    esac
done

if (( displays > 1 && ! displays_in_game )); then
    # The probe adds the displays, shows them and removes the last one three seconds before the end
    desktop=1
    (( seconds >= 15 )) || seconds=15
fi
if (( displays_in_game )); then
    # 5 s with no wish, 5 s shown, 1.5 s for the pictures on their way and 4 s with none
    (( seconds >= 18 )) || seconds=18
fi
if [[ ${client} == quest ]] && (( hold || displays > 1 || displays_in_game || tracking || hand_controllers || tracking_wine || openvr_wine )); then
    print -u2 "--tracking, --hand-controllers, --tracking-wine and --openvr-wine belong to the probe: they cannot combine with --quest."
    exit 2
fi
if [[ ${client} == quest && -n ${hand_interaction} ]]; then
    print -u2 "--hand-interaction belongs to the probe: its checks need the probe's still head and controllers."
    exit 2
fi
if (( openvr_wine )); then
    # The program ends a few seconds before the probe, so its verdict is in the log when the probe is done
    (( seconds >= 20 )) || seconds=20
    wine='C:\opencomposite\openvr_smoke.exe'
    wine_args=(--seconds $(( seconds - 6 )) --expect-input)
    [[ -z ${openvr_actions} ]] || wine_args+=(--actions "${openvr_actions}")
    (( ! openvr_overlay )) || wine_args+=(--overlay)
    (( ! openvr_gl )) || wine_args+=(--gl)
    export CRANVR_OPENCOMPOSITE=1
    # The run tests the path of an OpenVR program, not the rate. On a Mac that other builds keep busy (a
    # load average above 20) the program loses frames to the scheduler: the report of its second-by-second
    # frame counts shows 120 in the quiet seconds and 60 to 100 in the busy ones. The rest of the loss was
    # the start (OpenComposite's restarts of the session). The run therefore asks the host for 72 Hz unless
    # --fps is given.
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

if [[ -n ${pause} && ${client} == quest ]]; then
    print -u2 "--pause belongs to the probe: it cannot combine with --quest."
    exit 2
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
if [[ -n ${hand_interaction} ]]; then
    # The program writes the test hand's words into this file, and the host reads them from it. Wine sees the Mac's
    # files on drive Z.
    words=${work}/test-hand.txt
    : > "${words}"
    export CRANVR_TEST_INPUT=${words}
    wine='C:\basalt\runtime\hand_interaction_smoke.exe'
    wine_args=(--words "Z:${words//\//\\}" --profiles "${hand_interaction}")
    (( seconds >= 45 )) || seconds=45
fi
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
(( display_test )) && export CRANVR_DISPLAY_TEST=main
if (( displays_in_game )); then
    # The host runs the displays and logs what they did in windows of one second
    host_args+=(--desktop)
    export CRANVR_DISPLAYS_LOG_SECS=1 CRANVR_POINTER_DRY_RUN=1
fi
if (( desktop )); then
    host_args+=(--desktop)
    # The probe's pointer script then moves no real mouse
    [[ ${client} == mac ]] && export CRANVR_POINTER_DRY_RUN=1
fi
host_args+=("${more_host_args[@]}")

pids=()
adb_cmd=()
package="" guardian_before=""
# The port of the cranvr app's host, where the headset goes back after a run
readonly owner_port=9950

# Starts the client again for the cranvr app's host. The run's host left its key on the headset, and
# the next test host at this address has another key, which the headset would refuse. So the launch
# also makes the headset forget the key that it keeps for the run's address. The key may come from a
# test host of another checkout: the headset then refused the run, and the forget clears the way for
# the next one. Port 9950 is the app's own: its address may hold the key of the user's host, which
# stays.
give_back() {
    local owner="" test_host=${mac_ip}:${native_port}
    local extras=()
    if "${adb_cmd[@]}" reverse --list 2>/dev/null | grep -q "tcp:${owner_port} tcp:${owner_port}"; then
        owner=127.0.0.1:${owner_port}
    elif [[ -n ${mac_ip} ]]; then
        owner=${mac_ip}:${owner_port}
    fi
    [[ -n ${owner} ]] || return 0
    if [[ -n ${mac_ip} ]] && (( native_port != owner_port )); then
        extras=(--es forget_host_key "${test_host}")
    fi
    if "${adb_cmd[@]}" shell am start -n "${package}/dev.cranpose.android.CranposeActivity" \
        --es host "${owner}" --ez connect true "${extras[@]}" >/dev/null 2>&1; then
        print "The headset connects to ${owner} again${extras:+ and forgets the key of ${test_host}}"
    fi
}

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
        [[ -z ${package} ]] || give_back
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
# An OpenVR program draws a picture with an upper and a lower half. The host writes both eyes of the game's newest
# frame, and the run checks that the upper half is the bright one (see below).
if (( openvr_wine )); then
    mkdir -p "${work}/eyes"
    export CRANVR_EYE_DUMP=${work}/eyes
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
        (( hand_hold )) && game_args+=(--expect-hand-held)
        (( hold )) && game_args+=(--expect-held)
        [[ -n ${game_stall} ]] && game_args=(--stall "${game_stall}")
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
(( displays > 1 )) && probe_args+=(--displays "${displays}")
(( displays_in_game )) && probe_args+=(--displays-in-game)
(( hold )) && probe_args+=(--hold-script)
(( hand_hold )) && probe_args+=(--hold-hands)
[[ -n ${pause} ]] && probe_args+=(--pause "${pause}")
# The pull test needs a head that stands still, so the hands stay in view
[[ -n ${left_trigger} ]] && probe_args+=(--left-trigger "${left_trigger}" --still-head)
[[ -n ${right_trigger} ]] && probe_args+=(--right-trigger "${right_trigger}" --still-head)
# The probe offers hands, body, face and eyes and plays the script of tracked.rs for what the host asks
(( tracking || tracking_wine )) && probe_args+=(--tracking --face-source "${face_source}")
(( hand_controllers )) && probe_args+=(--tracking --no-controllers)
# A program that compares its controller poses with the probe's script (the OpenVR program does, with and without an action manifest) needs the head still: its room is then the probe's room
(( openvr_wine )) && probe_args+=(--still-head)
# The hand interaction program places the test hand from the head, which must stand still
[[ -n ${hand_interaction} ]] && probe_args+=(--still-head)
probe_args+=("${more_probe_args[@]}")
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
    if (( openvr_overlay )); then
        # The frames of the program reach the host although an overlay is up: the host's count of game frames
        python3 -I - "${work}/host.out" "${fps}" <<'PYTHON' || { print -u2 "The host got too few game frames from the OpenVR program while its overlay was up; see ${work}/host.out"; openvr_exit=1; }
import json, sys
fps = float(sys.argv[2])
counts = []
for line in open(sys.argv[1], errors="replace"):
    if line.startswith("@cranvr") and '"type":"stats"' in line:
        counts.append(json.loads(line[8:]).get("game_frames") or 0)
# The first windows hold the start of the program; the run ends with a window that the program left early
counts = counts[2:-1]
if not counts:
    print("overlay: no stats windows", file=sys.stderr)
    sys.exit(1)
mean = sum(counts) / len(counts) / 5.0
print("overlay: the host got %.1f game frames a second at %g Hz (windows: %s)" % (mean, fps, counts))
sys.exit(0 if mean >= 0.5 * fps else 1)
PYTHON
    fi
    # The picture of each eye reaches the host the right way up: the program's upper half is bright, its lower half nearly black
    python3 -I - "${work}/eyes" <<'PYTHON' || { print -u2 "The picture of the OpenVR program is not the right way up in the host; see ${work}/eyes"; openvr_exit=1; }
import struct, sys, zlib
def read_png(path):
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    pos, idat, width, height = 8, b"", 0, 0
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            width, height = struct.unpack(">II", body[:8])
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    raw = zlib.decompress(idat)
    stride = width * 3 + 1
    return width, height, [raw[y * stride + 1:(y + 1) * stride] for y in range(height)]
def brightness(rows):
    return sum(sum(row) for row in rows) / max(1, sum(len(row) for row in rows))
bad = False
for eye in ("left", "right"):
    width, height, rows = read_png("%s/eye-%s.png" % (sys.argv[1], eye))
    quarter = max(1, height // 4)
    top, bottom = brightness(rows[:quarter]), brightness(rows[-quarter:])
    print("eyes: the %s eye's upper quarter has brightness %.0f, its lower quarter %.0f" % (eye, top, bottom))
    if not top > 2 * bottom:
        bad = True
sys.exit(1 if bad else 0)
PYTHON
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
hand_interaction_exit=0
if [[ -n ${hand_interaction} ]]; then
    # The Windows program ends on its own: wait for its verdict
    for i in {1..60}; do
        grep -qE "hand_interaction_smoke: (PASS|FAIL)" "${work}/game.log" && break
        sleep 0.5
    done
    grep -h "hand_interaction_smoke:" "${work}/game.log" || true
    grep -q "hand_interaction_smoke: PASS" "${work}/game.log" || { print -u2 "The hand interaction program did not pass; see ${work}/game.log"; hand_interaction_exit=1; }
    grep -h "the game uses" "${work}/host.log" | tail -2 || true
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
    if [[ -n ${game_stall} ]]; then
        # The headset shows the game's last picture while the game sends nothing: no frame of the host's own scene
        # (the probe's "wrong badges") lies between the game's last frame and its first frame after the stall
        python3 -I - "${work}/probe.log" "${work}/game.log" <<'PYTHON' || game_exit=1
import re, sys
probe, game = (open(path, errors="replace").read() for path in sys.argv[1:3])
stall = re.search(r"fake-game: no frames from unix (\d+) ms for ([\d.]+) s", game)
again = re.search(r"fake-game: frames again at unix (\d+) ms", game)
if not stall or not again:
    print("stall: the fake game did not log its stall", file=sys.stderr)
    sys.exit(1)
begin, end = int(stall.group(1)), int(again.group(1))
wrong = []
for line in re.findall(r"probe: frames with the wrong badges, at unix ms: (.*)", probe):
    for first, last, count in re.findall(r"(\d+)\.\.(\d+) ms \((\d+)\)", line):
        wrong.append((int(first), int(last), int(count)))
inside = [(a, b, n) for a, b, n in wrong if b >= begin and a <= end]
print("stall: the game sent no frame for %d ms; frames of the host's own scene during that time: %d" % (end - begin, sum(n for _, _, n in inside)))
if inside:
    print("stall: the host drew its own scene while the game stalled: %s" % inside, file=sys.stderr)
    sys.exit(1)
PYTHON
    fi
    grep -hE "fake-game: (input|tracking|controllers made|hand controller)" "${work}/game.log" || true
    (( game_exit == 0 )) || print -u2 "The fake game failed or saw wrong input; see ${work}/game.log"
fi
displays_exit=0
if (( displays > 1 && ! displays_in_game )); then
    # Every display got its own stream and pictures: the probe's summary counts them, and the host says
    # which displays it captured
    grep -h "probe: displays:" "${work}/probe.log" || { print -u2 "The probe printed no summary of the displays; see ${work}/probe.log"; displays_exit=1; }
    for (( n = 2; n <= displays; n++ )); do
        grep -q "Native display ${n}: capturing display" "${work}/host.out" || { print -u2 "The host captured no display ${n}; see ${work}/host.out"; displays_exit=1; }
        grep -hE "probe: desktop display ${n}:.*pictures" "${work}/probe.log" | head -1 || true
    done
fi
if (( displays_in_game )); then
    # A game with no display shown costs nothing: the host's windows with no wish and after the wish ended
    # captured, encoded and sent nothing, and the probe received nothing. The windows in between did work.
    python3 -I - "${work}/probe.log" "${work}/host.out" <<'PYTHON' || displays_exit=1
import re, sys
probe, host = (open(path, errors="replace").read() for path in sys.argv[1:3])
def numbers(pattern, text):
    found = re.search(pattern, text)
    return [int(n) for n in found.groups()] if found else None
idle = numbers(r"nothing wished for \d+ s: (\d+) units arrived, (\d+) pictures decoded", probe)
shown = numbers(r"shown for \d+ s: (\d+) units arrived, (\d+) pictures decoded", probe)
after = numbers(r"after the wish ended: (\d+) units arrived in the first 1\.5 s, (\d+) in the next 4 s", probe)
windows = [
    {"runs": m[2] == "a game runs", "shown": int(m[1]), "captured": int(m[3]), "encoded": int(m[4]), "sent": int(m[5]), "cursors": int(m[6])}
    for m in re.findall(r"Native displays: (\d+) known, (\d+) shown, (a game runs|no game runs); in the last [0-9.]+ s: (\d+) pictures captured, (\d+) encoded, (\d+) sent, (\d+) cursor datagrams sent", host)
]
free = lambda w: w["captured"] == w["encoded"] == w["sent"] == w["cursors"] == 0 and w["shown"] == 0
problems = []
if idle is None or shown is None or after is None:
    problems.append("the probe printed no counts of the in-game script")
else:
    print("displays in a game: nothing wished: %d units, %d pictures; shown: %d units, %d pictures; after the wish: %d units in 1.5 s, %d in the next 4 s" % (*idle, *shown, *after))
    if idle != [0, 0]:
        problems.append("units or pictures arrived while nothing was wished")
    if shown[0] == 0 or shown[1] == 0:
        problems.append("no pictures arrived while the displays were shown")
    if after[1] != 0:
        problems.append("pictures arrived after the wish ended")
quiet = [w for w in windows if w["runs"] and free(w)]
busy = [w for w in windows if w["runs"] and w["captured"] > 0 and w["encoded"] > 0 and w["sent"] > 0]
print("displays in a game: the host's log has %d windows of a running game with no display shown and no capture, encode or send, and %d windows with all three" % (len(quiet), len(busy)))
if len(quiet) < 4:
    problems.append("fewer than 4 windows of the host's log show a game that costs nothing")
if not busy:
    problems.append("no window of the host's log shows a display at work")
# After the last window with pictures the windows go quiet again (the first one may hold the end of the work)
last_busy = max((i for i, w in enumerate(windows) if w["sent"] > 0), default=-1)
tail = windows[last_busy + 1:]
if len(tail) < 3 or not all(free(w) for w in tail[1:]):
    problems.append("the host's windows after the wish ended are not all quiet (%d windows)" % len(tail))
for problem in problems:
    print("displays in a game: " + problem, file=sys.stderr)
sys.exit(1 if problems else 0)
PYTHON
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
# The probe's pointer script presses the left, right and middle buttons and leans the thumbstick in
# both axes. The host logs what it would have injected (a dry run). Each kind must show, once the run
# lasts for two turns of the script.
mouse_exit=0
if (( desktop )) && [[ ${client} == mac ]]; then
    python3 -I - "${work}/host.out" "${seconds}" <<'PYTHON' || mouse_exit=1
import re, sys
text = open(sys.argv[1], errors="replace").read()
totals = [0] * 7
for m in re.finditer(r"mouse events: (\d+) moves, (\d+) downs \((\d+) right, (\d+) middle\), (\d+) ups, (\d+) px scrolled, (\d+) px scrolled across", text):
    totals = [a + int(b) for a, b in zip(totals, m.groups())]
moves, downs, right, middle, ups, forward, across = totals
print(f"host: mouse events (dry run): {moves} moves, {downs} downs ({right} right, {middle} middle), {ups} ups, {forward} px scrolled, {across} px scrolled across")
if int(sys.argv[2]) >= 14:
    missing = [name for name, n in [("left", downs - right - middle), ("right", right), ("middle", middle), ("up and down scroll", forward), ("sideways scroll", across)] if n <= 0]
    # The last press may go down in the window of the last log line and up after it
    unpaired = abs(ups - downs) > 1
    if missing or unpaired:
        print("the host injected no " + " and no ".join(missing) + (" and its ups differ from its downs" if unpaired else ""), file=sys.stderr)
        sys.exit(1)
PYTHON
fi
result=0
python3 -I "${root}/scripts/e2e-report.py" "${work}/host.out" "${fps}" "${report}" "${extra[@]}" || result=$?
# A pause empties some of the host's 5-second windows, so the report's frame rate says nothing then: the
# probe's verdict on the pause counts, and it is part of the probe's exit code
[[ -z ${pause} ]] || result=0
# A stall empties some of the host's 5-second windows in the same way: the stall check counts
[[ -z ${game_stall} ]] || result=0
(( client_exit == 0 )) || result=1
(( mouse_exit == 0 )) || result=1
(( game_exit == 0 )) || result=1
(( tracking_exit == 0 )) || result=1
(( hand_interaction_exit == 0 )) || result=1
(( openvr_exit == 0 )) || result=1
(( displays_exit == 0 )) || result=1
grep -hE "probe: (done|frame arrival|native|desktop|audio|pause test|displays|in-game displays|[0-9.]+ s: the focus)" "${work}/probe.log" 2>/dev/null || true
print "Report: ${report}"
exit ${result}
