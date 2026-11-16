# cranvr

cranvr streams VR from an Apple Silicon Mac to a Meta Quest. The Quest runs the stock ALVR client: any 20.x version, such as 20.14.0 from the Meta store or v20.14.1 from GitHub. ALVR compares only the major version. The Mac side is Rust. It uses ALVR's server core (MIT) for the network protocol, tracking and bitrate control. It renders and composes with Metal. It encodes HEVC with VideoToolbox.

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
| `host` | `cranvr-host`: the ALVR server core, the frame loop, the Metal shaders (`host/src/pattern.metal`), the game link (`runtime_link.rs`) and the desktop (`desktop.rs`). |
| `app` | `cranvr`: the window. It runs `cranvr-host` and shows its status. |
| `probe` | `cranvr-probe` (a headless ALVR client) and `cranvr-fake-game` (a stand-in for a game), for tests. |
| `quest` | `cranvr-quest`: cranvr's own Quest app, the start of native mode. Its own Cargo workspace. |
| `scripts` | Build and run the Wine side for games. |
| `third_party/basaltvr` | cranvr's patches to BasaltVR's runtime and DXMT fork. |

## Build

```bash
cargo build --release
```

The first build downloads ALVR v20.14.1 from GitHub with its submodules.

## Run with a Quest

1. Install the ALVR client on the Quest: 20.14 from the Meta store, or `alvr_client_android.apk` of v20.14.1 from the ALVR GitHub releases. Its major version (20) must match the server core's. A v21 nightly build does not connect. The window then shows a notice.
2. Put the Mac and the Quest on the same network. Use 5 GHz or 6 GHz Wi-Fi. Connect the Mac by cable if you can.
3. Start the window:

   ```bash
   ./target/release/cranvr
   ```

4. Press "Start host". Open ALVR on the Quest. The Quest's hostname (such as `1234.client`) appears under "Headset". Press "Pair".
5. On the first connection the client shows "The streamer is restarting" once. It reconnects by itself.

### cranvr's Quest app

`quest/` holds cranvr's own Quest client, the base of native mode. It is ALVR's OpenXR client of v20.14.1 with its own package, `dev.cranvr.client`, labeled "cranvr". It installs beside the ALVR app from the Meta store. Its protocol is still ALVR's v20 protocol, so the host serves both apps the same way. Native mode will change the protocol in the host and in this app together.

```bash
scripts/build-quest.sh --install
```

The script builds everything from source on this Mac: the client with `cargo ndk`, the Khronos OpenXR loader from the OpenXR SDK source in BasaltVR's build tree. It packages the APK with the Android SDK's `aapt2` and signs it with the Android debug key. A Quest in developer mode installs it over USB; it then shows under Unknown Sources in the app library. It has not run on a Quest yet. `scripts/e2e.sh --quest --cranvr-client` runs the harness with it.

#### The window display on Android

On a device without an OpenXR runtime, such as the Galaxy Tab S7+ or the Pixel 9 Pro, the app shows the stream in a window (`quest/src/flat`). The device is the head:
- Its gyroscope (400 Hz) and game rotation vector give the pose.
- Its screen shows both eyes side by side.

The app asks Android for 120 Hz. It then shows blank frames for a moment and measures the vsync rate by the median gap between vsyncs. The host streams at that rate. A compositor with variable refresh, such as the Pixel's, gives an idle window a lower rate, so the window must show frames while the app measures.

At each refresh the app waits until 6 ms before the next vsync. It then takes the newest decoded frame and the newest sensor pose. ALVR's stream renderer warps the frame from the pose the host drew it for to that newest pose. This is what a headset's compositor does. The app then swaps. The decoder hands out only the newest decoded frame: the warp hides a late frame, so ALVR's queue against network jitter is not needed. EGL's frame timestamps give the real display time of every frame. The app logs two numbers every 2 seconds:
- **Round trip:** the sensor sample of the pose that the host drew for, to the display of that frame. The pose goes to the Mac. The frame comes back.
- **Motion to display:** the newest sensor sample, which warps the frame, to the display.

