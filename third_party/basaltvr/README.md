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
- A runtime without the patch closes the connection at a message that it does not know. cranvr-host
  sends the message to every runtime, because the cranvr app starts no game with a runtime older than
  its game support. The control protocol keeps its version 9.
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
  documented in `basalt_control_protocol.h`. cranvr-host sends 20 to 23 to every runtime, as it
  sends 18. The control protocol keeps its version 9.
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
  `XrEyeGazeSampleTimeEXT`. Patch 0012 adds `XR_EXT_hand_interaction`.
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

- The runtime writes to stderr, which the cranvr app keeps as the game's log (`game.err.log`; a Unity game keeps it in its `Player.log`), what a game's input does. At the first `xrAttachSessionActionSets` of a session it tells how many action sets and actions are attached and how many actions have a binding for the Touch controllers. It tells when a controller joins or leaves (`xrSyncActions` sees its profile). For each action of each hand it tells the first time that the action is active and its first two changes, with the value (pressed, a float, a vector or a tracked pose). Each kind stops after 80 lines. A game that binds nothing, or whose bindings no controller input reaches, shows no such line.

`0011-controller-poses-fresh-under-load.patch`

- A controller's pose counted as stale when a reader found it newer than the reader's clock reading. The readers (`controlInputSnapshot` for the controllers, `controlHeadPose` for the head and `trackedFresh` for hands, body, face and eyes) read the clock first and then wait for `gControlLock`. The receiver thread holds that lock exclusively while it handles a message and sets the message's arrival time under it. A message handled during the wait has an arrival time after the reader's clock reading, and `now >= arrival` failed. The pose action of that hand went inactive for that call, and OpenComposite then took the hand tracking pose or none, which a game draws at the origin, on the floor. The failure needs the receiver and a reader to meet, so it grows with the Mac's load: an OpenVR program that reads both controllers in every frame, with the probe offering tracking at a load average of about 30, logged 40 such transitions in 40 seconds, and 2 with the fix. `arrivedWithin` treats a later arrival as fresh.
- A pose is fresh for 1.5 seconds (it was 0.5). Under load the receiver ran 200 to 350 ms late. The host sends a mask when a controller is gone and an `INPUT_RESET` after a second of silence, so the runtime's own limit only has to catch a dead link, and the socket timeout catches that after 5 seconds.
- `basalt::note` writes the runtime's input notes to stderr and to `game-input.log` beside the control file (see `README.md` of the repository). New notes: "the right controller's pose stopped arriving" and "arrives" (read by `xrSyncActions`), "came after a gap of N ms" (a gap of 200 ms or more between two poses of a hand) and "a pose ... is older than what the runtime has" (the timestamp check refused a pose, with the three timestamps). Each kind stops after 40 lines, and the transitions after 80.

`0012-hand-interaction.patch`

- The runtime offers `XR_EXT_hand_interaction` with its graphics extension. It needs no headset feature: a Touch
  controller serves the profile too, as the extension asks of a runtime with controllers.
