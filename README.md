# cranvr

cranvr streams VR from an Apple Silicon Mac to a Meta Quest. The Mac side is Rust. It renders and composes with Metal, encodes HEVC with VideoToolbox and sends the frames with cranvr's own protocol (`proto/`, crate `cranvr-proto`). The headset side is a Cranpose app (`client/`, package `cranvr-client`, application id `dev.cranvr.client`). It uses only Cranpose's public API and plain Rust crates. It has no ndk, JNI, GL, OpenXR or platform code of its own. The same crate also builds as a flat app for an Android phone or tablet.

The goal is to play Windows SteamVR games on a Quest from a Mac. Windows games run under Wine with the OpenXR runtime and the DXMT fork from [BasaltVR](https://github.com/Stooovie/BasaltVR) (MIT). A game that speaks OpenVR runs through [OpenComposite](https://gitlab.com/znixian/OpenOVR) (GPL 3), which turns its OpenVR calls into OpenXR calls to that runtime. The game's frames reach the host as shared IOSurfaces. No frame of a Direct3D 11 game passes through the CPU. An OpenGL game's frames do: OpenComposite copies them into the runtime's images (see "OpenVR games").

## What works

- A head-tracked test scene for a headset on a desk.
- Foveated encoding at four levels: Off, Low, Medium and High. The host keeps the full pixel density around the direction straight ahead of each eye and squeezes the edges. The client unwarps them. The default level Medium encodes about a third of the pixels of an eye. It makes the full Quest 3 eye size fit at 120 Hz (see "Foveated encoding").
- Windows OpenXR games under Wine. Head pose, controller poses, buttons and haptics pass both ways.
- Windows OpenVR games under Wine, through OpenComposite. The app has a choice for it in the Settings tab, in the Wine card and in each game's options. OpenGL games too: Google Earth VR draws its picture in a test host (see "Not tested yet").
- The Mac desktop as a panel in the headset, with the controller as the mouse. The Mac's sound plays in the headset.
- A display lock. The host starts each frame of its test scene so that it reaches the headset's display on its grid.
- Discovery of the Mac by the headset, a pairing that the user allows on the Mac, an encrypted and authenticated session and a USB cable link.
- The `cranvr` window (`app/`, built with Cranpose). It starts the host, asks the user to allow a new headset, sets up the games and starts them.
- An end-to-end harness, `scripts/e2e.sh`, with a test client on this Mac or a Quest. The host reports the client's latency by stage.

## Parts

| Folder | What it does |
|---|---|
| `vt` | HEVC encoder and decoder through plain C bindings to VideoToolbox. |
| `common` | The foveation math and the ids of the controller inputs, for the host and the probe. |
| `proto` | `cranvr-proto`: the handshake, the messages, the media packets with Reed-Solomon parity, the clock sync, the discovery and the keys. It builds for macOS and for Android. |
| `host` | `cranvr-host`: the frame loop, the Metal shaders (`host/src/pattern.metal`), the headset link (`headset/native.rs`), the game link (`runtime_link.rs`) and the desktop (`desktop.rs`, and `headset/native/displays.rs` for the displays of the headset). |
| `app` | `cranvr`: the window. It runs `cranvr-host` and shows its status. |
| `probe` | `cranvr-probe` (a client of the protocol on this Mac for tests) and `cranvr-fake-game` (a stand-in for a game). |
| `client` | `cranvr-client`: the Cranpose app for the Quest and for Android phones. Its own Cargo workspace. |
| `scripts` | Build the client, bundle the app, set up Wine and Steam, run games, run the end-to-end test. |
| `third_party/basaltvr` | cranvr's patches to BasaltVR's runtime and DXMT fork. |
| `third_party/wine` | cranvr's patch to Wine's HLSL compiler (`wined3d.dll`) and its test program. |

## Build

```bash
cargo build --release
scripts/build-client.sh            # target/client/cranvr-client.apk
scripts/build-client.sh --install  # and install it with adb
scripts/build-client.sh --phone    # cranvr-phone.apk for an Android phone or tablet
```

`scripts/bundle-app.sh --install` builds `cranvr.app` and installs it in `/Applications`.

## Run

1. Put the Mac and the headset on the same network. Use 5 GHz or 6 GHz Wi-Fi, or the USB cable.
2. Start the window: `./target/release/cranvr`. The host starts by itself.
3. Open cranvr on the headset. The connect panel lists the Macs that run cranvr. Press yours. A headset that the Mac does not know shows a code of six digits. The window on the Mac shows the same code with Allow and Deny. Press Allow when the two codes are the same.

`docs/quest-first-run.md` is the checklist for a first run with Steam and Beat Saber. The window saves its settings in `~/Library/Application Support/cranvr/app.json`. The app reads the file entry by entry. An entry that it cannot read, such as an action name that no action has, keeps its standard value. The window then shows an error that names the entry. A save keeps such an entry as the file has it until the user changes that setting. A file that is no JSON at all stays as it is: the app runs with the standard settings and saves no change. A change of refresh rate, bitrate, foveation or desktop restarts the host. The headset reconnects by itself.

Without the window:

```bash
./target/release/cranvr-host --fps 120 --desktop
```

`cranvr-host --help` lists every option. `--status` prints status lines (`@cranvr` and a JSON object) on stdout for a program that runs the host. `host/src/status.rs` lists them.

### What you should see

With the desktop off, the host shows a test scene:
- a sky with a grid line every 15 degrees and brighter lines every 45 degrees;
- a floor with 50 cm squares and a green line every meter;
- a yellow ball at the left controller and a cyan ball at the right controller;
- a small red square near the left edge of the left eye and a blue square near the left edge of the right eye.

A game replaces the scene while it runs. The headset's Room setting (in its console) decides what fills the view while no game runs. With Passthrough (the default) the room shows around the panels and the client leaves the streamed scene out. With Scene the host's picture fills the view. The launch argument `room` (`scene` or `passthrough`) decides for one launch.

The headset's Interface setting decides how the hand menu and the console draw: 3D (the default) or Flat. It is a row of the console's Headset page and of the headset's page in the Mac's app. In 3D each menu is a Cranpose `SpatialTray`: the words stay on a panel at full resolution, and a tray of 3D geometry with light and shadows lies under them (#111). The ball of the hand menu is a 3D ball. A key is a key cap that sinks under a press. A toggle is a rocker that flips. A choice of numbers on a page, such as the volume, is a slider whose knob slides in a groove. The title row of the hand menu has a switch "3D", and one press there gives the flat menus back. A tray shows in 180 ms and hides in 140 ms. A closed menu composes no tray. While a game runs and no controller model shows, the flat ball stands in for the 3D one, so the closed menu adds nothing to the game's frame. On a Quest 3 with a stream at 120 Hz the open 3D menu took 3.10 ms of the GPU and 9 layers, the open flat menu 0.12 ms and 8 layers, and every frame came in time (`scripts/ui3d-budget.sh`). The launch argument `interface` (`3d` or `flat`) decides for one launch.

## The headset app

`client/` holds the app. In a headset the panels are movable panels in the room. On a desktop they draw side by side in a flat window.

- The connect panel lists the Macs that it found, takes a typed address and shows the state of the link.
- The stream is a `StreamedScene`. Cranpose's `VideoDecoder` outputs frames. The scene keeps up to three decoded frames in a queue. At each headset frame it shows the frame whose target display time lies closest to the display time of that headset frame. The headset's compositor shows each frame from the pose that the host rendered it for and corrects it for the head's newer pose.
- The stats panel shows the frame rate, the decode time, the loss, the FEC recoveries, the sound and the latency. It also shows what the scene did with the decoded frames: how many it showed a second, how many it skipped and how many headset frames showed the last picture again.
- Every button, row and ball shows a light veil while a ray or the mouse rests on it and a stronger one while a press holds it. The trigger's press ticks the controller, and so does a click at the release. A press that the ray turns off its button ends there and ticks nothing. A request that takes the Mac a while, such as a game's start, a stop or a new display, says so on its button ("Starting...", "Adding...") until the menu shows the result or the Mac says why not. The console waits 90 seconds for an answer and the hand menu 20 seconds. The Mac's window does the same for Start, Open Steam, Play and Stop Steam and games. Cranpose's clickables draw the veils, so no button of the app draws its own.
- A press never moves a panel. A pull takes what the ray pointed at before the pull, so a pull that tips the ray from a button onto the handle below it presses the button. The handle under a panel and the sphere at its corner grow while a ray points at them and grow more while a trigger presses them. The panel moves only once the hand moves it more than 24 mm.
- A controller's ray starts at the tip of its drawn model (the node `laser_begin` of the runtime's model), on the line of the runtime's aim pose.
- A `SpatialFrameEffect` runs once for each headset frame. It hands the head pose, the eye views, both controllers and the predicted display time to the thread that sends the tracking.

The launch arguments are intent extras on Android (`adb shell am start -n dev.cranvr.client/dev.cranpose.android.CranposeActivity --es host 192.168.1.20:9950 --ez connect true`) and `--name=value` on a desktop (`--host=192.168.1.20:9950 --connect`):

| Argument | Meaning |
|---|---|
| `host` | The Mac's address and port for the text field. |
| `connect` | Connect at once. |
| `media` | `udp` or `tcp`: the media path. |
| `room` | `scene` or `passthrough`, for this launch. `none` shows neither the room nor the host's picture, for a screen capture of the menus on black. |
| `interface` | `3d` or `flat`: how the menus draw, for this launch. |
| `console` | Open the console and keep it open, for a headset on a desk. |
| `address_entry` | Open the address field of the connect panel at the start. |
| `controls_page` | `left` or `right`: open the console on the Controls page of that controller, for a screen capture. |
| `panels` | `hidden`: start with the connect and stats panels hidden, for this launch. A test of the beams and the models needs it. |
| `scripted_pointer`, `scripted_pitch` | Only in a build from `scripts/build-client.sh --scripted-hand`: with `scripted_pointer=desktop` both controllers play over the Mac's desktop panel for 12 seconds in turn, with right clicks, middle clicks, left clicks, a drag and scroll in both axes. The left controller points at the picture's 25% and 45% point, the right one at its 75% and 55% point. A host in a dry run (`CRANVR_POINTER_DRY_RUN=1`) logs the mouse events at those points. `scripted_pitch` tilts the head that the panel follows, in degrees, for a headset that looks down. With `scripted_pointer=hand` the left controller alone plays: its ray points level and straight ahead for 4 seconds, turns 35 degrees to the right and 25 degrees up in 1 second, stays there for 4 seconds and turns back. With `hand_menu=open` the menu shows that the hand's panels move and turn with the controller. With `scripted_pointer=models` both controllers stand still with their rays away from the panels, and the right one presses the trigger, A, the grip and the thumbstick forward and to the right in turn, 4 seconds each with 5 seconds of rest; the log says each change ("scripted models:"). With `scripted_pointer=console` the right controller points at the console and away from it, 4 seconds each, and the script opens the console once a game runs. |
| `log_poses` | Only in a build from `scripts/build-client.sh --scripted-hand`: `true` logs the head's and the controllers' grip and aim poses every 2 seconds ("scripted poses:"), for a check of the controllers lying on a desk. `scripts/desk-check.sh` runs it against a test host and prints the gap between each aim pose and the node `laser_begin` of the runtime's model. |
| `scripted_text`, `scripted_hand` | Only in a build from `scripts/build-client.sh --scripted-hand`: a scripted hand (`left` or `right`) presses the address field and then the keys of the text. It shows that the keyboard opens and types on a headset on a desk. The build installs over the plain app, so install the plain build after the test. |
| `scripted_pointer=pages` | Only in the scripted build: walks the pages of the Mac's settings with the right controller (see `client/src/scripted.rs`). It opens the hand menu with B, opens the first listed page there, presses a change on it that restarts nothing, goes Back and opens the first game's Options, where it presses a change too. The log says each aim ("scripted pages:"), and the Mac's app saves each change. `scripts/pages-check.sh` runs it against a test app with its own folder. |
| `scripted_pointer=ui3d` | Only in the scripted build: walks the 3D menus with the right controller (see `client/src/scripted.rs`). It presses the 3D ball, the menu's key Stream, a key and a toggle on the page Stream, and drags the slider of a later page to its next value. `walk_wait_ms` sets the wait before the first press. `scripts/ui3d-check.sh` runs it against a test app with its own folder and keeps the screen captures that show no room. |
| `scripted_pointer=menu` | Only in the scripted build: checks where the hand menu stands (`scripted_hands` is `right`, `left` or `both`, see `client/src/scripted.rs`). `scripts/menu-check.sh` runs it against a test host and prints the numbers. `scripted_pointer=palm` plays a left palm turned to the eyes and a right hand that pinches its ball, and `scripted_pointer=none` plays both controllers as missing. `scripted_pointer=press` checks that a press is never a move on the hand menu in front of the user, with `hand_menu=open`, `default_places`, `shown=connection` and `first_start` (see `client/src/scripted.rs`). |
| `gaze_pitch` | The angle in degrees below the eyes of the ball that a look presses while no input is tracked (34 by default). `0` puts it straight ahead, where a headset on a desk looks. |
| `scripted_point` | Only in the scripted build: `ball` points the scripted hand at the hand menu's ball (with `hand_anchor=view`) and pinches in a cycle. Each pinch presses the ball, and the host's log shows when the app held the hand back from the game. |
| `default_places` | Puts every panel at its default place and keeps no carry, for a test that aims at the panels where the code puts them. The user's places stay. |
| `hand_menu`, `hand_anchor` | `hand_menu=open` opens the hand menu at the start and keeps it open. In a flat window it shows the menu among the panels, for a screenshot. `hand_anchor=view` puts the ball, the menu and the hand's display in a subspace that follows the head, for a headset on a desk: the left controller's subspace shows nothing while no controller is tracked. `hand_anchor=head` puts them at the same place in a subspace that follows every move of the head, so their layers ride in the head's space as they ride in the controller's aim space on the hand. |
| `shown`, `pinned` | A test's panels (`display1,stats`) and the display on the hand (`pinned=2`). They show in the room and in every game, and nothing is saved. |
| `forget_host_key` | A comma list of addresses with their ports, such as `192.168.1.20:9961,192.168.1.20:9962`: the headset forgets the key of each host at launch. A test that connected to a test host on a network address leaves its key behind, and the headset then refuses the next host on that address. |
| `first_start`, `forget_menu_hint` | `first_start` shows the hand menu's hint as at a first start and saves nothing, for a test that opens the menu. `forget_menu_hint` forgets that the menu was opened, so the hint shows from the next start. A test that opened the menu without `first_start` needs it on the user's next launch. |
| `usb_port` | The port that the headset asks on the USB cable, for a test host on another port. |
| `usb_network` | `false`: the headset does not ask for its USB network for this launch, so its USB functions stay as they are. A test of `adb reverse` needs it, because the switch to the USB network cuts adb for a moment. |
| `desktop`, `desktop_width`, `cursor`, `desktop_pointer_test` | The desktop panel: `desktop false` asks for none, `desktop_width` is the widest picture to ask for. |

The client's modules: `connection.rs` runs the link on its own threads. `hub.rs` hands the numbers to the UI. `tracking.rs` turns Cranpose's `XrFrame` into the protocol's messages. `focus.rs` and `hands.rs` hold the rules for the input focus and for the user's hands. `ui.rs` holds the panels, `console.rs` and `console_panel.rs` the console, `displays.rs` the Mac's displays (their streams, decoders and wishes), `display_panel.rs` their panels, `display_wish.rs` what each asks of the host, `hand_menu.rs` and `menu_model.rs` the hand menu, `panel_layout.rs` which panels show and the ranks of their layers, `hold.rs` and `hover.rs` the hands that a panel holds back from the game, `audio.rs` the sound, `usb.rs` the cable and `phone.rs` the phone mode.

### Tracking

- The Hello carries the system name, the eye size to ask for, the refresh rates of `HeadsetProperties`, the fields of view and the eyes' places in the head. A change of the distance between the eyes goes to the host as a `Views` message.
- The runtime's display time and clock reading are on one clock. The client adds their difference to its own clock reading, so the display time lands on the client's clock.
- Each headset frame sends one tracking sample: the head pose, the grip pose and the aim pose of each tracked controller, each with its velocities. It also carries the display time that the poses are predicted for. The input of both hands (buttons, touches, trigger, grip, thumbstick) goes as a datagram with every sample, so a lost datagram leaves a button wrong for one frame at most.
- A recenter of the headset goes to the host as a `Recenter` message. The host then puts its desktop screen in front of the user again.
- The floor goes to the host as a `Floor` message. It comes from the stage that the runtime gives. The poses are in the app's room (OpenXR's LOCAL space), whose origin lies where the head was when the app started. The host moves the room up by the height of the floor before it gives the poses to its test scene or to a game, so the floor lies at height 0 there.
- The host's haptic pulses play on the controllers through Cranpose's `Haptics`.
- Hands, body, face and eyes (`tracked.rs`): the Quest build asks Cranpose for the four kinds of tracking in `build.rs` (`Use::hand_tracking()` and its siblings). An OpenXR instance fixes its extensions before the app runs, so the declaration cannot wait for a game. It writes the permissions to the manifest, and Cranpose asks the user for them before the OpenXR instance exists. The headset offers a kind to the host (`ClientMsg::TrackingOffer`) only while its tracker runs, which is after the user allowed it. A Quest 3 has no eye cameras, so it never offers the eyes. Its face comes from the microphone, which Cranpose calls the audio data source. The offer names the source of the face (`Features::FACE_AUDIO` or `FACE_VISUAL`), and each face sample carries its own. Cranpose names the source per frame, so the client offers a face of an unknown source until a valid sample names it. A face from Android XR's own extension has the 68 weights of Jetpack XR. The client maps them to Meta's 70 weights by `FaceBlendShapeType::expression()` and leaves the weights that Android XR lacks at zero. It offers that face as a visual one, because Android XR reads the face with the headset's cameras. The client sends a feature only while the host wishes it (`HostMsg::TrackingWish`): one datagram per headset frame, from a buffer that it allocated once. Cranpose gives no velocities for joints, so the client estimates the velocity of each wrist and of the body's root from the poses of two frames, as it does for the head.