```bash
scripts/e2e-flat.sh --seconds 30 --warp-check
```

The script installs the app, starts the host and runs the stream over Wi-Fi with UDP. `--serial` picks the device. `--usb` runs it over `adb forward` with TCP instead. `--warp-check` starts the app twice more and checks the picture (`scripts/flat-warp-check.py`). The first start turns only the warp by 10 degrees (`debug.cranvr.warp_yaw_deg`): the eye badges must move right by that angle. The second start turns the head by 10 degrees in the world (`debug.cranvr.world_yaw_deg`), for the host and for the warp: the badges must stay. A yellow line fixed in the world must move right. A latency number would pass with a broken warp or a mirrored picture; these checks would not. On a phone with a camera cutout the window is narrower than the screen: the script finds the window in the screenshot. The device must stand upright for the second check. A phone that lies flat looks down at the floor, so the yellow line is out of view. The second check found a mirror in an earlier build: the turn from the sensor axes to the screen had the wrong sign.

Medians on the Galaxy Tab S7+ with its panel at 60 Hz ("Motion smoothness: Standard"). The Tab uses 2.4 GHz Wi-Fi.

| Step | Link, seconds | Round trip | Motion to display | Swap to display |
|---|---|---|---|---|
| First run: a frame per swap, ALVR's decoder queue | USB, 16 | 106 ms | 84 ms | 61 ms |
| Paced to 6 ms before the vsync | Wi-Fi, 20 | 65 ms | 27.4 ms | 19.2 ms |
| Newest decoded frame instead of ALVR's queue | Wi-Fi, 30 | 49.6 ms | 27.4 ms | 18.7 ms |

The warp check moved both badges by 10.0 degrees, within 0.07 degrees of the turn.

Medians on the Pixel 9 Pro with its panel at 120 Hz, over 5 GHz Wi-Fi with UDP:

| Render lead | Seconds | Shown fps | Round trip | Motion to display | Swap to display |
|---|---|---|---|---|---|
| 4 ms | 20 | 60.6 | 46.3 ms | 24.8 ms | 17.6 ms |
| 5 ms | 20 | 91.3 | 41.0 ms | 18.0 ms | 12.2 ms |
| 6 ms (default) | 30 | 119.0 | 41.0 ms | 18.5 ms | 13.0 ms |
| 7 ms | 20 | 119.6 | 39.9 ms | 19.6 ms | 13.9 ms |

The host streamed at 119.5 fps. About 100 of those frames per second reached the display as new frames. The other refreshes showed the previous frame, warped to the new pose. With a lead of 4 ms the render missed every second refresh. With 5 ms it missed about one refresh in four.

The first warp check moved both badges by 10.0 degrees, within 0.05 degrees of the turn. In the second check both badges stayed in place. The script found no yellow line: the Pixel lay flat on its back, by its game rotation vector, so the view looked down at the floor. The mirror check has not run on the Pixel.

The pacing changed for the Pixel: the measurement of the vsync rate and the guard on the period estimate. The Tab has not run since that change.

The Pixel has a Mali GPU. Mali refuses a fragment shader without a default precision, so ALVR's frame copy failed to compile there. Adreno GPUs, in the Quest and in the Tab, accept such a shader. `quest/alvr-graphics-local` is ALVR's graphics crate with one line added: the copy sets high precision, which every OpenGL ES 3.0 GPU supports in fragment shaders.

On the Tab at 60 Hz, motion to display adds up from these parts:
- The newest sensor sample is already 5.7 ms old when the warp reads it (median of `pose_age_ms`).
- The render takes about 3 ms.
- The display shows the swap 18.7 ms later.

SurfaceFlinger composes every refresh and shows a composition one refresh later (`eglGetCompositorTimingANDROID` reports 16.7 ms), so the last part cannot fall below one refresh.

On the Pixel at 120 Hz the sensor part and the display part are smaller. The render takes about as long:
- The newest sensor sample is 2.4 ms old when the warp reads it.
- The render takes 3.4 ms.
- The display shows the swap 13.0 ms later. The swap waits about 2.6 ms for the compositor. The compositor then needs 10.5 ms to the display, by `eglGetCompositorTimingANDROID`.

