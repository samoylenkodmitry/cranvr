#!/bin/zsh
# End-to-end test of the stream from this Mac to a client. It runs cranvr-host, a client and an
# optional game, then writes a JSON report: every 5-second stats line of the host (fps, render and
# encode time, the client's latency split by stage) and the median and worst of each.
#
# Usage: scripts/e2e.sh [--seconds N] [--fps HZ] [--game | --wine PROGRAM [--wine-arg ARG]... [--sound]] [--desktop]
#                       [--quest [SERIAL] [--room scene|passthrough] | --native-tcp] [--native-drop PERCENT] [--native-port PORT]
#                       [--host-arg ARG]... [--probe-arg ARG]... [--report FILE]
#                       [--tracking [FEATURES] | --hand-controllers | --tracking-wine | --hand-interaction [hand|both]
#                        | --openvr-wine | --openvr-actions MODE | --openvr-overlay | --openvr-gl | --openvr-vulkan | --steamvr
#                        | --xr-vulkan [enable2|enable1|array]
#                        | --swapchain-churn d3d11|d3d11-array|vulkan|vulkan-array [--churn-cycles N] [--churn-frames]]
#                       [--openvr-arg ARG]...
#                       [--displays N] [--hold] [--hand-hold] [--displays-in-game]
#                       [--game-stall AT:FOR] [--moving]
#                       [--pause AT:FOR] [--left-trigger AT:FOR] [--right-trigger AT:FOR]
#                       [--face-source visual|audio|unknown]
#
#   --seconds N   how long the stream runs (default 12: the report needs two 5-second windows after the
#                 first; 14 with --desktop, whose pointer check needs two turns of the probe's script)
#   --fps HZ      the stream's refresh rate (default 120, as the host). The probe also offers 144,
#                 165, 180 and 240 Hz, which measure the room left above a Quest 3's 120 Hz
#   --game        a fake game sends the frames through the BasaltVR runtime protocol. With the
#                 probe it also checks the controller input of the probe's script.
#   --wine PROGRAM a Windows OpenXR program runs in cranvr's Wine stack without sound, through
#                 scripts/run-game.sh: a Windows path such as 'C:\basalt\runtime\d3d11_runtime_smoke.exe'.
#                 The probe then counts every frame of the right size, since the program draws no
#                 eye badges, so the run fails unless the host counted frames of the program. The
#                 run starts the program once the client has linked. The program gets the variables
#                 of BasaltVR's test with a live headset (see start_program), so
#                 d3d11_runtime_smoke.exe runs its checks against the run's host. --wine-arg passes
#                 one argument to the program.
#   --tracking [FEATURES]  hands, body, face and eyes: the fake game uses these features (a comma
#                 list of hands, body, full_body, face and eyes; default all), the probe offers
#                 them and plays the script of probe/src/tracked.rs, and the fake game checks what
#                 reaches it. The run also checks that the host asked the headset only after the
#                 game created its trackers. Implies --game.
#   --hand-controllers  a game without hand code: the probe tracks hands and no controller, and the
#                 fake game checks that the host made controllers of the hands (a pinch is the
#                 trigger, the aim pose points from the shoulder through the fingers, the grip pose
#                 sits at the palm joint with the axes of OpenXR's grip pose: +X into the right palm
#                 and out of the back of the left hand, -Z from the little finger to the index
#                 finger). Implies --game.
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
#                 scripts/run-game.sh writes for CRANVR_OPENVR=opencomposite (this option sets it). It
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
#   --openvr-vulkan  the same program with Vulkan, as a Vulkan game draws (openvr_smoke.exe --vulkan): the device of
#                 GetOutputDevice and an R8G8B8A8 image per eye, which OpenComposite blits into the runtime's B8G8R8A8
#                 images on the GPU. Each eye is exact black with a gray band down its left edge and a block of the
#                 eye's color in its upper half. --openvr-arg --format --openvr-arg bgra takes B8G8R8A8 images, which
#                 OpenComposite copies as they are, and --openvr-arg --samples --openvr-arg 4 multisampled ones, which it
#                 resolves. Implies --openvr-wine.
#   --openvr-arg ARG  one more argument of openvr_smoke.exe in the --openvr-* runs. --openvr-arg --scale --openvr-arg 1.118
#                 draws the eyes at 1.118 times the recommended size, as Serious Sam VR does: with --host-arg --game-scale
#                 --host-arg 1 its images of 2307x2468 lie above a Quest 3 eye (#164)
#   --xr-vulkan [enable2|enable1|array]  an OpenXR program that renders with Vulkan: C:\basalt\runtime\vulkan_runtime_smoke.exe
#                 runs in Wine with XR_KHR_vulkan_enable2 (the default), with XR_KHR_vulkan_enable (enable1) or with one
#                 swapchain of two slices (array). It draws the picture of --openvr-vulkan into the runtime's IOSurface
#                 images. The run checks the program's verdict, the host's count of game frames (90 percent of the rate
#                 in the windows after the first) and the colors of each eye in the host's copy of the eyes. It works with
#                 --quest too. Set CRANVR_HOME to a stack that scripts/setup-wine.sh built with the runtime and the Wine
#                 files to test.
#   --swapchain-churn d3d11|d3d11-array|vulkan|vulkan-array  a leak test of swapchain images: C:\basalt\runtime\swapchain_churn.exe
#                 runs in Wine and creates and destroys a swapchain of three images --churn-cycles times (default 1000) through
#                 Direct3D 11 or Vulkan (-array: swapchains with two slices). Its images are 333 pixels high, which no
#                 game uses, and of a new width each time. The run measures the program's memory footprint (`footprint`)
#                 and the live global IOSurfaces of that height (scripts/iosurfaces.py) after the first swapchain and
#                 after the last one, and the IOSurfaces once more after the program ended. It fails when the IOSurfaces
#                 of the destroyed swapchains stay, also in another process such as launchd or the host, or when the
#                 IOSurfaces of the footprint ("IOSurface" and "Owned physical footprint (unmapped)") grew by more than a
#                 twentieth of the memory that the swapchains had. The whole footprint moves with the GPU memory that Metal
#                 makes purgeable: the run reports it. The run lasts 10 s.
#                 --churn-frames shows each swapchain in one frame and destroys it right after xrEndFrame, while the GPU
#                 may still draw the frame and copy its slices: the runtime must keep the images until the GPU is done,
#                 and the host gets each swapchain's frame. The runtime paces the frames, so the default is then 120
#                 swapchains (a second at 120 Hz).
#                 Set CRANVR_HOME to a stack that scripts/setup-wine.sh built with the runtime and DXMT to test.
#   --steamvr     the same program through SteamVR for Windows and cranvr's driver (steamvr/driver_cranvr.cpp):
#                 scripts/run-game.sh writes the OpenVR path file for CRANVR_OPENVR=steamvr, and the program's
#                 VR_Init starts SteamVR from the prefix's Steam library. The checks are those of --openvr-wine. The
#                 run lasts at least 60 seconds, because SteamVR takes 10 to 20 seconds to start. SteamVR ends with
#                 the program. Set CRANVR_HOME to a stack with SteamVR in its Steam library and the driver.
#                 The probe offers a Quest 3's view (44 degrees up, 55 down), and the eye check also fails on
#                 black rows at the bottom of an eye: a runtime that swaps the up and down angles leaves them.
#   --face-source SOURCE  with --tracking and --tracking-wine: where the probe's face comes from.
#                 audio (the default) is a Quest 3's microphone face, visual is a face from cameras,
#                 and unknown is an offer that names no source, as a headset's offer before its face gives data. The
#                 offer, the host's file for the runtime and each sample carry the source. The
#                 Windows program checks that the support structure says it, that a face tracker
#                 which asks for the other source is refused, and each sample's dataSource.
#   --moving      the fake game's picture moves all the time: a landscape with fine detail pans across
#                 both eyes (cranvr-fake-game --moving), so a headset that lies still gets a moving
#                 picture, as a game sends while the head turns. Implies --game. A quality test of the
#                 eye stream on a Quest takes it, such as a comparison of --host-arg --foveation levels.
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
#   --sound       with --wine: the program plays its sound through Wine's default output (scripts/run-game.sh
#                 without --mute). With --desktop the probe reports the sound's peak level, so the run shows
#                 whether the program's sound reaches the stream.
#   --desktop     the host shows this Mac's screen. The probe asks for the desktop stream, plays a
#                 pointer script (the host only logs its mouse events: CRANVR_POINTER_DRY_RUN) and
#                 counts the Mac's sound. The script presses the left, right and middle buttons and
#                 leans the thumbstick in both axes. A run of 14 seconds or more fails when the host's
#                 log shows no press of one button, no scroll in one axis or a press without its
#                 release. It counts every frame of the right size, since the desktop scene draws no
#                 eye badges. The run fails when the probe gets the sound for less than 80 percent of
#                 the stream time. Without System Audio Recording for the terminal app the host sends
#                 no sound. The run then skips that check. --host-arg --virtual-display
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
#   --cable usb-network|adb  with --quest: the headset reaches the run's host over the USB cable
#                 instead of Wi-Fi. usb-network: the headset's USB network, with no `adb reverse` for the
#                 run's port; the host's log must show a link-local IPv6 peer and UDP media. adb: `adb
#                 reverse` for the run's port, with the headset's USB network not asked for
#                 (usb_network false); the log must show the peer 127.0.0.1 and TCP media. The headset
#                 needs a few seconds to change its USB functions between the two, so an adb run right
#                 after a usb-network run waits for adb to settle. With --host-arg --no-cable (the
#                 Connection choice Wi-Fi) the way is set up the same, and the log must show the
#                 headset's Wi-Fi address and UDP media: the host takes no link over the cable.
#                 Without --quest the client is cranvr-probe on this Mac, over loopback with UDP
#                 media and parity packets. It simulates a headset's display: it reports the
#                 frame timing and the loss, and it checks the eye badges of the host's scene.
#   --room scene|passthrough  with --quest: the headset's room for the run (default scene). With
#                 passthrough the headset shows the stream only while a game runs and reports no
#                 frame timing without one.
#   --native-tcp  with the probe: the media goes over TCP, as it does over USB with `adb reverse`
#   --native-drop PERCENT  with the probe: it drops this share of the media packets, so the
#                 parity has loss to recover. UDP media only: TCP has no parity and loses nothing
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
# the right picture. It exits with 1 otherwise. Every program that the run starts (a Windows program or
# the fake game) also fails the run when its log has a FAIL line ("FAIL: ..." or "NAME: FAIL ...") or
# when it ends on its own with an exit code other than 0. A program that still runs at the end, such as
# a game, ends with the run.

