# Controllers, hands and tracking in games

## Controllers in games

The host feeds BasaltVR's runtime (`host/src/runtime_link.rs`) from the client's `Tracking` and `Input` messages. For each tracking sample it sends the head pose, the grip pose and the aim pose of each controller that the headset tracks with the velocities of both. It also sends the mask of tracked controllers. It sends every change of a button, a touch, the trigger, the grip and the thumbstick with the path ids of the Touch profile (`common/src/inputs.rs`). The runtime's haptic requests go back to the client as `Haptics` datagrams.

Each controller has two poses. The grip pose is the place of the hand that holds the controller. The aim pose is where the controller points. The client sends both as the headset reports them, with no offset. The host adds only the floor's height. It hands both poses to the runtime in one control message for each hand, `CONTROLLER_MOTION_AIM` (cranvr's runtime patch 0004, in `third_party/basaltvr/patches/runtime`). The runtime gives a game the aim pose for an action space on `/input/aim/pose` and the grip pose for one on `/input/grip/pose`. A hand that sends no aim pose has its aim space at the grip pose.

The app starts no game while its game support is older than the app expects, so every runtime that the host serves has patch 0004. The host sends the aim pose with every controller message. The runtime's control protocol keeps its version 9.

The host's stats line shows what reached the game in each 5 seconds: `Game input: 601 left and 601 right controller poses, 601 and 601 of them with an aim pose (tracked mask 0b11), 21 button and axis values, 2 haptic pulses from the game; left hand at (-0.20, 1.50, -0.40) m, moved 0.00 m; right hand at (0.31, 1.40, -0.40) m, moved 1.02 m`. The position and the distance of each hand tell a controller that lies on a table, or that reports a pose at the room's origin, from one that moves: a count of poses alone does not. `scripts/e2e.sh --game` runs a fake game (`cranvr-fake-game`) that checks the input of the probe's script: both hands tracked, each hand with the grip pose and the aim pose that the probe scripted (`probe/src/hands.rs`), the right hand moving, a pull of the right trigger, a press of A, the right stick and the left grip. The fake game reads the runtime's control messages. It does not call `xrLocateSpace`, so this run checks the way from the probe through the host to the runtime's control channel.

### Beat Saber's menu shows one pointer at a time

The owner saw that Beat Saber's menu points with one saber and that the left saber becomes the pointer only after its trigger is pressed. A run with the probe as the headset (`scripts/e2e.sh --wine 'C:\Program Files (x86)\Steam\steamapps\common\Beat Saber\Beat Saber.exe' --left-trigger 70:4 --right-trigger 85:3 --seconds 105`, with `CRANVR_EYE_DUMP` set) shows that the game decides this itself:

- Both controllers reach the game from the start of the menu. The runtime's log in the game's `Player.log` lists the `devicepose` and `pointer` actions of both hands as active with "pose ... is tracked" before any trigger moves, the lines "the left controller's pose arrives" and "the right controller's pose arrives", and the host's stats line counts 361 left and 361 right poses with aim poses in each 5 seconds, the left hand at its scripted place.
- The left trigger reaches the game: the first change of the `trigger` and `triggerpressed` actions of the left hand is 1.00 and "pressed" at the probe's second 70, and a release follows at 74.
- The game picks one pointer. The probe's right hand points before the pull: the eye dumps show its laser (190 to 250 bright pixels in the part of the picture that holds it). At the second 70 of the probe, when the left trigger goes down, the laser goes away in the first dump after the press and stays away for 14 seconds (0 to 4 bright pixels, the stars). When the right trigger goes down at the second 85, the laser is back in the first dump (164 to 315). The left hand's own laser lies outside the dumped part of the picture, so the run did not show it.
- The game's own names say the same. `VRUI.dll` holds `VRPointer` with `_leftVRController`, `_rightVRController`, `IsLeftControllerDown`, `IsRightControllerDown`, `SelectLeftController`, `SelectRightController`, `get_lastSelectedVrController`, `_leftLaserPointer`, `_rightLaserPointer`, `ShowLastSelectedPointer` and `HideLaserPointers`: the menu shows the laser of the controller that pressed last.

The input path is not at fault, so nothing changed in it. In a song both sabers show, because the game draws both then.

## Hands, body, face and eyes in games

A Windows VR game under cranvr's Wine stack gets the user's hands, body, face and eyes through the OpenXR extensions that a PC headset offers. The runtime lists an extension only while the connected headset offers its feature:

| Feature | Extensions | Data |
| --- | --- | --- |
| hands | `XR_EXT_hand_tracking` | 26 joints for each hand, with the joints' velocities |
| body | `XR_FB_body_tracking` | 70 joints, the default set, with a T-pose skeleton |
| full body | `XR_META_body_tracking_full_body` | 84 joints with the legs |
| face | `XR_FB_face_tracking2` | 70 blend shape weights and two confidences |
| eyes | `XR_EXT_eye_gaze_interaction` | a gaze pose on `/user/eyes_ext/input/gaze_ext/pose` |

