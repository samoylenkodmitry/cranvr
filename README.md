# cranvr

cranvr streams VR from an Apple Silicon Mac to a Meta Quest. The Quest runs the stock ALVR client (the host's default, `--protocol alvr`) or cranvr's own Cranpose client (`--protocol native`, see "cranvr's Cranpose client"). The stock client can be any 20.x version, such as 20.14.0 from the Meta store or v20.14.1 from GitHub. ALVR compares only the major version. The Mac side is Rust. It uses ALVR's server core (MIT) for the network protocol, tracking and bitrate control. It renders and composes with Metal. It encodes HEVC with VideoToolbox.

The goal is to play Windows SteamVR games on a Quest from a Mac. Windows games run under Wine with the OpenXR runtime and the DXMT fork from [BasaltVR](https://github.com/Stooovie/BasaltVR) (MIT). The game's frames reach the host as shared IOSurfaces. No frame passes through the CPU.

## Status

Done and tested on one Mac with a headless ALVR client (`cranvr-probe`). Not tested on a real Quest yet.

- Milestone 1: a head-tracked test scene.
- Milestone 2: foveated encoding, compatible with the stock client. The host encodes about a third of the pixels; the client unwarps them.
- Milestone 3: Windows OpenXR games under Wine. A real D3D11 OpenXR program ran at 120 Hz. Head pose, controller pose, grip input and haptics passed through both ways.
- The Mac desktop in VR: the main display on a screen in front of you. The right controller works as a mouse.
- Milestone 4: the `cranvr` window, built with Cranpose.
- An end-to-end harness, `scripts/e2e.sh`, with a client on this Mac, on an Android phone or on a Quest. The host reports the client's latency by stage.
- Game sound in the headset: the host captures what the Mac plays with ScreenCaptureKit and sends it as ALVR's game audio.
- A notice in the window when the headset runs an ALVR app of another major version, such as a v21 nightly build.

Results on an M5 MacBook (release build, the probe on the same Mac), with the encoder's strict rate control of the time:

| Test | Refresh rate | Per eye | Host fps | Render or compose | Encode |
|---|---|---|---|---|---|
| Test scene, foveated | 120 Hz | 2048 × 2208 (encoded 1216 × 1152) | 119.6 | 3.2 ms | 5.9 ms |
| D3D11 OpenXR program under Wine, foveated | 120 Hz | 2048 × 2208 | 118.7 | 0.9 ms | 5.7 ms |
| D3D11 OpenXR program under Wine, foveated | 72 Hz | 1680 × 1760 | 71.7 | 1.2 ms | 5.9 ms |
| Test scene, no foveation | 120 Hz | 1248 × 1312 | 111 | 2.5 ms | 6.9 ms |

The host draws its own scenes as soon as a pose arrives. A frame budget that refills at the stream's refresh rate keeps the host from sending more frames than the headset shows. Two A/B pairs at 90 Hz against drawing at the next vsync gave 4.1 and 5.0 ms for the game stage, against 6.2 and 7.1 ms.

The host's encoder overlaps up to two frames. VideoToolbox's low-latency rate control, which the host used before, takes one frame at a time: a 2432 × 1152 frame then took 6.1 to 6.5 ms. Without it the same frame takes 3.5 to 3.9 ms, and the encoder kept up with 232 frames a second instead of 170. A data rate limit over each second keeps the stream at or under the bitrate target: without the limit the encoder sent 12 to 16 Mbps at an 8 Mbps target. `--strict-rate-control` brings back the old mode for A/B runs on Wi-Fi. The default refresh rate is 120 Hz; a headset without 120 Hz gets its closest rate.

| Run, 120 Hz, 100 Mbps | Host fps | Encode | Motion to display | Frames shown |
|---|---|---|---|---|
| Mac, probe over loopback | 118.9 | 3.5 ms | 9.6 ms | 100% |
| Galaxy Tab S7+ over USB | 120.0 | 3.4 ms | 34.4 ms | 95.5% |
| Tab, strict rate control (old) | 117.7 | 6.5 ms | 36.4 ms | 96.9% |
| Tab over USB, D3D11 OpenXR program under Wine | 119.7 | 5.1 ms | 34.2 ms | 97.2% |

Game sound does not slow the stream: a Tab run at 120 Hz with the sound on kept 119.5 fps and showed 97.2% of the frames. The probe got 20.5 seconds of sound in a 20-second run.

Above 120 Hz, which a Quest 3 cannot show, the probe measures the room left: on the Mac at 240 Hz the host sent 216 frames a second with a 3.6 ms encode and 9.1 ms from the pose to the display. The probe's pose timing limited it there, not the encoder. On the Tab at 165 Hz it sent 159.7 with 27.5 ms from the pose to the display. The Tab's own screen runs at 120 Hz, so its 165 Hz display is the probe's simulated one.

Foveated encoding is on by default. It makes the full Quest 3 eye size fit at 120 Hz. Other programs on the Mac lower these numbers: check `uptime` before you trust a measurement.

## Parts

| Folder | What it does |
|---|---|
| `vt` | HEVC encoder and decoder through plain C bindings to VideoToolbox. |
| `common` | ALVR's foveation math, for the host and the probe. |
| `proto` | `cranvr-proto`: cranvr's own protocol for native mode, with no ALVR code. It builds for macOS and for Android. |
| `host` | `cranvr-host`: the ALVR server core, the frame loop, the Metal shaders (`host/src/pattern.metal`), the game link (`runtime_link.rs`) and the desktop (`desktop.rs`). |
| `app` | `cranvr`: the window. It runs `cranvr-host` and shows its status. |
| `probe` | `cranvr-probe` (a headless client of ALVR's protocol or of the native one) and `cranvr-fake-game` (a stand-in for a game), for tests. |
| `client` | `cranvr-client`: cranvr's Cranpose client for the Quest and for Android phones, the client of native mode. Its own Cargo workspace. |
| `scripts` | Build and run the Wine side for games. |
| `third_party/basaltvr` | cranvr's patches to BasaltVR's runtime and DXMT fork. |

## Build

```bash
cargo build --release
```

The first build downloads ALVR v20.14.1 from GitHub with its submodules.

## Run with a Quest

The host speaks two protocols. `--protocol alvr` is the default. It serves the stock ALVR client, from the Meta store or from ALVR's releases. `--protocol native` serves cranvr's own client, which "cranvr's Cranpose client" below describes. The window starts the host in ALVR mode while "Headset app" in its Settings tab says "ALVR client" (the default). It starts the host in native mode for "cranvr client". These steps use the stock ALVR client:

1. Install the ALVR client on the Quest: 20.14 from the Meta store, or `alvr_client_android.apk` of v20.14.1 from the ALVR GitHub releases. Its major version (20) must match the server core's. A v21 nightly build does not connect. The window then shows a notice.
2. Put the Mac and the Quest on the same network. Use 5 GHz or 6 GHz Wi-Fi. Connect the Mac by cable if you can.
3. Start the window:

   ```bash
   ./target/release/cranvr
   ```

4. Press "Start host". Open ALVR on the Quest. The Quest's hostname (such as `1234.client`) appears under "Headset". Press "Pair".
5. On the first connection the client shows "The streamer is restarting" once. It reconnects by itself.

### cranvr's Cranpose client

`client/` holds the client of native mode. It is a Cranpose app. It uses only Cranpose's public API and plain Rust crates such as `cranvr-proto`. It has no ndk, JNI, GL, OpenXR or platform code. Its Android application id is `dev.cranvr.client2`, so it installs beside the ALVR app.

```bash
scripts/build-client.sh --install
cargo run --manifest-path client/Cargo.toml -- --host=127.0.0.1:9950 --connect
```

The host serves it with `cranvr-host --protocol native --pair CODE`, where `CODE` is the six digits that the connect panel shows (see "Pairing" below). A client on the Mac itself needs no pairing. The host foveates its pictures by default, as ALVR does: it keeps the full pixel density in the middle of each eye and compresses the edges, so the encoder codes about a third of the pixels. The headset undoes the compression: the streamed scene unwarps the frame in its shader (`StreamedView::foveation`). The compositor receives each eye's full view. The client builds the layout from the host's parameters (`Foveation::new` in `common/`, the code that the host's encoder runs). The decoded frame of a Quest 3 eye of 1680 by 1760 pixels is 1984 by 928 pixels. `--no-foveation` turns the foveation off, for comparisons.

A faint dotted line showed near the top of both eyes along thin lines of the test scene on a Quest 3 (`quest-foveated.png` of 17 November 2026). The host's compression caused it, and the client's unwarp did not. The host's shader took one sample of the scene for each encoded pixel. At the edges one encoded pixel stands for 4 by 5 pixels of the full view, so a line of a few pixels was in some encoded pixels and missed by the others. The decoded frame already held the line as a row of dashes before the client touched it. A CPU unwarp of that frame with bilinear filtering (what the client's shader does) gave the dots again. The shader now filters: the test scene widens a line that is thinner than the pixel's footprint and dims it by the same share, and `compose` (a game's frames) averages up to 3 by 3 bilinear taps in a compressed edge pixel (`compression` and `MAX_TAPS` in `host/src/pattern.metal`). A test paints a line of one pixel across the edge of a game's image and follows it through the encoded frame (`a_thin_game_line_stays_continuous_in_the_compressed_edge` in `host/src/render.rs`). With one sample for each pixel the line is missing in some columns. With the taps its least brightness is 39 percent of its mean brightness above the background. The test scene's line varied by 24 percent along its length before and varies by 11 percent now. `compose` costs 0.87 ms of GPU time for a frame of 2432 by 1152 pixels on an M5, where it cost 0.44 ms. The same line on the Quest after the change is continuous.

The client asks for the headset's recommended eye size. The host takes it unless `--eye-width` says otherwise. The decoder has limits. The client asks Cranpose for them (`VideoDecoder::capabilities`) before the Hello. It asks for the recommended size unless the frame that the host would encode lies beyond the decoder's largest picture or beyond its frame rate at the refresh rate. It then takes a smaller eye, down to half the recommended size. After the Welcome it checks the frame that the host really chose and logs a warning. On a Quest 3 the decoder (`c2.qti.hevc.decoder`) reports pictures up to 8192 by 8192 pixels, so the recommended size stays.

A client that takes no foveation gets a smaller eye: 1248 pixels at 120 Hz, 1504 at 90 Hz and 1728 at 72 Hz, unless `--eye-width` says otherwise. The width of 2064 belongs to a foveated eye. Without that rule the host sent such a client a picture of 4128 by 2162 pixels. The host's encoder took 10.5 ms for each frame of that size, so the host sent 85 to 95 frames a second. The Quest 3 also showed green blocks in part of the picture. Its decoder advertises HEVC level 6.2, so the cause of the green blocks is not known. A Quest that is not on the Mac's network connects over USB: `adb reverse tcp:9950 tcp:9950`. The address is then `127.0.0.1:9950`. A loopback address makes the client ask for the media over TCP.

The app has four parts. In a headset the panels are movable panels in the room. On the desktop they draw side by side in a flat window.
- The connect panel holds the host address, a keypad for it, the Connect button and the state of the link. Cranpose's preferences service remembers the address.
- The stream is a `StreamedScene`. Cranpose's `VideoDecoder` outputs frames (the decoder keeps 6 pictures: the scene queues 3. The frame shown last holds one. The decoder writes one). The scene keeps up to three decoded frames in a queue. At each headset frame it shows the frame whose target display time lies closest to the display time of that headset frame. It shows the picture again while no waiting frame is closer. The headset's compositor shows each frame from the pose that the host rendered it for and corrects the picture for the head's newer pose. The client takes the render poses and the target display time from the frame's metadata (`FrameMeta::eye_poses` and `target_display`). It takes the field of view from the views that it last sent to the host. Each eye's picture is one half of the frame. The right eye of a foveated frame is mirrored.
- The stats panel shows the frame rate, the decode time, the loss, the FEC recoveries and the latency. It also shows what the scene did with the decoded frames: how many it showed a second, how many it skipped (a closer or newer frame took their place) and how many headset frames showed the last picture again (`StreamStats::counts`).
- A `SpatialFrameEffect` runs once for each headset frame. It hands the head pose, the eye views, both controllers and the predicted display time to the thread that sends the tracking.

The launch arguments `host`, `connect` and `media` (`udp` or `tcp`) set the address, start the connection and choose the media path. On Android they are intent extras: `adb shell am start -n dev.cranvr.client2/dev.cranpose.android.CranposeActivity --es host 192.168.1.20:9950 --ez connect true`. On the desktop they are `--host=ADDRESS`, `--connect` and `--media=udp`.

#### Phone mode

`scripts/build-client.sh --phone [--install SERIAL]` builds `target/client/cranvr-phone.apk`. The APK holds the same crate as a flat Android app for a phone or a tablet that has no OpenXR runtime. Its application id is `dev.cranvr.client2.phone` and its label is "cranvr phone". It installs beside the headset's APK.

The build differs from the headset's in four places.
- The Cargo feature `phone` replaces `android`. The entry is `cranpose::android_main!`. The headset's APK keeps `cranpose_xr::android_main!`.
- The build has no OpenXR loader and no `xr` service. `build.rs` declares `Use::device_motion()`.
- The Gradle property `cranvrPhone=true` switches the module (`client/android/app/build.gradle.kts`). The script sets it.
- The phone's manifest (`client/android/app/src/phone/AndroidManifest.xml`) turns the activity to landscape with the sensor. The headset's manifest stays as it was.

The launch arguments are the headset app's: `adb shell am start -n dev.cranvr.client2.phone/dev.cranpose.android.CranposeActivity --es host 192.168.1.20:9950 --ez connect true`. The command `cargo run --manifest-path client/Cargo.toml -- --phone --host=127.0.0.1:9950 --connect` runs the phone's app in a desktop window. That window has no motion sensor, so its head looks straight ahead.

`PhoneApp` in `client/src/ui.rs` is the app.
- The picture fills the screen. A tap on the picture opens the connect panel and the stats panel over it. They stand side by side and scroll. They hold the bodies of the headset's two panels (`ConnectBody` and `StatsBody`). The connect panel has no keypad, because the phone has a keyboard. The Hide panels button closes them. They open again when the stream ends. The Recenter button puts the host's front where the screen points. The screen stays on while the stream runs (`set_keep_screen_on`).
- Cranpose's `StreamedScene` draws nothing outside a headset at this revision. The function `TemporaryFlatPicture` in `ui.rs` draws the decoder's whole frame with `PlayerSurface`. Both eyes show side by side, scaled to fit. Delete the function and its call when `StreamedScene` has its flat mode (the branch `flat-streamed-scene` of Cranpose).
- The head comes from `latest_device_motion()`. `client/src/phone.rs` reads it once for each frame of the screen. The screen frame (`Quaternion::in_screen_frame` with the display's rotation) is a head frame. One quarter turn about x takes the sensors' z-up frame to the host's room. The first look along the horizon becomes the host's front. The head stays at a height of 1.6 m. Two sensor samples give the angular velocity. The pose that goes out lies ahead at the display time. Each screen frame wakes the sender, so the host gets one tracking sample for each refresh.
- The Hello carries one view shape for both eyes. It spans 90 degrees across the screen's long side. The screen's aspect gives the other side. Both eyes sit at the head's center. The host renders two eyes as usual and the phone shows the left one. The eye size has the screen's aspect. The host picks the width (1248 pixels at 120 Hz without foveation). Cranpose has no public call for the display's refresh rate, so the client measures it. The rate comes from the lower quartile of the frame clock's intervals in the first seconds, snapped to the nearest standard rate. The Hello asks for no foveation. It waits for the screen's size and rate, as the headset's Hello waits for the eye views.
- The decoder, the sound and the connection are the headset's. The phone has no desktop panel, no console and no controller emulation. A host that runs with `--desktop` still puts the Mac's screen into the scene of the eye stream, because the phone never asks for the desktop panel. The phone sees that screen as part of the picture. It has no pointer for it.
- A phone's scene shows the newest frame. Cranpose reports nothing about it. The client sends the frame timing when the frame arrives. The picture is ready one mean decode time after the last packet. The next screen frame takes it. That frame reaches the display one period later. The host's display lock plans with these estimates. The host's `total_ms` and `horizon_ms` rest on them.
- The log has a `phone head:` line every 2 seconds. It shows the orientation that the client sent and the orientation for which the host rendered the newest frame. The stats panel shows the direction of each. A tablet that lies still sends the orientation that the frames come back with.

On 18 November 2026 a Galaxy Tab S7+ ran against a copy of the main checkout's host binary. The Tab ran DeX with a window of 1618 by 1234 pixels. DeX held the display at 60 Hz. The host ran with `--protocol native --no-game-audio` and showed its test scene. The eyes were 1248 by 952 pixels at 60 Hz, HEVC without foveation. The numbers are the means of 2 s intervals after 10 s of warm-up. The "shown" rate of the picture is not counted: the decoder's `dropped` counter says how many decoded pictures the picture did not take.
- Over USB (`adb reverse`, TCP media): 21 intervals. The client received 59.7 frames a second and decoded 60.0. The screen ran at 61.0 frames a second. The decoder dropped 0.5 pictures a second. Decode took 4.3 ms. The round trip took 1.6 ms. The network took 1.8 ms. The stream used 4.1 Mbps. The time from the tracking sample to the end of the decode was 35 ms.
- Over Wi-Fi (192.168.50.44, UDP media, Tailscale paused for the run): 22 intervals. The client received 59.3 frames a second and decoded 59.5. The screen ran at 61.4 frames a second. The decoder dropped about 4 pictures a second. Decode took 4.5 ms. The round trip took 4.0 ms. The network took 7.7 ms. Of 30,632 packets 77 were lost. One unit was recovered and 2 were lost. The time from the sample to the end of the decode was 43 ms.
- The tablet leaned on the desk and looked 44 degrees below the horizon. In each 2 s log line it sent the orientation (-0.372, 0.002, 0.004, 0.928). The host rendered the newest frame for the same orientation each time. The screenshot showed the checker floor from that angle. With `--desktop` the Mac's screen showed in the scene and the stream ran at the same rates.

Not tested:
- a moving head (nothing turned the tablet) and the yaw direction on a device
- Recenter, a reconnect and a phone in portrait
- a window in full screen mode and a 120 Hz display
- the sound and a game
- the headset's APK in a headset after the change to the shared panels

#### Controllers in games

The host feeds BasaltVR's runtime (`host/src/runtime_link.rs`) from the client's `Tracking` and `Input` messages in the same way as from ALVR's. For each tracking sample it sends the head pose, the grip pose and the aim pose of each controller that the headset tracks with the velocities of both, and the mask of tracked controllers. It sends every change of a button, a touch, the trigger, the grip and the thumbstick with the path ids that ALVR gives them, for the Touch profile. The runtime's haptic requests go back to the client as `Haptics` datagrams, which the client plays through Cranpose's `Haptics`.

Each controller has two poses. The grip pose is the place of the hand that holds the controller. The aim pose is where the controller points. Cranpose's `Controller` carries both (`grip` and `aim`). The client sends both as the headset reports them, each with its own velocities and with no offset (`left_grip`, `right_grip`, `left_aim` and `right_aim` of `Tracking`, protocol version 3: a version 2 client and a version 3 host refuse each other). The host adds only the floor's height. It hands both poses to the runtime in one control message for each hand, `CONTROLLER_MOTION_AIM` (cranvr's runtime patch 0004, in `third_party/basaltvr/patches/runtime`). The runtime gives a game the aim pose for an action space on `/input/aim/pose` and the grip pose for one on `/input/grip/pose`. A hand that sends no aim pose has its aim space at the grip pose, as it has with ALVR.

A runtime without patch 0004 closes the connection at a message that it does not know. The patched runtime therefore writes `runtime-features.txt` next to the control file when its session starts, as it writes `game-program.txt`. The host sends the new message only to a runtime that wrote the file. An older runtime gets the grip pose alone. The host then logs a warning that the game finds its aim space at the grip pose. The runtime's control protocol keeps its version 9.

Native mode no longer adds ALVR's Quest offset to the grip pose. With ALVR the client core applies that offset (15 degrees about the x axis and a shift of 5 mm, mirrored for the right hand: `get_controller_offset` in `alvr/client_openxr/src/interaction.rs` of ALVR v20.14.1). The server core then shifts each hand 11 cm along the controller's axis (`left_controller_position_offset` 0, 0, -0.11 in the session with ALVR's default controller settings). It adds the lever arm to the linear velocity. It drops linear velocities below 0.05 m/s and angular velocities below 10 degrees a second. It turns the angular velocity into the controller's frame. ALVR mode stays as it is: its single pose serves as the grip pose and as the aim pose. The native path hands the runtime the headset's own poses and velocities, so a game's hands sit where the headset reports them. A game that sat well with ALVR's shift sits 11 cm differently in native mode. The host's own desktop pointer takes its ray from the aim pose in native mode. `--pointer-pitch` tilts the grip pose only for a client that sends no aim pose.

The host's stats line shows what reached the game in each 5 seconds: `Game input: 601 left and 601 right controller poses (tracked mask 0b11), 21 button and axis values, 2 haptic pulses from the game`. `scripts/e2e.sh --native --game` runs a fake game (`cranvr-fake-game`) that checks the input of the probe's script: both hands tracked, each hand with the grip pose and the aim pose that the probe scripted (`probe/src/hands.rs`: the two hands and the two kinds of pose differ in every number, so a swap shows), the right hand moving, a pull of the right trigger, a press of A, the right stick and the left grip. The fake game reads the runtime's control messages. It does not call `xrLocateSpace`, so this run checks the way from the probe through the host to the runtime's control channel. The runtime's own smoke test (`d3d11_runtime_smoke`, patched with the series) locates an aim space and a grip space of one hand and checks both. The game answers a pull of the left grip with a haptic pulse, which the probe counts. A Quest on a desk tracks no controllers, so this is the test of the path without a hand.

#### The console

A hold of the left menu button (0.8 seconds) shows or hides the console, a movable panel with the games to play and the main settings, below and ahead of the head and tilted up like a desk. A shorter press goes to the game only. The console stands in a `FollowingSubspace` of its own that each opening creates, so it opens in front of the head. The user can carry it. The pose of the last carry is relative to that subspace's origin, so the next opening puts the console at the same place relative to the head. A button row at the bottom has Close, Disconnect (while the link runs) and Exit. Exit ends the link and calls `cranpose::request_exit()`, which closes the app. The host's menu has no Exit item, so the client adds its own. The console shows when the first menu arrives and while no game runs. It hides when a game starts. The cranvr app writes the games and the settings to `vr-menu.json`. The host adds the stream's state and sends the menu as `HostMsg::Menu` when the client connects and when it changes (at most once a second). A press sends `ClientMsg::Console` with the request as JSON (`{"action": "play", "game": "Beat Saber"}`, `{"action": "stop"}`, `{"action": "set", "fps": 90}`). The host hands it to the app as a `vr_request` status line, as it does with ALVR. The app plays the game or changes the setting, and a changed refresh rate, game size or latency setting restarts the host. The client then connects again by itself every 2 seconds until the stream is back, unless the user pressed Disconnect. The launch argument `console` (`--console` on the desktop, `--ez console true` on Android) opens the console and keeps it open, for a headset on a desk that has no controller to hold the button. The console also has two settings of the client itself. They never go to the host. The preferences service saves them (`room` and `volume`). Room is Passthrough (the default) or Scene. It tells what fills the view while no game runs. With Passthrough the client asks for the room through Cranpose's `local_spatial_environment()` (opacity 1) and leaves the `StreamedScene` out, so the panels stand in the user's room. A running game brings the streamed scene back and the client asks for opacity 0. The game counts as running while the host hides the desktop (`HostMsg::Desktop` with `visible` false), which the host says as soon as it changes. A host that offers no desktop leaves the answer to the menu's status, which names the connected game and arrives at most once a second. The console shows Room and Volume also while the host sends no menu. With Scene the host's picture fills the view as before. A headset without passthrough control keeps the scene. The host still encodes and sends its scene while the room shows. The client's decoder drops those pictures, so the client sends no frame timing to the host then, and the host's latency numbers and its learned display lead stay as they were until a game starts. Volume is 100%, 150% (the default), 200% or 300%. The audio renderer multiplies each sample by it. A soft limiter above 0.8 keeps loud parts below 1. The peak in the audio log stays the level before the gain. `client/src/console.rs` holds the menu, the requests and the rules, `client/src/console_panel.rs` the panel.

`client/src/connection.rs` runs the link on its own threads. `client/src/hub.rs` hands the numbers to the UI. A `LaunchedEffect` copies them into Cranpose state four times a second.

`client/src/tracking.rs` turns Cranpose's `XrFrame` into the protocol's messages:
- The Hello carries the system name, the eye size to ask for (see above) and the refresh rates of `HeadsetProperties`. It also carries the fields of view and the eyes' places in the head from the first tracked frame. A change of the distance between the eyes goes to the host as a `Views` message.
- The runtime's display time and clock reading are on one clock. The client adds their difference to its own clock reading, so the display time lands on the client's clock.
- Each headset frame sends one tracking sample. The head's velocities are the runtime's. A runtime that gives none leaves the estimate from the last two poses. A controller's grip pose and aim pose go as the headset reports them, each with its velocities. The buttons, touches, trigger, grip and stick go to the host with every sample.
- A recenter of the headset goes to the host as a `Recenter` message. The host then puts its desktop screen in front of the user again.
- The host's haptics pulses play on the controllers through `Haptics`.
- The client sends the input of both hands with every tracking sample, as a datagram, so a lost datagram leaves a button wrong for one frame at most. The host turns it into the button and axis values that the game's runtime reads (the same ALVR path ids as with ALVR). The left menu button goes to the game too.
- The streamed scene asks for the views of each frame when the frame enters its queue. The client sends back the frame's timing from that moment: the frame loop took the frame now. The headset frame that does so reaches the display a known lead later. A frame whose target lies beyond that headset frame waits in the queue. The client then reports the display time of its own point. A frame that came after its point shows in the next headset frame. The client says so too (`Shown::late` in `client/src/hub.rs`). The display time in the timing is the frame's own point, computed when the frame enters the queue. It is not the time of the draw. A frame that is skipped still reports one. The host's "sample to display" figure therefore counts to the frame's point. It is not comparable with the figures of the version that reported the draw.
- The floor goes to the host as a `Floor` message. The client takes it from the stage that the runtime gives (`XrFrame::floor_height`) and sends it when it is known and again when it moves by a centimeter.

The poses are in the app's room, the LOCAL space of OpenXR. Its origin lies where the head was when the app started, so the head's height is about 0. The client sends the poses as they are. The host moves the room up by the height of the floor below its origin before it gives the poses to its test scene or to a game, so the floor lies at height 0 there. The render poses that go back in the frame's metadata stay in the room. Until the client has sent a floor, the host moves nothing.

Without a headset, as in a flat window, the client sends placeholder values: a still head at 1.6 m and two still controllers. The host renders pictures for them. The client decodes only in a headset: the decoded frames go to the streamed scene, which exists only there. Cranpose's `VideoDecoder` has a VideoToolbox backend on macOS. It reports that the decoder is hardware. The flat window does not use it. The flat window still connects, asks for the limits, counts frames, restores lost packets and reports its timing. It runs against a local host with a foveated stream.

#### Pairing

A headset on another machine needs pairing. The client makes a key for itself (Ed25519) and keeps it in Cranpose's preferences service. The connect panel shows a code of six digits that comes from the key's public half. The host has a key too. It makes one at its first start and keeps it in `host-identity.txt` in the config folder (readable by the user alone). The host's side:
1. Start the host with `--pair CODE`, for example `cranvr-host --protocol native --pair 482913`.
2. Press Connect in the headset. The host pairs the headset whose code matches and writes its public key to `paired-headsets.txt` in the config folder. The headset remembers the host's key under the address that the user typed.
3. A later start needs no flag. The host remembers the headset's key and the headset remembers the host's.

Each connection proves both keys (see "The sealed session"). A device that only saw the public keys and the code on the network cannot sign. The host checks the headset's proof before it looks at the code. The pairing window stays open for ten minutes or for one pairing. Five tries with a wrong code close it. A headset refuses a host whose key differs from the one that it remembers and says so in the connect panel. It says nothing about itself to that host first. A new install of the host that lost `host-identity.txt` has a new key: press "Forget host key" in the connect panel (or remove the preference `host-key:ADDRESS`), then connect again.

The first connection of a headset to an address trusts the host's key on first use. The headset remembers the key after the host accepted the headset. Someone who controls the network during that first connection can pose as the host to the headset and show it a picture. That someone cannot reach the real host through the headset: it cannot sign for the headset's key. The pairing code does not bind the host's key yet. Binding it would make the first connection safe as well.

The host listens on this Mac only until a headset is paired or `--pair` is given. `--trust-all` accepts every client without pairing, for tests. `--trust NAME` counts for the ALVR protocol only. A client on this Mac connects without pairing. So does a headset over USB through `adb reverse`, because its connection comes from the loopback address. The headset does not check the host's key when it connects to a loopback address. The session is sealed in every case.

The cranvr app starts the native mode for the cranvr client. Its Settings tab has a "Headset app" choice: "ALVR client" (the default) or "cranvr client". The cranvr client makes the app start the host with `--protocol native`. The Headset card then shows this Mac's address with the port, such as `192.168.1.20:9950`, for the headset's connect panel. The card has a field for the six digits of that panel. The Pair button restarts the host with `--pair CODE`. The app does not save the code, so a later start of the host opens no pairing window. The host reports the pairing state in a `native` status line: the port, whether it takes this Mac's clients only, the names of the paired headsets and the seconds that the pairing window stays open. A headset that the host refuses because it is not paired shows up as a `network` line with the problem `not_paired`. The Network choice, Low latency and the trusted names count for the ALVR client only. The app hides them in native mode. It does not pass the host options that belong to them.

#### The sealed session

Protocol version 4 encrypts and authenticates everything after the version exchange. The crate refuses a peer of version 3 or lower, which spoke in clear text. The ALVR mode is unchanged.

The handshake (`proto/src/secure.rs`, `proto/src/link.rs`) has three flights after the preambles:
1. The client sends a fresh X25519 key.
2. The host answers with its fresh key, its Ed25519 key and its signature over the transcript. The transcript hashes both preambles (a downgrade of the version breaks it), both fresh keys and the host's key. The client checks the signature and, when it remembers a key of this host, compares the keys.
3. The client sends its Ed25519 key and its signature over the same transcript, already sealed. The host verifies it. The signature covers the host's fresh key, so a recorded proof does not serve in another session.

HKDF-SHA256 turns the X25519 secret into four keys (salt: the transcript): the TCP path and the UDP path, each in two directions. ChaCha20-Poly1305 seals each packet. The nonce is four zero bytes and the packet's counter. A key never seals two packets with one counter.

| Path | What is on the wire | Order and replay |
|---|---|---|
| TCP | 4-byte length, 8-byte counter, content, 16-byte tag. It carries the control messages. With TCP media (USB) it carries the media and the datagrams too. | The counter must be the next one. A record that fails to open, repeats or is missing ends the session. |
| UDP | 8-byte counter, content, 16-byte tag. Carries the media and the datagram messages. | A window of 4096 counters tolerates loss and reordering. A replayed or older packet and one that fails the tag are dropped like a lost one. A forged packet cannot move the window: it moves after the tag held. |

The content is the kind byte and the body, as before. The sender seals in the buffer that it built the packet in: no copy and no allocation per packet. The sender computes the parity over the plain packets. It seals each packet on its own. A lost packet stays recoverable. A changed packet counts as lost. The sealed datagram keeps the 1400-byte limit, so the media payload of a packet is 1352 bytes (it was 1376). The host's UDP socket learns the client's address from the first datagram that opens with the session's key.

The sealing hides the content. The sizes and the times of the packets and the addresses stay visible. The Hello's device name and capabilities are sealed.

What it costs. `cargo test --release -p cranvr-proto --test secure seal_cost -- --ignored --nocapture` seals and opens the packets of a frame as the host and the client do. On the Mac (an M5, load average 30 to 40 from other builds, medians of 500 frames, runs agreed within 5 percent unless the load spiked):

| Frame | Packets (data and 10% parity) | Seal | Open |
|---|---|---|---|
| 12 kB, the test scene | 10 | 19 us | 19 us |
| 104 kB, one frame of 100 Mbps at 120 Hz | 85 | 155 us | 157 us |
| 600 kB, a key frame | 489 | 0.90 ms | 0.91 ms |

A packet of 1400 bytes takes 1.8 to 1.9 us to seal and as much to open, which is 0.75 GB/s. Cutting a 104 kB unit into packets and computing its parity takes 14 us, so sealing multiplies the sender's work per frame by about 12 and adds 0.16 ms to a frame of 8.3 ms. Sealing takes about 2 percent of the host's core at 120 Hz with 100 Mbps. The client's `Stats` and its log line show the opening time per frame, per packet and as a share of one core.

#### Display lock

The headset shows a new frame at each point of a grid, 120 times a second. Its compositor takes decoded frames some milliseconds before that point (about 16 ms on a Quest 3 with this client). A host that renders at the arrival of each pose has a fixed delay from pose to arrival. On the first Quest runs the arrival fell near the edge of the pick, Jitter put two frames into one interval and none into the next. The headset took 78 to 88 of 120 frames a second.

The host now plans each frame of its own scene (`host/src/headset/display_lock.rs`). Each tracking sample carries the display time that its poses are predicted for. The times of successive samples give the host the headset's display grid. The compositor's reports give the lead (display time minus pick time) and how long a frame takes from its start to the end of its decode. For each frame the host
- picks the target, the grid point at which the frame should show;
- starts the frame so that its decode ends a margin before the pick of that point. The margin comes from the 90th percentile of the measured time and an extra that rises when a frame misses and falls while frames hit;
- renders for the pose that is valid at the target, which it gets by moving the sample's pose with its velocities (at most 80 ms ahead);
- puts the target in the frame's metadata (`FrameMeta::target_display`, on the client's clock).

A planned frame starts at its planned time, not at the arrival of a pose. When no newer pose came since the last frame, it renders again for the same sample, with the pose moved on to the new target. Such a frame carries the sample's time plus one or two nanoseconds, so that the notes, the encoder and the headset's reports find it. At most two frames in a row repeat a sample.

The client passes the target through: `Hub::views_of` turns it into the runtime's clock (`Feed::to_runtime_time`) and puts it in `StreamedViews::target_display_time`. The scene then shows each frame at its point. A frame that comes early waits in the queue. Only a frame that comes after the take for its point is late. A late frame is skipped. Its point shows the picture before it. While a game runs, the game sets the pace and the lock does not plan the start of a frame. `--no-display-lock` turns the lock off for a comparison.

A frame that no plan started, such as a game's frame, still gets a target. Two faults made it show late and made the headset's queue skip frames. The host took the target from the headset's newest report, which ends in a point that lies ahead of the frame by a lead and a period. Every frame that the host sent between two reports got that point. The grid helper also did not reach back from the report's point, so a frame that could make an earlier point took the report's. The target did not count the lead either: the headset takes a frame one lead before the point that shows it. The host now takes the first point of the lock's grid at or after the frame's arrival plus the lead (`DisplayLock::point_for`). A frame that follows another within one period takes the next point, and never more than one point past the first that it could make, so a backlog stays short. A game's frame is planned onto the grid by its target only. The host does not move its submit time: the runtime's `xrWaitFrame` paces the game with a clock of its own, and moving it would need a change of the runtime. A game that finishes two frames for one pose now and then (a game near the rate of the poses) also gets both frames sent. The host dropped the second one before. In a test with Beat Saber on a loaded Mac a fifth of the game's frames were such frames.

The probe models the headset's queue (`probe/src/compositor.rs`: the display grid, the take one lead before each point and Cranpose's queue of three frames, which the source of `FrameQueue` gives). Its report line shows how many frames the queue showed, skipped and repeated, and how many frames came after their own point. With the lock off and the host's test scene it ran the old and the new host one after the other on a loaded Mac (load average 8 to 17, so only the shares count): the old host's frames came late in 93 to 98 percent of the cases and the queue skipped 18 frames a second. The new host's frames came late in 0.1 to 3 percent and the queue skipped 1 to 3. Of the frames that the host sent, the queue showed 82 of 101 with the old host and 98 of 101 with the new one.

On 17 November 2026 a Quest 3 on the desk (Wi-Fi, UDP media, 120 Hz, the test scene, 17 intervals of 2 s after a warm-up of 10 s, medians, the Mac's load average 16 to 28) ran the old and the new host in the order old, new, old, new with the lock off. The old host: the headset showed 99.9 and 89.9 frames a second, skipped 18 and 24, repeated 20 and 30, and every frame came late at the take. The new host: 115.8 and 115.8 shown, 0 skipped, 4 repeated, 0 percent late. The price is latency: the host's figure from the tracking sample to the display went from 34.1 and 35.6 ms to 42.5 and 42.2 ms, because a frame now waits for its own point instead of showing in the next headset frame, as a frame of the lock does (the lock gave 6 to 7 ms more in the earlier runs too). With the lock on the new host showed 119.8 frames a second (2 skipped, 2 repeated, 2 percent late, 43.6 ms) and the old host 113.8 (8 skipped, 6 repeated, 5 percent late, 43.4 ms) at load averages of 45 and 51. Beat Saber's first screen (the Steam game through the host's Wine stack, muted, native mode, 34 intervals after 50 s): with the old host the client received 97.9 frames a second and showed 79.9, skipped 18 and repeated 39.9, all frames late at the take, 48.1 ms; with the new host it received 117.8 and showed 117.8, skipped 2 and repeated 2, no frame late, 52.8 ms. The game's frame rate on the Mac varies from second to second while it loads, so the two Beat Saber arms differ in more than the host. The controllers sleep on a desk, so no hand reached the game on the Quest: the host's log showed a tracked mask of 0b0.

#### The desktop panel and the sound

The native client shows the Mac's desktop as its own panel and plays the Mac's sound.

The desktop stream:
- The host captures the display at its full pixel size (ScreenCaptureKit, YUV 4:2:0, up to 60 pictures a second) and encodes it with a second HEVC session (`host/src/headset/native/desktop.rs`). The units of this stream carry the stream id `DESKTOP_STREAM` (1) in the media header. They share the UDP or TCP media path with the eye stream. Their frame numbers, key frames and parameter sets are their own. ScreenCaptureKit sends a picture only when the display changes, so a still desktop costs nothing.
- The client asks for the stream with `ClientMsg::Desktop` (a width and a rate). The host answers with `HostMsg::Desktop`: the picture's size in pixels, the display's size in points, the bitrate and whether the panel is visible. A game hides the panel. The host then stops the capture and sends a key frame when the panel shows again. A client that asks for no desktop, or a host without `--desktop`, leaves the desktop in the eye stream as before. While the panel shows, the host leaves its own screen out of the eye stream.
- The pictures are not mirrored. Cranpose's compositor layer turns a decoder's surface upright. The old Quest app's layer needed a mirror on the Mac (`CRANVR_DESKTOP_FLIP=1` brings it back for such a client).
- The client shows the stream in a `SpatialExternalSurface` (mono). A second `VideoDecoder` in surface mode writes into it (`client/src/desktop.rs`). The decoder has its own thread: the eye stream's decode never waits for it. The decoder waits up to 30 ms for an input buffer. With Cranpose's default of 4 ms a busy Quest refused pictures. Each refused picture cost a key frame.
- The headset carries the panel. `movable_with(MovePolicy::system(...))` on the surface's modifier gives it a handle under it that a controller grabs with its trigger. The panel follows the controller. The thumbstick forward and back moves it away and near. A move event carries no size, so the client reads the thumbstick in the headset frame during a carry. The thumbstick left and right resizes the panel. The client keeps the width itself. At the release the client keeps the pose of the last move event in Cranpose's preferences service (`client/src/place.rs`: the keys `pose:desktop` and `width:desktop`). The next start puts the panel there with `SubspaceModifier::with_pose`. The connect panel, the stats panel and the console keep their poses the same way. All panels stand in `FollowingSubspace(FollowTarget::View(FollowMode::Snap), ...)`. Its origin lies 1 m ahead of the head at the first tracked frame. It moves there again after a recenter. A pose is relative to that origin, so a saved pose means the same place relative to the user's view at each start. A panel that the user never carried starts at its default pose in that frame: the desktop 1.5 m ahead of the head and 10 cm below eye level, 1.6 m wide. The first start therefore opens in front of the user. The launch argument `desktop_place` is gone. Cranpose gives the app no way to read the origin that it chose, so the app cannot put the panel into the gaze of a headset that lies on a desk.
- The controller acts as the mouse over the panel. Cranpose finds where the controller's ray meets the surface and reports it to `on_pointer_event` as `Enter`, `Move`, `Down`, `Up` and `Exit`, with the position in dp from the surface's top left corner. A dp is a millimeter. The client divides the position by the surface's width and height in dp to get u and v from the picture's top left corner. It sends u and v, the trigger (1 after `Down`, 0 after `Up`) and the thumbstick as `ClientDatagram::Pointer` for each event that changes them. It sends the same every 50 ms while the ray is on the panel or the trigger is down. Cranpose picks the controller that points: the one whose trigger was pulled last. A trigger that was down before the ray met the panel does not click. A press that leaves the panel's edge keeps its button down until the trigger lets go. Cranpose holds the point of a press against the shake of the hand: it sends no `Move` until the ray has turned 10 degrees in the first 350 ms of the press, or 4 degrees later. The events carry no scroll and do not say which controller points. The client therefore reads both thumbsticks in the headset frame while the ray is on the surface. The stick that leans further scrolls. The host moves the Mac's mouse, clicks and scrolls with the code of the compat mode (`desktop::Pointer`). The scroll speed follows the time between datagrams. A pointer that goes silent with its button down gets its button lifted after 250 ms. `CRANVR_POINTER_DRY_RUN=1` makes the host log the mouse events and post none: a test with a controller on a desk must not click on the user's Mac. `--es desktop_pointer_test circle` makes the client send the pointer of a controller that circles over the picture, pulls the trigger every 3 seconds and leans the thumbstick every 6 seconds. The test sends its points straight to the uplink, so Cranpose's ray is not part of it.
- The Mac's cursor is part of the pictures (the host's capture draws it). The client's own cursor, a small panel in front of the picture, does not show. Cranpose puts the layers of its surfaces after the layers of its panels, so a panel over a surface lies under the surface. A ring that the client placed beside the picture showed. A ring over the picture did not, at 2 cm and at 30 cm in front of it. The cursor's position also goes out as `HostDatagram::Cursor` (every 2 ms while the mouse moves). `CRANVR_DESKTOP_CURSOR_IN_VIDEO=0` on the host leaves the cursor out of the pictures, for a client that draws its own. The controller's ray ends on the panel and shows where the user points without delay. The Mac's cursor follows with the delay of the pictures.
- Launch arguments of the panel (extras on Android, `--name=value` on the desktop): `desktop` (`false` asks for no desktop), `desktop_width` (the widest picture to ask for, default 3840 or the decoder's limit), `desktop_pointer_test` (`circle`) and `cursor` (`none` draws no cursor ring when the host leaves the cursor out of the pictures).

The sound:
- The host sends the captured sound as PCM: 48 kHz, 16 bits, stereo, in datagrams of 240 frames (5 ms, 960 bytes). That is 1.54 Mbps on top of a video stream of 40 to 100 Mbps. Opus would cut it to about 0.1 Mbps. PCM adds no encode or decode time and needs no codec on the Quest. A datagram carries the host's clock for its first sample, so the client can tell how old the sound is.
- The client plays it through Cranpose's audio backend (`client/src/audio.rs`). `cranpose_audio::backend::open` runs a renderer on the platform's output thread (AAudio, low-latency mode, 4 ms bursts on a Quest 3). The renderer takes stereo frames from `cranpose_audio::ring`. The renderer never locks or allocates. A jitter buffer between the network thread and the renderer:
  - plays after the buffer holds 30 ms. It plays again after an underrun when the buffer holds 30 ms again;
  - keeps its fill near the target by changing the playback rate by at most 0.5% (this absorbs the drift between the Mac's and the Quest's clocks and the difference of a device that does not run at 48 kHz);
  - drops its oldest sound down to the target when the fill passes 90 ms;
  - leaves a gap of silence for a datagram that never came. It drops a datagram that came late.
- ScreenCaptureKit hands over a buffer of 960 frames (20 ms) 50 times a second. The jitter buffer therefore saws between a high and a low fill. A flat window on the Mac plays no sound: the host would capture it again.
- The client logs the buffer's fill (now, the range and the target), the burst of the device, the age of the sound when it was queued, the lost, late and dropped datagrams and the underruns every second. It counts underruns twice. The buffer's count rises when the jitter buffer runs dry. The device's count (`AudioSink::underrun_count`, AAudio's XRun count) rises when the device has no frame to play. A dry jitter buffer fills the device's buffer with silence and leaves the device's count alone. The log also gives the device's output latency and the frames of its burst, once the device reports them. The stats panel shows both counts and the latency. A platform that keeps no count shows a dash.

Numbers from a Quest 3 on the desk (Wi-Fi 6 on 5 GHz, 5560 MHz, link speed 1200 Mbps), UDP media, 120 Hz, the foveated eye stream, on 17 November 2026. The room was dark: the headset showed "Finding position in room" and did not track, so `debug.oculus.guardian_pause` was 1 during the runs (0 before and after). The headset lay still. The medians are those of intervals of 2 s after 8 s of warm-up.



The eye stream with and without the desktop panel. Arm A: the host without `--desktop`, the client with `--ez desktop false`. Arm B: `--desktop`, the panel at 3840 by 2160 pixels. The order was A, B, A, B, 52 s each. The Mac's load averages (1, 5 and 15 minutes) were 6.9 21.9 26.4, 6.1 19.1 25.1, 5.8 16.8 23.8 and 9.9 15.9 23.1.

| Arm | Eye frames shown of the headset's | Skipped, repeated a second | Eye decode | Pose to decode end |
|---|---|---|---|---|
| A, no desktop | 117.8 of 121.2 | 4, 2 | 3.53 ms | 19.3 ms |
| B, desktop | 113.8 of 121.1 | 10, 6 | 4.17 ms | 30.5 ms |
| A | 117.8 of 121.0 | 4, 2 | 3.57 ms | 19.1 ms |
| B | 114.8 of 121.0 | 10, 5 | 4.34 ms | 28.6 ms |

With the panel the eye stream showed 3 to 4 frames a second less and took 9 to 11 ms more from the pose to the end of the decode. Arm A also left out the host's `--desktop`, so the host's flag and the panel changed together. The cause of the cost is not measured.

The desktop stream in arms B (the display changed about 57 times a second):
- The client received and decoded 57.2 and 57.3 pictures a second of 3840 by 2160 pixels. It lost none.
- The decoder's report (the picture's submission to its output) gave 5.95 and 5.90 ms as the mean and 15.8 and 15.9 ms as the longest.
- The client skipped 8 and 4 pictures in the arm while it waited for a key frame. It refused none. It asked for 4 and 3 key frames in 52 s. With Cranpose's default input wait of 4 ms the decoder refused up to 16 pictures in 20 s. The key frames that followed made it worse.
- The median bitrate of an interval was 1.2 Mbps. A key frame lifts an interval to 3 to 5 Mbps. The host's encode of a picture took 13 to 18 ms from the call to the encoded frame, at a load of 20 to 30. The probe on the Mac got 2.1 ms from VideoToolbox's decoder.
- A screenshot of the Quest shows the Mac's text readable, the whole picture upright and the Mac's cursor in it. The panel's surface was made at 1600 by 900 dp (2400 by 1350 pixels at Cranpose's 1.5 pixels to a dp) and at 2560 by 1440 dp (3840 by 2160 pixels). A crop of the screenshot looked the same in both and showed the same part of the picture, so the picture is not cut. The panel was narrower than the eye's 2064 pixels in both. How sharp a panel that fills the view looks is not tested.