set -euo pipefail
# zsh lowers the priority of a background job by 5 ("nice") unless this option is off. The host and the game run as
# background jobs here. The cranvr app starts them at normal priority, so a test with the default option measured a
# game that lost the Mac's CPU time to every build that ran beside it.
unsetopt BG_NICE

root=${0:A:h:h}
source "${root}/scripts/guardian.sh"
seconds=12 seconds_given=0 fps=120 fps_given=0 game=0 desktop=0 sound=0 client=mac serial="" report=""
displays=1 hold=0 hand_hold=0 displays_in_game=0 display_test=0
tracking=0 tracking_features=hands,body,full_body,face,eyes hand_controllers=0 tracking_wine=0 hand_interaction="" openvr_wine=0 openvr_actions="" openvr_overlay=0 openvr_gl=0 steamvr=0 face_source=audio
openvr_vulkan=0 openvr_args=() xr_vulkan="" swapchain_churn="" churn_cycles=1000 churn_cycles_given=0 churn_frames=0
game_stall="" moving=0
wine="" wine_args=() wine_given=0
native_tcp=0 native_drop=0 native_port=9950 pause="" left_trigger="" right_trigger="" cable="" room=scene
more_host_args=() more_probe_args=()
while (( $# )); do
    case $1 in
        --seconds) seconds=$2; seconds_given=1; shift 2 ;;
        --fps) fps=$2; fps_given=1; shift 2 ;;
        --game) game=1; shift ;;
        --moving) game=1 moving=1; shift ;;
        --tracking)
            game=1 tracking=1; shift
            if (( $# )) && [[ $1 != --* ]]; then tracking_features=$1; shift; fi ;;
        --hand-controllers) game=1 hand_controllers=1; shift ;;
        --game-stall) game=1 game_stall=$2; shift 2 ;;
        --tracking-wine) tracking_wine=1; shift ;;
        --hand-interaction)
            hand_interaction=hand; shift
            if (( $# )) && [[ $1 != --* ]]; then hand_interaction=$1; shift; fi
            [[ ${hand_interaction} == (hand|both) ]] || { print -u2 -r -- "--hand-interaction takes hand or both"; exit 2; } ;;
        --openvr-wine) openvr_wine=1; shift ;;
        --openvr-overlay) openvr_wine=1 openvr_overlay=1; shift ;;
        --openvr-gl) openvr_wine=1 openvr_gl=1; shift ;;
        --openvr-vulkan) openvr_wine=1 openvr_vulkan=1; shift ;;
        --openvr-arg) openvr_args+=("$2"); shift 2 ;;
        --xr-vulkan)
            xr_vulkan=enable2; shift
            if (( $# )) && [[ $1 != --* ]]; then xr_vulkan=$1; shift; fi
            [[ ${xr_vulkan} == (enable2|enable1|array) ]] || { print -u2 -r -- "--xr-vulkan takes enable2, enable1 or array"; exit 2; } ;;
        --swapchain-churn)
            swapchain_churn=$2; shift 2
            [[ ${swapchain_churn} == (d3d11|d3d11-array|vulkan|vulkan-array) ]] || { print -u2 -r -- "--swapchain-churn takes d3d11, d3d11-array, vulkan or vulkan-array"; exit 2; } ;;
        --churn-cycles) churn_cycles=$2; churn_cycles_given=1; shift 2 ;;
        --churn-frames) churn_frames=1; shift ;;
        --steamvr) openvr_wine=1 steamvr=1; shift ;;
        --openvr-actions)
            openvr_wine=1 openvr_actions=$2; shift 2
            [[ ${openvr_actions} == (touch|vive|both) ]] || { print -u2 -r -- "--openvr-actions takes touch, vive or both"; exit 2; } ;;
        --face-source)
            face_source=$2; shift 2
            [[ ${face_source} == (visual|audio|unknown) ]] || { print -u2 -r -- "--face-source takes visual, audio or unknown"; exit 2; } ;;
        --desktop) desktop=1; shift ;;
        --sound) sound=1; shift ;;
        --displays) displays=$2; shift 2 ;;
        --display-test) display_test=1; shift ;;
        --hold) game=1 hold=1; shift ;;
        --hand-hold) game=1 hand_controllers=1 hand_hold=1; shift ;;
        --displays-in-game) game=1 displays_in_game=1; shift ;;
        --wine) wine=$2 wine_given=1; shift 2 ;;
        --wine-arg) wine_args+=("$2"); shift 2 ;;
        --cranvr-client|--native) shift ;;
        --native-tcp) native_tcp=1; shift ;;
        --native-drop) native_drop=$2; shift 2 ;;
        --native-port) native_port=$2; shift 2 ;;
        --cable) cable=$2; shift 2 ;;
        --room)
            room=$2; shift 2
            [[ ${room} == (scene|passthrough) ]] || { print -u2 -r -- "--room takes scene or passthrough"; exit 2; } ;;
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

