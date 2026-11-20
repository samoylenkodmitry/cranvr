# BasaltVR patches

cranvr runs Windows OpenXR games under Wine with two parts of
[BasaltVR](https://github.com/Stooovie/BasaltVR) (MIT license), pinned at commit
`6116e59124ef0f11ba157571b6f44f901eb6ba59`:

- the OpenXR runtime (`runtime/pe`), which games load as their headset;
- the DXMT fork (`runtime/dxmt-fork`), which shares each swapchain image with cranvr-host as an
  IOSurface.

cranvr-host speaks the runtime's control protocol (version 9) in `host/src/runtime_link.rs`.
BasaltVR's own Swift streamer is not used.

`scripts/build-basaltvr.sh` clones BasaltVR into `third_party/basaltvr/src` (not committed),
applies these patches and builds both parts.

## patches/runtime

`0001-refresh-rate-and-quest3-view-size.patch`

- The control file takes an optional fifth field, the refresh rate in Hz (60 to 240).
  `xrWaitFrame` paces the game at that rate. Without the patch the runtime paces every game at
  72 Hz. cranvr-host writes the rate negotiated with the headset.
- The per-eye size limit grows from 1680x1760 to 2064x2208, the Quest 3 panel size.
- The runtime tests expect the new limit.

`0002-game-path-and-per-game-view-size.patch`

- At session creation, before its HELLO, the runtime writes its program path to
  `game-program.txt` next to the control file. cranvr-host reads the file when the game connects,
  then removes it. The control protocol stays as it is: BasaltVR's own peers check the HELLO's
  exact length.
- `game-sizes.txt`, next to the control file, holds lines `<share> <program path>`. cranvr-host
  writes a line when a game stays below the headset's rate for a minute. The runtime reads the
  share for its own program once per process and scales the recommended view size by it, to even
  sizes of at least 256 pixels. A game reads its view size only when it starts.

`0004-controller-aim-pose.patch`

- A new control message, `CONTROLLER_MOTION_AIM` (18), carries the grip pose and the aim pose of one
  hand with the velocities of both. An action space on `/input/aim/pose` follows the aim pose, one
  on `/input/grip/pose` the grip pose, in `xrLocateSpace` and `xrLocateViews`. A hand that the daemon
  fed with the plain `CONTROLLER_MOTION` has no aim pose, and its aim space follows the grip pose.
- A runtime without the patch closes the connection at a message that it does not know. The runtime
  therefore writes `runtime-features.txt` (the word `aim`) next to the control file at session
  creation, as it writes `game-program.txt`. cranvr-host sends the message only to a runtime that
  wrote the file. The control protocol keeps its version 9.
- The test daemon sends the new message for the left hand and a plain one when the controller comes
  back. The smoke test locates the aim space and the grip space of that hand and checks both.

`0005-tracking-hands-body-face-eyes.patch`

- The runtime offers `XR_EXT_hand_tracking`, `XR_FB_body_tracking`, `XR_META_body_tracking_full_body`,
  `XR_FB_face_tracking2` and `XR_EXT_eye_gaze_interaction`, each while the connected headset offers its
  feature. cranvr-host writes the features as words to `tracking-features.txt` next to the control file.
  The runtime reads it when a game lists the instance extensions, before the control session exists. It
  also fills the support structures of `xrGetSystemProperties`. Without the file the runtime lists what
  it listed before, and the gate `DXMT_BASALT_IOSURFACE=1` must be on.
- New control messages: `TRACKING_REQUEST` (19, to the host: the features that the game uses),
  `HAND_TRACKING` (20), `BODY_TRACKING` (21), `FACE_TRACKING` (22) and `EYE_GAZE` (23). They are
  documented in `basalt_control_protocol.h`. The runtime writes the word `tracking` to
  `runtime-features.txt`, and cranvr-host sends 20 to 23 only to a runtime that wrote it. The
  control protocol keeps its version 9.
- A hand and a body arrive as a root pose with velocities and joints in the headset's tracking
  space. The runtime moves them into LOCAL like the head and the controllers, and moves them forward
  by their velocity from the sample's arrival to the game's locate time, at most 50 ms. A sample older
  than 500 ms is inactive, as for a controller. `xrLocateHandJointsEXT` also fills `XrHandJointVelocitiesEXT`.
- The body tracker has the default set of 70 joints, and the full body set of 84 when
  `XR_META_body_tracking_full_body` is enabled. `xrGetBodySkeletonFB` returns a T-pose skeleton with
  an adult's proportions, because the headset sends none.
- `XR_EXT_eye_gaze_interaction` is an interaction profile: `/interaction_profiles/ext/eye_gaze_interaction`
  with a pose on `/user/eyes_ext/input/gaze_ext/pose`. The runtime binds an action to it, reports
  it active while the gaze is tracked, and gives the pose in `xrLocateSpace` with
  `XrEyeGazeSampleTimeEXT`. `XR_EXT_hand_interaction` is not implemented.
- The runtime tells the host which features the game uses when a game creates or destroys a
  tracker, or attaches an eye gaze action. The host asks the headset for these only.
- `tests/tracking_smoke.cpp` reads every feature through the loader and compares it with the script
  of cranvr-probe (`scripts/e2e.sh --tracking-wine`).

`0006-face-data-sources.patch`

- `tracking-features.txt` may name the headset's face source after `face`: `face_visual` for cameras, `face_audio` for the microphone. A Quest 3's face comes from the microphone. The runtime fills `XrSystemFaceTrackingProperties2FB` from the words. A face with no named source is of an unknown source, and the runtime then reports both sources.
- `xrCreateFaceTracker2FB` takes `requestedDataSources` by the extension's rules. An empty list accepts any source. A duplicate, a value that is no source and a missing list for a counted one are `XR_ERROR_VALIDATION_FAILURE`. One case deviates from the extension on purpose. The extension says that a runtime must return `XR_ERROR_FEATURE_UNSUPPORTED` when it believes that it will never serve the list, and a list without a source of the headset's face is such a list. This runtime makes the tracker and serves the headset's face instead, so that a game which lists the cameras only still gets the face of a Quest 3. One condition in `createFaceTracker2` restores the error. While the headset offers no face, the tracker is made and reads no face.
- `xrGetFaceExpressionWeights2FB` reports the source of the newest sample as `dataSource`. The sample says it with flag 4 of `FACE_TRACKING`.
- `tests/tracking_smoke.cpp` takes `--face-source visual|audio|unknown`. It checks the support structure, the answer of the creation to each list of sources, a tracker with no list, a tracker that lists only the other source and the `dataSource` of each sample. `--live-face` accepts any weights, for a face from a real headset.

`0007-stage-reference-space.patch`

- `xrCreateReferenceSpace` accepts `XR_REFERENCE_SPACE_TYPE_STAGE` and gives the LOCAL space. The host puts the floor at height 0 of that space (see `host/src/headset/native.rs`). OpenComposite asks for a STAGE space for the standing universe of an OpenVR game and stops without it.

`0008-action-sets-attach-to-a-later-session.patch`

- `xrAttachSessionActionSets` accepts an action set that was attached to a session which no longer exists. The runtime kept the set's `attached` mark for the life of the set and refused the second attach with `XR_ERROR_ACTIONSETS_ALREADY_ATTACHED`. An OpenVR game that sets an action manifest (IVRInput) at its start reaches that call twice. OpenComposite creates the action sets and attaches them to its first session. When the game submits its first frame, OpenComposite destroys that session, makes a new one on the game's own graphics device and attaches the same sets again. It stopped there with the error, so no game with an action manifest could show a picture. The set stays attached, so its actions and the suggested bindings stay frozen. A set that a live session holds, a second attach to the same session, an action created after the attach and a suggestion after the attach keep returning the error.

`0009-display-refresh-rate-extension.patch`

- The runtime offers `XR_FB_display_refresh_rate`. The headset's rate (the control file's refresh rate) is the only rate: `xrEnumerateDisplayRefreshRatesFB` lists it, `xrGetDisplayRefreshRateFB` returns it, and `xrRequestDisplayRefreshRateFB` accepts it and 0 (the default). A request for another rate returns `XR_ERROR_DISPLAY_REFRESH_RATE_UNSUPPORTED_FB`, which the extension allows for a rate that is not on the list. A Unity game that uses the extension asked for it and got `extension not enabled`. OpenComposite reads the rate from it.