### The eye size and the decoder

The client asks for the headset's recommended eye size. The host takes it unless `--eye-width` says otherwise. The client asks Cranpose for the decoder's limits (`VideoDecoder::capabilities`) before the Hello. It asks for the recommended size unless the frame that the host would encode lies beyond the decoder's largest picture or beyond its frame rate at the refresh rate. It then takes a smaller eye, down to half the recommended size. The client learns the host's foveation level only with the stream, so it sizes the eye for the level Low, whose frame is the largest of the foveated levels. A Quest 3's decoder takes it at the recommended size. A smaller eye goes to a client that takes no foveation and to every client of a host at the level Off: 1248 pixels at 120 Hz, 1504 at 90 Hz and 1728 at 72 Hz, unless `--eye-width` says otherwise.

### Foveated encoding

The Settings tab of the Mac app and the Stream page of the console pick the level: Off, Low, Medium or High (`--foveation` of the host). A change restarts the host. The app saves it as `foveation_level`. Each level keeps the full pixel density in a region of each eye and squeezes the edges by an edge ratio. The math follows ALVR (`common/src/foveation.rs`). The host sends the layout in the stream's parameters. The client's stream layer (Cranpose's `FoveatedEncoding`) unwarps any layout on the GPU. The Quest 3 has no eye tracking. The region therefore stays fixed. The host centers it on the direction straight ahead of the eye, from the view in the headset's Hello (`FoveationConfig::centered`). A Quest 3's eye sees 54 degrees out, 40 in, 44 up and 55 down. Straight ahead therefore lies at 62 percent of the width from the outer edge and 40 percent of the height from the top. ALVR's own shift put the region's middle at 53 percent of the height. That shift squeezed the view 15 degrees above straight ahead.

| Level | Full-density region (share of the view) | Edge ratio | Frame for a 1680x1760 eye | Pixels of an eye |
|---|---|---|---|---|
| Off | the whole view, at an eye of 1248x1308 | 1 | 2496x1308 | 55 percent of 1680x1760 |
| Low | 60 x 55 percent | 2.5, 3 | 2560x1248 | 54 percent |
| Medium | 45 x 40 percent | 4, 5 | 1984x928 | 31 percent |
| High | 35 x 30 percent | 5, 6 | 1600x736 | 20 percent |

**Measured on the Mac** (`host/src/tests/foveation_levels.rs`, `cargo test --release -p cranvr-host foveation_levels -- --ignored --nocapture`). The test runs the host's own encoder and a VideoToolbox decoder at 120 Hz for 240 frames. It undoes the warp as the headset does. It compares the view with the same eyes drawn at the full eye size with no foveation. The PSNR of the Y plane counts in three regions by the angle from straight ahead on each axis: the center within 15 degrees, the ring from 15 to 35 degrees and the edge beyond. The game scene is a landscape with fine detail that pans at about 60 degrees a second, as `compose` takes a game's eyes. The test scene is the host's own scene with a head that turns. The stream sent 70 percent of the target, as the eye stream does. The encode and decode times are medians on an M5 while the cranvr app's host streamed to a headset.

| Level | Mbps | Game: center | ring | edge | SSIM center | Test scene: center | ring | edge | Encode ms | Decode ms |
|---|---|---|---|---|---|---|---|---|---|---|
| Off | 40 | 27.6 | 27.6 | 27.2 | 0.944 | 29.8 | 33.0 | 36.1 | 3.8 | 2.6 |
| Low | 40 | 33.2 | 33.3 | 22.6 | 0.968 | 36.0 | 38.1 | 35.6 | 3.5 | 2.6 |
| Medium | 40 | 37.5 | 26.4 | 19.2 | 0.978 | 37.7 | 34.5 | 31.3 | 2.3 | 1.6 |
| High | 40 | 38.9 | 23.0 | 18.6 | 0.983 | 38.6 | 30.9 | 29.1 | 1.7 | 1.2 |
| Off | 12 | 26.3 | 26.5 | 26.0 | 0.906 | 29.4 | 32.2 | 35.2 | 3.6 | 2.6 |
| Low | 12 | 30.2 | 30.4 | 21.9 | 0.931 | 32.8 | 35.0 | 33.9 | 3.5 | 2.6 |
| Medium | 12 | 32.3 | 25.5 | 19.0 | 0.943 | 33.7 | 32.6 | 30.3 | 2.3 | 1.5 |
| High | 12 | 33.3 | 22.6 | 18.5 | 0.952 | 34.2 | 29.9 | 28.5 | 1.7 | 1.1 |
| Medium, ALVR's shift | 40 | 32.1 | 26.7 | 20.1 | 0.972 | 37.3 | 35.3 | 31.9 | | |
| Medium, ALVR's shift | 12 | 29.6 | 25.7 | 19.7 | 0.935 | 33.4 | 33.0 | 31.0 | | |

The rows "ALVR's shift" are the stream before this change. They have the frame of Medium with the region where ALVR put it. Their encode and decode times are the same as Medium's. Centering the region made the center 0.3 to 5.4 dB sharper at the same bitrate. The game scene gains most. The striped tops of its posts lie about 14 degrees above straight ahead, where ALVR's shift squeezed the view.

The test also counts the PSNR of 32 strips across the rows within 15 degrees of straight ahead. The PSNR rises and falls smoothly from the edges to the center at every level. Dips of up to 0.8 dB below both neighbors show at the same strips with Off. Off has no warp. The picture itself makes these dips. No other dip exceeds 0.6 dB. No dip lies at the end of a full-density region. The warp shows no seam. The squeezed edges lose fine detail. Stripes of 6 pixels show moire at the edges of Medium and High.

**A quantizer per region.** VideoToolbox has no public way to give the encoder a quantizer per region. Its public frame options are a key frame, a base QP for the whole frame and the long-term references. The HEVC encoder lists a private property `EnableUserQPMap` and takes it. No documented way hands the encoder a map. Maps of one value given with the guessed private frame keys `QPMap` and `QPMapSize` had no effect: QP 10 and QP 50 gave the same frame sizes. A quantizer map would also need every pixel of the eye in the frame. The row `edge blur` of the test stands in for such a map. It keeps every pixel of a 1680x1760 eye and blurs the edges as much as Medium squeezes them. The encoder then spends few bits at the edges. The center of the edge blur matches Medium (37.3 dB at 40 Mbps). Its frame of 5.9 megapixels takes 6.1 ms to encode and 5.1 ms to decode on the Mac. Medium takes 2.3 and 1.6 ms. A warp keeps the work of the encoder and the decoder small. A map of quantizers cannot do that.

**Measured on a Quest 3.** A Quest 3 lay on a desk. It took the stream over the USB network from a test host on port 9961 (`scripts/e2e.sh --quest SERIAL --cable usb-network --native-port 9961 --moving --seconds 30 --host-arg --foveation --host-arg LEVEL --host-arg --game-scale --host-arg 1`). The fake game pans a landscape with fine detail across both eyes at 120 Hz. The levels ran in turn with 6 to 8 runs each. The table gives the medians over the runs. Some runs had ALVR's shift and some the centered region. The frames have the same size in both. The encode and the decode thus do the same work. The sharpness is the mean square of the Laplacian of the luma in a square of 300 pixels in the middle of the left eye's panel. It comes from screencaps that a script measured and removed at once. The moving picture makes it noisy. The still column comes from one run of the host's own scene for each level, with 5 captures in each run and the headset at rest.

| Level | Host encode ms | Quest decode ms | Encode + decode | `total_ms` | Late frames | fps | Sharpness, moving | Sharpness, still |
|---|---|---|---|---|---|---|---|---|
| Off | 5.44 | 4.36 | 9.80 | 45.0 | 0.17% | 119.3 | 185 | 55 |
| Low | 5.16 | 4.31 | 9.47 | 46.4 | 0.40% | 119.5 | 359 | 194 |
| Medium | 3.40 | 4.25 | 7.65 | 43.2 | 0.00% | 120.0 | 394 | 215 |
| High | 2.57 | 3.76 | 6.33 | 41.5 | 0.00% | 120.0 | 497 | 215 |

Two runs of each level went over Wi-Fi at 12 Mbps (`--host-arg --no-cable`). Medium had encode plus decode of 7.19 ms and a `total_ms` of 49.2. High had 5.80 and 48.5. The network took 4.2 ms with both levels. The late frames were 3.1 and 3.2 percent.

The eye badges of the host's scene show where the unwarped view puts a fixed point. The screencaps have 2064 by 2208 pixels for each eye. The red badge's centroid in the left eye lay at the same place for Low, Medium and High within 2 pixels across and 1 pixel down. It lay 2 to 4 pixels across and 5 pixels down from its place with Off, which has no warp. The blue badge of the right eye lay within 3 pixels of one place for the three levels. It lay 7 to 8 pixels down from its place with Off. The offset from Off is the same for every level. Each level has its own shift. The centering thus puts the picture in its place. The cause of this offset of 0.2 to 0.3 degrees is not known.

**The default is Medium, on the cable and over Wi-Fi.** Off and Low cost 1.8 to 2.2 ms more encode and decode than Medium on the Quest. That exceeds the bound of 1 ms set by #93. The center of Off is also 8 to 10 dB worse at 40 Mbps. High gains 0.9 to 1.4 dB in the center at 40 Mbps. It saves 1.3 ms of encode and decode. It loses 3.4 to 3.6 dB in the ring from 15 to 35 degrees. Its full-density region ends about 20 degrees from straight ahead instead of 26. Medium's center already stands at 37.5 dB (SSIM 0.978) at the cable's 40 Mbps. The ring therefore counts more than the last decibel of the center. High makes the same trade at 12 Mbps: 0.5 to 1.0 dB more in the center and 2.7 to 2.9 dB less in the ring. The network sets the latency over Wi-Fi. Wi-Fi therefore keeps Medium too. Medium gives a center 0.3 to 5.4 dB sharper than the stream before this change. It uses the same frame and adds no latency. High is the level to try for a sharper center and the lowest latency.


### The console

The Console row of the hand menu shows or hides the console, a movable panel with the games to play and the Mac's settings, below and ahead of the head and tilted up like a desk. A button that the mapping gives "Open the console" does the same. While the link streams, such a button needs a hold of 0.8 seconds, so a shorter press goes to the game only (see "Controls"). The console shows when the first menu arrives and while no game runs. It hides when a game starts.

**One model of the settings.** The cranvr app on the Mac owns every setting (`app.json`) and every action. It describes them as pages of items (`cranvr_common::menu`, `app/src/pages.rs`): a choice, a toggle, a line of text, a heading, or a row of buttons that run an action or open another page. Each item has an id, its words, its value and a hint for the window. A per-game item also names the game's program. The window draws the pages with one composable (`ui::PageItems`). The app writes them into `vr-menu.json` with the controls and a one-line `notice` while it has something to say, without the window's hints. The host sends the file as `HostMsg::Menu` when the client connects and when it changes (at most once a second). The stream's state (the refresh rate, the frames a second, the latency and the game that runs) changes every few seconds, so it travels apart as `HostMsg::MenuStatus`, and the pages go out only when they change. With the owner's three games the menu is 8.3 KB. The console draws the same pages with one set of rows (`client/src/page_model.rs` lays them out, `console_panel.rs` draws them). A press sends `ClientMsg::Console` with a generic request: `{"action": "set", "id": "fps", "value": "90"}` for a setting, `{"action": "run", "id": "game.play", "subject": "C:\\Games\\...\\game.exe"}` for an action. The host hands it to the app as a `vr_request` status line. The app applies it with the function that its window's buttons call (`pages::apply`, `run::act`): it checks the value against the setting's options, saves the settings and restarts the host when the setting needs it. The client knows no setting by its meaning, so a new setting in `pages.rs` shows in the window and in the headset with no change of the client (protocol 16).

**The pages.** The console's main page is the Mac's first page: the games, each with Play (Starting... until the host names the game's program), Options and Stop while it runs, and Stop Steam and games. Under it stand the links to the other pages, three to a row, and the buttons Close, Controls, Disconnect (while the link runs) and Exit. A game's Options page has its VR system (As in Settings, OpenComposite, SteamVR or Off, see "OpenVR games"), "Each eye in its own pass", "Both eyes in one pass", cranvr's reasons for what it chose and what the game starts with. The other pages, in the order of the links:
- Steam: the game support's version, Rosetta 2, Steam's state and the sign-in note, the setup's steps with their progress, the one setup button that the Mac needs (Set up games, Update game support, Repair Steam, Install Steam or Retry), Open Steam and Stop Steam and games. Rosetta's install asks for the Mac's password on the Mac, and Steam's sign-in happens in its window on the Mac.
- Stream: the refresh rate, the bitrate, the foveation, the game's render size and anti-aliasing, the panel pixels, the controllers from the hands, the host's start and the learned game sizes with their Forget button.
- Connection: Automatic, Wi-Fi or USB cable, the path of the stream now, the cable's notes, Add USB support and the network's notes.
- Desktop: the desktop view, the screen in VR (the main display or an added one), the added display's settings, the screen updates and the three permissions, each with Ask and Open Settings. macOS asks on the Mac's screen.
- Sound: the game sound and the microphone in games (the headset, each input of the Mac, or off).
- Wine: its paths, the game window on the Mac, the OpenVR choice for every program and what the text fields of the window hold.
- Headset: the Room (Passthrough or Scene) and the Volume (100% to 300%), which the headset applies.
- Mac: the host with Restart the host, a settings file that cranvr cannot read, the paired headsets and the Mac's addresses.
- Add game: the Windows programs that the Mac found in the top folder of each game's folder in a Steam library and in `C:\Games`, which no game starts yet (24 at most), each with Add and Run once. The headset has no file chooser, so it adds a game from this list. A request names only a program of this list: the app refuses any other path. The window lists them too, beside its file chooser.
- Log: Show the newest lines puts the newest 14 lines of the log on the page for a minute. The lines change all the time, so the menu carries them only then.

**The headset's own page on the Mac.** The headset owns its panels, so it sends the Mac a page of its own (`ClientMsg::HeadsetPage`, `client/src/headset_page.rs`): the rows of the hand menu with Show and Hide of the console, the connection panel and the stream numbers, each display with where it stands, Hand or Room and Remove, Add display, Recenter and Controls, and a row with Open the hand menu, Disconnect and Close the headset app. A display says where it stands and whether it shows, not its cost, so the page changes only with a press. The client sends it when it changes. The host hands it to the app as a `headset_page` status line, and the window's Headset card draws it with the same items. A press there goes back through the host's input (`headset JSON`) as `HostMsg::HeadsetRequest`, and the headset does it as the hand menu's button does.

**Room and Volume.** The Mac keeps them (`headset_room`, `headset_volume` in `app.json`) and sends them with the menu. The window's Settings tab and the console's Headset page change them. The headset applies them and saves the last ones for the time without a link. A Mac that has none yet sends null, and the headset tells it its own saved choice once for each connection, so a user's choice from before stays. The launch extra `save_choices=false` keeps the headset's saved room, volume and controls as they are, for a test with a test host on the user's headset.

The window draws the same pages: the Settings tab has Connection, Stream, Sound and Mac, the Desktop tab has Desktop, and the Play tab has Steam, the games and Wine. A page has Back and Close at its bottom. A choice that restarts the host restarts the stream, and the client connects again by itself every 2 seconds until the stream is back, unless the user pressed Disconnect.

A press shows on its key until the Mac answers: "Changing..." on an option until the menu shows the new value, an action's own words ("Starting...", "Stopping...") while the Mac marks its button busy and for 2.5 seconds before the Mac can say so. A key that waits 90 seconds says "no answer". A new notice of the app ends the wait, because the app says there why a request failed.

The audio renderer multiplies each sample by the volume of the Headset page. A soft limiter above 0.8 keeps loud parts below 1.

### The hand menu

A ball floats 11 cm above the left controller. The right controller's ray presses it, because a controller's own ray passes through the panels that ride on it. A press opens the hand menu, a panel 47 cm wide that stands 21 cm to the right of the hand, with its bottom edge 3 cm above the ball. A second press closes it. The left menu button and B open and close it too (see "Controls"). The menu lists:

- every panel of the app, each with a button that shows or hides it: the console, the connection panel, the stream numbers and each display of the Mac;
- under each display, where it stands (in the room, or with the menu: on the left hand, on the right hand or in front of you) and what showing it costs: the picture's size, the pictures a second, the Mbps and the decode time, or "Hidden: it costs nothing";
- for each display a second button that puts it on the left hand or back in the room, and for an added display a third that removes it;
- the pages of the Mac's settings, three buttons to a row, as the host's menu lists them. A press opens the console on that page and closes the hand menu;
- at the bottom Add display (while the host can make one), Recenter and Controls. Recenter makes the panels stand in front of the head again. Controls opens the console on its page of the controls.

The text is 20 to 26 mm tall, which is 2.5 to 3.3 degrees at the 45 cm from which a hand holds the menu to the eyes. A press on a button gives a short click in the hand that works the menu. The menu closes by itself after 20 seconds in which no ray was over a panel, so a menu that the user forgot open in a game does not keep the right hand's buttons from the game.

`menu_model.rs` holds the rows as data (the words, the buttons, the height), `hand_menu.rs` draws them and `panel_layout.rs` keeps which panels show.

**Which panels show.** Without a game the user's choice is saved. The first start shows the connection panel, the stream numbers and the first display. While a game runs no panel shows until the user picks one, and the pick ends with the game. A panel that is not shown costs nothing (see "Several displays"). The ball is the one panel that a game with nothing shown carries. The client logs `layers composed by the app: 1 panels and 0 display surfaces` for it.

**A display keeps its place.** Each display keeps its place by its stream: shown or hidden, in the room or on the hand, its pose and its width. A host that does not offer a display, such as a test host with no displays, changes none of it. The display then shows nothing. Its place comes back when a host offers it again. A display that a host adds during a session, such as one that the user asked for, shows at once. The Remove button of an added display takes its place away. The client logs `the panels' saved layout: room [...], hand [...]` at each start and `the panels' layout is saved: ...` at each save.

**A display on the hand.** The Hand button of a display puts it above the left hand, 42 cm wide, with its bottom edge above the ball. A 1920 point wide Mac display then shows 13 point text 3 mm tall, which a Quest 3 resolves at 45 cm. The user can resize it with the corner sphere (28 to 70 cm) and carry it with the sphere under it. A display on the hand asks the host for 1920 pixels across at 30 pictures a second, because it is small. While a display rides on the hand, the ball becomes a pill with a second round button. One press on it shows or hides that display, also inside a game, for quick actions on the Mac. The Room button in the menu puts the display back in its place.

**Where it stands.** The menu goes where the user's input reaches it (`menu_place.rs`):
- Two controllers: on the menu hand, and the other hand points at it. The setting "Hand menu" in the Mac app's Controls tab and on the console's Controls page names the hand: Left hand, Right hand or Auto, the default. Auto takes the hand that did not last pull its trigger on a panel while the menu was closed, and the left hand before any pull. So a left-handed user who works a panel with the left hand gets the menu on the right hand. The menu never moves while it is open or a ray is on it. On the right hand the menu and the display stand to the left of the hand, as a mirror.
- One controller, or none: in front of the user, 55 cm ahead of the eyes with its top edge 6 cm below them, tilted to face them, and that controller points at it. A controller's own ray passes through the panels on it, so a menu on the only hand could not be pressed. The ball stands 28 cm to the left of the nose and 33 cm below the eyes, and a display that rides with the menu stands to the right of it. They stand in `FollowingSubspace(FollowTarget::View(FollowMode::soft(TrackedDimensions::TranslationAndYaw)))` (Cranpose#2465): they hold still for the small moves of the head that come with pointing at them, and they glide back into view after a turn of more than 24 degrees or a step of more than 20 cm. A look down moves nothing.
- A controller that the runtime stops tracking for 2 seconds (a flat battery, a hand out of the cameras' view) gives the menu up, and the menu moves to the view. It comes back to the controller after 0.3 seconds of tracking. Both controllers count as present at the start, so a user with both sees the ball on the hand from the first frame.

**The first start.** A line under the ball names the ways to open the menu: the buttons that the mapping gives the menu on the controllers that are present, with "Hold" while a link streams and "Press" while none does, for example "Hold the menu button or B to open the menu". Without such a button it says "Point at the ball and pull the trigger". Without a controller it says "Show a palm and pinch it with the other hand" while the cameras track a hand, else "Look down at this ball for a second". It shows at the first start and again when the tracking falls to one controller, and it goes when the menu opens, whichever way. The first opening is saved, so later starts show no line. The line is a panel only while it shows. The launch extra `first_start` shows it as at a first start and saves nothing, for a test (`menu_hint.rs`).

**Hands without controllers.** A hand that the cameras track and that turns its open palm to the eyes (its normal within 50 degrees of them, for 0.2 s, with no pinch) gets a ball 7 cm in front of the palm (`palm.rs`). It rides in `FollowingSubspace(FollowTarget::Hand(hand, HandJointType::Palm, ...))` (Cranpose#2467). The other hand's pinch presses it through Cranpose's hand pointer, which ignores the panels on its own hand. The right palm's pinch belongs to the system's menu, so nothing asks for it. The left palm's pinch reaches the app as the menu gesture and opens or closes the hand menu as the menu button does. The ball goes 0.3 s after the palm turns away. The menu opens in front of the user, where both hands' pointers reach it. While a game owns the hands (`hands.rs`) no ball shows.

**No input at all.** While no controller is present and the cameras track no hand, the ball in the view gives way to a ball that a look presses (`gaze.rs`): it stands 45 cm ahead of the eyes and 34 degrees below them, and it follows the head's place and heading in every frame, so the app knows where it is. A Quest 3 has no eye tracking, so the look is the head's ray: the user tips the head down until the ball is in the middle of the view. When the ray passes within 4 degrees of the ball for 1 s, the menu opens or closes. The ball turns blue while the look counts. The launch extra `gaze_pitch=0` puts it straight ahead, for a headset on a desk.

**From the Mac.** The button "Open the menu in the headset" stands under the headset's status at the top of the cranvr window while a headset is connected. It writes `menu open` to the host's input, and the host sends `HostMsg::OpenMenu` (protocol 15). The headset opens the menu where it stands now and logs `the Mac opens the hand menu in front of you`. This serves a user whose controllers do not reach the menu.

The client logs each move: `the hand menu moves in front of you: the left controller is lost, the right one tracked`. Places that the user carried the menu or the display to are kept for each place: `hand-menu` and `hand-display` on the left hand, `hand-menu-right` and `hand-display-right` on the right one, `view-menu` and `view-display` in front of the user.

**Where it rides on a hand.** The ball, the menu and the hand's display stand in one `FollowingSubspace(FollowTarget::Controller(hand, FollowMode::Tight(TrackedDimensions::All)))`. They hold their place against the controller: they move and turn with it in every frame, also when the hand tilts or rolls. The places above are in the controller's own frame: beside the controller, above its top and back along its ray toward the face. Cranpose hands their layers to the compositor in the space of the controller's aim, so the compositor places them with its newest pose of the controller at the time the frame shows. The log line `the frame has N layers, K of them in the space of the head or a controller` counts them.

**Carrying a hand panel.** The menu and the hand's display have a carry sphere under them. The other controller grabs it and carries the panel: a move and a turn of that hand set the panel's offset from the menu hand's controller. After the carry the panel follows the hand at the new offset. The offsets are kept in the app's preferences under the names above, apart from the places of the panels in the room. A display that goes back to the room stands where it stood there.

A headset on a desk tracks no controller, so the menu stands in front of it after 2 seconds. The launch extra `hand_anchor=view` instead puts the three in a subspace that follows the head, 30 cm left of the nose, 35 cm below it and 40 cm ahead, as a left hand would hold them. Places carried there are kept apart (`hand-menu-view`, `hand-display-view`). `hand_menu=open` opens the menu at the start. In a flat window the same extra shows the menu among the panels, for a screenshot.

### Panels past the layer limit

A Quest 3 takes 16 layers in a frame. Each display of the Mac is a layer, each panel is one and each handle sphere under a panel is one. The stream, the rays and the room take theirs too. Four displays with the console, the connect panel and the hand menu need more than 16. Cranpose (#2450) then draws some pictures into the projection layer of the 3D content, so every panel still shows. The handles go there first. Then the panels of the lowest rank go there, and their text shows softer. The Mac's displays are surfaces: they keep their own layers before every panel.

The client ranks its panels with `SubspaceModifier::layer_priority` (`panel_layout::layer_rank`). A panel ranks higher when the user reads more text on it, and smaller text:

| Rank | Panels | Why |
|---|---|---|
| 3 | the Mac's cursor ring over a display | The displays come first. On a runtime with no depth test, a ring in the projection layer can draw under the layer of its display. |
| 2 | the console and the connect panel | Most of their text is 20 to 22 mm tall at 0.6 to 1 m, which is 1.3 to 2 degrees. |
| 1 | the hand menu: the ball and the menu | Its text is 20 to 26 mm tall at 45 cm, which is 2.5 to 3.3 degrees. It stays readable when it shows softer. |
| 0 | the stream numbers | The user reads them only to check the link. |

The host draws the Mac's cursor into the pictures by default, so a display has no ring unless the host runs with `CRANVR_DESKTOP_CURSOR_IN_VIDEO=0`. Panels of one rank keep their layers by Cranpose's own order: the panel that a hand points at first, then the nearer one. The client's log names each change of the plan (`cranpose xr: the frame needs N layers and gets M of the 16 that the runtime takes: the projection layer draws P of them, Q are left out`).

### Panels in a game and the hands

A hand that points at a panel presses it with its trigger, works it with its thumbstick and carries it with a grab. The game must see none of that. The client decides, for each frame of the headset, which hands a panel holds back (`hold.rs`). The sender puts the answer into the datagram that carries the same frame's input, `ClientDatagram::InputHold`, so no press of a held hand reaches the game before the hold does. Every input datagram carries the hold.

The host gives the game rest values for a held hand: no button, no touch, no trigger, no grip and a centered thumbstick. The poses stay, so the game still draws the controller. A press that is still down when the hold ends stays up for the game until the user lets go of it, so a trigger that the user holds after the ray left the panel presses nothing in the game. The same holds for a pinch that a hand makes into a game's controller (`hand_mask.rs`, `tracking.rs`). The host clears the hold when the headset loses the focus or leaves.

Cranpose gives each hand a pointer of its own, and a pointer event carries the id of its pointer: `PointerId(1)` for the left hand and `PointerId(2)` for the right hand (Cranpose#2432). The client therefore knows which hand's ray is on a panel (`hover.rs`). The root of each panel of the app reads the events of its pointers with `pointer_input` (`hover_modifier`), and a display's surface reads them in its own callback. An event with another id comes from the window's mouse. It counts for both hands. A pull on a panel keeps the hand held until the trigger lets go, also when the ray leaves the panel during the press. A hand stays held for 150 ms after the ray left the last panel, and for 1.5 seconds while the hand menu is open and the hand works it: the other hand of a menu on a hand, or the hand that points at a menu in front of the user. A grab of a panel's handle (the carry sphere under a panel, the resize sphere at its corner) holds the hands whose triggers are down from its start to its end.

What the app cannot know without more from Cranpose: that a hand's ray is on a handle. A handle gives the app no hover and no hand, only the start and the end of a grab. The pull that starts a grab therefore reaches the game for the frames before the grab's start. `XrFrame` or the pointer's events would have to name the panel or the handle that each hand's ray meets for an exact answer.

### Controls

Each button of each controller does one thing in the headset's own interface: the panels, the Mac's desktop panel and the headset itself. The mapping is a page of the app's Settings tab (General and Controls) and the page that the console's Controls button opens. A game that runs still gets every button. The mapping decides only what the headset does.

The inputs of a Touch controller are the trigger, the grip, the two face buttons (A and B on the right, X and Y on the left), the press of the thumbstick, the thumbstick's lean up and down, its lean left and right and the left controller's menu button. The Meta button belongs to the system and is not mapped. Each input takes one of these actions:

| Action | What it does |
| --- | --- |
| Left click, Right click, Middle click | The mouse button on the Mac's desktop panel. The host posts a real mouse event at the pointed place. |
| Scroll | A thumbstick axis scrolls the Mac's desktop panel. The host posts scroll events, up and down and to the sides. Only a thumbstick axis offers it. |
| Open the menu | Opens or closes the hand menu, which shows and hides every panel and holds the console. Every button and the left menu button offer it. |
| Show or hide panels | The connection panel and the stream numbers together, as the panels' buttons of the hand menu do one by one. |
| Open the console | Shows or hides the console. |
| Recenter | Brings the panels in front of the user. The panels' subspace snaps to the head again. The front of a game's room is the system's: the Meta button's hold recenters it, and the headset passes that on to the game. |
| Keyboard | Opens or closes the address field of the connect panel with its keyboard, while no link runs. |
| Nothing | |

The standard mapping: the trigger is the left click, A and X are the right click, Y shows or hides the panels, the thumbstick scrolls in both axes and its press is the middle click, as the press of a mouse's wheel is. The grip does nothing. The left menu button and B open the hand menu, so a user with one controller in hand still has a button for the menu. The console is a row of the menu. The two controllers are otherwise the same, so either hand can point. The menu button offers no mouse button. A saved `app.json` names every entry of the mapping, so a user who saved a mapping keeps it: "Use the standard controls" in the Mac app or Standard in the console's Controls page gives the new one.

The mouse actions work on the Mac's desktop panel. Cranpose's pointer turns each input into one of Compose's five pointer buttons (`PointerSettings::set_mapping`): the client gives each input that some hand maps to a mouse button a button of its own (`client/src/controls.rs`: trigger primary, A and X secondary, the stick's press middle, the grip back, B and Y forward) and gives no button to an input that does something else, so it reaches no panel as a click. A press then arrives at the desktop panel with the id of its hand (1 left, 2 right) and with the button of its input, and the panel looks up the action of that input and that hand in the user's mapping. So each hand has its own mapping, also for the thumbstick's two axes. The headset's own panels are different: Cranpose has one mapping for both hands, and the trigger clicks their buttons and takes their handles whatever it is set to. The thumbstick scrolls Cranpose's own panels while any axis of the mapping scrolls. The Mac has one mouse, so one controller drives it: the one that holds a button keeps it, the one that presses next takes it, and else the first that points at the panel (`Pointers` in `client/src/displays.rs`).

The five actions of the headset work wherever the controller points. While no link streams, a press does the action. While the link streams, the button must stay down for 0.8 seconds, so that a short press stays with a game, as the menu button's hold always did (`client/src/controls.rs`). The rule keys on the stream and not on the host's word that a game runs, because that word can be late or missing.

The beams show only while one of the app's own panels shows (connect, stats, console, desktop). A game draws its own rays and its own controllers. With every panel hidden in a game, the beams hide (`PointerSettings::set_beams_visible`). The ball is pressed without a beam then. The hold of a button that shows the panels again needs no beam. The build declares `Use::controller_models()` and turns on Cranpose's `gltf` feature, so a Quest shows its own models of its controllers (`XR_FB_render_model`) in the hands. Another runtime gets Cranpose's plain model. The trigger, the grip, the buttons and the thumbstick of the Quest's models move with each press.

Outside a game the models show with the beams. In a game the setting "Controller models during a game" decides, because the game draws its own controllers at the same place: "Always", "While pointing at a cranvr panel" (the default) or "Never". With the middle choice a hand shows its model while its ray points at one of the app's panels. A hand whose ray points at no panel shows none (`client/src/models.rs`). A model that is off costs no projection layer. A shown model costs about 1.8 ms of the headset's GPU time per frame on a Quest 3 and moves the predicted display latency from 19 to 25 ms. Cranpose keeps the scene's swapchains 5 seconds after the last model hides, so a model that comes back within them makes no new swapchains. The setting lives with the controls: the Mac app shows it on the Controls page of Settings, and the console in the headset shows it on its Controls page. A change in the headset goes to the Mac, which keeps it in `app.json` and sends it back with the menu. The rule names no game: it reads the host's word that a game runs and the hover of each hand's pointer.

`cranvr_common::controls` holds the mapping for the app, the host and the headset. The app keeps it in `app.json` under `controls` (an entry that the file leaves out takes the standard action), writes it into `vr-menu.json` and the host sends it to the headset with the menu. A change in the console goes back as `{"action": "set", "controls": {...}}` in `ClientMsg::Console`. The app saves it and the next menu repeats it. The console shows the change at once and keeps it against a menu that was on its way, for up to 3 seconds. The headset keeps the last mapping that the Mac sent (Cranpose's preferences service), so the buttons keep working without a link. The console's page shows that mapping and changes nothing while no link runs.

### Focus and pause

The headset's runtime tells an app how much of the headset it has. Cranpose maps the state of the OpenXR session to the app's lifecycle (`cranpose::local_lifecycle_state()`). `Resumed` means that the app has the input focus. `Paused` means that the app is on display without the focus, under the system's menu or beside another app's panel. `Stopped` means that the headset shows none of the app. Meta's rules say that an app without the focus keeps drawing, shows no controllers and takes no input. Cranpose's backend does that for the panels and the streamed scene. The client does it for the link (`focus.rs`, `tracking.rs`):

- The sender tells the host with `ClientMsg::Focus` when the state changes. The values are `Focused`, `Visible` (the lifecycle's `Paused`) and `Hidden` (`Stopped`). An app that starts without the focus says so once after the welcome. A client that never says anything is focused, and so is a host that never hears anything. The message is a new variant at the end of `ClientMsg`, so a host from before it skips it.
- The sender sends no tracking and no input while the app is not focused. The connection goes on with its pings and loss reports, so the session lives. The scene keeps the last picture on display. The stats panel shows a Focus line. The client plays no haptic pulse of the host.
- The host lets go of every button that the headset held and tells the game's runtime that no controller is tracked. It stops the video of the eye stream and of the desktop panel. It stops the sound too. The game keeps running on the Mac, because the runtime has no message for a change of focus. When the focus comes back, the host starts the eye stream and the desktop stream with key frames. The next tracking sample brings the controllers back. The host's log names each change and the frames that it sent in the time before: `the headset's app is visible without the input focus: the stream pauses. It was focused for 6.0 s, and the host sent 677 video frames in that time`.
- A flat window of the client on a Mac keeps its placeholder samples at all times, whichever window the user works in.

`scripts/e2e.sh --pause 6:6` runs the probe through such a pause on a Mac. The probe loses the focus at 6 seconds for 6 seconds, as the client does under the system's menu. It counts the frames that still arrive. The run passes when none arrives after the first 0.3 seconds of the pause and the stream goes on after it. With `--game` the fake game keeps running through the pause.

### Hands and the panels

An app that declares `Use::hand_tracking()` (`build.rs` does) has a hand pointer: a tracked hand with no controller in it points at the panels, and a pinch presses. A controller that the runtime tracks points first. Every panel works with a pinch.

The client does not send such a hand to the host as a controller (`held` in `tracking.rs`). The runtime reports it through the hand interaction profile, with an aim pose and a pinch as the trigger, and a Quest keeps that aim pose valid when no hand is in view. The host decides by itself when a hand makes a controller, from the hand's joints (see "Controllers made of hands"): it asks for hands only while a controller is missing. A hand that counted as a controller would hide the missing one from the host. The game would then get a pose and a trigger when no hand is in view.

The same pinch must not press a panel behind the game's picture while it is the game's input. **While the host asks the headset for the hands and a tracked hand points at the panels, the hands belong to the game, and the panels that stand in the room do not show** (`hands.rs`). That covers the connect panel and the stats panel. It covers the ball above the left controller as well. A panel that is not composed gets no hit, so no pinch can press a button such as Stop behind the picture. The console stays. The user opens it on purpose with a hold of the left menu button.

- The host asks for the hands (`HostMsg::TrackingWish`) in two cases: a game creates a hand tracker, or a game without hand code runs while a controller is missing. Without a game the host asks for no hands, so the panels work with a pinch before a game starts and after it ends. The client names no game.
- A user who holds both controllers has no hand pointer, so the panels stay. OpenComposite asks for hands for every OpenVR game, and such a game keeps its panels and its ball.
- A hand that the cameras lose keeps the answer for a second and a half, so the panels do not flicker. The wish ends with the game or the link, and the panels come back at once.
- The way out for a user who plays with hands only: the panels stay hidden. The user picks up a controller, whose ray points first, and holds its menu button or B to open the hand menu. Without a controller the button "Open the menu in the headset" of the cranvr app opens the hand menu, which shows while the hands belong to the game. The user can also end the game in the cranvr app on the Mac or leave the headset app through the system's menu.

### Text entry

A text field asks for the platform's software keyboard when it takes focus. In a Quest, Cranpose draws the keyboard as a panel of keys under the panel of the field. A press of a controller's ray or a pinch of a hand on a key types. On Android XR the system's keyboard shows instead. The address field of the connect panel is the app's only text field. It takes the typed address of a Mac. A headset also keeps the keypad in that panel, which edits the same text. It stays until the drawn keyboard has run on a Quest 3, because Cranpose tested the keyboard in a window and in an emulator only. The phone build uses the system's keyboard.

### The Meta store check

Cranpose has an entitlement check for the Meta Horizon Store (the `meta-entitlement` feature of `cranpose-xr`). cranvr leaves it off. The real application id needs the owner's Meta developer account. Whoever turns the check on also accepts the license of Meta's Platform SDK, which the feature downloads from Maven Central. To turn it on:

1. Add the feature to the Cranpose crate in `client/Cargo.toml`: `cranpose-xr = { git = "...", rev = "...", features = ["meta-entitlement"] }`.
2. List the service in `client/android/app/build.gradle.kts` and give the id: `services.addAll("xr", "meta-entitlement")` and `metaAppId.set(providers.gradleProperty("cranposeMetaAppId"))` in the `cranpose { ... }` block.
3. Build with the id from the developer dashboard (My Apps, Development, API): `-PcranposeMetaAppId=ID`.

A failed check quits the app. A build that installs without the store fails the check, so a test needs the app's store channel or a test user.

### Phone mode

`scripts/build-client.sh --phone [--install SERIAL]` builds `target/client/cranvr-phone.apk` from the same crate. Its application id is `dev.cranvr.client.phone` and its label is "cranvr phone". The build differs from the headset's in four places: the Cargo feature `phone` replaces `android` (the entry is `cranpose::android_main!`), the build has no OpenXR loader and no `xr` service, the Gradle property `cranvrPhone=true` switches the module and the phone's manifest turns the activity to landscape with the sensor. `cargo run --manifest-path client/Cargo.toml -- --phone --host=127.0.0.1:9950 --connect` runs the phone's app in a desktop window.

- The picture fills the screen. A tap opens the connect panel and the stats panel over it. The Hide panels button closes them. They open again when the stream ends. The Recenter button puts the host's front where the screen points.
- The head comes from the device's motion sensors (`client/src/phone.rs`). The first look along the horizon becomes the host's front. The head stays at a height of 1.6 m.
- The Hello carries one view shape for both eyes. It spans 90 degrees across the screen's long side. The host renders two eyes as usual and the phone shows the left one. Cranpose has no public call for the display's refresh rate, so the client measures it.
- A phone's scene shows the newest frame and Cranpose reports nothing about it. The client sends the frame timing when the frame arrives: the picture is ready one mean decode time after the last packet, the next screen frame takes it and that frame reaches the display one period later. The host's `total_ms` and `horizon_ms` rest on these estimates.
- The phone has no desktop panel, no console and no controller emulation.

### Latency numbers

The host's stats line carries a `latency` block with the averages of the client's frame timing reports. The client reports for each frame that the streamed scene took: the tracking sample, the last packet's arrival, the end of the decode, the take by the headset's frame loop and the display time of the headset frame that shows the frame. The display time is the frame's own point on the headset's display grid, computed when the frame enters the scene's queue. A frame that the scene skips still reports one.

| Field | From, to |
|---|---|
| `total_ms` | the tracking sample to the display time |
| `pose_to_send_ms` | the tracking sample to the start of the host's send: the uplink, the wait for the planned start, the render and the encode |
| `network_ms` | the start of the send to the last packet |
| `decode_ms` | the last packet to the end of the decode |
| `decode_queue_ms` | the end of the decode to the take |
| `vsync_wait_ms` | the take to the display time |

The headset samples its poses at the start of a headset frame, 17 ms before the display time that the poses are predicted for. A frame shows at a point of the display grid. So `total_ms` of a frame is 17 ms plus a whole number of periods: 42 ms for three periods, 50 ms for four. Most frames of the test scene show after three. A frame shows after four when its pose reached the host too late for the plan's start, and after more when the pose never came. The mean falls when more frames make the three, which needs the host's time from the start to the end of the decode (about 14 ms at the median and 20 to 26 ms at the 97.5th percentile) plus the lead (16.3 ms) to fit between the pose's arrival and the point. `decode_queue_ms` is the time that a frame that made its point waits for the headset's take. A frame that starts earlier at the arrival of its pose waits longer and shows at the same point. The wait therefore says little about the latency.

The numbers below come from `scripts/e2e.sh --quest SERIAL --cranvr-client --native-port 9962 --seconds 30` on a Quest 3 over Wi-Fi at 120 Hz with the test scene. Each row is one run: the mean over its stats lines after the first. The load is the Mac's one-minute load average at the start and at the end of the run, with other programs running. `late` and `replaced` are the host's shares of frames that showed after their point and of frames that the headset replaced with a newer one. `shown` and `skipped` are the client's frames per second that the scene showed and skipped.

| Run | `total_ms` | `decode_queue_ms` | late | replaced | shown | skipped | load |
|---|---|---|---|---|---|---|---|
| 1 | 43.07 | 9.43 | 2.43% | 0.17% | 116.4 | 2.8 | 7.9, 6.9 |
| 2 | 43.44 | 9.72 | 1.90% | 0.24% | 117.6 | 2.0 | 7.4, 6.6 |
| 3 | 42.25 | 9.04 | 1.94% | 0.07% | 118.0 | 1.9 | 5.7, 4.5 |
| 4 | 43.84 | 9.90 | 2.14% | 0.14% | 118.2 | 1.9 | 12.0, 8.6 |
| 5 | 42.37 | 9.19 | 2.41% | 0.00% | 117.1 | 2.7 | 6.2, 5.9 |
| median | 43.07 | 9.43 | 2.14% | 0.14% | 117.6 | 2.0 | |

The median run has `pose_to_send_ms` 10.1, `network_ms` 3.6, `decode_ms` 3.6 and `vsync_wait_ms` 16.3. The round trip was 3.6 to 3.8 ms. A run with a worse Wi-Fi (a round trip above 5 ms) or on a loaded Mac shows more frames after four periods and a mean of 50 ms and more. The display lock's share of misses rests at about 2.4%.

The block also has the headset's frame rate, the video rate, the round trip, the loss, the parity share and the display lock's numbers. The client's stats panel shows the time from the tracking sample to the end of the decode and to the display. A headset that shows no stream (Room at Passthrough, no game) reports no timing, so the block is `null` then.

## Finding the Mac

The connect panel lists the Macs that run the cranvr host. The client broadcasts a query by UDP to port 9950 (`proto/src/discovery.rs`). It sends it to `255.255.255.255`, to the broadcast address of its own /24 network and to this machine. Each host answers the sender: the Mac's name, the TCP port of the session, the start of the host key's SHA-256 fingerprint and the protocol version that it speaks. The client lists the answers and forgets a Mac that has not answered for five seconds. A press on a Mac connects to the address that the answer came from, at the port in it. A query is longer than any answer, so a host cannot serve to amplify a spoofed query. The answer proves nothing: the handshake proves the host's key. Several hosts on one Mac share the UDP port (`SO_REUSEPORT`). `--discovery-port N` changes the port. 0 turns the answers off. A router that drops broadcasts leaves the list empty. The address field is the way around it.

## Pairing

A headset on another machine needs the Mac's user to allow it once. The client makes a key for itself (Ed25519) and keeps it in Cranpose's preferences service. The host makes a key at its first start and keeps it in `host-identity.txt` in the config folder (readable by the user alone).

1. The user presses the Mac in the headset's connect panel. The handshake proves the headset's key. The host does not know it, so it holds the connection and sends `AwaitApproval`.
2. Both sides compute a code of six digits. The headset's panel shows it with "Allow it on the Mac". The host lists the headset in its `native` status line, under `pending`, with the key, the name, the address and the code. The cranvr app shows a card: "Meta Quest 3 wants to connect. It shows the code 482 913." with Allow and Deny. The app also brings its window to the front.
3. The user compares the two codes and presses Allow. The app writes `allow KEY` to the host's input (`deny KEY` for Deny). The host writes the headset's public key to `paired-headsets.txt` in the config folder and starts the session on the held connection.
4. A later connection needs no answer. The host remembers the headset's key and the headset remembers the host's, under the address and under the fingerprint, so a new address of the Mac does not make a new pairing.

The host holds one request at a time: another unknown headset gets the reason `Busy`. A request ends with the answer, after three minutes (the headset hears `NoAnswer`) or when the headset closes the connection. A person who runs the host by hand with `--status` types `allow KEY` or `deny KEY` into its input. Without `--status` the host leaves its input alone, because a background job that reads its terminal would stop. `--trust-all` accepts every client without pairing, for tests. A client on this Mac connects without pairing. So does a headset over USB through `adb reverse`, because its connection comes from the loopback address.

Each connection proves both keys. A headset refuses a host whose key differs from the one that it remembers and says so in the connect panel. A new install of the host that lost `host-identity.txt` has a new key: press "Forget host key" in the connect panel (or remove the preference `host-key:ADDRESS`), then connect again.

### The code

The code protects the Mac. A machine that poses as the Mac to the headset and as the headset to the Mac holds two connections with two sets of keys. The two screens must show two different codes, so the user denies the headset. The code is the SHA-256 of the handshake's transcript (both preambles, both fresh keys and the host's key), the headset's key and two random numbers, one from each side, taken mod a million.

A short code alone would not hold: the machine in the middle could try a million fresh keys until the two codes agree. So the host commits first. `AwaitApproval` carries a hash of the host's number, the transcript and the headset's key. The headset answers with its own number (`PairNonce`). Then the host reveals its number (`PairReveal`). The headset checks it against the hash. Neither side can steer the code. A machine in the middle ends with equal codes with a chance of one in a million. `proto/tests/secure.rs` runs such a machine against a real host and client.

### Versions

The protocol has one version, `PROTOCOL_VERSION` in `proto/src/lib.rs`. cranvr has no public release, so it supports no other version. Each side sends a preamble first. The client's has six bytes: the magic `CRNV` and the version. The host's has seven: the magic, the version and whether the host takes a link over the USB cable (see "The USB cable"). A client reads the host's first six bytes, and the seventh only from a host of its own version. A peer of another version gets no session. The host answers with its own preamble and closes the connection. Each side then names the side to update, which is the side with the lower version: "Update cranvr on the Mac." or "Update the cranvr app on the headset." The host shows it in a `network` status line (`incompatible_client`). The discovery answer carries the host's version, so the connect panel shows the advice on the Mac's entry before a press. A change of a message, of the handshake or of a datagram raises the version. Both apps are built from one commit. Version 11 has the host's word on the cable in its preamble. Version 9 has the displays of the Mac ("Several displays"), the host's word that a game runs and the hands that a panel holds back from the game ("Panels in a game and the hands"). The first display has no messages of its own: every display, the first included, uses `DisplayWish`, `DisplayStream`, `DisplayMouse` and `DisplayCursor` with its stream.

## The USB cable

A headset on a USB cable at the Mac connects over the cable without a typed address. Two ways run over the cable. The headset's USB network needs no Developer Mode. `adb reverse` needs Developer Mode. The headset prefers the USB network when both answer.

**The USB network** (`client/src/usb_network.rs`). The headset app asks Horizon OS for a network of the USB transport without the internet (Cranpose's `request_network`). The headset then switches its USB functions to NCM (2833:5013 becomes 2833:5018; adb stays in the new functions). macOS makes an interface for it, such as `en10`, with an IPv6 link-local address. The headset sends the discovery query to `ff02::1` on its interface `usb0`. The host answers from its IPv6 responder, which joins no multicast group: macOS hands every datagram of its port to such a socket, also on an interface that appears after the host started. The host listens on `[::]:PORT` beside `0.0.0.0:PORT`. The headset binds its sockets to the network and connects to the Mac's link-local address with its scope. The media goes over UDP with parity. The client tells the host in its capabilities that the link runs over the cable (`ClientCaps::cable`, protocol version 10), so the host sends at the cable's bitrate. A headset that the Mac does not know pairs with a code once, as over Wi-Fi, because the Mac cannot tell a link-local peer on a cable from one on another network. The headset asks while it has the focus. Horizon OS keeps a released network for about 30 s before it switches the USB functions back. The first time, macOS asks "Allow accessory to connect?" for the new USB functions. The link waits for that answer. `--ez usb_network false` keeps the USB functions as they are for one launch.

Measured on a Quest 3 with a USB 2 cable (480 Mbps, `scripts/e2e.sh --quest SERIAL --cable usb-network|adb --native-port 9961 --seconds 30`, the host's test scene at 120 Hz, two runs of each way in turn):

| | USB network | `adb reverse` |
|---|---|---|
| fps (median) | 119.99, 119.96 | 119.87, 119.98 |
| late frames (median) | 0.17 %, 0.33 % | 2.33 %, 0.17 % |
| pose to display (median) | 32.9 ms, 33.0 ms | 34.2 ms, 33.0 ms |
| network time (median) | 1.23 ms, 1.20 ms | 1.96 ms, 1.96 ms |
| sent (median) | 34.6 Mbps, 33.6 Mbps (16 to 19 % of it parity) | 28.2 Mbps, 28.2 Mbps |
| media | UDP | TCP |

Both ways hold 120 fps. The USB network saves about 0.7 ms of network time. The late frames and the time from the pose to the display differ within the noise of two runs. A probe of the link moved 324 to 326 Mbps from the Mac to the headset over TCP.

**`adb reverse`.** The cable gives the stream TCP and a steady link. The Mac looks for the adb tool (the PATH, Android Studio's SDK, Homebrew's android-platform-tools). A Mac with none shows "Add USB support" in the Settings tab, which downloads Google's platform-tools into cranvr's folder. The Mac counts a device only when `dev.cranvr.client` or the phone build is installed on it. The app then sets up `adb reverse` for the host's port. The headset lists "This Mac by USB cable" first (`app/src/usb.rs`, `client/src/usb.rs`, `cranvr_proto::link::probe_host`). The Settings tab has a Connection choice: Automatic, Wi-Fi or USB cable. When the cable comes out, the headset connects to the same Mac over Wi-Fi by itself after about three seconds. A Wi-Fi link never moves to the cable while it runs.

**The choice of Wi-Fi** stops both ways over the cable. The app takes its `adb reverse` away and writes `cable off` to the host's input (`--no-cable` when it starts the host). The host's preamble then says that it takes no link over the cable, on its IPv4 and its IPv6 listener. The headset's question over the cable still gets the Mac's key with that answer. The headset lists no cable line for that Mac and connects to it over Wi-Fi: the key finds the Mac's Wi-Fi entry, also for a launch at `127.0.0.1`. A headset that comes over the cable anyway, because it asked a moment before the change, hears `WiFiOnly` and connects again over Wi-Fi. Automatic and USB cable write `cable on`. A change counts from the next connection: a running stream keeps its path. The headset keeps its request for the USB network, so the cable's way comes back without a new switch of its USB functions.

## The sealed session

The protocol encrypts and authenticates everything after the version exchange.

The handshake (`proto/src/secure.rs`, `proto/src/link.rs`) has three flights after the preambles:
1. The client sends a fresh X25519 key.
2. The host answers with its fresh key, its Ed25519 key and its signature over the transcript. The transcript hashes both preambles (a downgrade of the version breaks it), both fresh keys and the host's key. The client checks the signature and, when it remembers a key of this host, compares the keys.
3. The client sends its Ed25519 key and its signature over the same transcript, already sealed. The host verifies it. The signature covers the host's fresh key, so a recorded proof does not serve in another session.

HKDF-SHA256 turns the X25519 secret into four keys (salt: the transcript): the TCP path and the UDP path, each in two directions. ChaCha20-Poly1305 seals each packet. The nonce is four zero bytes and the packet's counter.

| Path | What is on the wire | Order and replay |
|---|---|---|
| TCP | 4-byte length, 8-byte counter, content, 16-byte tag. It carries the control messages. With TCP media (USB) it carries the media and the datagrams too. | The counter must be the next one. A record that fails to open, repeats or is missing ends the session. |
| UDP | 8-byte counter, content, 16-byte tag. Carries the media and the datagram messages. | A window of 4096 counters tolerates loss and reordering. A replayed or older packet and one that fails the tag are dropped like a lost one. |

The sender computes the parity over the plain packets and seals each packet on its own, so a lost packet stays recoverable. The sealed datagram keeps the 1400-byte limit, so the media payload of a packet is 1352 bytes. The host's UDP socket learns the client's address from the first datagram that opens with the session's key. The sealing hides the content. The sizes and the times of the packets and the addresses stay visible.

Sealing and opening a packet of 1400 bytes takes about 1.9 us on an M5 (0.75 GB/s). Sealing the frames of a stream of 100 Mbps at 120 Hz takes about 2 percent of one core. `cargo test --release -p cranvr-proto --test secure seal_cost -- --ignored --nocapture` measures it. The client's stats and its log line show the opening time per frame, per packet and as a share of one core.

## Display lock

The headset shows a new frame at each point of a grid, 120 times a second. Its compositor takes decoded frames some milliseconds before that point (about 16 ms on a Quest 3). A host that renders at the arrival of each pose has a fixed delay from pose to arrival. Jitter puts two frames into one interval and none into the next.

The host plans each frame of its own scene (`host/src/headset/display_lock.rs`). Each tracking sample carries the display time that its poses are predicted for. The times of successive samples give the host the headset's display grid. The compositor's reports give the lead (display time minus pick time) and how long a frame takes from its start to the end of its decode. For each frame the host
- picks the target, the grid point at which the frame should show;
- starts the frame so that its decode ends a margin before the pick of that point. The margin comes from the 90th percentile of the measured time and an extra that rises by 0.2 ms when a frame misses and falls by 0.005 ms while frames hit, so it rests at about 2.4 percent of misses;
- renders for the pose that is valid at the target, which it gets by moving the sample's pose with its velocities (at most 80 ms ahead);
- puts the target in the frame's metadata (`FrameMeta::target_display`, on the client's clock).

A planned frame starts at its planned time, or earlier at the arrival of a pose. The frame renders for the newest pose at its planned start. A pose that arrives less than a period minus half a millisecond before that start is the last one before it, because the next pose comes one period later. The frame then starts at once with the same pose and has the time up to the planned start as extra. The margin falls while the frames hit, so the plans move later until the misses return to their share. The wait for a planned start runs on a timer until 1.5 ms before the start and spins from there, because a timer wakes the thread up to a millisecond late. The time from a frame's start to the end of its decode is counted from the start that the frame took.

When no newer pose came since the last frame, a planned frame renders again for the same sample, with the pose moved on to the new target. Such a frame carries the sample's time plus one or two nanoseconds, so that the notes, the encoder and the headset's reports find it. At most two frames in a row repeat a sample.

The client passes the target through: `Hub::views_of` turns it into the runtime's clock and puts it in `StreamedViews::target_display_time`. The scene then shows each frame at its point. A frame that comes early waits in the queue. A frame that comes after the take for its point is late and is skipped. While a game runs, the game sets the pace and the lock does not plan the start of a frame. A frame that no plan started, such as a game's frame, takes the first point of the lock's grid at or after its arrival plus the lead (`DisplayLock::point_for`). `--no-display-lock` turns the lock off for a comparison.

The probe models the headset's queue (`probe/src/compositor.rs`: the display grid, the take one lead before each point and Cranpose's queue of three frames). Its report line shows how many frames the queue showed, skipped and repeated. It also shows how many frames came after their own point.

## The desktop panel and the sound

The client shows the Mac's displays as panels (see "Several displays") and plays the Mac's sound.

The stream of a display:
- The host captures the display at its full pixel size (ScreenCaptureKit, YUV 4:2:0, up to 60 pictures a second) and encodes it with a second HEVC session (`host/src/headset/native/desktop.rs`). The units of the first display's stream carry the stream id `DESKTOP_STREAM` (1) in the media header and share the media path with the eye stream. A further display has the next id. ScreenCaptureKit gives a picture only when the display changes, so a still desktop costs nothing.
- The encoder's time line is the real time since the capture started: VideoToolbox's rate control gives each picture the bits of the time since the one before. Before, the pictures stood 1 ms apart on it, so a picture got 750 bytes at 6 Mbps: the key frame of the 10 s interval came out at 23 KB and left the text unreadable. A display's frames use a quantizer of 18 at most while the last second's frames stay under 2.5 times the bitrate. VideoToolbox reads that cap only when a session starts, so a hard picture, such as a video that fills the display, moves the display to a session without the cap. Once the last second holds less than an eighth of the bitrate, a capped session starts again with a key frame of the newest picture. A key frame comes first and after a loss that the client reports, never on a timer. A display that stops changing gets its last picture again once, 250 ms later. `host/src/tests/display_quality.rs` decodes the stream of a made-up desktop and compares each picture with its source: the still text of the hand's display stays at 39 dB in every scene and is sharp again 0.9 s after a video that filled the display. With the old time line it fell to 12.6 dB.
- The client asks for a display's stream with `ClientMsg::DisplayWish` (a width and a rate). The host answers with `HostMsg::DisplayStream`: the picture's size in pixels, the display's size in points and the bitrate. The host hides no display for a game: the client decides which displays show (see "Several displays"). A host without `--desktop` offers no display and leaves the desktop in the eye stream. While the client owns the panels, the host leaves its own screen out of the eye stream. `--no-desktop-layer` stops the panels.
- The pictures are not mirrored. Cranpose's compositor layer turns a decoder's surface upright.
- The client shows the stream in a `SpatialExternalSurface` (mono). A second `VideoDecoder` in surface mode writes into it (`client/src/displays.rs`, `client/src/display_panel.rs`). The decoder has its own thread, so the eye stream's decode never waits for it. The decoder waits up to 30 ms for an input buffer: with Cranpose's default of 4 ms a busy Quest refused pictures. Each refused picture cost a key frame.
- A handle under the panel carries it. The thumbstick forward and back moves it away and near. Left and right resizes it. The client keeps the pose and the width in Cranpose's preferences service (`client/src/place.rs`).
- The controller acts as the mouse over the panel. Cranpose reports where the controller's ray meets the surface as `Enter`, `Move`, `Down`, `Up`, `ButtonDown`, `ButtonUp` and `Exit`. The client sends u and v on the picture, the mouse buttons that are down (left, right, middle) and the thumbstick's lean in two axes as `ClientDatagram::DisplayMouse` (with the display's stream) for each event that changes them and every 50 ms while the ray is on the panel. The Mac has one mouse: the client names the controller that owns it. The host posts real mouse events at the pointed place (`host/src/desktop.rs`): moves and drags with the button that is down, presses and releases of each button with the click count of a double click, and scroll events in pixels. A button that is down when the controller leaves the panel, or when the client falls silent for 250 ms, goes up. `CRANVR_POINTER_DRY_RUN=1` logs the mouse events and posts none. The host's stats line counts them (`mouse events: 120 moves, 12 downs (4 right, 4 middle), 12 ups, 1440 px scrolled, 600 px scrolled across`), and `scripts/e2e.sh --desktop` fails when the probe's script of presses and leans leaves one kind out.
- The Mac's cursor is part of the pictures. Its position also goes out as `HostDatagram::DisplayCursor`. `CRANVR_DESKTOP_CURSOR_IN_VIDEO=0` leaves the cursor out of the pictures.
- macOS asks for three permissions on first use. Screen Recording lets the host capture the display. System Audio Recording lets it capture the Mac's sound. Accessibility lets the controller move the mouse. Without Accessibility the panel still shows. The app asks for each missing permission when it starts. A card at the top of its window has a button for each.
- `--desktop-fps` sets how often the host captures the display (default 30). Each capture of a 6K display costs the window server about 3 ms of GPU time.


The displays and the eye stream share the Mac's video encoder:
- The Mac has one engine for HEVC encodes. Every session of every process takes turns on it. A display's picture of 3840 x 2160 holds the engine for about 9 ms, longer than an eye frame at 120 Hz takes (8.33 ms). An eye frame that comes while a display's picture encodes waits for it.
- A display's session runs out of real time (`EncoderConfig::real_time`). The eye stream's real-time session then goes first.
- The host's stats line gives the eye frames' encode time as a mean and a 95th percentile (`encode_p95_ms` in the status line).
- `host/src/tests/encoder_sharing.rs` runs the eye stream at 120 Hz (the level Medium for the Quest 3's eye of 1680 x 1760, a frame of 1984 x 928, 40 Mbps) beside a display at 40 Mbps through the host's own encoders, with nothing else on the encoder: `cargo test --release -p cranvr-host encoder_sharing -- --ignored --nocapture`. The table gives two runs of 20 s each on an M5, before and after the change. The display's desktop has a spinner and a line of text that change in every picture.

| Display beside the eye stream | Eye encode mean, ms | p95, ms | Eye frames over 8.33 ms | Before: mean, p95, over |
|---|---|---|---|---|
| none | 4.1 to 4.5 | 6.3 to 6.5 | 0 to 0.1 % | |
| 2560 x 1440 at 30, as in a game | 4.5 to 4.6 | 6.7 to 7.1 | 0.2 to 0.4 % | 4.3, 10.9 to 11.5, 8 to 11 % |
| 3840 x 2160 at 30 | 4.4 to 4.6 | 8.9 to 9.7 | 7 to 12 % | 4.1 to 5.2, 10.4 to 12.1, 12 to 17 % |
| 3840 x 2160 at 60, as with one display and no game | 5.7 to 7.3 | 11.1 to 11.7 | 20 to 41 % | 6.2 to 7.5, 12.9 to 13.6, 24 to 45 % |

- The level Medium does not slow the eye stream. The level Off encodes a frame of 2496 x 1308 and takes 7.2 ms (p95 13.6 ms) with no display.
- In a game the client asks for a display of 2560 x 1440 at 30 pictures a second (`client/src/display_wish.rs`). The eye stream then keeps its time. With one display and no game the client asks for 3840 x 2160 at 60, and a fifth to two fifths of the eye frames still take longer than 8.33 ms. The host then counts them as "encoder busy". With its Room setting at Passthrough the headset shows no eye stream while no game runs, so these frames are not seen.
- A run of `scripts/e2e.sh --desktop --probe-arg --display-fps --probe-arg N` measures the whole host with the probe as the headset. The probe asks for a larger eye (2064 x 2208, a frame of 2432 x 1152) and decodes both streams on the Mac. Its eye took 6.2 ms (p95 11.4 ms) with no display, 6.2 ms (p95 12.3 ms) beside the Mac's display at 22 pictures a second, and 8.9 ms (p95 26 ms) at 43 pictures a second, where the eye stream fell to 112 frames a second.

The sound:
- The host captures what this Mac plays with a Core Audio process tap, from every app but cranvr's own (`host/src/game_audio.rs`). The tap holds no screen stream. The sound came from ScreenCaptureKit before. Its stream on the main display made the window server work on every change of the screen, even with every display hidden (issue #118). It sends the sound as PCM: 48 kHz, 16 bits, stereo, in datagrams of 240 frames (5 ms, 960 bytes). That is 1.54 Mbps on top of a video stream of 40 to 100 Mbps. PCM adds no encode or decode time and needs no codec on the Quest. A datagram carries the host's clock for its first sample, so the client can tell how old the sound is.
- The capture takes the sound before the Mac's volume, so the Mac can stay muted while the headset plays it. It needs System Audio Recording, which the app asks for at start. The host starts the tap only when the permission is there, so a test host from a terminal shows no prompt: without the permission it sends no sound. "Game sound in the headset" in the settings turns it off. `--no-game-audio` does the same.
- The client plays it through Cranpose's audio backend (`client/src/audio.rs`). The renderer runs on the platform's output thread (AAudio, low-latency mode) and takes stereo frames from a ring. A jitter buffer between the network thread and the renderer plays after it holds 30 ms, keeps its fill near the target by changing the playback rate by at most 0.5%, drops its oldest sound when the fill passes 90 ms and leaves a gap of silence for a datagram that never came.
- The client logs the buffer's fill, the age of the sound when it was queued, the lost, late and dropped datagrams and the underruns every second. The stats panel shows them.
- The tap hands over each buffer of 10 ms about 1.1 ms after its newest sample (max 1.3 ms). ScreenCaptureKit handed over 20 ms about 16 ms after its newest sample (median, p95 27 ms). The client queues a datagram about 8 ms after the host's stamp of its first sample (13 ms with ScreenCaptureKit's larger buffers). The Quest's output then adds about 41 ms. The jitter buffer holds its target of 30 ms. The capture's clock ran 5 ppm fast against the Mac's clock (ScreenCaptureKit: 6 ppm), which the client's rate control absorbs. A rhythm game such as Beat Saber has an audio latency setting: set it once with its calibration.

### The microphone in games

A game under Wine records from the capture device "cranvr microphone", the default one of the prefix (patch 0008 in `third_party/wine`). "Microphone in games" in the app's settings chooses what the device records: the headset's microphone (the default), an input device of this Mac, or nothing. The app writes the choice into `microphone.txt` in the config dir. A game hears a new choice when it opens its microphone again. No host restart is needed.

- macOS adds a virtual input device only through an audio driver in `/Library/Audio/Plug-Ins/HAL`, which needs an administrator's install. A process tap of Core Audio needs the Audio Capture permission and adds a device that every app sees. Wine's CoreAudio driver is cranvr's own file already, so the device lives there: no install, and only games under cranvr's Wine see it.
- The host maps the ring `microphone.pcm` in the config dir: a header of 64 bytes, then 16384 frames of 16-bit samples, one channel at 48 kHz (`host/src/headset/native/microphone.rs`). Wine's driver maps the same file. It takes the frames that are due by the Mac's clock and stays 15 ms behind the newest one.
- While a game's stream of the device runs, the driver writes a heartbeat into the ring in each period. The host checks it every 100 ms. A heartbeat younger than 500 ms sends `HostMsg::Microphone(true)` to the headset, and an older one sends `false`. The headset's microphone is closed at all other times, so it costs nothing outside a game that records. A headset app that has no microphone says so in `ClientCaps::microphone`, and the host asks it nothing.
- The client records through Cranpose's `backend::open_microphone`: an AAudio input stream in low-latency mode on a Quest (`client/src/microphone.rs`). The preset is the one for speech recognition (see the numbers below). The client mixes the channels down to one, resamples to 48 kHz if needed and sends datagrams of 10 ms (`ClientDatagram::Microphone`, 480 frames of 16 bits: 0.77 Mbps, half the rate of the game sound). Each datagram carries the capture time of its first frame on the protocol's clock.
- The host writes each datagram into the ring at once. A lost datagram becomes silence of its length (100 ms at most), so the ring keeps the headset's time. A datagram that comes after a newer one is dropped.
- The app asks for Android's microphone permission when a game first records. The recording starts once the person allows it.
- With "a Mac input device", Wine records from it through Core Audio, as it records from any device of the Mac. macOS asks once whether cranvr may use the microphone. A device that is not connected records silence.
- `--no-microphone` turns the host's side off: the ring stays closed and the headset is never asked.

Measured on a Quest 3 at the Mac over the USB network, in runs of 8 s. The Mac's speakers played a burst of noise of 20 ms once a second at the Mac's low volume, and `microphone_check.exe` (patch 0008's test program) recorded under Wine with `--save`. A script found each burst as the first 2.5 ms whose level passed 3.5 times the room's noise (5 times for the call preset), and deleted the saved sound after:
- Speech preset, as shipped: the game got all 8 bursts 97 to 115 ms after the Mac played them (median 108 ms).
- Call preset: 7 of 8 bursts came 118 to 156 ms after. The eighth came at 46 ms, which is noise before the burst. Echo cancellation and gain control cost about 30 ms, so the client records with the speech preset. The launch extra `microphone_preset=communication` takes the call preset.
- A run with datagrams of 5 ms and a ring lag of 10 ms gave 96 to 113 ms for 5 of 8 bursts: no gain within the noise. cranvr keeps 10 ms and 15 ms.
- Most of the delay lies in the headset before the app gets the sound. A probe on the Mac that sent clicks through the host into the ring was heard by the game 2 to 16 ms later (runs of 3 s), and the network takes about 2 ms. The Mac's own output and the air take some milliseconds more. AAudio's timestamps gave 83 to 116 ms from the capture to the host in different runs. Android's unprocessed preset gives silence on the Quest 3.
- Recording costs the headset about 1 % of one core: the thread of Android's recorder appears only while a game records. The host's work is a check of the heartbeat ten times a second and a copy of 100 datagrams a second, within the noise of the stream's 5 to 13 %. The game's stream of the device took 0.5 % of a core under Wine.
- A game that starts to record waits for the headset's first sound: it got 110 to 150 ms of silence at first, and then 0 to 431 frames of silence in each 5 s (0.2 % at most).

## Several displays

The first version of the Cranpose client showed one display. Several displays existed before: the old Quest app (`quest/`, removed in #36) had an Add display button, and the host (`desktop_layer.rs`, `DisplayProcess`, `--display-helper`, removed in #52 with ALVR) made a virtual Mac display for each added panel, four at most. The native protocol had only one stream for the desktop, so the Cranpose client never had them. They are back, on cranvr's protocol:

- Protocol version 9 has one set of messages for every display, the first included: `ClientMsg::Display(Add | Remove { stream })`, `ClientMsg::DisplayWish { stream, wish }`, `HostMsg::Displays(list)`, `HostMsg::DisplayStream { stream, params }`, `HostMsg::GameRuns(bool)`, `ClientDatagram::DisplayMouse { stream, mouse }` and `HostDatagram::DisplayCursor { stream, cursor }`. The client's input travels in `ClientDatagram::InputHold`, with the hands that a panel holds back. The version 8 messages for the first display alone (`Desktop`, `Mouse`, `Cursor`) and the plain `Input` are gone, and so is the host's hiding of the display for a game. A display's stream is `DESKTOP_STREAM + index`, four at most.
- The host adds a virtual display of 1920 x 1080 points (HiDPI, 3840 x 2160 pixels) to the right of the Mac's displays, in a helper process (`cranvr-host --display-helper W H SERIAL`, `host/src/virtual_display.rs`). The helper arranges the display for the login session, because macOS undoes an arrangement for the app alone when the helper ends. The serial number of each display comes from the process's own number, so a test host beside the app's host does not make macOS take two displays for one. The display goes away when the client removes it, when the headset leaves or when the host ends. A display that the user removes in the system's settings closes its panel.
- Each display is one stream with its own capture, encoder, key frames, mouse and cursor (`host/src/headset/native/desktop.rs` for one display, `displays.rs` for the list, the add and the remove).

**A display that is not shown costs nothing.** The client wishes the displays that show and no other. The host runs no capture and no encoder for a display with no wish. It sends no picture and no cursor for it, and the headset runs no decoder and no surface for it. During a game the host keeps the video of a display that the client wishes: it tells the client that a game runs with `HostMsg::GameRuns` and hides nothing. The client owns the panels of the Mac's displays, so the host's own frame loop draws no screen into the eye stream and moves no mouse with a controller, also while the client shows no display. Without a headset the frame loop also stops its own capture of the desktop. Before issue #118 it ran from the host's start until the first headset came. WindowServer then worked on every change of the screen.

**What a shown display costs.** It costs the Mac's encoder and the headset's decoder. The client asks for less when more displays show or a game runs (`client/src/display_wish.rs`): one display in the room gets the full size at 60 pictures a second, two get the full size at 30, three or four get 2560 pixels across at 30, a display over a game gets 2560 at 30 (1920 when more than two show), and a display on the hand gets the width that its size needs at 30: the headset's pixels across the panel at 35 cm, at least 1920 (2560 for a panel wider than 55 cm). The host scales the bitrate with the pixels a second (40 Mbps at 3840 x 2160 and 60 a second, at least 6). The menu shows the cost of each display from the client's counts. In a test, three displays of 2560 pixels at 30 pictures a second ran beside the eye stream at 120 frames a second.

**The numbers in the log.** The host writes `Native displays: N known, M shown, a game runs; in the last 5 s: P pictures captured, Q encoded, R sent, S cursor datagrams sent`. The client writes `displays: N wished; in the last 5 s: P pictures received, Q sent to a decoder` and `layers composed by the app: ...`. A game with no display shown logs zeros in all of them.

**Tests.** `scripts/e2e.sh --displays 3` makes the probe add two displays, wish the video of all three, decode each stream, play the pointer script on each and remove the last display three seconds before the end. The run fails when a display gets no pictures or when the list does not shrink. It makes real virtual displays, which rearrange the Mac's displays for a moment: do not run it while the owner plays. macOS stalls the host's eye stream for about 3 seconds while it makes the displays, so run it with `--seconds 30`: the average of a 20 second run stays below the threshold of 115 frames a second, and the later windows run at 120. `--display-test` makes an added display show the main display again, which leaves the Mac's displays alone. A test host with `CRANVR_DISPLAY_TEST=main` and `CRANVR_DISPLAY_TEST_COUNT=4` starts each session with four such displays, for a headset on a desk that has no hand to press Add display. `scripts/e2e.sh --displays-in-game` runs the fake game: for 5 seconds the probe wishes no display, then it shows its displays for 5 seconds, then it hides them. The run fails unless the host's log shows 4 or more windows of one second with a game that runs, no display shown and nothing captured, encoded or sent, and the same after the wish ended, and unless the probe counted no unit and no picture in those parts. `scripts/e2e.sh --hold` makes the probe's right hand press a panel in every other window of its script (a trigger at 0.35 and the B button, for 0.5 s, with the press lasting 0.1 s longer than the hold). The fake game (`--expect-held`) fails when the game ever sees that trigger value or that button, and when the presses that belong to the game do not arrive. `scripts/e2e-flat.sh` runs the client in a flat window against a test host with the fake game (the window opens on this Mac's screen for the run). It fails unless the host's log shows at least 3 windows of a game that runs, no display shown and nothing captured, encoded or sent, and unless the client's last `layers composed by the app` line says `1 panels and 0 display surfaces` (the ball). A flat window runs no display decoder, so the same count on a headset reads `displays: 0 wished; ... 0 pictures received, 0 sent to a decoder` in the client's log.

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

## Games

Games need Wine with cranvr's DXMT fork and BasaltVR's OpenXR runtime. The app's Play tab has one button, "Set up games". It runs the steps that the Mac still lacks, in this order, with a line of progress for each step:

1. Rosetta 2, which runs Wine's Intel code. macOS asks for an administrator's password. The Play tab says so before the button is pressed.
2. Game support: the app downloads `cranvr-game-support-macos.tar.xz` (about 240 MB) from the public release at https://github.com/samoylenkodmitry/cranvr-game-support, checks its SHA-256, unpacks it and installs it with `setup-wine.sh`. It takes about 1.2 GB on disk. `scripts/install-game-support.sh [ARCHIVE|URL]` does the same without the app.
3. Steam for Windows. At the end the Steam window opens. The user signs in there once.

A failed step keeps its message on screen with a Retry button, which runs the steps that are not done. Games on another disk need no setting in cranvr: in the Steam window, Settings, Storage adds a folder on that disk. The Play tab lists the games of every Steam library folder.

The game support has a version, a date such as `2026.11.19`, in `scripts/game-support-version.txt`. `package-game-support.sh` puts it into the archive as `VERSION`. `setup-wine.sh` stamps it into `wine/game-support-version.txt` in the config folder after a good install. When the stamp is older than the app's version, or missing, the Play tab shows "Update game support". After a change of a runtime, DXMT, OpenComposite or Wine patch: set a new date in the version file, run `build-basaltvr.sh`, `build-opencomposite.sh`, `build-wine.sh`, `build-steamvr-driver.sh` and `package-game-support.sh`. Publish the archive, its `.sha256`, `cranvr-opencomposite-source.tar.xz` and `cranvr-wine-source.tar.xz` as a release of the public repository, tagged `v` plus the date.

To build game support from source instead (about an hour, mostly LLVM):

```bash
xcodebuild -downloadComponent MetalToolchain
softwareupdate --install-rosetta
brew install cmake meson ninja mingw-w64 bison
scripts/build-basaltvr.sh
scripts/build-opencomposite.sh
scripts/build-wine.sh
scripts/setup-wine.sh third_party/basaltvr/src
```

`build-basaltvr.sh` clones BasaltVR at a pinned commit into `third_party/basaltvr/src`, applies cranvr's patches and builds the OpenXR runtime and the DXMT fork. `build-opencomposite.sh` clones OpenComposite at a pinned commit into `third_party/opencomposite/src`, applies cranvr's patches and builds `vrclient_x64.dll` with the same MinGW-w64 toolchain (a minute). `build-wine.sh` clones Wine at the tag `wine-11.10` into `third_party/wine/src`, applies cranvr's patches and builds the eight files of Wine that the game support replaces (about two minutes, under Rosetta 2): `wined3d.dll`, which holds Wine's HLSL compiler, `winemac.so` and `opengl32.so`, which run an OpenGL game that asks for the compatibility profile, `wininet.dll`, `wtsapi32.dll`, which SteamVR asks for the lock state of the screen, `user32.dll`, which SteamVR's dashboard needs to start, `ntdll.so` and `winecoreaudio.so`, which gives games the microphone (see `third_party/wine/README.md`). `build-steamvr-driver.sh` builds cranvr's SteamVR driver into `target/steamvr-driver/cranvr`. `setup-wine.sh` puts Wine 11.10 with the DXMT fork in `~/Library/Application Support/cranvr/wine`, registers the runtime in the Wine prefix, installs OpenComposite as `C:\opencomposite` and the SteamVR driver as `C:\cranvr\steamvr`. It puts the patched files into the engine and the DLLs also into the prefix. It checks the runtime, OpenComposite, the HLSL compiler (it compiles a shader with `isnan`) and OpenGL (`gl_compat_check.exe` draws with GLSL 1.10 in the context that Google Earth VR asks for). `scripts/package-game-support.sh` packs the built trees into the archive, its `.sha256` and the sources of OpenComposite and of the patched Wine files in `target/game-support`.

Start the host first: it writes `runtime-control.txt` into its config dir. Then start a game from the window or with the script:

```bash
scripts/run-game.sh 'C:\basalt\runtime\d3d11_runtime_smoke.exe' --render-frames 7200 --visual
```

The smoke program draws a green marker at the left controller and a pale grid. A squeeze of the left grip grows the marker and sends a haptic pulse.

### Steam games

Steam runs in the prefix with BasaltVR's web helper shim (`scripts/setup-steam.sh`):

1. In the window's Play tab, press "Open Steam" (after "Set up games" it has opened Steam already). Log in. Install the game.
2. The tab lists the installed games. Press "Play" next to one: the window starts it by its .exe with the host's environment.
3. Without the window, start Steam with `scripts/run-game.sh --steam`. While Steam runs, start the game by its .exe: `scripts/run-game.sh 'C:\Program Files (x86)\Steam\steamapps\common\Beat Saber\Beat Saber.exe'`.

A rebuild of BasaltVR gives the web helper shim new bytes, and an update of the game support brings them to the prefix. A file check or an update of Steam puts Valve's helper back. The app puts the shim back by itself before each start of Steam and writes a line in the Log tab. Valve's helper then goes to the backup beside it, which the shim starts. The Play tab asks for "Set up games" only when the helper is missing, or when it is neither Valve's nor a shim with that backup. `scripts/run-game.sh --steam` does not repair the helper: run `scripts/setup-steam.sh` first when Steam's window stays black. Do not use `steam.exe -applaunch`: a game that Steam starts gets Steam's environment, not the one with the runtime's control file. For a game under `steamapps\common` the script and the window find the Steam app ID in Steam's `appmanifest` files and set `SteamAppId`, which the game's Steam check needs.

### Games from a folder

A program that Steam does not list, such as a copy of a Steam game with mods, joins the Play tab through "Add a game from a folder" in the card "Other Windows programs". Choose the program's .exe. The app saves the game in `app.json` (`added_games`: its name, which is its folder's name, and the program's Mac path) and lists it with Play, Options and Remove, as it lists a Steam game. The console in VR lists it by its name too. Remove takes the game out of the list with its options and deletes no file. A name that a Steam game or an earlier game uses gets a number.

cranvr reads the program's folder once, when the game is added, and fills in the launch option "Steam app ID" (`steam_app_id`) with its reason in the Options panel. The user can change it (a changed value loses its reason). The game's Steam check asks for the ID. cranvr takes it from `steam_appid.txt` next to the program, else from the Steam game that the folder is a copy of: a Steam game with the same program name whose folder holds at least four fifths of the files and folders of the original (and at least three). A copy has no manifest of its own. The app sets `SteamAppId` and `SteamGameId` from the option, and starts Steam first for a game that has an ID. `scripts/run-game.sh` reads the same option from `app.json`. `CRANVR_STEAM_APP_ID` sets it for one start. Without an option, a program under a Steam library's `steamapps\common` gets the ID of its install folder's manifest, as before.

cranvr sets no Wine DLL override for such a game. A copy of Beat Saber with BSIPA holds BSIPA's `winhttp.dll`, and the guides for Wine and Proton name `WINEDLLOVERRIDES=winhttp=n,b` for it. BSIPA has patched the game's own files, so it starts without the override. With the override it starts twice, and its log reports sharing violations on its own files. A mod loader that does not start without the override gets it from the user, in the field "Wine DLL overrides" of the Options panel.

The runtime speaks OpenXR with D3D11. A game that uses OpenVR runs through OpenComposite (see "OpenVR games"). A Steam game starts the program that Steam would start: the launch entry of the game in Steam's app cache, `appcache/appinfo.vdf` (binary KeyValues, version 29, `app/src/appinfo.rs`), with its arguments and its working folder. Of the entries for Windows it takes the first of type `vr`, then a plain one, then another choice of Steam's dialog. It leaves out entries for a 32-bit system, a beta branch, a DLC or another realm, and entries whose program is no `.exe` file in the game's folder. The Lab starts as `TheLab\win64\TheLab.exe -popupwindow`: the largest program of its folder is `SecretShop.exe`, one of its side experiences. An entry without a working folder starts the program in its own folder, as before; which folder Steam itself takes then is not measured. A game that Steam's cache does not know, such as a game added from a folder, starts the program that the app finds in its folder: three folders deep, preferring a 64-bit folder.

A game that the app or `scripts/run-game.sh` starts draws nothing into its Mac window: `DXMT_SKIP_PRESENT=1` (cranvr's DXMT patch 0008) sends each frame's GPU work without the present but tells the game that the present worked. `CRANVR_MAC_MIRROR=1` for the script draws the window again.

### OpenVR games

Most SteamVR games speak OpenVR. Each game's launch options choose its OpenVR runtime: OpenComposite (the default), SteamVR (see "SteamVR games") or none. The game support installs [OpenComposite](https://gitlab.com/znixian/OpenOVR) as `C:\opencomposite` (GPL 3, built from source by `scripts/build-opencomposite.sh`). It is an OpenVR runtime that makes OpenXR calls, here to BasaltVR's runtime, which is the active OpenXR runtime of the prefix.

A game finds OpenComposite through the prefix's OpenVR path file, `%LOCALAPPDATA%\openvr\openvrpaths.vrpath`. Valve's `openvr_api.dll`, which every OpenVR game carries, reads it and loads `bin\vrclient_x64.dll` from the runtime that it names. No file of a game changes. An OpenXR game ignores the file. Steam reads the file too: with SteamVR installed, Steam starts OpenVR as its VR watchdog about 8 s after its own start, from the runtime that the file names, and it writes its own paths into the file. So the file names a game's runtime only while the game runs (`app/src/openvr.rs`, `app/src/game_run.rs`). The app writes it when it starts a game and puts the file from before back when the game ends: when no process of the game's folder runs any more, so a launcher or a mod loader that starts the game again counts as the same run. While no game runs, the file names SteamVR when the Steam library has it, or no runtime: a Steam start puts one of them in place of a file that names OpenComposite. A game started with Off finds no runtime. `scripts/run-game.sh` follows the same rule: `CRANVR_OPENVR=opencomposite`, `steamvr` or `off` chooses for one start, else the setting `wine.openvr` of `app.json` (`opencomposite` by default), and `--steam` gets SteamVR or no runtime. A Steam start during a game's run still finds OpenComposite, as the Play button starts Steam and then the game: OpenComposite refuses Steam's watchdog with an error and no message box (its patch 0011).

The choice is OpenComposite by default and visible in four places: "OpenVR games" in the Settings tab and in the Wine card, a row "OpenVR" in each game's options with "As in Settings", "OpenComposite", "SteamVR" and "Off", and each game's row in the console in VR. A game's own choice is `openvr` in its entry of `launch` (`opencomposite`, `steamvr` or `off`), absent when it follows the setting.

A game that draws nothing through OpenComposite gets SteamVR (`app/src/game_run.rs`, `app/src/vr_system.rs`). A run of a game through OpenComposite watches the host's status lines: the game's sessions and its frames. It drew nothing when OpenComposite's log of the run names the game's folder, no frame reached the host and cranvr did not stop the game, and one of three holds. OpenComposite stopped the game (its log shows an abort: The Lab stops so with "Cannot create DX texture swap chain" and a message box on the Mac, and the verdict came 5.3 s after the Play button). The game closed its last session and opened none for 10 s while it kept running. The game ended by itself. The last two need a stats window with no game frame while the game held a session, as the host counts frames only while a headset streams. A game that follows the setting then gets SteamVR as its own choice, with the reason "cranvr picked SteamVR because the game drew nothing through OpenComposite.", which the Play tab shows. The window and the console show a notice. A game that OpenComposite stopped hangs in the message box, so the app ends it and starts it again through SteamVR at once. Any other game takes SteamVR at its next start. A game whose choice the user made keeps it, and the notice says to try SteamVR. Without SteamVR in the library nothing changes, and the notice says to install it. A change of the choice in the Play tab or in the console clears the reason.

What the port to Wine needed, in `third_party/opencomposite/patches` (see its README):

- OpenComposite builds for Windows with MSVC. The build here uses MinGW-w64 and Direct3D 11 only (no Vulkan: the runtime offers Direct3D 11). An OpenGL game's textures are copied into Direct3D 11 images (see below).
- MSVC and GCC pass the address of a returned struct and `this` in the other order. A virtual method such as `GetProjectionMatrix`, which an MSVC game calls, would write its result over the interface object. The generated interface classes pass the address as a parameter.
- The runtime offers images of the order B8G8R8A8 only, and many games draw R8G8B8A8. OpenComposite copies such a game's image into the runtime's image with a draw.
- The runtime offers the STAGE space, which is the LOCAL space with the floor at height 0 (runtime patch 0007). OpenComposite needs it for the standing space of OpenVR.
- A game that sets an action manifest (IVRInput) needs runtime patch 0008. OpenComposite creates its action sets once. It attaches them to its first OpenXR session, destroys that session when the game submits its first frame (it makes a new one on the game's own graphics device) and attaches the same sets to the new session. The runtime refused the second attach.
- OpenComposite builds the OpenXR bindings from the game's default bindings. It loads the file for the Touch controller (`oculus_touch`) when the manifest names one. Without it, OpenComposite takes the best of the other files, and its patch 0003 maps the Vive wand's trackpad to the Touch thumbstick. `scripts/e2e.sh --openvr-actions touch|vive|both` tests a manifest with each.
- The runtime offers `XR_FB_display_refresh_rate` with the headset's rate as the only rate (runtime patch 0009). OpenComposite answers the display frequency of the headset from it (its patch 0004). It answered 90 Hz before.
- The runtime's log lines (runtime patch 0010) say which actions a game binds and the first input that reaches each of them. They go to the game's stderr: the cranvr app keeps them in `game.err.log` of the config folder, and a Unity game keeps them in its own `Player.log`. Steam keeps no stderr of a game that it starts, so runtime patch 0011 also appends the lines to `game-input.log` in the config folder (beside `runtime-control.txt`; it keeps the lines of the last runs, up to 256 KB). OpenComposite writes its own log to `drive_c/users/<user>/AppData/Local/OpenComposite/logs/opencomposite.log` in the prefix. Its patch 0006 adds a line there when the source of a controller's pose changes (the runtime's grip pose, a held pose, the hand tracking fallback or no pose), and keeps a controller where it was for a moment when the runtime has no pose of it. Its patch 0007 gives the headset and each controller a serial number of its own: a game that tells its controllers apart by their serial numbers (Serious Sam VR does) took two controllers that both said `<unknown>` for one, and the right one never worked. SUPERHOT VR's `Player.log` shows 3 action sets with 50 actions, 17 of them bound for the Touch controllers (34 bindings), and the probe's right trigger at 0.75, A, left grip and the stick at 0.5 reaching the game's actions.
- OpenComposite runs scene applications only. For another application type it showed a message box, and Steam's VR watchdog (type 6) waited in that box until Steam's own watchdog ended Steam after 15 s. Its patch 0011 refuses such a type with `VRInitError_Init_InvalidApplicationType` and one log line. `openvr_smoke.exe --app-type watchdog` checks it after each install.
- An OpenGL game (Google Earth VR, built on Google's ION) needs four changes. macOS has no compatibility profile of OpenGL 3.2 and newer: Wine's Mac driver makes the core context for such a request (Wine patch 0002). The core profile does not compile GLSL 1.10 or GLSL ES 1.00, so Wine's `opengl32` gives such a shader a preamble of GLSL 1.50 (Wine patch 0003). The game loads its pictures with WinINet on several threads, and Wine's WinINet crashed when two threads closed handles at a time (Wine patch 0004). OpenComposite reads each eye of the game's OpenGL texture into a pixel buffer and copies it into the runtime's Direct3D 11 image one frame later (its patch 0009, 0.7 to 0.95 ms per eye). Earth VR makes 72 frames a second at 72 Hz in a test host. See `third_party/wine/README.md` and `third_party/opencomposite/README.md`.
- A game without an action manifest reads the legacy controller state and takes its clicks from the button events of that state (`VREvent_ButtonPress` and its kin). OpenComposite's OpenXR port sent none of them (its patch 0010 sends them). Google Earth VR started its welcome tour 4 seconds after a trigger press with the patch, and never in 13 presses without it.
- A game that sends a pulse with no amplitude in every frame (SUPERHOT VR stops the haptics of its hands about 130 times a second) cost the host and the headset a datagram each time. The host sends only the first stop after a pulse (`forwards_haptic` in `runtime_link.rs`); the probe reported 9398 of 9402 pulses without amplitude in one minute.

OpenComposite takes the hidden area mesh from `XR_KHR_visibility_mask`, which the runtime lacks, so it returns an empty one, as OpenVR allows. It leaves out overlays and the keyboard of OpenVR (OpenComposite patch 0008): the runtime takes no quad layer, so a loading screen that an OpenVR game shows as an overlay does not appear, and the game's frames go on. It supports games with 64-bit Direct3D 11 or OpenGL programs and 8-bit images. A 32-bit game, a Direct3D 12 or Vulkan game and a game with 10-bit or float images are not supported. An OpenGL game's frames pass through the CPU and reach the headset one frame later than a Direct3D 11 game's.

A game folder with OpenComposite's own `openvr_api.dll` or an `opencomposite.ini` from another setup bypasses the choice: the game loads that DLL, not the runtime in the prefix. Put Valve's file back. OpenComposite stops a game with a message box for an option that it does not know, such as a section header in the `.ini`.

### SteamVR games

A game that needs SteamVR itself (its menus, its overlays, its input bindings, or a check for SteamVR) runs in SteamVR for Windows under Wine with the launch option "OpenVR: SteamVR". SteamVR (app 250820, free) comes from the user's Steam library in the prefix. The game support installs cranvr's SteamVR driver as `C:\cranvr\steamvr` (`steamvr/driver_cranvr.cpp`, built with MinGW-w64 by `scripts/build-steamvr-driver.sh`). The OpenVR path file names SteamVR as the runtime and the driver's folder in `external_drivers`, so the game's `VR_Init` starts SteamVR, as on Windows, and SteamVR loads the driver. The game list leaves SteamVR out: it is a runtime, not a game.

The driver speaks the control protocol of BasaltVR's runtime with the host (`host/src/runtime_link.rs`), over the same control file (`BASALT_CONTROL`, which SteamVR inherits from the game):

- `IServerTrackedDeviceProvider` adds a headset and two Touch controllers. The headset has a display component (no lens: the identity distortion, the field of view and the eye distance of `VIEW_CONFIG`) and a direct mode component. The controllers use SteamVR's own Touch profile (`{oculus}/input/touch_profile.json`) and render model; their raw pose comes from the grip pose and the model's `openxr_grip` component. Buttons, axes, haptics (`/output/haptic`) and the headset's proximity follow the host's messages.
- The direct mode component gets SteamVR's layers for each frame. It acquires the keyed mutex of the compositor's sync texture, so its GPU waits for the compositor's draws, and draws the layers into three pairs of images that it shares with the host as IOSurfaces (DXMT's export), with no copy through the CPU. A thread of the driver waits for that GPU work and sends `FRAME_READY` with the host's timestamp of the head pose that SteamVR drew with. It sends SteamVR a vsync event each period of the headset's rate.
- After a crash of its server SteamVR turns the drivers off (`blocked_by_safe_mode` in `steamvr.vrsettings`). cranvr turns its driver on again before it starts a game with SteamVR, and says so in the Log tab.
- The first process whose layers reach a frame and that is not SteamVR's compositor is the game. The driver writes its program into `game-program.txt` beside the control file and connects again, so the host names the game. When the game ends, the driver asks SteamVR to quit (`VREvent_DriverRequestedQuit`): SteamVR starts with each game and ends with it.
- The driver sets `Prop_GraphicsAdapterLuid_Uint64` of the headset to the first DXGI adapter, where SteamVR's panels make their Direct3D devices.

What SteamVR needed under Wine and DXMT:

- Wine patch 0006: Wine answered no question about the session's lock state. SteamVR's compositor took the screen as locked, marked the head's pose of every frame invalid and faded the picture out.
- Wine patch 0007: Wine's `user32.dll` had no `DwmGetDxSharedSurface`. SteamVR's dashboard imports it through `vrdesktop.dll` and did not start without it, so the menu button opened nothing.
- DXMT patches 0013 to 0018 (see `third_party/basaltvr/README.md`): buffers and textures that processes share, a keyed mutex and the DXGI usage that Chromium's GPU process asks for, a desktop duplication that delivers no frame, a swapchain for a window of another process and textures that a process opened and shares again. SteamVR's compositor and its Chromium panels need them.

`scripts/e2e.sh --steamvr` runs `openvr_smoke.exe` through SteamVR and the driver and checks both eyes, the controllers' buttons and poses and a valid head pose in 90 percent of the frames.

The menu button of the left controller opens SteamVR's dashboard over the game, as on Windows.

### What the host and the app do for a game

Neither the host nor the app knows a game by name. A game's quirk is a launch option (`app/src/launch.rs`, shown in the Play tab), set by the user or by cranvr with a reason shown in the window.

- **Render size.** `--game-scale F` (default 0.7, "Game render size" in the Settings tab) makes a game render each eye at this share of the stream's eye size. The host keeps a render size per game. A game that stays below 95% of the headset's rate for 10 of the 12 stats windows in its first minute after loading gets a smaller share at its next start. The step aims 10% under the rate, with a floor of 40%. When a smaller size did not raise a game's rate by 5%, the host stops lowering it. The runtime names the game in `game-program.txt` and reads the shares from `game-sizes.txt` in the config folder. cranvr's SteamVR driver reads them too: it runs in SteamVR's server, which inherits `CRANVR_GAME`, the game's program, from the game that starts SteamVR. The Settings tab lists the learned sizes and forgets them. A game that makes fewer frames than the headset's rate adds 10 to 18 ms of latency. The host warns in the log 20 seconds after the game connects when the game stays below 95% of the rate.
- **Anti-aliasing.** Games under Wine use at most 2x anti-aliasing by default ("Game anti-aliasing" in the Settings tab, `CRANVR_MAX_MSAA` for `scripts/run-game.sh`, 0 for no limit). cranvr's DXMT patch then reports 4x and 8x as unsupported, so a game that checks picks 2x. It applies when a game starts. A game started from the Steam window gets it after Steam restarts.
- **Panel pixels.** While a game runs, a display in a scaled mode shows its panel's own pixels ("Panel pixels while a game runs" in the Settings tab, on by default, `--keep-display-modes` turns it off). A scaled mode, such as 3008 x 1692 points on a 3840 x 2160 TV, makes macOS draw every change at 6016 x 3384 pixels and shrink it. A helper process (`cranvr-host --unscale-displays`) sets each such display to the panel's pixels with at least as many points, so no window moves, for itself only. When the game disconnects, the host ends the helper. macOS then gives each display its mode back.
- **A black picture.** A game that renders both eyes in one pass into a swapchain with two slices can leave the second slice empty under DXMT. A game of another engine version (Beat Saber 1.44.1 on Unity 6000.0 with the OpenXR plugin 1.14.3) can leave both slices empty. The host finds the symptom (`host/src/eye_check.rs`): about once a second it reads a grid of 64 x 64 pixels from each eye of the game's newest frame. When the left eye has content (at least 8 lit samples) and no sample of the right eye is lit for 4 checks in a row, it sends one `black_eye` status line for the game run. The app answers in `app/src/black_eye.rs`: it turns on the launch option "Each eye in its own pass" for that program, restarts the game and shows a notice in the window and in the console. The option sets `BASALT_MAX_ARRAY_SIZE=1` (runtime patch 0003), so the runtime rejects two-slice swapchains and the game draws each eye into a swapchain of its own. The host also reports a picture that is black in both eyes for 90 checks in a row (`"eye": "both"`), because a game that loads shows a black picture for about 40 seconds before its first scene. The app answers both reports in the same way. A single lit sample in either eye ends the count. A flicker between states never counts. The host also logs the number of lit samples of each eye whenever the state changes ("Eye check: 88 of 256 samples lit in the left eye, 87 in the right eye"), which tells a black picture from a lit one in a test run. A host started with `CRANVR_EYE_DUMP=FOLDER` writes both eyes of the game's newest frame as `eye-left.png` and `eye-right.png` into the folder at each check, so a test run can look at what the game drew.

## A virtual display for the headset

This section describes the display that the Desktop tab makes at the start, which replaces the first display. The displays that the user adds from the headset's hand menu are made by helper processes and removed again (see "Several displays").

By default the panel in VR shows the Mac's main display as it is. The Desktop tab can add a display instead: one more monitor of this Mac that only the headset shows (`host/src/virtual_display.rs`). It is 1920 x 1080 points with HiDPI (3840 x 2160 pixels) at 60 Hz, or 2560 x 1440. macOS has no public call for this. The host uses CoreGraphics' CGVirtualDisplay classes through the Objective-C runtime. The host makes the display at start. The display goes away with the host.

1. Start cranvr. The virtual display appears to the right of your displays, as an extra screen. Your main display keeps its place and resolution. No window moves.
2. Connect the headset. The panel in VR shows the virtual display.
3. Drag a window onto the virtual display with the Mac's mouse, or point at it with the right controller. Type with the Mac's keyboard.

cranvr moves no window by itself. Two switches in the Desktop tab change that. Both are off by default:
- "Main display while the headset is on": at the first pose of the stream the virtual display becomes the main display. macOS then moves the menu bar, the Dock and the windows of the main display onto it. New windows open there too. When the headset leaves, every window goes back to its place and size. The change applies to the host process only (`kCGConfigureForAppOnly`), so macOS also undoes it when the host ends. `cranvr-host --desktop --virtual-display 1920x1080 --vr-main-display` does the same without the window.
- "Bring the cranvr window into VR": the cranvr window moves onto the virtual display while the headset streams. It moves back afterwards.

A cranvr window that sits on the virtual display when no headset streams moves to the main display: nobody would see it there.

## Test without a headset

```bash
cargo test --release
cargo test --release -p cranvr-app -- --ignored
cargo test --manifest-path client/Cargo.toml
./target/release/cranvr-host --config-dir /tmp/cranvr-test &
./target/release/cranvr-probe --seconds 10
./target/release/cranvr-probe --seconds 10 --tcp            # media over TCP, as over USB
./target/release/cranvr-probe --seconds 10 --drop-percent 3 # lets the parity recover packets
```

The probe is a client of the protocol on this Mac. It connects to port 9950, sends a head that turns 20 degrees a second and scripted controllers once per refresh, restores lost packets with the parity, decodes with VideoToolbox and checks the red and blue eye badges of the host's scene. It simulates a headset's display and reports the frame timing and the loss to the host. It exits with code 0 when at least half of the expected frames decode with the right eye colors. A client on this Mac needs no pairing. The ignored app test starts the host the way the window does and connects the probe. The client's ignored tests drive a real host with the client's connection code in a flat window.

The game path without Wine:

```bash
./target/release/cranvr-probe --game --seconds 14 &
./target/release/cranvr-fake-game --control /tmp/cranvr-test/runtime-control.txt --seconds 10
```

`cranvr-fake-game` speaks the runtime's protocol and shares IOSurfaces like the Wine runtime. Its eye badges have swapped colors, so `--game` tells its frames from the test scene.

## End-to-end test

`scripts/e2e.sh` runs the host, a client and an optional game, then writes a JSON report. The report holds every 5-second stats line of the host and the median and worst of each number: the frame rate, the render and encode times and the latency block (see "Latency numbers"). The script exits with 0 when the host and the client both reach 90 percent of the target rate after the first stats line. With the probe it must also show 95 percent of the frames that reached its decoder.

```bash
scripts/e2e.sh --fps 120 --game
scripts/e2e.sh --native-tcp --native-drop 3
scripts/e2e.sh --quest 2G0YC5ZH2000XX --native-port 9961 --seconds 30
scripts/e2e.sh --seconds 30 --wine 'C:\basalt\runtime\d3d11_runtime_smoke.exe' \
    --wine-arg --render-frames --wine-arg 14400 --wine-arg --visual
```

`--openvr-wine` runs `C:\opencomposite\openvr_smoke.exe`, a Direct3D 11 OpenVR program (source: `third_party/opencomposite/tests/openvr_smoke.cpp`), against the real runtime and the probe. It finds OpenComposite through the prefix's OpenVR path file, as a game does, and fails without it. It starts as an application of type Scene, reads the recommended size and the eye transforms (through the vtable in the order of MSVC's callers and through the FnTable), draws a different color in each eye and submits both eyes in every frame, reads the head pose and reads the legacy controller state that the probe scripts (left grip, right trigger at 0.75, button A). It also checks the eye distance. It asks the host for 72 Hz unless `--fps` says otherwise: it tests the path of an OpenVR program, not the rate. It also checks that the display frequency of the headset, as the runtime tells it to a game, is the rate of the host (`--expect-hz`). The program prints the interval of each frame, the frames of each second and the rate once the start has settled.

The frame rate of the program at 120 Hz had two causes. The first was OpenComposite's restart of the OpenXR session. A game restarts it twice at its start (at the first input call and at the first frame), and each restart slept for 250 ms between two looks at the session's state. The first second of a run held 47 to 50 frames instead of 120 (two frames of 300 ms). OpenComposite patch 0003 looks every 2 ms, and the first second holds 97 to 110 frames. Three interleaved runs of 20 seconds at a load average of 15 to 47 made 110.7, 113.4 and 116.0 frames a second with the older OpenComposite and 118.3, 118.5 and 118.7 with the new one. The second cause is the Mac's load. Another build that keeps the Mac busy (a load average above 20) stalls the program for 60 to 120 ms at a time, and `xrWaitFrame` of the runtime then skips a whole frame period. The second-by-second counts show 120 in the quiet seconds and 60 to 100 in the busy ones. The steady rate (seconds 3 to 19) was 119.1 to 119.6 with the new OpenComposite in those runs. This load is the host's neighbours, not a cost of OpenComposite: no steady-state cost of the OpenVR path showed up, and the runs on a loaded Mac differ from run to run. The OpenXR smoke does not print a frame rate to compare with.

`--openvr-overlay` keeps an overlay up for the whole run, as a loading screen does, and checks that the host still counts the frames of the program (OpenComposite leaves the overlay out, since the runtime takes no quad layer). `--openvr-actions touch|vive|both` runs the same program with IVRInput. It writes an action manifest with default bindings (for Touch controllers, for the Vive wand or for both, as a game ships them) to `%TEMP%`, sets it, updates the action set in each frame and reads the actions that the probe's scripted controllers drive: the grab (the left grip), the trigger's pull and click, the thumbstick or the trackpad, the A button (Touch bindings only) and both hands' pose actions, which must follow the probe's script (the left hand stays, the right hand swings, both at the height that the probe's floor gives). It pulses the left hand's haptic output when the grab fires, and the run fails when no pulse reaches the probe. The probe keeps its head still (`--still-head`), so the game's room is the probe's room. Set `CRANVR_HOME` to a stack that `setup-wine.sh` built with OpenComposite.

`--openvr-gl` runs the same program with OpenGL, as Google Earth VR draws: it asks Wine for an OpenGL 3.2 context with the compatibility profile, draws with a shader of GLSL 1.10 and submits one OpenGL texture with both eyes side by side. OpenComposite copies it into the runtime's Direct3D 11 images. The run checks the picture of each eye in the host as `--openvr-wine` does: a run made 72.0 frames a second at 72 Hz with a blue upper half in the left eye and an orange one in the right eye.

`--wine` runs a Windows OpenXR program in cranvr's Wine stack without sound. The probe then counts every frame of the right size, since a real program draws no eye badges. `--desktop` starts the host with its desktop panel. The probe then asks for the desktop stream, plays a pointer script (the host only logs its mouse events, as `CRANVR_POINTER_DRY_RUN=1` does) and counts the Mac's sound. It plays no sound: it measures what arrives. The run fails when the probe gets the Mac's sound for less than 80 percent of the stream time.

`--quest` starts cranvr's client on a Quest attached by USB (install it with `scripts/build-client.sh --install SERIAL`). adb keeps its display on without a face in the headset. The script pauses the guardian for the run. It sets the exact old value back at the end, an empty value included, also after a failure, Ctrl-C or a kill with TERM or HUP (`scripts/guardian.sh`). Only `kill -9` leaves the guardian paused. The Quest and the Mac must share a network. The host runs with `--trust-all` on `--native-port` (default 9950). A headset keeps one host key for each address and port. Pick a port that the headset has not used with another host at this Mac's address, such as 9961. The app gets this Mac's address and the room `scene` through its launch extras. The run keeps the host's key in `target/e2e/host-identity.txt`, because a headset refuses a known address with a new key. At the end the run starts the client again for the cranvr app's host on port 9950: at 127.0.0.1 when the app linked that port over the cable, else at this Mac's address. On a port other than 9950 the same launch makes the headset forget the key that it keeps for the run's address (`forget_host_key`), so the next test host at that address finds no old key. A run that ends without its cleanup, such as one killed with `kill -9`, still leaves its key behind. Before a Quest run, the harness reads the device's network over adb. It warns when a VPN on the device routes this Mac's address, when the device uses 2.4 GHz Wi-Fi and when its Wi-Fi link runs below 400 Mbps. Two runs with the same setting differ by up to 5 ms in total, so compare two settings with several pairs and keep every report. Check `uptime` before a run: other programs on the Mac lower the rates.

A phone or tablet runs the phone build of the client against a host on this Mac. Use `adb reverse tcp:9962 tcp:9962` and a host with `--native-port 9962`, or the Wi-Fi address of the Mac.

### Measure a game with a Quest on the desk

`scripts/perf/` holds the tools behind the game measurements.

```bash
scripts/perf/game-run.sh aa2 20
CRANVR_MAX_MSAA=0 scripts/perf/game-run.sh aa-game 20
HZ=240 scripts/perf/game-run.sh uncapped 20
scripts/perf/metal-trace.sh bs
python3 scripts/perf/gpu-by-kind.py target/perf/bs.xml "Beat Saber"
```

`game-run.sh` needs the cranvr app running and a headset that is paired with it. It wakes the headset without a wearer and starts cranvr's client and the game without sound. `CRANVR_HOST=ADDRESS:PORT` gives the client the Mac's address. Without it the client uses the address of its last connection. It prints one summary line of the rates and the latency stages. Then it sets the guardian back to its exact old value, also after a failure or a kill. The proximity sensor stays closed, so the headset stays awake. `metal-trace.sh` records 5 seconds of GPU work of every process on the Mac while a game runs. `gpu-by-pass.py` sums that work per process and per pass. `gpu-by-kind.py` sums it per kind of pass. `display-modes.swift` lists the modes of the external display or holds one for its own run.

### A hand for a test run

A test with nobody in the headset can press the buttons of a game's menu. `CRANVR_TEST_INPUT=FILE` makes the host replace the right controller that the client reports with the one that FILE describes. The host reads FILE about every 30 ms. The file holds `NAME=VALUE` words, such as `turn=100 yaw=12 pitch=25 trigger=1`:

- `yaw` and `pitch` (degrees) say where the controller points, from the direction that the head faces. A positive yaw turns to the left and a positive pitch turns up.
- `trigger` (0 to 1) is the pull. A pull of 0.7 or more clicks.
- `primary`, `secondary`, `stick_x` and `stick_y` press A and B and lean the stick. `stick_click` presses the stick down, the click that a game written for the Vive wand's trackpad reads.
- `grip` (0 to 1) is the grip's pull. A pull of 0.7 or more clicks.
- `x`, `y` and `z` place the grip in meters right, up and forward of the head (default 0.2, -0.35, 0.3).
- `turn` (degrees, positive to the left) turns the game's room about the vertical axis through the head. The game sees the head and both controllers turned. The headset and the probe still see the picture for the real pose. A game that puts its panels at the side of a headset on a desk then shows them in front of it. The turn changes only the poses that go to the game. The poses of the frames stay as the headset sent them.

`CRANVR_EYE_DUMP=FOLDER` writes both eyes of the game's newest frame once a second. `CRANVR_EYE_DUMP_WIDTH=1500` makes the files as wide as an eye (1444 pixels) instead of at most 640. A test reads the dump, finds a button in the picture, writes the angles of its direction into FILE and pulls the trigger for half a second. The pointer of a game's menu points a little higher than its controller: for the pictures of the probe's eye (a field of 90 degrees), the controller had to point 18.5 degrees above the direction of the button.

### The pages of the settings on a Quest

`cranvr --headless` runs the app with no window, for a test beside the user's app: `CRANVR_HOME` names its folder and `CRANVR_NATIVE_PORT` its port. It starts the host, answers the headset's requests as the window does and saves the settings in its own `app.json`. Each line of its input is a press of the window, as a request in JSON. It watches no USB cable, asks macOS for no permission and moves no window. `scripts/pages-check.sh SERIAL WORKDIR` runs it on port 9961 with a made-up game and the scripted build of the client (`scripted_pointer=pages`): the right controller opens the first page of the Mac's settings from the hand menu, changes a setting there and changes the made-up game's VR system on its Options page. At the end it presses two buttons of the window on the headset's own page (Show or Hide of the stream numbers, and Recenter). The client starts with `save_choices=false`. The script prints the entries of the test app's `app.json` that changed and the client's lines of the walk.

`scripts/ui3d-check.sh SERIAL WORKDIR [CAPTURES]` runs the walk of the 3D menus (`scripted_pointer=ui3d`) in the same way and takes a screen capture while the hand menu is open and while the console shows each page. `scripts/capture-check.py` checks each capture before anyone sees it: it keeps a picture only when at least 40% of its pixels are exact black and at most 15% are other grey pixels, so a picture of the room never stays. `scripts/ui3d-budget.sh SERIAL WORKDIR` measures the Flat and the 3D interface with the hand menu closed and open, with a test host that streams its scene at 120 Hz: the frame rate, the stale frames, the compositor's time, the app's GPU time and the layer count from the runtime's VrApi line (`scripts/vrapi-summary.py`).

## Other settings of the host

- The eye stream's frames stand on the encoder's time line at the real time between them, at most two frames apart after a pause. VideoToolbox's rate control gives a frame the bits of that time. Before, the frames stood 1 ms apart, so the stream sent 5 to 8 percent of its bitrate (issue #90).
- The link limits the bitrate that the user set: 40 Mbps at 120 Hz over the cable and 12 Mbps at 120 Hz over a network, the same bits a frame at other rates. The stream sends about 70 percent of the target, under the data rate limit of each second. On a Quest 3 over the cable a target of 100 Mbps made 3 to 7 percent of the frames late in bursts, and 40 kept them at 0.2 percent. Over a Wi-Fi 6 network late frames rose from 4 percent at the old 8.5 Mbps to 18 percent at 28 Mbps sent, so a network keeps the old rate until the host adapts the bitrate to the link (issue #97). `host/src/tests/eye_quality.rs` measures the quality on the Mac: a moving game picture at 120 Hz rose from 34.5 dB (8.5 Mbps) to 39.5 dB at the cable's limit and 43.4 dB at 70 Mbps. The window shows what the stream sends and the link's limit.
- The encoder overlaps up to two frames. A data rate limit over each second keeps the stream at or under the bitrate target. `--strict-rate-control` switches to VideoToolbox's low-latency rate control: even frame sizes, one frame at a time, slower per frame on an M5 (6.1 ms against 3.6 to 5.0).
- `--foveation off|low|medium|high` sets the level of the foveated encoding (see "Foveated encoding"). Off encodes every pixel at full density, with a smaller eye.
- `--pointer-pitch DEG` tilts the pointing ray down from the controller's handle (default -40). It applies to a client that sends no aim pose. A client's aim pose gives the ray as it is.
- A headset with Tailscale that accepts a subnet route for its own network sends its UDP to the Mac through the subnet router, which changes the sender address. The stream may then not arrive. The handshake still works over TCP, so the headset connects, gets no video and leaves after about 5 seconds, again and again. The host reports it after two such connections in a row. The fix is on the headset: exclude the cranvr app from the VPN, or stop using the subnet route there. The host also warns when this Mac reaches the headset through a VPN tunnel of its own. `--tcp` sends the media over TCP to every headset.

## Network exposure

- The host listens on TCP 9950 (`--native-port`) on all interfaces. A client that the host does not know waits for the user's Allow and gets no stream before. Everything after the handshake is sealed.
- The host answers discovery queries on UDP 9950 (`--discovery-port`) on all interfaces.
- The game link listens on 127.0.0.1 only. A game must show the token from `runtime-control.txt` (mode 0600).

Use pairing for real use. `--trust-all` accepts any client on the network and is meant for tests.

## Not tested yet

- An OpenVR game that draws a picture in the headset. The Direct3D 11 test program `openvr_smoke.exe` ran through the real runtime in a Quest 3 on a desk, with the picture of each eye checked by `adb exec-out screencap -p` (R8G8B8A8 and typeless B8G8R8A8 textures). Serious Sam VR (Steam app 465240) draws its own picture on the Mac: the host's eye check counted 213 of 256 lit samples in each eye at the menu. A copy of each eye (`CRANVR_EYE_DUMP`) shows its intro and its loading screen. Before the Wine patch (`third_party/wine`) the same game drew 0 lit samples and every copy was black. The game makes about 72 frames a second at 72 Hz in a test host. At 120 Hz it made 36 to 116 frames a second, 85 on average, which is below the headset's rate. The first start takes about 2.5 minutes while the game compiles about 1900 shaders and keeps them in the `Temp/ShaderCache` folder of its own install folder (a test of the Wine patch needs that folder empty). Its picture has not been seen in a headset yet: the Quest was off adb during the run, then at 8% battery. Google Earth VR (Steam app 348250) draws its picture in both eyes of a test host at 72 frames a second at 72 Hz, and its view turns with the probe's head (copies of each eye by `CRANVR_EYE_DUMP`). In a Quest 3 on a desk, with a test host at 72 Hz, it showed its welcome screen and its Touch controller in both eyes at 67 to 72 game frames a second. Its horizon lay as tilted as the headset on the desk. A trigger press of the host's test hand on "Click to start" started the welcome tour (OpenComposite patch 0010): the lit samples of each eye went from 53 to 100 percent, the mean brightness from 47 to 172. The game starts to load the Earth only after the head moves: the test turned the game's room for 30 seconds. With the probe's head perfectly still it draws no controller, also with a test hand that moves smoothly. Its OpenVR calls are the same as with a turning head (`logAllOpenVRCalls`). The headset on the desk, which is never perfectly still, was enough. A worn headset with controllers and the seated universe of OpenVR are untested. IVRInput action manifests run in the test program (`--openvr-actions`), not yet in a game.
- OpenComposite uses `XR_EXT_hand_tracking` when the runtime offers it, so the host logs "the game uses hands" for an OpenVR game and makes no controllers of the user's hands for it.
- The focus and pause of this version on a Quest 3: the Universal Menu over the app (`systemux://file-manager`), the display going off and a worn headset that is taken off. The probe and an Android XR emulator ran them, a Quest did not. A headset that stops the app's threads while it is `Hidden` would end the link after the host's 5 seconds of silence. The emulator kept its pings.
- The drawn keyboard of Cranpose on a Quest 3, and the pinch of a real hand on the panels. The hands rule (`hands.rs`) ran in the Android XR emulator with a scripted hand (`scripts/build-client.sh --scripted-hand --xr-store android-xr`) and the fake game that asks for hands: the connect panel, the stats panel and the ball were there before the game, gone while it asked, and back within a few seconds after it ended. The emulator reports the hand interaction profile for both hands, and the client's log says that it sends them as hands. A real hand's pinch has not been tried.
- The STAGE space (runtime patch 0007) is offered to every OpenXR game now. Beat Saber 1.46 (Unity's OpenXR plugin) was run with the probe as the headset, once with a runtime that lacks STAGE and once with one that offers it. Both runtimes had a trace of `xrCreateReferenceSpace`, `xrLocateViews` and `xrLocateSpace`. Without STAGE the game creates a LOCAL and a VIEW space and locates its views in LOCAL. With STAGE it creates a STAGE space too and locates its views in STAGE at first. Later in the run it makes a new LOCAL space and locates its views there. The head's height is the same in each case: 1.900 m for a probe whose head stands 1.6 m above its room's origin with the floor 0.3 m below that origin. The runtime keeps STAGE as a LOCAL space, so the two cannot differ in the runtime. How the game places its camera in the Floor mode that STAGE enables was not compared by picture.
- Hands, body, face and eyes from a worn headset. The tests run with the probe's script and the client's own tests. A real hand in view, a real face and the permission prompts of a user who answers them have not been tried. The face source of a worn Quest 3 and the face of a real Android XR headset have not been seen either.
- The cranvr client in a worn headset: the controllers in a game, the carry of the desktop panel by a ray, a recenter and the cable from a Quest.
- Beat Saber during a song. The game's frame rate and latency were measured on its first screen with the headset on a desk.
- A game that hides the desktop together with a recenter.
- A host that restarts during a stream with several clients one after the other.
- The controller as a mouse on the virtual display in a real headset.
- The hand menu, a display on the hand and the panels in a game with real controllers. A Quest 3 on a desk ran the scripted build (`--scripted-hand`) with a test host and the fake game: the menu's rows and buttons (a screen capture), a scripted hand's pinch on the ball (the menu opened and closed, and the host logged that the app held the right hand back), a display in the room (57 pictures a second at 3840 x 2160, 5.9 ms decode), a display on the hand (29 pictures a second at 1920 x 1080, 3.3 ms decode) and a display in a game (29.5 pictures a second at 2560 x 1440). The ball and the menu stood in a subspace that follows the head (`hand_anchor=view`). The ball above a tracked left controller, the press of the right controller's ray on it, and the sizes and places of the menu and of the hand's display in a worn headset are not tried. The hold of a real controller's trigger has run in the probe only.
- A display that the user adds from the menu in a worn headset. The probe added two real virtual displays (`scripts/e2e.sh --displays 3`), and the headset's menu has the button, but no press of it was tried.
- Sound with the Mac muted in a worn headset.
- The foveation levels in a worn headset or with a real game. The levels ran on a Quest 3 on a desk with the fake game and the host's scene. No click has pressed the window's buttons for the level or for the Connection choice. Tests and runs by hand sent the host's arguments and input lines that these buttons send.

## Next steps

`docs/plan.md` lists the work that the protocol allows. `docs/cranpose-xr.md` covers Cranpose apps for VR stores.

## License

MIT. The foveation math follows ALVR v20.14.1 (MIT, `third_party/licenses/ALVR-MIT.txt`). BasaltVR is MIT.

Game support also ships [OpenComposite](https://gitlab.com/znixian/OpenOVR) (GPL 3, copyright of its authors) as a separate program: the DLL `opencomposite/vrclient_x64.dll`, which games load as an OpenVR runtime and which calls the OpenXR runtime over the OpenXR interface. The GPL applies to it and to cranvr's patches of it in `third_party/opencomposite/` (the patches, `shim/` and `mingw-x86_64.cmake`). The rest of cranvr stays MIT. The archive carries OpenComposite's license and the licenses of its parts in `LICENSES/` and its source in `SOURCE/opencomposite-source.tar.xz`. The same file, `cranvr-opencomposite-source.tar.xz`, sits in each release of https://github.com/samoylenkodmitry/cranvr-game-support. It holds the pinned commit of OpenComposite (`cff07db75c4823afe93ed7027b03d5f7bc86f164` at https://gitlab.com/znixian/OpenOVR) with its two libraries, the patches and the build script, which rebuilds the DLL.