# The pointer check of --desktop needs two turns of the probe's script
(( desktop && ! seconds_given && seconds < 14 )) && seconds=14
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
    print -u2 -r -- "--tracking, --hand-controllers, --tracking-wine and --openvr-wine belong to the probe: they cannot combine with --quest."
    exit 2
fi
if [[ ${client} == quest && -n ${hand_interaction} ]]; then
    print -u2 -r -- "--hand-interaction belongs to the probe: its checks need the probe's still head and controllers."
    exit 2
fi
if (( openvr_wine )); then
    # The program ends a few seconds before the probe, so its verdict is in the log when the probe is done.
    # SteamVR starts within the program's VR_Init, which takes its time before the program's seconds count.
    if (( steamvr )); then
        (( seconds >= 60 )) || seconds=60
        program_seconds=$(( seconds - 35 ))
        export CRANVR_OPENVR=steamvr
    else
        (( seconds >= 20 )) || seconds=20
        program_seconds=$(( seconds - 6 ))
        export CRANVR_OPENVR=opencomposite
    fi
    wine='C:\opencomposite\openvr_smoke.exe'
    wine_args=(--seconds ${program_seconds} --expect-input)
    [[ -z ${openvr_actions} ]] || wine_args+=(--actions "${openvr_actions}")
    (( ! openvr_overlay )) || wine_args+=(--overlay)
    (( ! openvr_gl )) || wine_args+=(--gl)
    (( ! openvr_vulkan )) || wine_args+=(--vulkan)
    wine_args+=("${openvr_args[@]}")
    # The run tests the path of an OpenVR program, not the rate. On a Mac that other builds keep busy (a
    # load average above 20) the program loses frames to the scheduler: the report of its second-by-second
    # frame counts shows 120 in the quiet seconds and 60 to 100 in the busy ones. The rest of the loss was
    # the start (OpenComposite's restarts of the session). The run therefore asks the host for 72 Hz unless
    # --fps is given.
    (( fps_given )) || fps=72
    # What the runtime tells the game about the display must be the rate that the host runs at
    wine_args+=(--expect-hz "${fps}")