Motion to display is 18.5 ms on the Pixel against 27.4 ms on the Tab at 60 Hz. The compositor's own delay sets the floor on both.

Front-buffer drawing (`debug.cranvr.front_buffer 1`) skips the window's buffer queue but not that refresh. Its display times are derived from the compositor's own timing, since a frame without a swap gets no display time; they came out at 41 ms. The Pixel's driver offers no EGL config with a mutable render buffer, so front-buffer drawing does not start there. A shorter lead than 6 ms missed refreshes, since the render sometimes takes 5 ms.

A round trip under 5 ms through the Mac is out of reach on this link. A ping between the Mac and the Tab takes 3.8 ms at best and 12 ms on average on 2.4 GHz Wi-Fi. Encode takes 2.5 to 3.5 ms. Decode on the Tab takes about 6 ms. Under 5 ms from motion to display needs a compositor that warps with a pose read just before the display, as a Quest's does. It also needs no extra refresh after the warp.

### A VPN on the headset

A headset with Tailscale that accepts a subnet route for its own network sends its UDP to the Mac through the subnet router. The router replaces the sender address, and ALVR drops every stream packet that does not come from the headset's own address. The handshake still works over TCP, because the Mac opens that connection and Android answers it on Wi-Fi. The headset then connects, gets no video and leaves after about 5 seconds, again and again.

The host reports this after two such connections in a row. With "Network: Auto" in the settings the window then switches the stream to TCP and says why. TCP gets through the VPN: the Mac opens the stream connection too. On the Galaxy Tab S7+ over 2.4 GHz Wi-Fi, TCP took about 2 ms more than UDP in the network stage and had resend spikes. The better fix is on the headset: exclude the ALVR app from the VPN, or stop using the subnet route there. The host also warns when this Mac reaches the headset through a VPN tunnel of its own.

The window saves its settings in `~/Library/Application Support/cranvr/app.json`. A change of refresh rate, bitrate, foveation or desktop restarts the host. The headset reconnects by itself.

Without the window:

```bash
./target/release/cranvr-host --fps 120 --trust 1234.client --desktop
```

`cranvr-host --help` lists every option.

### Game sound

The host captures what this Mac plays, from every app but the host. The headset plays it. The capture takes the sound before the Mac's volume: in a test at a Mac volume of 15%, a tone reached the client at its full source level. Turn the Mac down while you play, or the Mac plays the sound too. The capture needs Screen Recording, like the desktop. "Game sound in the headset" in the settings turns it off; `cranvr-host --no-game-audio` does the same. The headset's microphone does not reach the Mac yet.

ALVR's client buffers about 50 ms of sound. A rhythm game such as Beat Saber has an audio latency setting: set it once with its calibration.

### What you should see

With the desktop off, the host shows a test scene:
- a sky with a grid line every 15 degrees and brighter lines every 45 degrees;
- a floor with 50 cm squares and a green line every meter;
- a yellow ball at the left controller and a cyan ball at the right controller;
- a small red square near the left edge of the left eye and a blue square near the left edge of the right eye.

With the desktop on, the host shows your main display on a screen 1.4 m in front of you, in a dim room. A game replaces both scenes while it runs.

### The Mac desktop