The sound (all four arms, about 10,000 datagrams each):
- The device ran at 48,000 Hz with bursts of 4.0 ms. The host's capture handed over 960 frames (20 ms) 50.0 times a second.
- The jitter buffer's fill ranged from 7.5 to 10.8 ms at its lowest to 52 to 53 ms at its highest in an interval of 2 s. The target is 30 ms.
- The sound was 15.5 ms old when the client queued it: the host's capture of a datagram's first sample to the queue. The host's 20 ms buffer causes most of it. The probe on loopback saw an age of 15 ms with a 95th percentile of 20 ms.
- No datagram was lost or late and no sound was dropped. The renderer ran dry 1, 3, 2 and 2 times in 52 s. Each of these is a gap of silence: the renderer then waits until the buffer holds 30 ms again. The cause is not known. The fill swings by 44 ms against 20 ms bursts, and the probe's arrival age on loopback stayed under 23 ms. So the Wi-Fi path or the Quest's scheduling adds the jitter. The target was not changed. This is an open defect.
- The rate of the playback stayed within 0.1% of the source's after the control loop took the average of the fill. Before that the rate followed each tooth of the fill and reached the limit of 0.5%.
- The total from the host's capture to the device's buffer is about 15.5 ms plus the fill, so about 45 ms. The device's own latency comes on top. Cranpose does not report it.
- The level, from one more run with the Mac's `say` command: the client logs the largest sample that its renderer wrote to the device's buffer in each interval of 2 s. It was 0.000 in every quiet interval and 0.71 to 0.74 in the three intervals while the Mac spoke. So a signal reached the Quest's output buffer. The probe on the Mac read a peak of 0.741 in the datagrams. Nobody listened at the headset.
- On the Mac the probe got 12.04 s of sound in 12.02 s, with no gap in the sequence and 15 ms of age on arrival (median).