fi
if [[ -n ${xr_vulkan} ]]; then
    # The program ends a few seconds before the run, so its verdict is in the log. The host's second 5-second
    # window lies inside the program's run, which starts about 2 s after the host.
    (( seconds >= 16 )) || seconds=16
    wine='C:\basalt\runtime\vulkan_runtime_smoke.exe'
    wine_args=(--seconds $(( seconds - 3 )))
    [[ ${xr_vulkan} == enable1 ]] && wine_args+=(--enable1)
    [[ ${xr_vulkan} == array ]] && wine_args+=(--array)
fi
if (( tracking_wine )); then
    # The Windows program waits for the headset's features, starts a session and reads the data
    wine='C:\basalt\runtime\tracking_smoke.exe'
    wine_args=(--seconds 25 --face-source "${face_source}")
    (( seconds >= 40 )) || seconds=40
fi

if [[ -n ${pause} && ${client} == quest ]]; then
    print -u2 -r -- "--pause belongs to the probe: it cannot combine with --quest."
    exit 2
fi
if [[ ${client} == quest ]] && (( native_tcp || native_drop )); then
    print -u2 -r -- "--native-tcp and --native-drop belong to the probe: they cannot combine with --quest."
    exit 2
fi
if (( native_tcp && native_drop )); then
    # TCP carries no parity and loses no packet. A packet that the probe drops there loses its frame, the probe waits for
    # a key frame, and a drop of 3 percent of the packets left 58 percent of the frames: a loss that no TCP link has.
    print -u2 -r -- "--native-drop tests the parity of UDP media: it cannot combine with --native-tcp."
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
if [[ -n ${swapchain_churn} ]]; then
    # The program stops after its first and after its last swapchain until the run has measured it (see below)
    churn_signal=${work}/churn-signal
    churn_height=333
    (( churn_frames && ! churn_cycles_given )) && churn_cycles=120
    wine='C:\basalt\runtime\swapchain_churn.exe'
    wine_args=(--cycles "${churn_cycles}" --height "${churn_height}" --signal "Z:${churn_signal//\//\\}")
    case ${swapchain_churn} in
        d3d11-array) wine_args+=(--slices 2) ;;
        vulkan) wine_args+=(--vulkan) ;;
        vulkan-array) wine_args+=(--vulkan --slices 2) ;;
    esac
    (( churn_frames )) && wine_args+=(--frames)
    (( seconds_given )) || seconds=10
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
package=""
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
        # A run over the USB network leaves the key under the cable's name
        extras=(--es forget_host_key "${test_host},usb-network:${native_port}")
    fi
    # The client counts as back when it still runs 3 s after its link (scripts/give-back.sh)
    if CRANVR_OWNER_HOST=${owner} "${root}/scripts/give-back.sh" "${serial}" -- "${extras[@]}"; then
        print "The headset runs on ${owner} again${extras:+ and forgets the key of ${test_host}}"
    else
        print -u2 "The headset did not come back on ${owner}"
    fi
}

cleanup() {
    # Guardian first: nothing below may keep it paused
    guardian_restore || true
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
    if [[ ${cable} == adb && ${native_port} != "${owner_port}" ]]; then
        "${adb_cmd[@]}" reverse --remove "tcp:${native_port}" >/dev/null 2>&1 || true
    fi
    if [[ ${client} == quest && -n ${package} ]]; then
        give_back
    fi
}
trap cleanup EXIT
guardian_traps

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
    # The pause lasts for the run. Cleanup sets the exact old value again (see guardian.sh).
    guardian_pause "${adb_cmd[@]}" || { print -u2 "Cannot read Guardian's value on the Quest"; exit 1; }
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
if (( openvr_wine )) || [[ -n ${xr_vulkan} ]]; then
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
        (( moving )) && game_args+=(--moving)
        "${bin}/cranvr-fake-game" --control "${work}/config/runtime-control.txt" --fps "${fps}" \
            --seconds $(( seconds + 4 )) "${game_args[@]}" > "${work}/game.log" 2>&1 &
    else
        game_args=()
        (( moving )) && game_args+=(--moving)
        "${bin}/cranvr-fake-game" --control "${work}/config/runtime-control.txt" --fps "${fps}" \
            --seconds $(( seconds + 60 )) "${game_args[@]}" > "${work}/game.log" 2>&1 &
    fi
    game_pid=$!
    pids+=(${game_pid})
fi

# The live IOSurfaces of the leak test's height, as "COUNT BYTES"
churn_count() {
    python3 -I "${root}/scripts/iosurfaces.py" "${churn_height}"
}
if [[ -n ${swapchain_churn} ]]; then
    # Surfaces of the height that an earlier run left in launchd stay until the user logs out: the run compares with
    # this count. Each line of churn.txt: the stage, the program's footprint and its IOSurface part in bytes, the count of
    # the surfaces and their bytes.
    print "start 0 0 $(churn_count)" > "${work}/churn.txt"