- Point the right controller at the screen to move the mouse. Pull the trigger to click. Push the thumbstick up or down to scroll. In cranvr's Quest app either controller points: the one whose trigger was pulled last.
- Click the right thumbstick to put the screen in front of you again.
- In cranvr's Quest app a bar under the screen is its handle. Point at it and hold the trigger to carry the screen. The screen keeps its angle to the ray: a turn of the wrist turns, tilts or rolls it by as much. Within 4 degrees of level its top edge snaps level. A click on the right thumbstick brings the screen upright in front of you. Push the thumbstick of that hand to move the screen away or near, also without the trigger while you point at the handle. Pull a corner, just outside the picture, to resize the screen. The app keeps the screens' places for the next start.
- A trigger pull turns the controller a little. A press on the picture therefore takes the point where the ray pointed 80 ms before. It holds that point until the ray turns more than 10 degrees in the first 0.35 seconds of the press, or more than 4 degrees later: a click from a shaky hand stays a click, and a held press drags. The rays go through a one-euro filter, so a resting hand's tremor barely moves them and a fast turn follows with little lag. Each press on a picture adds a line to `panels-presses.txt` in the app's files folder: its length, the ray's largest turn and whether it dragged.
- Buttons right of the handle pause and resume the screen's picture, set its quality (Auto, then 1920, 2560 or 3840 pixels across) and add a screen. The handle and the buttons are one picture at 1600 pixels per meter that the compositor shows as its own layer, with the same filters as the screen: drawn in the overlay layer, their text shimmered. "Add display" makes the host add a virtual Mac display of 1920 x 1080 points, to the right of the other displays, with its own screen in VR. "Close" on an added screen removes it and its display. Four screens at most.
- In cranvr's Quest app the console stands below and ahead of you, tilted up like a desk. It shows the stream's rate and latency and the game that runs. It has a button for each installed game and buttons for the main settings: the refresh rate, the game's render size, the anti-aliasing limit and low latency. A press on a game's button starts the game. A change of the anti-aliasing limit applies at the next game start. Every other setting restarts the host, so the stream stops for a few seconds. The handle at the console's top carries it like a screen. A click on the right thumbstick brings the console back below and ahead of you when the ray points at the console or at nothing. A click at nothing brings the first screen back too. The console hides while a game runs, as the screens do.
- Each added display lives in a helper process (`cranvr-host --display-helper W H SERIAL`). The host's own view of the displays goes stale, because its main thread runs no event loop. A second display made in the host then failed to arrange, and macOS mirrored the main display onto it at 1920 x 1080. The helper arranges its display for the login session; an arrangement for the app alone was undone when the helper ended, and macOS mirrored the TV for a moment.
- macOS asks for two permissions on first use. Screen Recording lets the host capture the display. Accessibility lets the controller move the mouse. Without Accessibility the screen still shows. The app asks for each missing permission when it starts. A card at the top of the window has a button for each one until macOS allows it. The host notices a new Accessibility permission within 2 seconds, without a restart.
- `--desktop-fps` sets how often the host captures the display (default 30). Each capture of a 6K display costs the window server about 3 ms of GPU time. At 60 captures per second the stream lost about 10% of its frames in a test.
- `--game-scale F` (default 0.7) makes a game render each eye at this share of the stream's eye size. Foveated encoding sends about a third of the eye's pixels anyway. At the full size a base M5 drew Beat Saber at 37 to 41 frames per second. The Settings tab sets it as "Game render size".
- A game that makes fewer frames than the headset's refresh rate added 10 to 18 ms of latency in tests. The headset's own loop then waits for frames and slows to about 100 frames a second. The host warns in the log 20 seconds after the game connects, when the game stays below 95% of the rate.
- Games under Wine use at most 2x anti-aliasing by default ("Game anti-aliasing" in the Settings tab, `CRANVR_MAX_MSAA` for `scripts/run-game.sh`, 0 for no limit). cranvr's DXMT patch then reports 4x and 8x as unsupported, so a game that checks picks 2x. Beat Saber set to 4x checked and used 2x: DXMT logged its first MSAA texture with 2 samples. Its first screen ran at 115 frames a second with the limit and at 94 without, in one pair of runs (app host, 60% render size, 120 Hz). The limit applies when a game starts. A game started from the Steam window gets it after Steam restarts.
- The host keeps a render size per game. A game that stays below 95% of the headset's rate for 10 of the 12 stats windows in its first minute after loading gets a smaller share of the render size at its next start. A game reads its size only when it starts. The step aims 10% under the rate, with a floor of 40%. When a smaller size did not raise a game's rate by 5%, the host stops lowering it: the size is not what limits that game. That last step stays in place until the user forgets the learned sizes. The runtime names the game in `game-program.txt`, and reads the shares from `game-sizes.txt` in cranvr's config folder. The Settings tab lists the learned sizes and forgets them. In a test, Beat Saber at 95 frames a second got 85% of its size, and its next start rendered 1038 x 1086 instead of 1228 x 1286. At 2x anti-aliasing and the 60% render size, Beat Saber's first screen ran at 108 to 115 frames a second through the app's host, close to the 114 at which a window counts as slow. In a 100-second run, while other programs built code, the host set 90% for its next start. The test entries were forgotten afterwards.
- While a game runs, a display in a scaled mode shows its panel's own pixels ("Panel pixels while a game runs" in the Settings tab, on by default; `--keep-display-modes` turns it off). A scaled mode, such as 3008 x 1692 points on a 3840 x 2160 TV, makes macOS draw every change at 6016 x 3384 pixels and shrink it. A helper process (`cranvr-host --unscale-displays`) sets each such display to the panel's pixels with at least as many points, so no window moves, for itself only: when the game disconnects, the host ends the helper, and macOS gives each display its mode back. In a test the TV went to 3840 x 2160 at 1x when Beat Saber connected and back to 3008 x 1692 three seconds after it quit; the desktop layer then ran at 56 frames a second again. In Metal traces WindowServer took 157 to 161 ms of GPU time per second in the unscaled mode against 442 in the scaled one.
- `--buffering-frames 1` (the "Low latency" switch in the Settings tab) lets the headset keep one decoded frame instead of two. ALVR's decoder shows the oldest waiting frame. It drops one only when the average queue grows past this number, so with 2 a frame often waits a full refresh. The switch is off by default until a test in a worn headset shows no stutter.
- `--pointer-pitch DEG` tilts the pointing ray down from the controller's handle (default -40). Change it if the cursor lands above or below where you point.

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