The pointer, from the Quest with the test `--es desktop_pointer_test circle` and the host in dry-run mode (`CRANVR_POINTER_DRY_RUN=1`). The test's controller aimed with the client's own ray math then, which the client has since replaced with Cranpose's surface events. The test now sends its points straight to the uplink. The run shows the path from the client's frame loop to the host's mouse code. It does not show that a real ray lands on the picture where the user points. The client sent 121 pointer datagrams a second. The host logged 237 mouse moves per 2 s, one button down and one button up every 3 s at the point where the circle was. It logged 1,500 pixels of scroll in each 2 s of the thumbstick's lean. In a run of 40 s the host counted 15 clicks. The probe on the Mac drove the same code path.

Not tested: a controller in a hand (a real ray on the surface, the carry by the handle, the resize, the saved poses, the console's Exit, the device's underruns), a game that hides the desktop, a recenter, the USB path from the Quest, a host that restarts during a stream, several clients one after the other.

### A VPN on the headset

A headset with Tailscale that accepts a subnet route for its own network sends its UDP to the Mac through the subnet router. The router replaces the sender address, and ALVR drops every stream packet that does not come from the headset's own address. The handshake still works over TCP, because the Mac opens that connection and Android answers it on Wi-Fi. The headset then connects, gets no video and leaves after about 5 seconds, again and again.