fi
program_pid=""
view_size=""
# Starts the Windows program once the client has linked, as a user starts a game after the headset linked. The host
# writes the stream's view size into the control file then, before its status line of the link, and the runtime reads
# the file when the program starts. A program that started before the link got a Quest 3 eye whatever the game render
# size, so its size depended on which of the two came first (#169).
start_program() {
    [[ -n ${wine} && -z ${program_pid} ]] || return 0
    # BasaltVR's test programs, such as d3d11_runtime_smoke.exe, check the runtime against the native test daemon of
    # BasaltVR's build unless these variables say what a live host gives a game. BasaltVR's own test with a headset
    # sets the same (runtime/scripts/test-headset.zsh). Without them the program failed its own checks under
    # cranvr-host before its first frame, and the run passed on the host's own scene (#169). Games read none of them.
    read -r _ _ view_width view_height _ < "${work}/config/runtime-control.txt"
    view_size=${view_width}x${view_height}
    export BASALT_EXPECT_VIEW_WIDTH=${view_width} BASALT_EXPECT_VIEW_HEIGHT=${view_height}
    export BASALT_REQUIRE_HEAD_POSE=1 BASALT_EXPECT_IOSURFACE=1
    # The probe's controllers follow its script. A Quest on a desk sends no controller input, and the program then
    # expects none.
    if [[ ${client} == mac ]]; then
        export BASALT_REQUIRE_CONTROLLER_INPUT=1
    fi
    print -r "Starting ${wine} in Wine at a view of ${view_size} (log: ${work}/game.log)"
    local mute=(--mute)
    (( sound )) && mute=()
    CRANVR_CONTROL=${work}/config/runtime-control.txt "${root}/scripts/run-game.sh" "${mute[@]}" "${wine}" "${wine_args[@]}" \
        > "${work}/game.log" 2>&1 &
    program_pid=$!
    pids+=(${program_pid})
    # The cranvr app starts a game at normal priority, so the run's program must run at nice 0 too: at nice 5 a
    # game lost frames to the Mac's other work (#155). The first look at the program's process keeps its nice
    # value. Its command line starts with its Windows path.
    (
        name=${wine##*\\}
        for i in {1..600}; do
            # awk reads all of ps's lines: an early exit would end ps with SIGPIPE, which pipefail turns into a failure
            value=$(ps -axo nice=,command= | awk -v name="${name}" '!seen && $2 ~ /^[A-Za-z]:/ && index($0, name) { found = $1; seen = 1 } END { print found }')
            if [[ -n ${value} ]]; then
                print -r -- "${value}" > "${work}/program-nice.txt"
                exit 0
            fi
            sleep 0.1
        done
    ) &
    pids+=($!)
}
if [[ -n ${swapchain_churn} ]]; then
    # The program waits at each stop until the run has measured it
    (
        warm="0 0"
        for stage in warm churned; do
            for i in {1..1200}; do
                grep -q "swapchain_churn: measure ${stage}" "${work}/game.log" 2>/dev/null && break
                grep -qE "swapchain_churn: (PASS|FAIL)" "${work}/game.log" 2>/dev/null && exit 0
                sleep 0.05
            done
            count=$(churn_count)
            # The surfaces go a few milliseconds after their last owner, and the host lets go of a swapchain's images when
            # its frame loop takes the event: the count gets a second to come down to the count after the first swapchain
            if [[ ${stage} == churned ]]; then
                for i in {1..20}; do
                    (( ${count%% *} <= ${warm%% *} )) && break
                    sleep 0.05
                    count=$(churn_count)
                done
            fi
            pid=$(pgrep -f '^C:.basalt.runtime.swapchain_churn.exe' | head -1)
            # The whole footprint moves with the GPU memory that Metal makes purgeable ("(graphics)"). The process's
            # IOSurfaces count in two categories: "IOSurface" for those mapped into it (MoltenVK maps its images) and
            # "Owned physical footprint (unmapped)" for the rest (DXMT's). The categories stay beside the report.
            footprint -p "${pid:-0}" -f bytes > "${work}/footprint-${stage}.txt" 2>/dev/null || true
            bytes=$(awk '/phys_footprint:/ { print $2; exit }' "${work}/footprint-${stage}.txt")
            mapped=$(awk '$2 == "B" && $4 == "B" && $6 == "B" {
                category = $8; for (i = 9; i <= NF; i++) category = category " " $i
                if (category == "IOSurface" || category == "Owned physical footprint (unmapped)") sum += $1
            } END { print sum + 0 }' "${work}/footprint-${stage}.txt")
            print "${stage} ${bytes:-0} ${mapped:-0} ${count}" >> "${work}/churn.txt"
            [[ ${stage} == warm ]] && warm=${count}
            : > "${churn_signal}.${stage}"
        done
    ) &
    pids+=($!)
fi
# SteamVR goes on drawing its own room after the program ends, so the host's last eyes are SteamVR's. The run keeps
# the eyes of 10 seconds into the program's frames for the check below.
if (( steamvr )); then
    (
        for i in {1..240}; do
            grep -q "openvr_smoke: initialised" "${work}/game.log" 2>/dev/null && break
            sleep 0.5
        done
        sleep 10
        mkdir -p "${work}/eyes-game"
        cp "${work}/eyes/eye-left.png" "${work}/eyes/eye-right.png" "${work}/eyes-game/" 2>/dev/null
    ) &
    pids+=($!)
fi

probe_args=(--seconds "${seconds}" --connect "127.0.0.1:${native_port}" --drop-percent "${native_drop}")
(( native_tcp )) && probe_args+=(--tcp)
(( game )) && probe_args+=(--game)
(( steamvr )) && probe_args+=(--quest-view)
# A real program, the desktop scene and the moving landscape of the fake game draw no eye badges
if [[ -n ${wine} ]] || (( desktop || moving )); then
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
        HOME=${work}/probe-home "${bin}/cranvr-probe" "${probe_args[@]}" > "${work}/probe.log" 2>&1 &
        probe_pid=$!
        pids+=(${probe_pid})
        if [[ -n ${wine} ]]; then
            # The probe links within a second. A probe that never links fails the run, and the program then starts
            # without a stream as before.
            for i in {1..100}; do
                headset_connected && break
                kill -0 ${probe_pid} 2>/dev/null || break
                sleep 0.1
            done
            start_program
        fi
        wait ${probe_pid} || client_exit=$?
        ;;
    quest)
        print "Starting ${package} on ${serial}"
        # The Cranpose client takes the host's address, the start of the connection and the room
        # from the extras of its launch
        "${adb_cmd[@]}" shell am force-stop "${package}"
        launch=(--es host "${mac_ip}:${native_port}")
        case ${cable} in
            usb-network)
                "${adb_cmd[@]}" reverse --remove "tcp:${native_port}" >/dev/null 2>&1 || true
                launch=(--es host "127.0.0.1:${native_port}" --ei usb_port "${native_port}") ;;
            adb)
                # A client that held the USB network leaves it up for about 30 s after its stop. The
                # headset's change back to its old USB functions would cut `adb reverse` in the run,
                # so the run waits until the headset has no network on usb0, and then for adb, which
                # is gone for a moment during the change.
                for i in {1..90}; do
                    networks=$("${adb_cmd[@]}" shell dumpsys connectivity 2>/dev/null) || networks="usb0 unknown"
                    [[ ${networks} != *usb0* ]] && break
                    sleep 1
                done
                sleep 3
                "${adb_cmd[@]}" wait-for-device
                "${adb_cmd[@]}" reverse "tcp:${native_port}" "tcp:${native_port}" >/dev/null
                launch=(--es host "127.0.0.1:${native_port}" --ei usb_port "${native_port}" --ez usb_network false) ;;
        esac
        # The headset's shell brings its home to the front after a stop. A start 70 ms after the stop
        # lost that race on 2026-11-27: the client stayed behind the home, never linked, and the run
        # waited 60 s for nothing. The run waits a second after the stop and starts the client again
        # when it has not linked in 15 s.
        sleep 1
        for attempt in 1 2; do
            "${adb_cmd[@]}" shell am start -n "${package}/dev.cranpose.android.CranposeActivity" \
                "${launch[@]}" --ez connect true --es room "${room}" >/dev/null
            for i in {1..30}; do
                headset_connected && break
                sleep 0.5
            done
            headset_connected && break
            (( attempt == 1 )) && print -u2 "The Quest did not connect in 15 seconds: the run starts its client again"
        done
        if ! headset_connected; then
            print -u2 "The Quest did not connect in two starts of 15 seconds. Check that it shares a network with this Mac."
            client_exit=1
        else
            print "Streaming for ${seconds} seconds"
            start_program
            # A signal ends the wait at once, so cleanup gives Guardian back without delay
            sleep "${seconds}" & wait $!
            if [[ -n ${cable} ]]; then
                # The host's log names the way that the run took
                path_line=$(grep -h "Native protocol: .* connected, .* media" "${work}/host.log" "${work}/host.out" 2>/dev/null | tail -1)
                print "Path: ${path_line##*Native protocol: }"
                if (( ${more_host_args[(Ie)--no-cable]} )); then
                    # The choice of Wi-Fi: an IPv4 address of the network, neither loopback nor the cable's link
                    [[ ${path_line} == *" at "[0-9]*"UDP media"* && ${path_line} != *" at 127."* ]] \
                        || { print -u2 "The headset on the cable did not stream over Wi-Fi with the host's cable off"; client_exit=1; }
                else
                    case ${cable} in
                        usb-network) [[ ${path_line} == *" at [fe80:"*"UDP media"* ]] || { print -u2 "The headset did not stream over its USB network"; client_exit=1; } ;;
                        adb) [[ ${path_line} == *" at 127.0.0.1:"*"TCP media"* ]] || { print -u2 "The headset did not stream through adb reverse"; client_exit=1; } ;;
                    esac
                fi
            else
                # What the headset shows, both eyes after its compositor
                "${adb_cmd[@]}" exec-out screencap -p > "${work}/quest.png" || true
            fi
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
    # The head's pose in the array of WaitGetPoses is valid in nearly every frame: a game draws its view with it
    python3 -I - "${work}/game.log" <<'PYTHON' || { print -u2 "The OpenVR program got too few valid head poses; see ${work}/game.log"; openvr_exit=1; }