### The desktop as a layer in cranvr's Quest app

In the eye stream the host draws the desktop into each eye's view, and the headset warps that view through the lens. Text is sampled twice and blurs. cranvr's Quest app shows the desktop as an OpenXR quad layer instead (`host/src/desktop_layer.rs`, `quest/src/desktop_layer.rs`):
- The headset's compositor samples the desktop once, at the display's own resolution, with the compositor's supersampling and sharpening.
- The compositor draws the layer with the newest head pose. A head turn over the desktop needs no new frame from the Mac and does not wait on Wi-Fi.
- The host captures the display at its full pixel size, 3840 × 2160 for a 1920 × 1080 virtual display, at up to 60 frames per second. ScreenCaptureKit sends a frame only when the display changes, so a still desktop sends almost nothing.
- A second HEVC encoder session encodes the captures. Each capture is first mirrored top to bottom on the Mac's media hardware (VTPixelRotationSession), because the Quest's compositor reads a decoder's surface bottom row first.
- The Quest's hardware decoder writes straight into a surface that the compositor shows (XR_KHR_android_surface_swapchain). No copy happens on the Quest's GPU.
- The cursor position goes over UDP every 2 ms while it moves. The app draws the cursor as a second small layer at the newest position every refresh, so the cursor never waits for a video frame.
- The eye stream leaves the screen out while the layer shows it. The host sends no desktop frames while the screen is out of the head's view or while a game runs.
- Each screen has its own capture and encoder on the Mac and its own decoder on the Quest. The app reports every refresh how the head sees each screen. A screen near the middle of the view gets up to 60 frames per second, one at the edge of the view 15, one out of view none: its capture stops after a second. In Auto quality a screen gets the narrowest of 1920, 2560 and 3840 pixels that covers twice the pixels per degree a Quest 3 shows, after the need held for a second (sharper) or five seconds (coarser). Every capture stops while a game is connected. The eye stream's own desktop capture stops while the layer shows the screen.
- With `CRANVR_DESKTOP_INSET=1` for the host, the screen that the head points at gets a sharp inset: a second stream of 1920 x 1080 pixels of only the part around where the head points, at the full density that the headset can show there. The screen's own picture then needs only half that density. The inset moves when the head points a quarter of its size away from its middle. ScreenCaptureKit changes the inset's part without a restart; the host sends the part with each frame, and the app places the inset by the part of the frame that its surface shows. The inset turns on only when the screen needs more than 2400 pixels across. It is off by default since 16 November 2026: on head turns a worn headset showed it flash pictures of other parts of the display, and its picture did not quite match the screen's. The app places the inset by the part of the newest decoded frame, which the compositor may not show yet. Without the inset, Auto quality gives the screen's own picture the whole density, up to 3840 pixels.
- The console (`quest/src/console.rs`) is one more quad layer. Its face is one picture at 1600 pixels per meter, like a screen's bar. A thread of its own draws the picture, because the face took 13 ms on the Mac. Boxes in the overlay frame the face with a depth of 3 cm. The cranvr app writes the games and the settings to `vr-menu.json` in cranvr's config folder. The host adds the stream's state and sends the menu to the headset when it changes, at most once a second. A press sends the button's request back to the host. The host hands it to the app as a `vr_request` status line. The compositor stacks the layers in the order of their list, not by depth. The app therefore lists the console after the screens when the console is nearer to the head than every screen.
- A game that disconnects gives the desktop back: after Beat Saber quit, the layer ran at 57 fps again within 3 seconds.
- A new host connection replaces the old one in the Quest app. A host that quit may leave its connection open on the headset for good, when Wi-Fi loses the close. The app then made the next host wait, and the layer did not come back.
- The layer shows the display that the eye stream captured when the host started. When that display is gone, the layer shows the main display. On 16 November 2026 Meta's Remote Desktop added its own display. macOS then mirrored the TV onto it, so the TV was no display of its own. The layer found no display to capture. The headset showed only the eye stream: a blurry desktop with the host's cyan marker for the controller and no ray. With the change the layer showed the new main display. When the display of a running layer goes away, the layer does not move to another one yet. A new start of cranvr's Quest app connects again and finds the main display.
- A game that loads sends no frames for seconds. The desktop stays hidden as long as the game is connected, so it does not flash over the loading game.
- The app places the panel itself, in the headset's stage space (`quest/src/panel.rs`). ALVR's server recenters every pose that the host sees, to the head's place and direction when the headset connects. A panel placed by the host showed turned and shifted, and the host's pointer missed it. The app also tests the controllers' rays against the panel it shows and sends the hit to the host over UDP port 9952. The host moves the Mac's mouse there.
- The app draws the controllers, their rays and the panel's handle in an overlay layer over all other layers, with the poses of the coming refresh. The host's markers for the controllers leave the eye stream while the layer shows. The controllers are simple shapes, not Meta's controller models.
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
- Unit tests cover carrying, tilting, pushing and resizing a screen, the click hold, the buttons, the bar picture, the console's layout and requests, the placing of an added screen, the gaze report and the inset's part. On the headset a test hook (`debug.cranvr.panel_test`) added a screen, set its quality, paused it and closed it. The user then added screens and resized one by hand. Not seen on the headset yet: the inset, the click hold and Beat Saber at the game scale of 0.7.
- On 16 November 2026 a test hook (`debug.cranvr.console_test`) set the console upright in front of a headset on the desk. The headset's screenshot showed the face in its frame with every label readable. The face first did not show: the app started its picture again every frame, so a picture that took longer than one frame never finished. The same hook sent a request to let the game choose its anti-aliasing and later one for at most 2x. The app wrote each to `app.json`. The console then marked the new choice. Nobody has pressed a console button in a worn headset yet. The app saves the console's place with the places of the screens.
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