The host reports this after two such connections in a row. With "Network: Auto" in the settings the window then switches the stream to TCP and says why. TCP gets through the VPN: the Mac opens the stream connection too. On the Galaxy Tab S7+ over 2.4 GHz Wi-Fi, TCP took about 2 ms more than UDP in the network stage and had resend spikes. The better fix is on the headset: exclude the ALVR app from the VPN, or stop using the subnet route there. The host also warns when this Mac reaches the headset through a VPN tunnel of its own.

The window saves its settings in `~/Library/Application Support/cranvr/app.json`. A change of refresh rate, bitrate, foveation or desktop restarts the host. The headset reconnects by itself.

Without the window:

```bash
./target/release/cranvr-host --protocol alvr --fps 120 --trust 1234.client --desktop
```

`cranvr-host --help` lists every option.

### Game sound

The host captures what this Mac plays, from every app but the host. The headset plays it. The capture takes the sound before the Mac's volume: in a test at a Mac volume of 15%, a tone reached the client at its full source level. Turn the Mac down while you play, or the Mac plays the sound too. The capture needs Screen Recording, like the desktop. "Game sound in the headset" in the settings turns it off; `cranvr-host --no-game-audio` does the same. The headset's microphone does not reach the Mac yet.

With `--protocol native` the host sends the sound as PCM to cranvr's Cranpose client (see "The desktop panel and the sound" above). ALVR's client buffers about 50 ms of sound. A rhythm game such as Beat Saber has an audio latency setting: set it once with its calibration.

### What you should see