The way from the headset to the game:

1. After its welcome, a client sends `ClientMsg::TrackingOffer` with the features that its headset has and its user allowed. The host writes them as words to `tracking-features.txt` next to the runtime's control file (`hands body full_body face eyes`, with `face_visual` or `face_audio` after `face`) and removes the file when the headset leaves. A game lists its instance extensions before its session exists, so the runtime reads the file at that moment. A game that starts before the headset connects sees no tracking extension.
2. The patched runtime (patch 0005) tells the host which features its game uses (`TRACKING_REQUEST`, message 19): a hand tracker, a body tracker or a face tracker, or an eye gaze action that the game attached. The host asks the headset for exactly these (`HostMsg::TrackingWish`) and asks again when the set changes. A feature that no game uses costs no traffic.
3. For each headset frame the client sends one datagram for each wished feature (`Hands`, `Body`, `Face`, `Eyes`, in `proto/src/tracking.rs`). Each carries the sample time and display time of the frame's `Tracking` message. Joints travel as poses relative to a root pose with full precision and a velocity. The relative part is quantized: a position to 0.1 mm and an orientation to its three smaller components in 16 bits, so the error stays below 0.1 mm and 0.005 degrees. The largest datagram, the 84 joint body, takes 1197 of the 1400 bytes of a packet. A face weight takes one byte.
4. The host lowers the poses by the headset's floor, as for the head and the controllers, and sends them to the runtime (messages 20 to 23, documented in `basalt_control_protocol.h`). The runtime sends the joints in the base space that the game names. A hand and a body move forward by their velocity for the time from the sample's arrival to the game's locate time, at most 50 ms. The runtime has no map from the headset's clock to the game's `XrTime`, so the arrival time stands in for the sample's time. The headset has already predicted the sample to its own display time.

The host sends messages 20 to 23 to every runtime, because the app starts no game with a game support that lacks patch 0005. A runtime lists a tracking extension only when `tracking-features.txt` names it.

### Where the face comes from

`XR_FB_face_tracking2` lets a game choose the data source of its face: the cameras (visual) or the microphone (audio). A Quest 3 has no camera that sees the mouth, so its face is the audio one. The offer carries the source, and runtime patch 0006 reports it to the game by the rules of the extension:

- `xrGetSystemProperties` sets `supportsVisualFaceTracking` and `supportsAudioFaceTracking` from the offer. A Quest 3 supports audio only. A face of an unknown source reports both. A headset that has not yet given a valid sample offers such a face.
- `xrCreateFaceTracker2FB` takes `requestedDataSources` by the extension's rules. An empty list accepts any source. A duplicate, a value that is no source and a missing list for a counted one get `XR_ERROR_VALIDATION_FAILURE`. While no headset offers a face, the runtime makes the tracker and reads no face, so that a game survives a reconnect of the headset.
- `xrGetFaceExpressionWeights2FB` sets `dataSource` from the newest sample, which says its own source.

One case deviates from the extension on purpose. The extension calls the list the sources that the game accepts. It says that a runtime must return `XR_ERROR_FEATURE_UNSUPPORTED` when it believes that it will never serve the list, and a list without a source of the headset's face is such a list. This runtime makes the tracker instead and serves the headset's face. One condition in `createFaceTracker2` (patch 0006) restores the error. A game that lists the cameras only still gets the face of a Quest 3, and `dataSource` tells it that the face comes from the microphone. Every game that creates a face tracker therefore reads the headset's face.

### Controllers made of hands

A game without hand code binds the Oculus Touch controller profile and knows nothing of hands. While the headset tracks a hand and no controller of that hand, the host makes the controller from the hand (`host/src/hand_controller.rs`):

- The grip pose sits at the palm joint with the axes of OpenXR's grip pose, where a game puts the model of a controller: its -Z runs from the little finger to the index finger, and its +X enters the palm of the right hand and leaves the back of the left hand. The palm joint of `XR_EXT_hand_tracking` has other axes: its +Z points to the wrist and its +Y out of the back of the hand. With the palm joint's axes a game turned the model: its front pointed along the fingers, not across the hand. The hand interaction profile gets the same grip pose. The aim pose starts between the thumb tip and the index tip and points from an estimated shoulder through that point, with no roll.
- Pinching thumb and index finger pulls the trigger: the value runs from 0 at 60 mm between the tips to 1 at 18 mm, and the runtime reads a trigger from 0.5 as a click. Closing middle, ring and little finger presses the squeeze button.
- The host does this with the existing controller messages, so it needs no patch of the runtime.
- A game that creates a hand tracker has hand code of its own and gets no controller made of a hand. The host also waits for 3 seconds after the game connected before it makes any, because a game with hand code creates its tracker soon after it starts. Neither follows the game's name. `--no-hand-controllers` turns the controllers off.
- While no controller is tracked for 0.3 s, the host asks the headset for hands. It stops asking when both controllers have been back for 3 s.