`0010-log-what-reaches-the-game.patch`

- The runtime writes to stderr, which the cranvr app keeps as the game's log (`game.err.log`), what a game's input does. At the first `xrAttachSessionActionSets` of a session it tells how many action sets and actions are attached and how many actions have a binding for the Touch controllers. It tells when a controller joins or leaves (`xrSyncActions` sees its profile). For each action of each hand it tells the first time that the action is active and its first two changes, with the value (pressed, a float, a vector or a tracked pose). Each kind stops after 80 lines. A game that binds nothing, or whose bindings no controller input reaches, shows no such line.

## patches/dxmt

`0005-public-atomic-for-new-metal-compiler.patch`: DXMT 0.80 calls a private Metal builtin
(`__metal_atomic_fetch_add_explicit` with four arguments). The Metal compiler in the macOS 27
SDK wants five, so the shader no longer compiles. The patch uses the public
`atomic_fetch_add_explicit` instead.

`0006-msaa-sample-count-limit.patch`: `DXMT_MAX_SAMPLE_COUNT=N` makes
`CheckMultisampleQualityLevels` report every sample count above N as unsupported, so a game
that checks picks N or fewer. `DXMT_CLAMP_SAMPLE_COUNT=1` also gives N samples to any 2D texture
that asks for more, for a game that does not check; a shader that reads a fixed sample index may
then break, so this part is off unless set. The patch logs the limit, the first refused count, the
first texture above the limit and the sample count of the first MSAA texture, with the prefix
`cranvr:`. Each Metal pass stores every sample
to memory, so 4x MSAA costs far more under DXMT than on a Windows GPU. cranvr's app and
`scripts/run-game.sh` set the limit to 2 by default.

`0007-resolve-pass-without-sample-store.patch`: a resolve that DXMT cannot fold into the render
pass before it runs as its own render pass. It loaded the multisampled texture, resolved it and
stored every sample back, although the pass changes none. The store now drops: the texture's
memory still holds the samples. In Beat Saber a pass took 0.31 ms instead of 0.38 ms on a base
M5, and the picture on the Quest looked the same. `DXMT_RESOLVE_STORE_SAMPLES=1` brings the
store back.