With the desktop off, the host shows a test scene:
- a sky with a grid line every 15 degrees and brighter lines every 45 degrees;
- a floor with 50 cm squares and a green line every meter;
- a yellow ball at the left controller and a cyan ball at the right controller;
- a small red square near the left edge of the left eye and a blue square near the left edge of the right eye.

With the desktop on, the host shows your main display on a screen 1.4 m in front of you, in a dim room. A game replaces both scenes while it runs.

### The Mac desktop

- Point the right controller at the screen to move the mouse. Pull the trigger to click. Push the thumbstick up or down to scroll. In cranvr's former Quest app either controller points: the one whose trigger was pulled last.
- Click the right thumbstick to put the screen in front of you again.
- In cranvr's former Quest app a ball under the middle of the screen's bottom edge is its handle. The console has one too. Cranpose draws the balls in the overlay's scene, so they show from every side. A ray catches a ball from any side, also from behind the screen. Point at it and hold the trigger to carry the screen. A screen grabbed from behind turns half around while you carry it, so its picture comes to face you. The screen keeps its angle to the ray: a turn of the wrist turns, tilts or rolls it by as much. Within 4 degrees of level its top edge snaps level. A click on the right thumbstick brings the screen upright in front of you. Push the thumbstick of that hand to move the screen away or near, also without the trigger while you point at the handle. Pull a corner, just outside the picture, to resize the screen. The app keeps the screens' places for the next start.
- A trigger pull turns the controller a little. A press on the picture therefore takes the point where the ray pointed 80 ms before. It holds that point until the ray turns more than 10 degrees in the first 0.35 seconds of the press, or more than 4 degrees later: a click from a shaky hand stays a click, and a held press drags. The rays go through a one-euro filter, so a resting hand's tremor barely moves them and a fast turn follows with little lag. Each press on a picture adds a line to `panels-presses.txt` in the app's files folder: its length, the ray's largest turn and whether it dragged.
- Buttons right of the handle pause and resume the screen's picture, set its quality (Auto, then 1920, 2560 or 3840 pixels across) and add a screen. The buttons are one picture at 1600 pixels per meter that the compositor shows as its own layer, with the same filters as the screen: drawn in the overlay layer, their text shimmered. "Add display" makes the host add a virtual Mac display of 1920 x 1080 points, to the right of the other displays, with its own screen in VR. "Close" on an added screen removes it and its display. Four screens at most.
- In cranvr's former Quest app the console stands below and ahead of you, tilted up like a desk. It shows the stream's rate and latency and the game that runs. It has a button for each installed game and buttons for the main settings: the refresh rate, the game's render size, the anti-aliasing limit and low latency. A press on a game's button starts the game. A change of the anti-aliasing limit applies at the next game start. Every other setting restarts the host, so the stream stops for a few seconds. The handle at the console's top carries it like a screen. A click on the right thumbstick brings the console back below and ahead of you when the ray points at the console or at nothing. A click at nothing brings the first screen back too. The console hides while a game runs, as the screens do. Hold the left controller's menu button for a moment during a game: the console then shows over the game, below and ahead of you, with a Stop button for the running game. Another hold hides it. The game gets the button too. The Exit cranvr button at the console's bottom closes the app. The headset then goes back to its home. Sideloaded apps such as cranvr show in the Quest's library under Unknown sources.
- While no game runs, the room shows around the screens through the headset's cameras. "Around you" on the console switches between the room and the floor. The app keeps the choice in `panels-room.txt` beside `panels.txt`. During a game the game fills the view.
- Each added display lives in a helper process (`cranvr-host --display-helper W H SERIAL`). The host's own view of the displays goes stale, because its main thread runs no event loop. A second display made in the host then failed to arrange, and macOS mirrored the main display onto it at 1920 x 1080. The helper arranges its display for the login session; an arrangement for the app alone was undone when the helper ended, and macOS mirrored the TV for a moment.
- macOS asks for two permissions on first use. Screen Recording lets the host capture the display. Accessibility lets the controller move the mouse. Without Accessibility the screen still shows. The app asks for each missing permission when it starts. A card at the top of the window has a button for each one until macOS allows it. The host notices a new Accessibility permission within 2 seconds, without a restart.
- `--desktop-fps` sets how often the host captures the display (default 30). Each capture of a 6K display costs the window server about 3 ms of GPU time. At 60 captures per second the stream lost about 10% of its frames in a test.
- `--game-scale F` (default 0.7) makes a game render each eye at this share of the stream's eye size. Foveated encoding sends about a third of the eye's pixels anyway. At the full size a base M5 drew Beat Saber at 37 to 41 frames per second. The Settings tab sets it as "Game render size".
- A game that makes fewer frames than the headset's refresh rate added 10 to 18 ms of latency in tests. The headset's own loop then waits for frames and slows to about 100 frames a second. The host warns in the log 20 seconds after the game connects, when the game stays below 95% of the rate.
- Games under Wine use at most 2x anti-aliasing by default ("Game anti-aliasing" in the Settings tab, `CRANVR_MAX_MSAA` for `scripts/run-game.sh`, 0 for no limit). cranvr's DXMT patch then reports 4x and 8x as unsupported, so a game that checks picks 2x. Beat Saber set to 4x checked and used 2x: DXMT logged its first MSAA texture with 2 samples. Its first screen ran at 115 frames a second with the limit and at 94 without, in one pair of runs (app host, 60% render size, 120 Hz). The limit applies when a game starts. A game started from the Steam window gets it after Steam restarts.
- The host keeps a render size per game. A game that stays below 95% of the headset's rate for 10 of the 12 stats windows in its first minute after loading gets a smaller share of the render size at its next start. A game reads its size only when it starts. The step aims 10% under the rate, with a floor of 40%. When a smaller size did not raise a game's rate by 5%, the host stops lowering it: the size is not what limits that game. That last step stays in place until the user forgets the learned sizes. The runtime names the game in `game-program.txt`, and reads the shares from `game-sizes.txt` in cranvr's config folder. The Settings tab lists the learned sizes and forgets them. In a test, Beat Saber at 95 frames a second got 85% of its size, and its next start rendered 1038 x 1086 instead of 1228 x 1286. At 2x anti-aliasing and the 60% render size, Beat Saber's first screen ran at 108 to 115 frames a second through the app's host, close to the 114 at which a window counts as slow. In a 100-second run, while other programs built code, the host set 90% for its next start. The test entries were forgotten afterwards.
- While a game runs, a display in a scaled mode shows its panel's own pixels ("Panel pixels while a game runs" in the Settings tab, on by default; `--keep-display-modes` turns it off). A scaled mode, such as 3008 x 1692 points on a 3840 x 2160 TV, makes macOS draw every change at 6016 x 3384 pixels and shrink it. A helper process (`cranvr-host --unscale-displays`) sets each such display to the panel's pixels with at least as many points, so no window moves, for itself only: when the game disconnects, the host ends the helper, and macOS gives each display its mode back. In a test the TV went to 3840 x 2160 at 1x when Beat Saber connected and back to 3008 x 1692 three seconds after it quit; the desktop layer then ran at 56 frames a second again. In Metal traces WindowServer took 157 to 161 ms of GPU time per second in the unscaled mode against 442 in the scaled one.
- `--buffering-frames 1` (the "Low latency" switch in the Settings tab) lets the headset keep one decoded frame instead of two. ALVR's decoder shows the oldest waiting frame. It drops one only when the average queue grows past this number, so with 2 a frame often waits a full refresh. The switch is off by default until a test in a worn headset shows no stutter.
- `--pointer-pitch DEG` tilts the pointing ray down from the controller's handle (default -40). It applies to ALVR and to a native client that sends no aim pose. A native client's aim pose gives the ray as it is. Change it if the cursor lands above or below where you point.

### A virtual display for the headset

By default the screen in VR shows the Mac's main display as it is. The desktop tab can add a display instead: one more monitor of this Mac that only the headset shows (`host/src/virtual_display.rs`). It is 1920 × 1080 points with HiDPI (3840 × 2160 pixels) at 60 Hz, or 2560 × 1440. On a MacBook a new display made the built-in screen go black, so it is no longer the default. The host makes the display at start. The display goes away with the host. macOS has no public call for this. The host uses CoreGraphics' CGVirtualDisplay classes, as DeskPad and BetterDisplay do, through the Objective-C runtime.

Work from the headset with these steps:
1. Start cranvr. The virtual display appears to the right of your displays, as an extra screen. Your main display keeps its place and resolution. No window moves.
2. Connect the headset. The screen in VR shows the virtual display.
3. Drag a window onto the virtual display with the Mac's mouse, or point at it with the right controller. Type with the Mac's keyboard.

cranvr moves no window by itself. Two switches in the desktop tab change that, and both are off by default:
- "Main display while the headset is on": at the first pose of the stream the virtual display becomes the main display. macOS then moves the menu bar, the Dock and the windows of the main display onto it. New windows open there too. When the headset leaves, every window goes back to its place and size. A headset that connects and leaves without a pose changes nothing. The change applies to the host process only (kCGConfigureForAppOnly), so macOS also undoes it when the host ends. `cranvr-host --desktop --virtual-display 1920x1080 --vr-main-display` does the same without the window.
- "Bring the cranvr window into VR": the cranvr window moves onto the virtual display while the headset streams, and back afterwards.

A cranvr window that sits on the virtual display when no headset streams moves to the main display: nobody would see it there.

Tested on this Mac with a 6K TV as the only display:
- Without a host option, macOS made a new virtual display the main one and mirrored the TV to it, which changed the TV's resolution. The host now turns mirroring off and places the virtual display to the right of the others.
- With the probe as the headset, Claude, Chrome and Ghostty moved onto the virtual display. They came back to their exact places and sizes when the probe left.
- The desktop view of the virtual display ran at 117.9 fps at 120 Hz. The same view of the 6K TV ran at 99.4 fps: each capture of a 6K display costs the window server more GPU time. A 2560 × 1440 virtual display (5120 × 2880 pixels) ran at 120.1 fps.
- With the probe streaming, the host was killed. macOS put every window and display back as before.
- Steam's Wine window keeps its global position. At (1151, 617) it sat on the TV before the switch. During the switch the same place lay inside the virtual display. After it the window was on the TV again. A Wine window that sits beyond 1920 × 1080 points on the main display before the switch falls outside every display during it. A Wine window opened during the switch opened on the virtual display and moved to the TV afterwards.
- The sound reached the probe with the switch on: a faint tone arrived at the same peak level as without it.

Not tested: the controller as a mouse on the virtual display in a real headset, and the switch with a Quest.

### The desktop as a layer in cranvr's former Quest app

cranvr's former Quest app was ALVR's OpenXR client of v20.14.1 under its own package. It was removed from this repository when the Cranpose client took its place on the Quest (`docs/plan.md`, milestone 6b). The native client shows the desktop as its own panel ("The desktop panel and the sound"). This section keeps what the removed app did and what its tests measured. The host's side, `host/src/desktop_layer.rs`, still serves it in ALVR mode. No client that speaks to it remains.