### The hand interaction profile

Runtime patch 0012 offers `XR_EXT_hand_interaction`, for a game that binds `/interaction_profiles/ext/hand_interaction_ext`, alone or beside the Touch controller's profile. The runtime lists the extension with its graphics extension. It needs no headset feature, because a controller serves the profile too.

- The profile has the paths of the extension: the aim, grip, pinch and poke poses, and a value and a `ready_ext` flag for each of `pinch_ext`, `aim_activate_ext` and `grasp_ext`. With the extension enabled, every profile of the hands also takes `pinch_ext/pose` and `poke_ext/pose`. The extension asks for that.
- A game that attaches an action with a binding for the profile makes the runtime ask the host for it (the bit `HAND_INTERACTION` of `TRACKING_REQUEST`). The host then asks the headset for hands and makes no controller of them, because the game has hand code. It sends each hand's poses and gestures (message 24), which it makes from the joints (`host/src/hand_interaction.rs`). The grip pose is the one of a controller made of a hand: it sits at the palm joint, turned to the extension's axes. Its -Z runs from the little finger to the index finger, and its +X enters the palm of the right hand and leaves the back of the left hand. The aim pose is the one of a controller made of a hand. The pinch pose sits between the tips of thumb and index finger, and the poke pose on the skin of the index fingertip. The pinch and the grasp use the distances of "Controllers made of hands". The aim activation is the pinch. A gesture is ready while the headset tracks the joints that it reads. A pinch at a panel of the headset's app reads 0 for the game, as a held trigger does.
- A Touch controller serves the profile too, as the extension asks of a runtime with controllers: its trigger is the pinch and the aim activation, its squeeze is the grasp, and every gesture is ready. Its pinch and poke poses lie 3 and 4 cm ahead of its aim pose.
- Each hand takes the profile of what the user holds, in `xrSyncActions` only. A tracked controller gets the Touch profile when the game bound it, else the hand interaction profile. A tracked hand without a tracked controller gets the hand interaction profile. While neither is tracked the hand keeps its profile, so a hand that leaves the cameras' view for a moment changes nothing. Each change of a profile queues `XrEventDataInteractionProfileChanged`, and `xrGetCurrentInteractionProfile` returns the new profile. The choice follows the bindings of the game, never its name.

### Tests

`cargo test` covers the messages (`proto/tests/tracking.rs`: quantization, packet sizes and a session over loopback), the host's link to the runtime (`host/src/tests/tracking.rs`) and the conversion of hands to controllers (`host/src/hand_controller.rs`). Three runs of `scripts/e2e.sh` cover the whole way. They use the probe's script of `probe/src/tracked.rs`, in which the head, the hands, the body and the face stand still:

```bash
# The fake game asks for the features. It checks every joint, weight and the gaze against the script.
# --face-source visual|audio|unknown sets where the probe's face comes from (default audio, a Quest 3's).
scripts/e2e.sh --native-port 9961 --tracking
# A game without hand code: the probe tracks hands and no controller.
scripts/e2e.sh --native-port 9961 --hand-controllers
# The real runtime: tracking_smoke.exe reads everything through OpenXR in Wine.
CRANVR_HOME=/path/to/a/test/stack scripts/e2e.sh --native-port 9961 --tracking-wine
# The hand interaction profile: hand_interaction_smoke.exe binds it alone and drives the host's test hand
# (pinch, fist, a move, a roll, a loss). "both" binds the Touch profile too and checks the switch of profiles.
CRANVR_HOME=/path/to/a/test/stack scripts/e2e.sh --native-port 9961 --hand-interaction
CRANVR_HOME=/path/to/a/test/stack scripts/e2e.sh --native-port 9961 --hand-interaction both
CRANVR_HOME=/path/to/a/test/stack scripts/e2e.sh --native-port 9961 --tracking-wine --face-source visual
```

`--tracking-wine` needs a stack built from the runtime under test (`CRANVR_HOME=... scripts/setup-wine.sh third_party/basaltvr/src`). `tracking_smoke.exe` checks that the runtime lists the extensions, answers the system properties, refuses a joint set that it lacks, returns a skeleton, and gives the 26 hand joints of both hands, 70 and 84 body joints, 70 face weights and the gaze within 0.5 mm and 0.1 degrees of the script. It also checks the joints in the view space and the joints' velocities. For the face it checks the support structure against `--face-source`, the answer of the creation to each list of sources and the `dataSource` of each sample. `--live-face` accepts any weights, so that the program can read the face of a real headset. The run checks that the host asked the headset for tracking only after the game created its trackers and that it asked for none after the game left.