Games need a one-time setup of the Wine side. The first build takes about an hour, mostly for LLVM.

```bash
xcodebuild -downloadComponent MetalToolchain
softwareupdate --install-rosetta
brew install cmake meson ninja mingw-w64
scripts/build-basaltvr.sh
scripts/setup-wine.sh third_party/basaltvr/src
```

`build-basaltvr.sh` clones BasaltVR at a pinned commit into `third_party/basaltvr/src`. It applies cranvr's patches and builds the OpenXR runtime and the DXMT fork. `setup-wine.sh` puts Wine 11.10 with the DXMT fork in `~/Library/Application Support/cranvr/wine`. It registers the runtime in the Wine prefix and checks it through the OpenXR loader.

Start the host first: it writes `runtime-control.txt` into its config dir. Then start a game from the window ("Game") or with the script:

```bash
scripts/run-game.sh 'C:\basalt\runtime\d3d11_runtime_smoke.exe' --render-frames 7200 --visual
```

The smoke program draws a green marker at the left controller and a pale grid. A squeeze of the left grip grows the marker and sends a haptic pulse.

### Steam games

Steam runs in the prefix with BasaltVR's web helper shim (`scripts/setup-steam.sh`). A real Steam game has not run through cranvr yet. The steps:

1. In the window's "Play" tab, press "Open Steam". Log in. Install the game.
2. The tab lists the installed games. Press "Play" next to one: the window starts it by its .exe with the host's environment.
3. Without the window, start Steam with `scripts/run-game.sh --steam`. While Steam runs, start the game by its .exe:

   ```bash
   scripts/run-game.sh 'C:\Program Files (x86)\Steam\steamapps\common\Beat Saber\Beat Saber.exe'
   ```