In the eye stream the host draws the desktop into each eye's view, and the headset warps that view through the lens. Text is sampled twice and blurs. The former app showed the desktop as an OpenXR quad layer instead:
- The headset's compositor samples the desktop once, at the display's own resolution, with the compositor's supersampling and sharpening.
- The compositor draws the layer with the newest head pose. A head turn over the desktop needs no new frame from the Mac and does not wait on Wi-Fi.
- The host captures the display at its full pixel size, 3840 × 2160 for a 1920 × 1080 virtual display, at up to 60 frames per second. ScreenCaptureKit sends a frame only when the display changes, so a still desktop sends almost nothing.
- A second HEVC encoder session encodes the captures. Each capture is first mirrored top to bottom on the Mac's media hardware (VTPixelRotationSession), because the Quest's compositor reads a decoder's surface bottom row first.
- The Quest's hardware decoder writes straight into a surface that the compositor shows (XR_KHR_android_surface_swapchain). No copy happens on the Quest's GPU.
- The cursor position goes over UDP every 2 ms while it moves. The app draws the cursor as a second small layer at the newest position every refresh, so the cursor never waits for a video frame.
- The eye stream leaves the screen out while the layer shows it. The host sends no desktop frames while the screen is out of the head's view or while a game runs.
- Each screen has its own capture and encoder on the Mac and its own decoder on the Quest. The app reports every refresh how the head sees each screen. A screen near the middle of the view gets up to 60 frames per second, one at the edge of the view 15, one out of view none: its capture stops after a second. In Auto quality a screen gets the narrowest of 1920, 2560 and 3840 pixels that covers twice the pixels per degree a Quest 3 shows, after the need held for a second (sharper) or five seconds (coarser). Every capture stops while a game is connected. The eye stream's own desktop capture stops while the layer shows the screen.
- With `CRANVR_DESKTOP_INSET=1` for the host, the screen that the head points at gets a sharp inset: a second stream of 1920 x 1080 pixels of only the part around where the head points, at the full density that the headset can show there. The screen's own picture then needs only half that density. The inset moves when the head points a quarter of its size away from its middle. ScreenCaptureKit changes the inset's part without a restart; the host sends the part with each frame, and the app places the inset by the part of the frame that its surface shows. The inset turns on only when the screen needs more than 2400 pixels across. It is off by default since 16 November 2026: on head turns a worn headset showed it flash pictures of other parts of the display, and its picture did not quite match the screen's. The app places the inset by the part of the newest decoded frame, which the compositor may not show yet. Without the inset, Auto quality gives the screen's own picture the whole density, up to 3840 pixels.
- The console is one more quad layer. Its face is a Cranpose composition. Cranpose's CPU renderer draws it into a picture at 1600 pixels per meter, one dp to a millimeter. A thread of its own holds the composition. It draws only when the picture changes, such as when the ray moves onto a button. The ray's point on the face moves Cranpose's pointer, so the button under the ray lights up. A trigger press presses the button there. Only the active ray, the one whose trigger was pulled last, lights a button. Cranpose's renderer fills the picture with an opaque color before it draws, so the app clears the face's round corners itself. On 16 November 2026 the first picture of 960 by 1251 pixels took 9.5 ms on the Quest 3. Cranpose made the app's library grow from 9.3 MB to 16 MB. Boxes in the overlay frame the face with a depth of 3 cm. The cranvr app writes the games and the settings to `vr-menu.json` in cranvr's config folder. The host adds the stream's state and sends the menu to the headset when it changes, at most once a second. A press sends the button's request back to the host. The host hands it to the app as a `vr_request` status line. The compositor stacks the layers in the order of their list, not by depth. The app therefore lists the console after the screens when the console is nearer to the head than every screen.
- The room view uses the passthrough layer of XR_FB_passthrough under all other layers. ALVR's client drops that layer when the host's ALVR settings turn passthrough off, which is the default. cranvr's app makes its own while the screens show and the user chose the room. It then leaves the eye stream's layer out, so the eye stream's floor and sky do not hide the room. During a game the app drops the passthrough layer again, so the cameras' picture costs the headset nothing then. The host's ALVR server sends its live settings every second. ALVR's client dropped the passthrough layer at each of them, so the room view started a new layer every second, and each new layer showed black at first. The user saw black frames now and then. The client now leaves the layer alone; four screenshots over four seconds then showed the room each time. On 16 November 2026 the headset's screenshot on the desk showed the room with the console in it. The test hook values `room` and `floor` switched it both ways. Not tested in a worn headset yet.
- A game that stops sending frames gives the desktop back too: after 3 seconds without a frame, or 90 seconds after it connected without any. The console then offers Stop. On 16 November 2026 SUPERHOT VR sent no frame after the user left the app through the Quest's menu, and the headset showed only black.
- The cause: BasaltVR's runtime gives up on the host when it hears nothing for 5 seconds (`kSocketTimeoutMilliseconds` in its `control.cpp`). It then stops the game's rendering but keeps the connection open, so the host never saw the game leave. The host sent the runtime nothing while the headset was away. Gaps of 1 to 2 seconds kept the game running; gaps of 6 and 11 seconds stopped it. The host now sends INPUT_RESET, which the runtime takes as the end of an ALVR session, whenever it sent the runtime nothing for a second. In a test on the desk SUPERHOT went on making frames after the app was gone for 13 seconds.
- A game that disconnects gives the desktop back: after Beat Saber quit, the layer ran at 57 fps again within 3 seconds.
- A new host connection replaces the old one in the app. A host that quit may leave its connection open on the headset for good, when Wi-Fi loses the close. The app then made the next host wait, and the layer did not come back.
- The layer shows the display that the eye stream captured when the host started. When that display is gone, the layer shows the main display. On 16 November 2026 Meta's Remote Desktop added its own display. macOS then mirrored the TV onto it, so the TV was no display of its own. The layer found no display to capture. The headset showed only the eye stream: a blurry desktop with the host's cyan marker for the controller and no ray. With the change the layer showed the new main display. The host checks every second that each panel's display still draws. When the first panel's display stops for two checks in a row, the panel moves to the main display. An added panel whose display left closes. Before this, the capture of a display that left tried to start again every 10 ms. The host then held 210 pipes after a few minutes, near the 256 files that macOS allows a process by default. The desktop layer sent no frames. A failed start now waits 200 ms before the next try. The host also raises its own file limit to 10240.
- A game that loads sends no frames for seconds. The desktop stays hidden as long as the game is connected, so it does not flash over the loading game.
- The app places the panel itself, in the headset's stage space. ALVR's server recenters every pose that the host sees, to the head's place and direction when the headset connects. A panel placed by the host showed turned and shifted, and the host's pointer missed it. The app also tests the controllers' rays against the panel it shows and sends the hit to the host over UDP port 9952. The host moves the Mac's mouse there.
- The app draws the controllers, their rays, the corners of a screen, the point on a screen and the console's frame in an overlay layer over all other layers. Cranpose draws this layer as a 3D scene: one `scene3d` box for each eye, with a camera made from the eye's OpenXR view. A thread of its own holds the composition. It has its own EGL context in the share group of ALVR's context. wgpu wraps the images of an OpenXR swapchain as textures. Cranpose draws into them. Each image of the swapchain lives within one frame, as with ALVR's own overlay. The render thread acquires an image as soon as the frame's eyes and shapes are known, before it renders the eye stream. The scene's thread draws both eyes meanwhile. It puts a GL fence after its commands. Before the frame ends, the render thread has ALVR's context wait for that fence on the GPU. It then releases the image. No thread waits for the GPU. The scene shows the poses of its own frame. Each eye draws into a box with the aspect of its own view, inside a cell of the swapchain. The layer hands the compositor that box. A Cranpose camera has one focal length for both directions. A box of another aspect would be stretched by another amount across than up. The thread draws only when the shapes or the eyes change. An image with no new picture stays acquired for the next frame, since it holds an older picture. `debug.cranvr.overlay=alvr` brings back ALVR's wgpu renderer. The app also uses that renderer when the scene fails to start. The host's markers for the controllers leave the eye stream while the layer shows. The controllers are simple shapes, not Meta's controller models.
- `debug.cranvr.mirror=1` shows a mirror of the first screen to its left: Cranpose draws the same picture on a surface bent into an S, in the overlay's scene. A second hardware decoder decodes the first screen's frames into an image reader. The render thread draws each new picture into a GL texture. The scene's thread copies it into a frame of Cranpose's `ExternalFrames`, and an `Image` fills the curved surface at 1920 pixels across. The mirror shows how a picture drawn in the scene compares with the compositor's own layer: the scene's picture is sampled into the eye picture of 1629 by 1760 pixels, then again by the compositor. On 16 November 2026 both decoders kept pace at 14.8 frames a second. The stream held 120 frames a second, and the render thread waited 0.1 ms a frame for the scene. The mirror takes no input. On 17 November 2026 the user saw it upside down in the headset: the decoder's buffer starts at the picture's top row, which the copy turned over. The picture's text was not readable on the mirror, while the screen beside it was sharp. Three things soften a picture drawn in the scene. The eye picture of 1629 by 1760 pixels has fewer pixels than the display of 2064 by 2208. Cranpose samples a picture with no mipmaps, so a 3840 pixel picture shrunk to the surface's 1300 or so pixels loses its fine lines. The compositor then samples the eye picture once more. The mirror now draws the picture at half size, each pixel the mean of 2 by 2 pixels of the decoded picture. While the mirror shows, the scene draws at 1.5 times the recommended eye picture, 2444 by 2640 pixels per eye. The headset's GPU went from about 45% to 52.6% busy. The stream held 120 frames a second at 55 ms. The compositor's own layer still samples the full picture once, at the display's resolution, with its supersampling and sharpening.
- The screens, their bars, the console's face and passthrough stay compositor layers. The decoder writes each screen's picture straight into its swapchain. The compositor samples a quad layer once at the display, so its text stays sharper than text drawn into the scene and warped again.
- Frames with the layer go to the compositor with the time of the coming refresh. The stream frame's own time can lie up to a second back. Then the compositor may place the layer by an old head pose. The property `debug.cranvr.layer_time` set to `frame` keeps the stream frame's time, for a comparison.

The app listens on TCP port 9950 and UDP port 9951. The host connects to the headset's address when ALVR connects, as ALVR itself does. The store ALVR app does not listen, so the host keeps the desktop in the eye stream for it. `cranvr-host --no-desktop-layer` keeps it there for cranvr's app too.

Tested on a Quest 3 lying on a desk, with `--screen-in-view` and the same picture of text on the virtual display:
- The eye stream blurred 13-pixel text and made 10-pixel text unreadable. The layer showed both sharply, in the middle of the view. The headset's own screen capture shows layers, so these checks run without anyone wearing the headset.
- A static picture as a layer compared source sizes and filters. 3840 × 2160 with the compositor's supersampling and sharpening read best. 1920 × 1080 read worse.
- A 60 fps test video on the virtual display: the host sent 57 desktop frames per second. The Quest decoded 57 per second.
- A synthetic cursor held still at a quarter of the display from its top left corner (`CRANVR_CURSOR_TEST=0.25,0.25` for the host; `circle` makes it circle once per second) showed in the top-left quarter in both eyes. It pointed at the same code column in both eyes, at lines one apart: about 0.4 degrees.
- Two decoders now run on the Quest. With the 60 fps video on the layer, the eye stream's decode time rose from 5.2 to 7.2 ms. The headset showed 113 frames per second instead of 118.

