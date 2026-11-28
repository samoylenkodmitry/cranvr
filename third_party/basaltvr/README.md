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
- The per-eye size limit grows from 1680x1760 to 2064x2208, the Quest 3 panel size. Patch 0018 lets a
  swapchain grow to twice this size.
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
- `basalt::note` writes the runtime's input notes to stderr and to `game-input.log` beside the control file (see `docs/games.md` of the repository). New notes: "the right controller's pose stopped arriving" and "arrives" (read by `xrSyncActions`), "came after a gap of N ms" (a gap of 200 ms or more between two poses of a hand) and "a pose ... is older than what the runtime has" (the timestamp check refused a pose, with the three timestamps). Each kind stops after 40 lines, and the transitions after 80.

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

`0013-vulkan-sessions.patch`

- The runtime offers `XR_KHR_vulkan_enable` and `XR_KHR_vulkan_enable2` when the prefix has Wine's `vulkan-1.dll` and
  DXMT's `winemetal.dll`. It loads both at the first Vulkan call, so a Direct3D 11 game loads neither. Vulkan 1.0 to 1.4
  is accepted. The game's instance and device need no extension of the runtime: `xrGetVulkanInstanceExtensionsKHR`
  and `xrGetVulkanDeviceExtensionsKHR` answer an empty list, and `xrGetVulkanGraphicsDevice2KHR` answers the first
  physical device (MoltenVK has one, the Mac's GPU). `xrCreateVulkanInstanceKHR` and `xrCreateVulkanDeviceKHR` create
  them through the game's `vkGetInstanceProcAddr`.
- A Vulkan session takes the game's device and queue. A swapchain offers `B8G8R8A8_UNORM` and `B8G8R8A8_SRGB` with
  three images. Each image lies on an IOSurface: `winemetal.dll` makes the IOSurface and its Metal texture (the call
  that DXMT's patch for the runtime uses), and the runtime creates the Vulkan image on that texture with
  `VkImportMetalTextureInfoEXT` (it needs Wine patch 0009, see `third_party/wine/README.md`). cranvr-host gets the
  IOSurface's ID as it gets a Direct3D 11 image's, so the game's picture reaches the encoder with no copy.
- A swapchain with two slices (both eyes in one image) keeps a private image of two layers, which the game draws. At
  `xrEndFrame` the runtime copies each layer into an IOSurface image of its own on the GPU. The copy cost about
  0.6 ms for two eyes of 1444x1546 on an M5.
- The runtime signals a frame to cranvr-host when the GPU has finished it: `xrEndFrame` submits a fence on the
  game's queue (with the slice copies, if any), and a waiting thread sends the frame when the fence signals, as the
  Direct3D 11 path does with its fence. `control.cpp` takes the frame's GPU sync as a `FrameSync` of function
  pointers, which the Direct3D 11 and the Vulkan sessions fill. The runtime's classes have no virtual methods and no
  `std::vector`, because MinGW's C++ runtime for them imports `libwinpthread-1.dll`, which Wine lacks: the DLL then
  failed to load.
- Each IOSurface image has a Metal texture of its own on a new IOSurface. Patch 0015 frees it with its swapchain.
- Every 600 frames the runtime writes a note to stderr: how long the fence's submission took and how long after
  `xrEndFrame` the GPU finished a frame, on average and at most.
- `tests/vulkan_runtime_smoke.cpp` renders with Vulkan through the Khronos loader: `--enable1` takes
  `XR_KHR_vulkan_enable`, `--array` one swapchain of two slices. It draws exact black, a gray band down the left
  edge and a block of the eye's color (blue left, orange right), and prints its rate and the time that `xrEndFrame`
  took (`scripts/e2e.sh --xr-vulkan [enable2|enable1|array]`).

`0014-dxmt-patches-checked-in-order.patch`

- BasaltVR's `build-dxmt-fork.zsh` checked each DXMT patch on its own against DXMT v0.80 before it applied any. A
  patch that changes lines of an earlier patch (cranvr's DXMT patch 0019 changes lines of BasaltVR's 0001) failed that
  check. The script now applies the whole set in order to a scratch index of git: a set that does not apply still
  leaves the tree as it was.

`0015-vulkan-images-free-with-their-swapchain.patch`

- A destroyed swapchain releases the Metal textures of its IOSurface images (`NSObject_release` of `winemetal.dll`),
  and with them the IOSurfaces (DXMT patch 0019). Patch 0013 kept every texture in a pool of 64 for the next
  swapchain of the same size, because `winemetal.dll` kept each IOSurface until the game ended. A game that made
  swapchains of ever new sizes failed with `XR_ERROR_RUNTIME_FAILURE` once the pool was full: after 21 swapchains of
  three images in `swapchain_churn.exe --vulkan`. The pool also saved no time: 1000 swapchains of three images of one
  size took 1.6 to 2.3 ms each with it and 1.0 to 1.5 ms without it, on a Mac under load. It is gone.
- MoltenVK's command buffers hold no reference to their textures, so a texture may go only after the GPU has
  finished with it. The swapchain waits, at most a second, for the newest fence of the frame handoff, which signals
  after the runtime's copies of the slices and every command of the game before its last `xrEndFrame`. A lock keeps
  `xrEndFrame` from resetting that fence during the wait. When the wait fails, the textures stay until the game ends,
  and the runtime writes a note. After the session's frame handoff is gone, every fence has been waited for already.
- cranvr-host keeps its own reference to each surface (a `CVPixelBuffer` from `IOSurfaceLookup`) from the swapchain's
  creation until it handles its destruction, and it waits for the GPU in each compose. The surface stays until the
  host lets go, so the host never reads freed memory.

`0016-swapchain-churn-test.patch`

- `tests/swapchain_churn.cpp` creates and destroys a swapchain of three images many times (`--cycles`, default 1000)
  with Direct3D 11 or with Vulkan (`--vulkan`), with one slice or two (`--slices`). The images are 333 pixels high,
  which no game uses, and of a new width each time (256 to 510), so no reuse of images hides a leak. It clears each
  image once and waits for the GPU before the swapchain goes. `--frames` begins the session and shows each swapchain
  in one frame, then destroys it at once while the GPU may still work on it. `--signal FILE` stops after the first
  and after the last swapchain until `FILE.warm` and `FILE.churned` exist, for a measurement of the process.
  `scripts/e2e.sh --swapchain-churn d3d11|d3d11-array|vulkan|vulkan-array [--churn-frames]` runs it.

`0017-shared-objects-churn-test.patch`

- `tests/shared_churn.cpp` makes, in each cycle (`--cycles`, default 200), six shared textures of DXMT's own path
  (R16G16B16A16_FLOAT, B8G8R8A8 with a legacy handle, mipmaps, two slices with an NT handle, 4x MSAA, a keyed mutex),
  one IOSurface texture and a shared fence. A second process of the program (`--child`) opens each one and checks its
  description and the color that the first process cleared it to. Then both release everything. At the end it checks
  that the handle of a released IOSurface texture, whose ID names a surface of another size by then, does not open.
  `--signal FILE` stops while both processes hold the first cycle's objects, after that cycle and after the last one,
  until `FILE.live`, `FILE.warm` and `FILE.churned` exist. `scripts/shared-churn.sh` runs it and counts the names in
  launchd.
- Before the fix, 21 swapchains of Direct3D 11 left 63 IOSurfaces (24 MB) alive while the program ran and also after
  it ended (launchd held them). With Vulkan, the 22nd swapchain failed, and the 63 surfaces of the first 21 went
  with the process. After the fix, 1000 swapchains left none, in each of the four ways, and none with `--frames`
  either (100 to 120 swapchains, each shown in a frame).

`0018-swapchains-above-the-view-size.patch`

- A game may render above the view size that the runtime recommends: Serious Sam VR asks for 1.118 times it, and
  a game's own supersampling asks for more. The runtime refused every swapchain above 2064x2208, and OpenComposite
  stopped such a game with a message box (cranvr issue #164). Now `maxSwapchainImageWidth`, `maxSwapchainImageHeight`,
  `maxImageRectWidth` and `maxImageRectHeight` name 4128x4416, twice the largest view in each axis, and
  `xrCreateSwapchain` accepts images up to that size. The recommended size stays as it was.
- The limit is fixed. It does not follow the configured view size, which shrinks with the game's render size, so a
  game that picks its own resolution gets no lower limit than before. Up to twice the largest eye, the host's one
  bilinear tap for each pixel of the encoded eye still reads every texel of the game's image. An image at the limit
  takes 73 MB: two eyes of three images take 437 MB. Direct3D 11 and Metal take textures up to 16384 pixels.
- `tests/d3d11_runtime_smoke.cpp` checks the four numbers, creates a swapchain at the limit and expects one pixel
  more to be refused.

`0019-smoke-accepts-a-sleeping-controller.patch`

- `tests/d3d11_runtime_smoke.cpp` knew two kinds of left controller: none (no interaction profile, as with no
  control daemon) and a live one (`BASALT_REQUIRE_CONTROLLER_INPUT=1`, or the native daemon's injected controller).
  cranvr-host gives a game the Touch profile of both hands at its start, also while the headset's controllers
  sleep, as Unity needs. On a Quest on a desk the program then failed: with no variable it expected no profile, and
  with `BASALT_REQUIRE_CONTROLLER_INPUT=1` it waited 30 s for a controller that sent nothing (cranvr issue #173).
- `BASALT_ACCEPT_MISSING_CONTROLLER=1` is a third mode. The left hand may have the Touch profile with inactive
  actions, or no profile. The program syncs for up to a second for the host's profiles, which follow its HelloAck.
  The bound sources, their localized names and the profile's events follow the profile. The squeeze's value, the
  pose's state and the controller's locations count only while they are active or tracked: a controller may wake
  during the run, so each check judges its own moment. The program waits for no controller. The two old modes
  stay as they were.

`0020-shared-fence-round-in-the-churn-test.patch`

- `tests/shared_churn.cpp` gives the shared fence a round in each cycle. The first process signals 1. The second waits
  for 1 on the CPU and signals 2. Its GPU then waits for 3, which the first process signals only after it saw 2, and
  signals 4. The first process's GPU waits for 4 and signals 5. Each process thus waits on the CPU and on the GPU for
  a value that the other one signaled, and both GPU waits are pending when they are made. A fence that does not open
  fails the run, where the program counted such cycles before. The first process now also waits for its own 1 before
  it sends the cycle: Metal orders no work on a texture of DXMT's stock path across processes, and without the wait
  the second process read zeros in one of 200 cycles of one run. The program prints in how many cycles the round went
  through, which `scripts/shared-churn.sh` checks.

`0021-stock-checks-keep-dxmt-names-in-their-process.patch`

- `runtime/scripts/test-dxmt.zsh`, which `scripts/build-basaltvr.sh` runs before each build, first checks the runtime
  with stock DXMT v0.80. Stock DXMT registers the port of each shared texture and each shared event with launchd, and
  launchd keeps such a name with the memory behind it until the user logs out (cranvr issues #154 and #160). Each
  build left about 20 names of that phase. Since patch 0018 the smoke's swapchain at the limit of 4128x4416 added
  three surfaces of 73 MB, 219 MB in all (cranvr issue #179).
- `tests/native/keep_dxmt_names.c` builds a small x86_64 library that the stock phase loads into its Wine processes
  with `DYLD_INSERT_LIBRARIES`. It answers `bootstrap_register2` and `bootstrap_look_up` for the names "DXMT_..."
  from a table of the process and passes every other name to launchd. The probes of the stock phase share their
  objects only inside their own process, so every check of the phase stays as it was, with the stock DXMT binaries
  and the swapchain at the limit. The names go with the process. launchd refuses to remove a registered name, and
  macOS 27 refuses `bootstrap_subset`, so the names cannot go to launchd and be removed later.
- The phase fails when the library kept no name, so a run without it cannot leave names in launchd unseen. The fork
  phase runs without the library: the fork serves its names through winemetal's broker (DXMT patch 0020).

`0022-shared-texture-order-in-the-churn-test.patch`

- `tests/shared_churn.cpp` checks the order of the GPU work of its two processes on the shared textures (cranvr issue
  #177, DXMT patch 0022). The CPU wait of patch 0020 is gone: the first process no longer waits for its own GPU
  before it sends a cycle's handles. Each process keeps its GPU busy for at least 4 ms before its clears, with copies
  of a texture of 4096 x 4096 that its clears wait for on the GPU. The second process reads the colors at once. It
  also clears bgra8 to another color after its own busy work, flushes and replies. The first process then reads that
  color at once. Each wrong color fails the run. The program prints in how many cycles all colors were right.
- In even cycles the first process takes each handle before its Flush. In odd cycles it flushes each clear before it
  takes the texture's handle.
- Last, the second process writes a texture and ends at once (TerminateProcess). The first process's copy of the
  texture must be done within 3 seconds.
- With stock DXMT 0.80 and cranvr's patches up to 0021 the second process read zeros of rgba16f in the second cycle.

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

Patches 0013 to 0018 run SteamVR for Windows under Wine (cranvr's SteamVR mode, `steamvr/driver_cranvr.cpp`). Each
applies on its own to DXMT v0.80. The build script checks the whole set in order (runtime patch 0014), because patch
0019 changes lines of BasaltVR's 0001. Patch 0020 changes lines of 0013, 0014, 0018 and 0019. Patch 0021 changes lines of 0020.
Patch 0022 changes lines of 0019 and 0020. Patch 0023 changes lines of 0013 and 0020.

`0013-shared-buffers-between-processes.patch`: SteamVR's compositor makes a constant buffer with
`D3D11_RESOURCE_MISC_SHARED` and stops at start when the buffer has no shared handle ("Failed to create shared frame
info constant buffer"). A buffer with the flag now lies on the memory of a named file mapping. A kernel allocation
carries the mapping's name, the description and the size, so another process opens the same memory with
`OpenSharedResource`. Such a buffer has no dynamic renames: a rename would move it off the shared memory.

`0014-keyed-mutex-orders-shared-textures.patch`: a texture made with `SHARED_KEYEDMUTEX` answers a query for
`IDXGIKeyedMutex`. SteamVR orders the access to the frames that its processes share with these mutexes, and Chromium's
GPU process asks for them too. The GPU keeps the order, as on Windows: the processes of a texture share a Metal shared
event and a count of releases in a small section of shared memory. `ReleaseSync` has the GPU set the event to the next
count after the caller's commands, and `AcquireSync` has the GPU wait for the last count before the caller's next
commands. Neither call waits for the GPU. A first version waited in `ReleaseSync` until the GPU had finished. The Lab
and SteamVR's compositor then lost 8.5 ms of each frame there, and SteamVR made about 50 frames a second at 72 Hz.

`0015-dxgi-usage-from-bind-flags.patch`: `IDXGIResource::GetUsage` follows the bind flags and the usage, as on Windows.
It returned 0. ANGLE in Chromium reads it for a texture that another process shared.

`0016-output-duplication-without-frames.patch`: `DuplicateOutput` gives a duplication that delivers no frame. It failed
with `DXGI_ERROR_UNSUPPORTED`, and SteamVR's desktop view treats that as fatal.

`0017-swapchain-for-a-window-of-another-process.patch`: a hidden window of the process stands in for a window of
another process. Chromium's GPU process makes the swapchains of the browser's windows, and DXMT refused them ("cross-process
swapchain not supported yet"). SteamVR's web panels use only the textures that Chromium shares.

`0018-opened-textures-share-again.patch`: a texture that a process opened with `OpenSharedResource` or
`OpenSharedResource1` gets a kernel allocation of its own with the private data of the shared one, so the process can
share it again, as on Windows. SteamVR's web helper passes the textures of Chromium's GPU process on to the
compositor. Without the patch each of them failed with `VROverlayError_InvalidTexture`, and SteamVR's panels and
controller hints stayed away from the headset. The private data comes from opening the shared resource: the query of
it gives only the size.

`0019-iosurface-textures-free-with-their-last-texture.patch`: `MTLDevice_newTextureFromIOSurface` (BasaltVR's 0001) made
a Mach send right of each new IOSurface and returned it in `WMTTextureInfo::mach_port`. Nothing freed it, so the
surface stayed until the process ended. DXMT also registered the right with launchd under the texture's shared name,
for `OpenSharedResource` in another process. launchd keeps a registered right until the user logs out and refuses to
remove the name (a register with no port answers `BOOTSTRAP_NAME_IN_USE`). Every swapchain image of every Direct3D 11
game and of SteamVR's shared textures therefore stayed after the program had ended: on 2026-11-27 launchd held 206
surfaces (529 MB) for 209 names of dead Wine processes on the owner's Mac. The call now makes no Mach port: the
texture is the process's only owner of the surface, which goes a few milliseconds after the last texture on it, in
any process, is released. A shared IOSurface texture registers nothing with launchd. Its runtime data carries the
surface's global ID (`iosurface_id`), and `OpenSharedResource` and `OpenSharedResource1` open the surface by that ID
(`WMTTextureInfo::reserved` in; 0 makes a new surface). The unique name stays in the runtime data, because patch 0014
names the section of the keyed mutex by it. A shared texture of DXMT's own path (`MTLSharedTextureHandle`: other
formats, mipmaps, arrays, MSAA) and a shared fence kept their registration with launchd until patch 0020. As with a
legacy shared handle on Windows, an IOSurface texture whose last texture is gone no longer opens; on Windows an NT
handle keeps the resource. BasaltVR's checks of the fork (`runtime/scripts/test-dxmt.zsh`, with the reopen of an NT
handle) pass with the patch. `scripts/e2e.sh --swapchain-churn` checks that no surface stays (see runtime patch
0016).

`0020-shared-ports-leave-launchd-with-their-object.patch`: DXMT registered the port of each shared texture of its own
path and of each shared event (a shared `ID3D11Fence`, the event of patch 0014's keyed mutex) with launchd. These
ports belong to the kernel and never die. launchd kept them, with the memory of the textures, until the user logged
out: one `scripts/e2e.sh --steamvr` left 30 names with 240 MB. winemetal now shares such a port through a broker.
`WMTBootstrapRegister` registers a service port whose receive right the process holds, one for each object. A thread
of the broker answers each request on it with a copy of the object's port. `WMTBootstrapUnregister` destroys the
service port when the object goes. launchd then drops the name at once. The end of the process destroys the service
port too. Such a texture cannot go by the ID of its IOSurface as patch 0019 does: Metal makes it on a surface that is
not global, so another process finds nothing under the ID. A process that shares an opened texture again (patch 0018)
serves its own port under a new name, so the next process does not depend on the creator. The patch also ends three
leaks of DXMT's own. An opened texture and an opened shared buffer kept one reference too many. The resource deleted
its keyed mutex through `IDXGIKeyedMutex`, which has no virtual destructor, so the keyed mutex's section and its event
stayed. The rights of the texture ports were never freed. Each kept memory until the process ended. The import of an
IOSurface texture of patch 0019 now compares the size and the pixel format of the surface that the ID names with the
runtime data. It refuses another surface with a log line, because macOS gives the lowest free ID to the next new
surface. `scripts/shared-churn.sh` checks all of this (runtime patch 0017). Patch 0021 makes a shared fence open in
another process.

`0021-shared-fences-open-in-another-process.patch`: a shared `ID3D11Fence` did not open in another process under Wine
11.10 (#161). DXMT stored the name of the fence's event in the sync object's private data with
`D3DKMT_ESCAPE_UPDATE_RESOURCE_WINE`, which Wine runs for resources only. DXMT's check of the result was the wrong way
round, so the fence went on without its name. `OpenSharedFence` read the name with
`D3DKMTQueryResourceInfoFromNtHandle`, which Wine answers for resources only and which gives the size of the data,
not the data. The fence's NT handle now shares a bundle of three objects, as Wine shares a texture with a keyed mutex:
a resource whose private data holds the event's name and a tag, a keyed mutex that nobody uses (Wine shares a sync
object with a resource only in such a bundle) and the fence's sync object. `OpenSharedFence` opens the bundle with
`D3DKMTOpenResourceFromNtHandle`, which gives the name and the sync object, and looks the name up in the broker of
patch 0020. A handle whose data has another size or no tag is no fence's. The opened fence keeps the bundle and can
share it again. Each process logs its open of another process's fence and its first wait on such a fence, on the CPU
(`SetEventOnCompletion`) and on the GPU (`ID3D11DeviceContext4::Wait`), with its process ID.

`0022-shared-textures-ordered-across-queues.patch`: Direct3D 11 asks a writer to call Flush before another device uses
a resource that they share without a keyed mutex. After that the reader sees the writes. Metal orders no work of two
command queues on a texture. DXMT made the reader's queue wait for nothing. A second process could copy a texture of
the stock path or an IOSurface texture before the writer's GPU had run the writes that the writer flushed (cranvr
issue #177). A keyed mutex orders its texture already (patch 0014). Each queue that holds another shared texture now
serves its shared event and its resource initializer's event through winemetal's broker. A section of shared memory
has a slot for each such queue. The texture's unique name names the section. A slot holds the event value of the
queue's newest chunk that wrote the texture and of the write before it. The encode thread notes a chunk's shared
textures in `access`. After the chunk's commands ran and before its first encoder, it stores the chunk's writes in its
slots, reads the other queues' slots and encodes a wait on another queue's event only while the other GPU has not
reached the value. A commit takes a stamp from a counter of all DXMT processes. Each process keeps its handle of the
counter's section open: Wine drops a section's name with its last handle, also while a view maps it. A reader waits
only for chunks that the writer committed before the reader's chunk. Its encode thread waits for the writer's encode
thread (at most 100 ms) only when such a chunk has not reached its order step. The GPU waits only for chunks that
wrote the texture and passed their order step. So two processes cannot wait for each other. The order state lives in
the `TextureAllocation`, which every chunk keeps until the GPU has finished it. An opener's slot comes when it opens
the texture. The creator's slot comes with the texture's first handle. It starts with the event value of the queue's
newest committed chunk. The first handle also submits the initial contents, which a reader waits for too. A texture
that never leaves by a handle gets no slot: a game's swapchain image that BasaltVR gives to cranvr-host by its
IOSurface ID costs nothing. A slot's section costs about half a millisecond to make and as much to free under Wine. A
watch thread sets the event of a process that ended to the value that a GPU of this process waits for.
`src/dxmt/dxmt_shared_order.hpp` gives the reasons and the costs. `DXMT_SHARED_ORDER=0` turns the order off.
`DXMT_SHARED_ORDER_STATS=1` times each encoded wait. `scripts/shared-churn.sh` checks it (runtime patch 0022).

`0023-shared-resources-outlive-their-device.patch`: DXMT lets a device go before its resources, as the Direct3D 11
documentation allows. A shared texture or buffer freed its D3DKMT allocation in its destructor through its device, with
a virtual call. An application that released the device first crashed there. SteamVR's client in Steam does so when
SteamVR quits after a game: it releases shared textures after their device. Steam for Windows
ended a few seconds after each SteamVR run of Half-Life: Alyx (cranvr issue #197). Its six crash dumps show
`~DeviceTexture` calling through the dead device: a call to address 0, to 0x9fa or `__cxa_pure_virtual`. The DXGI
device now keeps its D3DKMT device in a `KmtDevice` with a reference count (util_d3dkmt.h). A texture or buffer with an
allocation keeps a reference and frees the allocation through it, so the D3DKMT device lives until its DXGI device and
its last shared resource let go.

`0024-no-build-paths.patch`: the game support is public, and winemetal.so held the full paths of airconv's three
shaders under the home folder of the Mac that built it (cranvr issue #200). The Metal compiler writes a shader's path
into the module as `source_filename` and `air.source_file_name`, and no option of it changes either name.
`tools/metal-air.sh` compiles a shader, names the module by the shader's file name in the text form (`air-opt -S`) and
assembles it again (`air-as`). meson.build maps the source folder to `dxmt` (`-ffile-prefix-map`) for C and C++ on
both machines, as `scripts/build-wine.sh` does for Wine and `scripts/build-opencomposite.sh` for OpenComposite.