import re, sys
found = re.search(r"openvr_smoke: (\d+) frames in .*?, (\d+) valid head poses", open(sys.argv[1], errors="replace").read())
if not found:
    print("head: the program reported no frame count", file=sys.stderr)
    sys.exit(1)
frames, valid = int(found.group(1)), int(found.group(2))
print("head: %d valid head poses of %d frames" % (valid, frames))
sys.exit(0 if frames > 0 and valid >= 0.9 * frames else 1)
PYTHON
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
    eyes=${work}/eyes
    (( steamvr )) && eyes=${work}/eyes-game
    python3 -I - "${eyes}" <<'PYTHON' || { print -u2 "The picture of the OpenVR program is upside down or leaves black rows in the host; see ${eyes}"; openvr_exit=1; }
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
    # The program's lower half is dark, not black: rows of exact black at the bottom are a part of the view that
    # the picture does not cover
    black = 0
    while black < height and not any(rows[height - 1 - black]):
        black += 1
    print("eyes: the %s eye's upper quarter has brightness %.0f, its lower quarter %.0f, %d black rows at the bottom of %d"
          % (eye, top, bottom, black, height))
    if not top > 2 * bottom or black > height // 50:
        bad = True
    # The eye's color: blue in the left eye, orange in the right one. A swap of the red and blue channels or of the
    # eyes shows here.
    red = sum(row[x] for row in rows[:quarter] for x in range(0, len(row), 3))
    blue = sum(row[x + 2] for row in rows[:quarter] for x in range(0, len(row), 3))
    print("eyes: the %s eye's upper quarter has red %d and blue %d in all" % (eye, red, blue))
    if (blue <= red) if eye == "left" else (red <= blue):
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
xr_vulkan_exit=0
if [[ -n ${xr_vulkan} ]]; then
    for i in {1..60}; do
        grep -qE "vulkan_runtime_smoke: (PASS|FAIL)" "${work}/game.log" && break
        sleep 0.5
    done
    grep -hE "vulkan_runtime_smoke:|Vulkan frames" "${work}/game.log" || true
    grep -q "vulkan_runtime_smoke: PASS" "${work}/game.log" || { print -u2 "The Vulkan program did not pass; see ${work}/game.log"; xr_vulkan_exit=1; }
    # The game's frames reach the host at the rate, and each eye has its color the right way up
    python3 -I - "${work}/host.out" "${fps}" "${work}/eyes" <<'PYTHON' || { print -u2 "The Vulkan program's frames or picture did not reach the host; see ${work}"; xr_vulkan_exit=1; }