Not done or not measured:
- The cursor's delay is not measured. By design it is the Wi-Fi delay plus at most one refresh of the headset.
- The cursor is a fixed arrow. Shape changes on the Mac, such as the text cursor, do not show.
- A worn headset showed the real mouse cursor fast. The controller as a mouse was not tested. This Mac has allowed Accessibility since 15 November 2026.
- With the stream frame's time the screen shook a little on head turns while the floor stayed still. With the refresh time the user saw much less shake and a still floor.
- The overlay lies over the panel with no depth test. A controller behind the panel shows in front of it.
- On 16 November 2026 the Cranpose overlay ran on a headset on the desk. A headset on the desk tracks no controllers, so the test hook `debug.cranvr.xr_probe` added a controller, a ray, a point on a screen and a corner in front of the head. Screenshots of the Cranpose overlay and of ALVR's renderer showed the same shapes in the same places, the console's frame included. Cranpose's edges show small steps: its scene has no multisampling. The scene's thread takes 2.6 ms of CPU time a frame for both eyes at 3360 by 1760 pixels. The render thread waits 0.7 to 0.8 ms a frame for it, 2.9 ms at most. The host's figure from motion to display came out at 49.5 and 49.3 ms with the Cranpose overlay. ALVR's renderer gave 49.0 and 46.7 ms in the same test. Both held 120 frames a second. The headset's GPU was 41 to 42% busy with the Cranpose overlay and 37 to 38% with ALVR's renderer. With 30 busy loops on the Mac, at a load average of 51 to 61, the Cranpose overlay held 118.5 to 119 frames a second at 48 to 49 ms. Two earlier designs failed that test. In the first, the render thread waited for an image that the compositor still read. That wait took 5.3 ms a frame. It raised the figure to 58 ms. The scene drew only 78 frames a second. In the second, the scene's thread kept an image from one frame into the next. Under load the compositor then held its images for 2 s at a time. The stream fell to 33 frames a second while the headset's GPU idled. The scene now keeps no image across frames. The test hook `exit` ended the session with the Cranpose overlay. The process then held no thread of the scene. A new start in the same process made a new scene that drew normally. While a game runs and the screens hide, the app leaves the overlay out of its frames. The scene then draws nothing. During a game it draws only while the console shows over the game. Nobody has measured that case. The app's library grew from 16 MB to 24 MB with Cranpose's GPU renderer. Cranpose first failed on the app's GL context. Its pipeline compiler threads could not create GL programs on a context that wgpu did not create. Cranpose now compiles pipelines on the drawing thread for such a context ([samoylenkodmitry/Cranpose#2241](https://github.com/samoylenkodmitry/Cranpose/pull/2241)). The scene first came out upside down. wgpu's GL output starts at the top row. OpenXR's GL images start at the bottom row. The eye camera now turns its vertical axis over (`debug.cranvr.xr_flip=0` turns this off). The first worn test on 16 November 2026 showed the overlay at a wrong depth: the ray went through the screens instead of ending on them. Each eye then drew into a box of 1680 by 1760 pixels. The Quest 3's left eye sees 54 degrees left, 40 right, 44 up and 55 down, which has another aspect. Cranpose fit the box's height, so its picture spanned 3% more angle across than the view. The compositor squeezed it into the view, toward each eye's own center. The two eyes' centers lie on opposite sides, so the shift changed the disparity by about 0.9 degrees. Things 2 m away looked about twice as far. Each eye now has a box of its view's aspect, 1629 by 1760 pixels. A test projects points through the eye's camera and checks that each lands where the compositor shows its direction. Nobody has worn the headset with this fix yet.
- Unit tests cover carrying, tilting, pushing and resizing a screen, the click hold, the buttons, the bar picture, the console's layout and requests, the console's Cranpose face (its hover, its presses, its height and its clear corners), the placing of an added screen, the gaze report and the inset's part. On the headset a test hook (`debug.cranvr.panel_test`) added a screen, set its quality, paused it and closed it. The user then added screens and resized one by hand. Not seen on the headset yet: the inset, the click hold and Beat Saber at the game scale of 0.7.
- On 16 November 2026 a test hook (`debug.cranvr.console_test`) set the console upright in front of a headset on the desk. The headset's screenshot showed the face in its frame with every label readable. The face first did not show: the app started its picture again every frame, so a picture that took longer than one frame never finished. The same hook sent a request to let the game choose its anti-aliasing and later one for at most 2x. The app wrote each to `app.json`. The console then marked the new choice. Nobody has pressed a console button in a worn headset yet. The app saves the console's place with the places of the screens. A `panels.txt` pushed by adb belongs to the shell user, and the app may not write it. On 16 November 2026 the app saved no move until 10:31 for that reason, and each start showed old places. The app now writes a new file and renames it over the old one. Remove the file with adb instead of pushing one. `debug.cranvr.console_test=exit` pressed Exit on a headset on the desk: the app ended its OpenXR session and its activity within half a second of the first menu, and the Quest showed its home. A new start in the same process connected again. In the first worn test the console did not show. The first menu came before the first head pose, so the console took its place by the stage's origin, under the floor. A new console now waits for the head's pose. The worn headset then showed it below and ahead. Later that day the console's face moved to Cranpose. The new face showed on the desk headset. The test hook value `press:Floor` pointed at the Floor button and pulled the trigger there: the room gave way to the floor. `press:Room` brought the room back. `exit` closed the session. A new start in the same process showed the face again. Its thread took no CPU time while the face stayed still. Nobody has pressed the Cranpose face in a worn headset yet.
- On 16 November 2026 the user saw the bar under the screen torn and skewed. wgpu shares the app's OpenGL context. It sets the upload row length for its own textures and leaves it set, so OpenGL read the bar picture's rows with another texture's length. The app now resets every upload setting before it writes the bar and the cursor pictures. The headset's own screenshot then showed the bar sharp, with every button label readable. The torn bar had led to presses on its buttons by mistake: one set the screen to 1920, one added a screen.
- An added screen made macOS put the TV back in its scaled 3008 x 1692 mode. macOS keeps display settings for each set of displays, and for the TV with cranvr's first added display it still had the scaled mode. The new screen then sat at x = 3008. The display helper now records each display's mode before it adds a screen and sets any changed mode back, in the same step that arranges the displays. In the repeated test the TV stayed at 3840 x 2160 and the new screen went to x = 3840.
- Pointer reports sent from the Quest moved the Mac's mouse to the right place on the 3840 x 2160 TV: the point at a quarter of the width and height went to (960, 540), the point at three quarters across and halfway down to (2880, 1080). A real trigger pull on the headset was not tested.
- The anti-aliasing limit and the sizes per game ran with Beat Saber only, on its first screen (the health and safety warning). No other game was tried. `DXMT_CLAMP_SAMPLE_COUNT=1`, for games that ignore the report, was not tried. On 16 November 2026 the Settings tab showed its new rows with the saved values. A press on Low latency wrote it to `app.json` and restarted the host with `--buffering-frames 1`. The list of learned sizes was empty, so its Forget button was not tried.
- The Mac's load is measured once, with a 60 fps desktop on a 6K display, while other programs ran. With the layer in view, cranvr-host used 20% of one CPU core and the GPU ran at 81%. With the headset asleep, cranvr-host used none and the GPU ran at 48%. In that test macOS's mediaanalysisd used two CPU cores. The Mac's temperature was not measured.
- Beat Saber on the base M5 ran at 37 to 41 fps with both captures stopped, after 31 to 34 fps before. The two runs used different conditions, so the gain is not proven.
- Beat Saber on the base M5 on 15 November 2026, at 120 Hz with the 0.7 render size. All runs stayed on the game's first screen, the health and safety warning, with the headset still on a desk: nobody pressed Continue. A song draws more. Other programs built code during the runs. The host ran without the desktop capture and without game sound. With the game's own 4x anti-aliasing the game made 91 frames a second. From the pose to the display took 68 ms. With 2x anti-aliasing the game made 119 frames a second and the time was 52 ms. The GPU ran at 85% with 4x and at 80% with 2x. The Mac's desktop alone kept it at 25%.
- The game's rate without the 120 Hz limit (the runtime paced at 240 Hz): 92 frames a second with 4x anti-aliasing at 0.7, 126 with 2x at 0.7, 140 with 4x at 0.5, 146 with 2x at 0.6, 213 with no anti-aliasing at 0.5. A song draws more than the first screen, so the margin above 120 matters.
- Every run in which the game held 120 frames a second took 44 to 52 ms from the pose to the display. The game took 10 to 14 ms of it. The encoder took 3.4 ms. The network took 5 ms. The decoder took 5 ms. The decoded frame waited 3 to 9 ms for the headset's render loop. The wait for the display took 16 ms. That last wait is two refresh periods at 120 Hz in every run that held the rate. This points at the Quest's compositor. Nothing on the Quest measured it.
- With one buffered frame the decoded frame waited 2.8 and 4.8 ms in two runs, against 7.4 to 9.1 ms in three runs with two. Two of those three also had Unity's `max_queued_frames=1`, which changed nothing. The whole time dropped from 48 to 50 ms to 44 to 46 ms. The headset showed 583 to 599 frames per 5 seconds with one buffered frame, against 591 to 600 with two. On the Galaxy Tab over USB one buffered frame dropped most frames (see the probe results below). A still headset shows no Wi-Fi jitter, so this is not proof that it stays smooth in play.
- Unity's `max_queued_frames=1` in Beat Saber's settings.ini changed nothing.
- A Metal System Trace of Beat Saber's first screen at 2x anti-aliasing and the 60% render size: the game used about 11 ms of GPU time per frame. DXMT's separate MSAA resolve passes took 23% of it, 7.4 passes per frame. Beat Saber renders both eyes into one two-layer texture and resolves each layer apart, so DXMT cannot fold these passes into the render pass. cranvr's DXMT patch 0007 drops their needless store of the samples: a pass then took 0.31 ms instead of 0.38 ms, about 0.5 ms per frame. The picture on the Quest looked the same.
- In the same traces WindowServer used 390 to 440 ms of GPU time per second while the game ran, about 40% of the game's own GPU time. The TV ran a scaled mode: macOS draws its 3008 x 1692 point desktop at 6016 x 3384 pixels and shrinks it to the 3840 x 2160 panel. Chrome and the Claude app used another 120 to 250 ms per second. Shrinking the game's own desktop window from 1280 x 720 to 160 x 90 changed the game's rate by no clear amount: 142 and 124 frames a second against 143 and 149, without the 120 Hz limit.
- With the TV switched for the test to an unscaled mode (3840 x 2160 points at 1x, 60 Hz), WindowServer used 157 and 161 ms of GPU time per second instead of 442. Beat Saber's own GPU time per frame fell from about 11 ms to 7.6 and 8.8 ms, as less work from WindowServer competed with it. Its frame rate moved by no clear amount: 100 and 90 frames a second against 91.
- Through the cranvr app's own host, with the desktop view at 60 captures a second, the game at 2x anti-aliasing made 113 to 117 frames a second in three runs and 100 in one. The desktop layer and its captures stopped while the game ran. Game sound made no clear difference: 100 and 114 frames a second with it, 117 and 116 without. The Mac's load fell during these runs.
- DXMT already folds the anti-aliasing resolve into the render pass. It still stores every sample to memory after each pass, which explains the cost of 4x anti-aliasing on the Mac's GPU.
- All tests ran with the headset lying on a desk and `--screen-in-view`. The upright screen in front of a worn headset was not tested.
- The host sends the whole display while any part of it is in view. It does not cut the picture down to the part the eye sees.
- During the Quest runs the headset's poses now and then reached the host up to about 95 ms late, so the eye stream ran at 82 to 104 fps instead of 120. Runs with and without the virtual display both showed it. A loopback run on the Mac showed no such gaps, so the host's event thread is not the cause. The cause on the network side is open: the ceiling near 100 ms is close to one Wi-Fi beacon interval, which points at power save on one of the two devices. The layer itself does not depend on these poses.

## Games

Games need Wine with cranvr's DXMT fork and BasaltVR's OpenXR runtime. The app's Play tab sets them up in three steps:

1. Rosetta 2, which runs Wine's Intel code. macOS asks for an administrator's password.
2. Game support: the app downloads `cranvr-game-support-macos.tar.xz` (about 240 MB) from the public release at https://github.com/samoylenkodmitry/cranvr-game-support, checks its SHA-256, unpacks it and installs it with `setup-wine.sh`. It takes about 1.2 GB on disk. `scripts/install-game-support.sh [ARCHIVE|URL]` does the same without the app.
3. Steam for Windows (see below).

From an empty home folder the install took 79 seconds with the download. The OpenXR runtime's check through the Khronos loader passed, and the runtime's smoke program connected to the host.

To build game support from source instead (about an hour, mostly LLVM):

```bash
xcodebuild -downloadComponent MetalToolchain
softwareupdate --install-rosetta
brew install cmake meson ninja mingw-w64
scripts/build-basaltvr.sh
scripts/setup-wine.sh third_party/basaltvr/src
```

`build-basaltvr.sh` clones BasaltVR at a pinned commit into `third_party/basaltvr/src`. It applies cranvr's patches and builds the OpenXR runtime and the DXMT fork. `setup-wine.sh` puts Wine 11.10 with the DXMT fork in `~/Library/Application Support/cranvr/wine`. It registers the runtime in the Wine prefix and checks it through the OpenXR loader. `setup-wine.sh` also takes an unpacked game support archive instead of the BasaltVR folder.

`scripts/package-game-support.sh` packs a built tree into the archive and its `.sha256` in `target/game-support`. The public repository holds the archive as a release asset, with the license texts and `SOURCES.md`. The app downloads `releases/latest/download/cranvr-game-support-macos.tar.xz`, so a new release replaces the old one for new installs. DXMT's `winemetal.so` names its shader sources by their path on the Mac that built it.

Start the host first: it writes `runtime-control.txt` into its config dir. Then start a game from the window ("Game") or with the script:

```bash
scripts/run-game.sh 'C:\basalt\runtime\d3d11_runtime_smoke.exe' --render-frames 7200 --visual
```

The smoke program draws a green marker at the left controller and a pale grid. A squeeze of the left grip grows the marker and sends a haptic pulse.

### Steam games

Steam runs in the prefix with BasaltVR's web helper shim (`scripts/setup-steam.sh`). On 16 November 2026 the user started Beat Saber from the console and played a song on Expert in a worn headset. The game made 109 to 120 frames a second at the 60% render size with 2x anti-aliasing. From the pose to the display took 48 to 55 ms. The user found the picture not very sharp. Beat Saber is the only game installed in cranvr's Steam so far. The steps:

1. In the window's "Play" tab, press "Open Steam". Log in. Install the game.
2. The tab lists the installed games. Press "Play" next to one: the window starts it by its .exe with the host's environment.
3. Without the window, start Steam with `scripts/run-game.sh --steam`. While Steam runs, start the game by its .exe:

   ```bash
   scripts/run-game.sh 'C:\Program Files (x86)\Steam\steamapps\common\Beat Saber\Beat Saber.exe'
   ```

A rebuild of BasaltVR gives the web helper shim new bytes. The app then reports that Steam needs its setup step, and "Install Steam" runs `setup-steam.sh` again. The script puts only Valve's own helper into the backup that the shim starts; an older shim just gets replaced. Before 16 November 2026 it copied the old shim over the backup of Valve's helper.

Do not use `steam.exe -applaunch`: a game that Steam starts gets Steam's environment, not the one with the runtime's control file. For a game under `steamapps\common` the script and the window find the Steam app ID in Steam's `appmanifest` files. They set `SteamAppId`, which the game's Steam check needs.