Do not use `steam.exe -applaunch`: a game that Steam starts gets Steam's environment, not the one with the runtime's control file. For a game under `steamapps\common` the script and the window find the Steam app ID in Steam's `appmanifest` files. They set `SteamAppId`, which the game's Steam check needs.

The runtime speaks OpenXR with D3D11. Games that use OpenVR only need OpenComposite (see BasaltVR's `docs/BOTTLES.md`). The Steam version of Beat Saber has used OpenXR since version 1.29.4, so it needs no OpenComposite. Only an older version, as installed by BSManager for mods, needs it.

## Test without a headset

```bash
cargo test --release
cargo test --release -p cranvr-app -- --ignored
./target/release/cranvr-host --fps 90 --trust-all --tcp --config-dir /tmp/cranvr-test &
HOME=/tmp/cranvr-probe-home ./target/release/cranvr-probe --seconds 10
```

The probe exits with code 0 when at least half of the expected frames decode with the right eye colors. `HOME` keeps the probe's ALVR client settings out of your own Library folder. The ignored app test starts the host the way the window does and connects the probe.

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
- `--android` builds `cranvr-probe` for Android with `cargo ndk` and runs it on a phone or a tablet attached by USB. It uses ALVR's client core with the Quest app's MediaCodec decoder and frame buffering, unchanged. A display loop takes one image per refresh, like the Quest's compositor. It samples the eye colors of every frame it shows with OpenGL ES through ALVR's EGL context, as the Quest app reads frames: a Qualcomm decoder writes them in a compressed format that only the GPU reads. The host reaches the device through `adb forward` of TCP ports 9943 and 9944. With `--wifi` it connects to the device's Wi-Fi address with a UDP stream, as a Quest connects. The run holds the device lock of Cranpose's device scripts.
- `--quest` starts the ALVR app on a Quest attached by USB. adb keeps its display on without a face in the headset. The Quest and the Mac must share a network.

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

`game-run.sh` needs the cranvr app running. It wakes the headset without a wearer and starts the Quest client and the game without sound. It prints one summary line of the rates and the latency stages. Then it gives the headset its proximity sensor and guardian back. `metal-trace.sh` records 5 seconds of GPU work of every process on the Mac while a game runs. `gpu-by-pass.py` sums that work per process and per pass. `gpu-by-kind.py` sums it per kind of pass. `display-modes.swift` lists the modes of the external display or holds one for its own run. Check `uptime` before a run: builds in other programs halve the rates.

## Other runtimes

### VeRantyx, tried on 16 November 2026

[VeRantyx](https://github.com/Ag3497120/VeRantyx) (commit 422f52e5def7) runs the Windows SteamVR on VXRuntime, a Windows runtime of its own. It does not use Wine. Its ALVR bridge is ALVR 20.14.1, so cranvr's Quest client connected to it. Neither game ran in the headset:

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