import json, struct, sys, zlib
fps = float(sys.argv[2])
counts = []
for line in open(sys.argv[1], errors="replace"):
    if line.startswith("@cranvr") and '"type":"stats"' in line:
        counts.append(json.loads(line[8:]).get("game_frames") or 0)
# The first window with game frames holds the program's start and the last one its end. A headset that links
# later than the probe moves both, so the windows without game frames around them do not count either.
lit = [i for i, count in enumerate(counts) if count > 0]
middle = counts[lit[0] + 1:lit[-1]] if lit else []
bad = not middle
if middle:
    rate = min(middle) / 5.0
    print("vulkan: the host got at least %.1f game frames a second at %g Hz (windows: %s)" % (rate, fps, counts))
    bad = rate < 0.9 * fps
def read_png(path):
    data = open(path, "rb").read()
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
for eye in ("left", "right"):
    width, height, rows = read_png("%s/eye-%s.png" % (sys.argv[3], eye))
    # The block of the eye's color lies at a quarter of the height; the band at the left edge keeps every row lit
    block = rows[height // 4][3 * (width // 2):3 * (width // 2) + 3]
    lower = rows[3 * height // 4][3 * (width // 2):3 * (width // 2) + 3]
    unlit = sum(1 for row in rows if not any(row))
    print("vulkan: the %s eye's block is %s, its lower half %s, %d rows without a lit pixel" % (eye, tuple(block), tuple(lower), unlit))
    blue_eye = eye == "left"
    if (block[2] <= 2 * block[0]) if blue_eye else (block[0] <= 2 * block[2]):
        bad = True
    if max(lower) > 8 or unlit:
        bad = True
sys.exit(1 if bad else 0)
PYTHON
fi
churn_exit=0
if [[ -n ${swapchain_churn} ]]; then
    for i in {1..60}; do
        grep -qE "swapchain_churn: (PASS|FAIL)" "${work}/game.log" && break
        sleep 0.5
    done
    grep -h "swapchain_churn:" "${work}/game.log" || true
    grep -q "swapchain_churn: PASS" "${work}/game.log" || { print -u2 "The swapchain program did not pass; see ${work}/game.log"; churn_exit=1; }
    # After the program's end only another process can hold its surfaces: launchd for a registered Mach port, or the host
    started=$(awk '$1 == "start" { print $4 }' "${work}/churn.txt")
    for i in {1..20}; do
        count=$(churn_count)
        (( ${count%% *} <= ${started:-0} )) && break
        sleep 0.1
    done
    print "exited 0 0 ${count}" >> "${work}/churn.txt"
    python3 -I - "${work}/churn.txt" "${churn_cycles}" "${churn_height}" "${swapchain_churn}" <<'PYTHON' || churn_exit=1
import sys
stages = {}
for line in open(sys.argv[1]):
    name, footprint, mapped, count, size = line.split()
    stages[name] = (int(footprint), int(count), int(size), int(mapped))
cycles, height, mode = int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
slices = 2 if mode.endswith("-array") else 1
# The IOSurfaces that the swapchains had: three images of 256 to 510 pixels across (swapchain_churn.cpp), a surface per slice
made = sum(3 * slices * (256 + 2 * (index % 128)) * 4 * height for index in range(1, cycles + 1))
missing = [name for name in ("start", "warm", "churned", "exited") if name not in stages]
if missing:
    print("churn: the run did not measure %s" % ", ".join(missing), file=sys.stderr)
    sys.exit(1)
mb = lambda value: value / 1048576.0
start, warm, churned, exited = (stages[name] for name in ("start", "warm", "churned", "exited"))
grown = churned[3] - warm[3]
print("churn: %d swapchains (%s); IOSurfaces %d high: %d before the program, %d after its first swapchain, %d after its last, %d after its end (%.1f MB)"
      % (cycles, mode, height, start[1], warm[1], churned[1], exited[1], mb(exited[2])))
print("churn: the program's footprint %.1f MB after its first swapchain, %.1f MB after its last; its IOSurfaces %.1f MB and %.1f MB (%+.1f MB); "
      "the swapchains had %.0f MB of IOSurfaces" % (mb(warm[0]), mb(churned[0]), mb(warm[3]), mb(churned[3]), mb(grown), mb(made)))
bad = []
if churned[1] > warm[1]:
    bad.append("%d IOSurfaces of destroyed swapchains stay while the program runs" % (churned[1] - warm[1]))
if exited[1] > start[1]:
    bad.append("%d IOSurfaces stay after the program ended" % (exited[1] - start[1]))
if warm[0] == 0 or churned[0] == 0:
    bad.append("the footprint of the program was not measured")
elif grown > made / 20:
    bad.append("the IOSurfaces of the footprint grew by %.1f MB, more than a twentieth of the swapchains" % mb(grown))
for problem in bad:
    print("churn: " + problem, file=sys.stderr)
sys.exit(1 if bad else 0)
PYTHON
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
    grep -h "the game uses" "${work}/host.out" | tail -2 || true
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

# Every program of the run fails the run with a FAIL line in its log or with an end of its own with an error code
# (#169). The OpenVR modes and the other modes with a verdict line required their PASS line only, and a plain --wine
# run required nothing of its program. A program that still runs now, such as a game or the fake game of a Quest run,
# ends with the run. A test program with a verdict line ends on its own after it: the run gives it 10 s for that, since
# Wine takes a few seconds to end a program that used the GPU.
program_exit=0
program_ends=0
(( openvr_wine || tracking_wine )) && program_ends=1
[[ -n ${xr_vulkan}${hand_interaction}${swapchain_churn} ]] && program_ends=1
check_program() {
    local name=$1 pid=$2 log=$3 code
    if grep -qE '^([A-Za-z0-9_.-]+: )?FAIL([: ]|$)' "${log}" 2>/dev/null; then
        print -u2 "${name} printed a FAIL line; see ${log}:"
        grep -E '^([A-Za-z0-9_.-]+: )?FAIL([: ]|$)' "${log}" | head -5 >&2
        program_exit=1
    fi
    [[ -n ${pid} ]] || return 0
    if (( program_ends )); then
        for i in {1..100}; do
            kill -0 "${pid}" 2>/dev/null || break
            sleep 0.1
        done
        if kill -0 "${pid}" 2>/dev/null; then
            print -u2 "${name} still runs 10 s after its verdict; see ${log}"
            program_exit=1
            return 0
        fi
    fi
    kill -0 "${pid}" 2>/dev/null && return 0
    wait "${pid}" && code=0 || code=$?
    print "${name} ended with exit code ${code}"
    if (( code != 0 )); then
        print -u2 "${name} ended with an error; see ${log}"
        program_exit=1
    fi
}
if [[ -n ${wine} ]]; then
    check_program "The Windows program ${wine##*\\}" "${program_pid}" "${work}/game.log"
    # The control file's size at the program's start went to the program. A size that changed during the run belongs to
    # a new link, and the program renders at the old one.
    read -r _ _ view_width view_height _ < "${work}/config/runtime-control.txt"
    if [[ -n ${view_size} && ${view_width}x${view_height} != "${view_size}" ]]; then
        print -u2 "The game's view size changed from ${view_size} to ${view_width}x${view_height} during the run"
        program_exit=1
    fi
fi
if (( wine_given )); then
    # The probe counts every frame of the right size, the host's own scene too: the host's count of the program's frames
    # tells a program that draws from one that failed before its first frame
    game_frames=$(python3 -I - "${work}/host.out" <<'PYTHON'
import json, sys
print(sum(json.loads(line[8:]).get("game_frames") or 0 for line in open(sys.argv[1], errors="replace")
          if line.startswith("@cranvr") and '"type":"stats"' in line))
PYTHON
)
    print "The host got ${game_frames} frames of the program"
    if (( game_frames == 0 )); then
        print -u2 "The host got no frame of the program: the stream showed the host's own scene"
        program_exit=1
    fi
fi
if (( game )) && [[ ${client} == quest ]]; then
    # The fake game of a Quest run outlasts the run: an end before it is a failure
    check_program "The fake game" "${game_pid}" "${work}/game.log"
elif (( game )); then
    check_program "The fake game" "" "${work}/game.log"
fi

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
# the sound arrives without a sound playing. A host started from a terminal captures only when the
# terminal app may record the system sound (System Audio Recording). Without it the run skips the check.
audio_line=$(grep -h "probe: audio:" "${work}/probe.log" 2>/dev/null || true)
if grep -qh "Game audio: cranvr may not record" "${work}/host.log" "${work}/host.out" 2>/dev/null; then
    print "The Mac's sound was not checked: the app that started this run may not record the system sound"
elif [[ ${audio_line} =~ '([0-9.]+) s of sound in' ]] && (( ! ${host_args[(Ie)--no-game-audio]} )); then
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
# The leak test runs too short for the report's windows of the stream: its own checks count
if [[ -n ${swapchain_churn} ]]; then
    print "churn: the stream's verdict above does not count; the leak test's checks do"
    result=0
fi
# The report's line above judges the stream alone. The run's verdict also holds the checks of the client and the
# programs: a run whose program failed printed only the stream's PASS before (#169).
failed=()
(( result == 0 )) || failed+=(stream)
(( client_exit == 0 )) || failed+=(client)
(( mouse_exit == 0 )) || failed+=(mouse)
(( game_exit == 0 )) || failed+=("fake game")
(( tracking_exit == 0 )) || failed+=(tracking)
(( hand_interaction_exit == 0 )) || failed+=("hand interaction")
(( openvr_exit == 0 )) || failed+=(OpenVR)
(( xr_vulkan_exit == 0 )) || failed+=("Vulkan program")
(( churn_exit == 0 )) || failed+=("swapchain churn")
(( displays_exit == 0 )) || failed+=(displays)
(( program_exit == 0 )) || failed+=(program)
if [[ -n ${wine} ]]; then
    program_nice=$(cat "${work}/program-nice.txt" 2>/dev/null || true)
    if [[ -n ${program_nice} && ${program_nice} != 0 ]]; then
        print -u2 "The program ran at nice ${program_nice}; the cranvr app starts games at normal priority"
        failed+=(nice)
    fi
fi
(( ${#failed} == 0 )) || result=1
grep -hE "probe: (done|frame arrival|native|desktop|audio|pause test|displays|in-game displays|[0-9.]+ s: the focus)" "${work}/probe.log" 2>/dev/null || true
print "Report: ${report}"
if (( result == 0 )); then
    print "e2e: PASS"
else
    print "e2e: FAIL (${(j:, :)failed})"
fi
exit ${result}