The runtime speaks OpenXR with D3D11. Games that use OpenVR only need OpenComposite (see BasaltVR's `docs/BOTTLES.md`). Google Earth VR and Serious Sam VR: The Last Hope bring only `openvr_api.dll`, so they do not start yet. The app looks for a game's program three folders deep, preferring a 64-bit folder: Serious Sam keeps it in `Bin/x64`.

A game that the app or `scripts/run-game.sh` starts draws nothing into its Mac window: `DXMT_SKIP_PRESENT=1` (cranvr's DXMT patch 0008) sends each frame's GPU work without the present, as DXMT does for a minimized window, but tells the game that the present worked. The window stays empty. `CRANVR_MAC_MIRROR=1` for the script draws it again. Steam's own window still draws. Beat Saber ran with it on the desk and showed in the headset.

On 16 November 2026 the user allowed a download of OpenComposite's 64-bit `openvr_api.dll` (2.4 MB) from its official site, opencomposite.znix.xyz. It sits beside the original DLLs of Google Earth VR and Serious Sam VR, which stay as `openvr_api.dll.cranvr-original`, with an `opencomposite.ini` that sets `initUsingVulkan=false` and `enableAudio=false`. Neither game runs yet, also when the app starts them with Steam running. Google Earth VR starts and then waits without reaching VR: OpenComposite logs only that it read its settings. Serious Sam VR quits within seconds and prints nothing. The causes are open.

SUPERHOT VR renders both eyes in one pass ("single pass instanced") into a swapchain with two slices. DXMT leaves the second slice empty, so the right eye stayed black. `BASALT_MAX_ARRAY_SIZE=1` (cranvr's runtime patch 0003) makes the runtime reject two-slice swapchains, as BasaltVR's design notes still describe, and Unity then draws each eye into a swapchain of its own. The app and `scripts/run-game.sh` set it for SUPERHOT. Both eyes then showed the game, at about 82 frames a second instead of 119 on the desk. Beat Saber also renders into two slices and shows both eyes, so the limit stays off for other games. The runtime copies each slice into a surface of its own and sends the surface's place (image × slices + slice) with each frame. On 16 November 2026 the host counted the slices a second time: it dropped two of every three frames of every game as frames with an unknown image, and Beat Saber ran at 40 frames a second until the host went back to the plain place. SUPERHOT also asks for a floor-level space, which the runtime does not offer yet: it then logs a warning many times a second. The Steam version of Beat Saber has used OpenXR since version 1.29.4, so it needs no OpenComposite. Only an older version, as installed by BSManager for mods, needs it.

## Test without a headset

```bash
cargo test --release
cargo test --release -p cranvr-app -- --ignored
./target/release/cranvr-host --fps 90 --trust-all --tcp --config-dir /tmp/cranvr-test &
HOME=/tmp/cranvr-probe-home ./target/release/cranvr-probe --seconds 10
```

The probe exits with code 0 when at least half of the expected frames decode with the right eye colors. `HOME` keeps the probe's ALVR client settings out of your own Library folder. The ignored app test starts the host the way the window does and connects the probe.

The native protocol (`cranvr-proto`) runs the same test. The host listens on `127.0.0.1:9950` unless `--trust-all`, a paired headset or `--pair CODE` allows a headset on the network. It opens none of ALVR's ports, so it runs beside an ALVR host:

```bash
./target/release/cranvr-host --protocol native --config-dir /tmp/cranvr-test-native &
./target/release/cranvr-probe --protocol native --seconds 10
./target/release/cranvr-probe --protocol native --seconds 10 --tcp            # media over TCP, as over USB
./target/release/cranvr-probe --protocol native --seconds 10 --drop-percent 3 # lets the parity recover packets
```

`scripts/e2e.sh --native` does the same with a report. The native mode sends the Mac's sound and, with `--desktop`, the desktop as a stream for the client's desktop panel (see "The desktop panel and the sound"). `--protocol alvr` stays the default.

The game path without Wine:

```bash
./target/release/cranvr-probe --game --seconds 14 &
./target/release/cranvr-fake-game --control /tmp/cranvr-test/runtime-control.txt --seconds 10
```

`cranvr-fake-game` speaks the runtime's protocol and shares IOSurfaces like the Wine runtime. Its eye badges have swapped colors, so `--game` tells its frames from the test scene.

The ALVR client and the ALVR server both bind UDP port 9943 for discovery. Two processes on one Mac cannot both hold it. For this reason the probe uses a copy of ALVR's client core in `probe/alvr-client-core-loopback`. That copy sends its discovery message from a random port. The probe also needs `--tcp` on the host, because the UDP stream port has the same conflict.

## End-to-end test

`scripts/e2e.sh` runs the host, a client and an optional game, then writes a JSON report. The report holds every 5-second stats line of the host and the median and worst of each number. The numbers are the frame rate, the render and encode times, and the client's latency split by stage: game, encode, network, decode, decoder queue, client compose, vsync wait and the total from the pose to the display. The script exits with 0 when the host and the client both reach 90 percent of the target rate after the first stats line. With the probe as the client it must also show 95 percent of the frames that reached its decoder: ALVR's client frame rate comes from the gaps between shown frames, so it stays near the target while the client drops frames.

```bash
scripts/e2e.sh --fps 120 --game
scripts/e2e.sh --android R52R40K1F2A --seconds 20
scripts/e2e.sh --android --host-arg --buffering-frames --host-arg 1.5
scripts/e2e.sh --android --wifi
scripts/e2e.sh --quest --fps 90 --seconds 60
scripts/e2e.sh --android --seconds 30 --wine 'C:\basalt\runtime\d3d11_runtime_smoke.exe' \
    --wine-arg --render-frames --wine-arg 14400 --wine-arg --visual
```

`--wine` runs a Windows OpenXR program in cranvr's Wine stack without sound. The probe then counts every frame of the right size, since a real program draws no eye badges. The probe plays no game sound: it measures what arrives. The run fails when the probe gets the Mac's sound for less than 80 percent of the stream time. The capture sends silence too, so a run needs no sound playing.

The client comes in three kinds:

- Without a flag, `cranvr-probe` runs on this Mac and decodes with VideoToolbox.
- `--android` builds `cranvr-probe` for Android with `cargo ndk` and runs it on a phone or a tablet attached by USB. It uses ALVR's client core with its MediaCodec decoder and frame buffering, unchanged. A display loop takes one image per refresh, like the Quest's compositor. It samples the eye colors of every frame it shows with OpenGL ES through ALVR's EGL context, as ALVR's client reads frames: a Qualcomm decoder writes them in a compressed format that only the GPU reads. The host reaches the device through `adb forward` of TCP ports 9943 and 9944. With `--wifi` it connects to the device's Wi-Fi address with a UDP stream, as a Quest connects. The run holds the device lock of Cranpose's device scripts.
- `--quest` starts the ALVR app on a Quest attached by USB. adb keeps its display on without a face in the headset. The Quest and the Mac must share a network. `--quest --cranvr-client` starts cranvr's Cranpose client instead (install it with `scripts/build-client.sh --install SERIAL`). The host then runs with `--protocol native --trust-all` on `--native-port` (default 9950). A headset keeps one host key for each address and port. Pick a port that the headset has not used with another host at this Mac's address, such as 9961. The app gets this Mac's address through its launch extras. The run keeps the host's key in `target/e2e/host-identity.txt`, because a headset refuses a known address with a new key.

The Galaxy Tab S7+ (Snapdragon 865) is the closest test device to a Quest: the XR2 of the Quest 2 comes from the same chip, and both use Qualcomm's video decoder. Compare two settings with two runs, A then B, and keep both reports.

Medians at 90 Hz over USB, 20 seconds each, with the strict rate control of the time:

| Device and setting | Client fps | Frames shown | Decode | Decoder queue | Network | Total |
|---|---|---|---|---|---|---|
| Tab S7+, ALVR's 2 buffered frames | 90.5 | 1719 of 1772 | 6.0 ms | 11.3 ms | 3.0 ms | 42.8 ms |
| Tab S7+, 1.5 buffered frames | 86.1 | 1600 of 1776 | 5.9 ms | 7.7 ms | | 38.3 ms |
| Tab S7+, Android's low-latency decoder key | 89.7 | 1732 of 1776 | 6.1 ms | 14.7 ms | | 43.6 ms |
| Pixel 9 Pro, ALVR's 2 buffered frames | 89.9 | 1645 of 1777 | 7.2 ms | 11.3 ms | 2.0 ms | 41.2 ms |

Medians at 120 Hz on the Tab S7+ over USB, 20 seconds each, with the strict rate control of the time:

| Setting | Host fps | Client fps | Frames shown | Decoder queue | Total |
|---|---|---|---|---|---|
| Test scene, ALVR's 2 buffered frames, run 1 | 119.9 | 119.5 | 2249 of 2335 | 10.8 ms | 37.3 ms |
| Test scene, ALVR's 2 buffered frames, run 2 | 119.5 | 121.0 | 2272 of 2350 | 8.5 ms | 35.7 ms |
| Test scene, 1.5 buffered frames | 119.3 | 113.5 | 2058 of 2343 | 6.5 ms | 33.8 ms |
| Fake game through the runtime protocol | 117.2 | 118.0 | 2208 of 2313 | 7.6 ms | 34.5 ms |

At 120 Hz decode took 5.8 ms, encode on the Mac 6.0 ms and the USB link 2.9 ms. A third run with the default showed 96.2 percent of the frames that reached the decoder: ALVR drops about 4 percent while the stream runs, as frames arrive unevenly. 1.5 buffered frames saved about 2 ms and showed 91.5 percent, so ALVR's 2 frames stay the default.

The probe cannot measure the wait from its submit to the display: it reports one refresh, 8.3 ms at 120 Hz and 11.1 ms at 90 Hz, and each total above includes it. A Quest's compositor reports its own wait, so these totals are not Quest totals.

Two runs with the same setting differ by up to 5 ms in total, so a gain smaller than that needs more pairs. The decoder queue holds about one frame: frames reach the device with gaps from 7 ms (p10) to 15 ms (p90) around 11 ms over USB, and ALVR keeps a frame in reserve against that jitter. One buffered frame made ALVR drop most frames. The low-latency decoder key changed nothing on the Tab.

Before a phone or Quest run, the harness reads the device's network over adb. It warns, in the output and in the report, when a VPN on the device routes this Mac's address, when the device uses 2.4 GHz Wi-Fi, and when its Wi-Fi link runs below 400 Mbps. On both test devices Tailscale routes 192.168.50.0/24 through an Orange Pi subnet router; see "A VPN on the headset" above. `--pause-tailscale` disconnects Tailscale on the device for the run and connects it again at the end. An app exclusion does not cover the probe, which runs as the shell user. The report records the protocol of each run.

Medians at 90 Hz over Wi-Fi on the Tab S7+ (2.4 GHz, 50 Mbps), 20 seconds each, with the strict rate control of the time:

| Setting | Mac fps | Frames shown | Network | Total |
|---|---|---|---|---|
| UDP, Tailscale paused, run 1 | 81.1 | 95.2% | 13.4 ms | 53.7 ms |
| UDP, Tailscale paused, run 2 | 80.8 | 95.6% | 14.1 ms | 55.2 ms |
| TCP, Tailscale paused, run 1 | 81.2 | 92.1% | 15.8 ms | 57.1 ms, spikes to 220 ms |
| TCP, Tailscale paused, run 2 | 80.0 | 92.6% | 15.9 ms | 56.4 ms |
| TCP, Tailscale on | 80.1 | 92.2% | 15.9 ms | 56.7 ms |
| UDP, Tailscale on | | no video | | |

The Mac drew about 81 of 90 frames a second: the Tab's 2.4 GHz uplink delivers poses in bunches, and the host draws only for a new pose.

At 120 Hz with the probe on the Mac, the test scene ran at 119.9 fps with 14.1 ms from the pose to the display.

### Measure a game with a Quest on the desk

`scripts/perf/` holds the tools behind the Beat Saber numbers above.

```bash
scripts/perf/game-run.sh aa2 20
CRANVR_MAX_MSAA=0 scripts/perf/game-run.sh aa-game 20
HZ=240 scripts/perf/game-run.sh uncapped 20
scripts/perf/metal-trace.sh bs
python3 scripts/perf/gpu-by-kind.py target/perf/bs.xml "Beat Saber"
```

`game-run.sh` needs the cranvr app running with "cranvr client" as its Headset app and a headset that is paired with it (see "Pairing"). It wakes the headset without a wearer and starts cranvr's Cranpose client and the game without sound. `CRANVR_HOST=ADDRESS:PORT` gives the client the Mac's address. Without it the client uses the address of its last connection. It prints one summary line of the rates and the latency stages. Then it gives the headset its guardian back. The proximity sensor stays closed, so the headset stays awake. `metal-trace.sh` records 5 seconds of GPU work of every process on the Mac while a game runs. `gpu-by-pass.py` sums that work per process and per pass. `gpu-by-kind.py` sums it per kind of pass. `display-modes.swift` lists the modes of the external display or holds one for its own run. Check `uptime` before a run: builds in other programs halve the rates.

## Other runtimes

### VeRantyx, tried on 16 November 2026

[VeRantyx](https://github.com/Ag3497120/VeRantyx) (commit 422f52e5def7) runs the Windows SteamVR on VXRuntime, a Windows runtime of its own. It does not use Wine. Its ALVR bridge is ALVR 20.14.1, so cranvr's former Quest client connected to it. Neither game ran in the headset:

- Google Earth VR: its crash reporter, `crashpad_handler.exe`, failed at its start because VXRuntime has no `NtSuspendProcess`. Earth.exe waited 60 seconds for the reporter, then ended itself. SteamVR kept streaming its empty room.
- Beat Saber 2.18.4 speaks OpenXR only. VXRuntime registers no OpenXR runtime, so the game started flat on the Mac. With SteamVR's OpenXR runtime registered by hand, SteamVR refused the game: "Returning HmdNotFound because the client-side presence check failed". A driver manifest with `"hmd_presence": ["*.*"]` changed nothing. The games that VeRantyx lists speak OpenVR.
- SteamVR's empty room, with no game, reached the headset at 120 fps. ALVR reported 73 ms from the pose to the display: 34 ms on the Mac, 7 ms on the network, 7 ms in the decoder and 25 ms on the headset. These numbers come from no game load, so they do not compare with a game through cranvr.

## Network exposure

- UDP 9943 and mDNS (UDP 5353) listen on all interfaces. The headset finds the host through them.
- ALVR's web API (TCP 8082) has no login. cranvr's copy of the server core (`host/alvr-server-core-local`) binds it to 127.0.0.1. Stock ALVR binds it to every interface.
- The game link listens on 127.0.0.1 only. A game must show the token from `runtime-control.txt` (mode 0600).

Use `--trust HOSTNAME` (or "Pair" in the window) for real use. `--trust-all` accepts any ALVR client on the network and is meant for tests.

## Next milestones

`docs/plan.md` has the milestones. It covers a native mode with a cranvr Quest client and the optimizations that mode allows. `docs/cranpose-xr.md` covers Cranpose apps for VR stores.

## License

MIT. ALVR is MIT. `probe/alvr-client-core-loopback` and `host/alvr-server-core-local` are ALVR's client core and server core with changes marked `cranvr`. BasaltVR is MIT.