- `/interaction_profiles/ext/hand_interaction_ext` takes the paths of the extension for both hands: the aim, grip,
  pinch and poke poses, and `/value` and `/ready_ext` of `pinch_ext`, `aim_activate_ext` and `grasp_ext`. A game that
  did not enable the extension gets `XR_ERROR_PATH_UNSUPPORTED` for the profile. With the extension enabled, every
  profile of the hands takes `/input/pinch_ext/pose` and `/input/poke_ext/pose` (the extension's rule). The Touch
  controller's pinch and poke poses lie 3 and 4 cm ahead of its aim pose, with its orientation.
- The Touch bindings and the hand interaction bindings share one table, and each record names its profile. A
  suggestion replaces the bindings of its own profile only.
- `xrSyncActions` picks each hand's current profile before the actions read it. A tracked Touch controller takes the
  Touch profile when the game bound it, else the hand interaction profile, whose pinch and aim activation are then the
  trigger and whose grasp is the squeeze, always ready. A tracked hand without a tracked controller takes the hand
  interaction profile. While neither is tracked, the hand keeps its profile. A change of the profile's path queues
  `XrEventDataInteractionProfileChanged`. A game that bound only the Touch profile gets it exactly when it did before.
- A new control message, `HAND_INTERACTION` (24), carries a hand's three values with their ready flags and the four
  poses with their velocities, which cranvr-host makes from the hand's joints. The runtime asks for it with the bit
  `HAND_INTERACTION` (128) of `TRACKING_REQUEST` while an attached action has a binding for the profile. The poses
  move forward by their velocity from the sample's arrival to the locate time, at most 50 ms, as a hand's joints do.
  A gesture that is not ready has the value 0. A value bound to a boolean action presses at 0.6 and lets go below 0.4.
  The data stays fresh for 1.5 seconds, as a controller's pose does (patch 0011): for a game that binds only this
  profile the hands are its controllers. The host sends a hand's loss at once, also when the headset's app pauses.
- Action spaces read a pose source (the hand, the pose and whether a tracked hand gives it) in `xrLocateSpace`,
  `xrLocateViews` and the base space of a tracker's locate call. The haptics go to a Touch binding only.
- `tests/hand_interaction_smoke.cpp` binds the profile alone or with the Touch profile, drives cranvr-host's test hand
  through its words file and checks the values, the ready flags, the poses and the switch of profiles
  (`scripts/e2e.sh --hand-interaction [hand|both]`).

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

`0010-optional-render-target-array-index-capability.patch`: `DXMT_RT_ARRAY_INDEX_FROM_ANY_SHADER=1` makes
`CheckFeatureSupport(D3D11_FEATURE_D3D11_OPTIONS3)` report `VPAndRTArrayIndexFromAnyShaderFeedingRasterizer`, as upstream DXMT
does. BasaltVR's patch 0004 turns the report off, so that Unity's OpenXR provider draws each eye in a pass of its own
(runtime patch 0003 refuses its two-slice swapchain). Unity's OpenVR provider owns its two-slice texture, which no runtime
refuses. With the report off, its instanced draws reach slice 0 only: the left eye shows the picture and the right eye
stays black, and "Each eye in its own pass" cannot help. With the report on, the vertex shader writes the slice and DXMT's
layered rendering delivers it: both eyes show the picture at the full frame rate. The launch option "Both eyes in one pass"
sets the variable. The patch sits in a second place of the same file as patch 0004 and applies on its own.

`0009-name-the-discard-and-exception-mode-messages.patch`: DXMT logged "Not implemented" with no name
once for `DiscardResource` and `DiscardView1` of `ID3D11DeviceContext1` and each time for `SetExceptionMode`
and `GetExceptionMode` of the device. The Discard calls are hints that a driver may ignore. A game that
asks the device for discard support calls them every frame. A reader of the log took the line for the
failure of a game with a black picture (#60). The patch names each call. The Discard calls log it once as a
warning ("is ignored: it is only a hint"), not as an error.

`0011-name-the-shaders-of-a-failed-pipeline.patch`: DXMT logged "Failed to create PSO" with Metal's message and nothing
else, so a reader could not tell which draw failed. The line now names the vertex shader, the pixel shader, the state
of the rasterizer, the geometry and stream output stages, the number of render targets, the depth format, the topology
and the sample count. `DXMT_SHADER_DUMP_FOLDER=FOLDER` writes the bytecode of each shader that a game creates as
`<hash>.cso` into the folder (the folder must exist), so a hash in the log names a file that a disassembler reads.

`0012-vertex-shader-without-output-draws-nothing.patch`: a vertex shader that declares no output draws nothing in
Direct3D 11: the rasterizer gets no position. DXMT still built the Metal pipeline with the rasterizer on whenever a
pixel shader was bound. Metal refused it ("RasterizationEnabled is true but the vertex shader's return type is void")
and DXMT set no pipeline for the draw, so the render encoder kept the one of the draw before it, or none. Unity 6000.0
does this at the start: it draws with an empty vertex shader (two inputs, no output, no instruction) and a pixel
shader that writes a constant. The pipeline now has the rasterizer off when the vertex shader declares no output and no geometry,
hull or domain shader and no stream output takes its place. `DXMT_RASTERIZE_OUTPUTLESS_VERTEX_SHADERS=1` brings the
former behavior back for a comparison. The log says once which vertex shader this was.
