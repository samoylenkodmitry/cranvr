# cranvr plan: a native protocol with a Cranpose client

## Should cranvr rewrite ALVR?

Size of ALVR v20.14.1 (lines of Rust, measured):

| Part | Lines |
|---|---|
| protocol packets and sockets | 1,500 |
| server core | 6,100 |
| client core | 3,200 |
| OpenXR client app | 4,300 |
| client graphics (lobby, reprojection, foveation) | 1,700 |
| settings schema | 2,900 |
| dashboard, launcher, events, adb and others | 8,600 |
| SteamVR driver and its encoders (C++) | 25,300 |

The parts that matter for cranvr total about 20,000 lines. A full port is not needed. cranvr can have two modes instead.

**Compat mode** serves the stock ALVR client. It keeps `alvr_server_core` as a dependency, which already works (milestone 1). A later step can replace that dependency with a small compat module. BasaltVR did this server side of the ALVR v20 protocol in about 2,000 lines of Swift. The replacement would also drop ALVR's web API, which listens on TCP 8082 on every network interface without a login.

**Native mode** serves cranvr's own Quest client. Both ends are ours, so the protocol and the pipeline can change freely. Estimated size: 8,000 to 10,000 lines.
- transport: 1,500
- host pipeline: 2,000
- Quest streaming core (OpenXR session, MediaCodec decode, layers): 4,000
- UI: Cranpose

The Quest client is itself a Cranpose XR app (see `cranpose-xr.md`). One APK also runs on Steam Frame and Pico.

## What native mode can do that ALVR v20 does not

Ordered by expected gain. "Measured" marks numbers from this Mac; the rest are estimates.

1. **Get past the encoder limit.** Measured: with VideoToolbox's low-latency rate control the M5 HEVC encoder needed about 2 ms per megapixel and took one frame at a time. Without it, and with two frames in flight, a foveated Quest 3 frame (2432 × 1152, 2.8 megapixels) takes 3.5 ms and the encoder keeps up with about 230 frames a second. Quest 3 shows about 9 megapixels per frame at native resolution.
   - Foveated encoding: full density in the center, less at the edges. It cuts the encoded pixels to about half.
   - Super resolution on the Quest: encode at a lower size, then upscale with Snapdragon Game Super Resolution in one GPU pass. Virtual Desktop ships this.
   - Together these fit Quest 3 sharpness into the encoder's limit at 120 Hz.
2. **Frame generation on the Quest.** The Mac renders and encodes 60 or 72 frames per second. The Quest shows 120 through `XR_FB_space_warp`. The Quest needs motion vectors and depth for each frame:
   - Cranpose apps have exact motion vectors and depth.
   - Windows games give depth at best. The Mac can estimate motion vectors from the last two frames on the GPU.
   - This halves the encoder load.
3. **Lock the frame clock to the Quest display.** ALVR v20 runs the server frame clock on its own. The client knows its display times. It can report them, and the host can start each frame so it arrives just before the Quest needs it. Expected: about half a frame less wait on average. The periodic stutter from clock drift goes away.
4. **Encode, send and decode in slices.** The frame splits into slices, for example two per eye, each in its own encoder session. Each slice goes out as soon as it is encoded. The Quest decodes the slices in parallel. Encode, network and decode then overlap. Expected: 4 to 8 ms less latency. Two encoder sessions may also raise the encoder's throughput; this needs a measurement.
5. **Recover from packet loss without a keyframe.**
   - UDP with forward error correction sized per frame.
   - Long-term reference frames: VideoToolbox has `kVTCompressionPropertyKey_EnableLTR` and an LTR refresh option. After a loss, the next frame can reference the last frame the Quest confirmed. A full keyframe is not needed, so no bitrate spike follows. Apple documents the acknowledgement flow poorly, so this needs an experiment first.
6. **Late poses.** The host reads the pose right before it submits GPU work. The pose is predicted by the measured total latency. Controllers are predicted on their own, because the sabers in Beat Saber show any error.
7. **USB-C.** `adb reverse` over a cable gives a steady, low-jitter link.
8. **No copies.** The Mac renders straight into the encoder's buffer (done). On the Quest, MediaCodec decodes into an AHardwareBuffer. Vulkan uses that buffer directly as the layer image.

## Milestones

| # | Work | Why this order |
|---|---|---|
| 1 | Test scene to the stock ALVR client | done |
| 2 | Foveated encoding in compat mode | done: 120 Hz at the full Quest 3 eye size |
| 3 | Windows games: an OpenXR runtime DLL in Wine shares frames as IOSurfaces | done with BasaltVR's runtime and DXMT fork, patched for 120 Hz (`third_party/basaltvr`) |
| 3b | The Mac desktop in compat mode: a screen in the scene, the controller as a mouse | done; native mode will send the desktop as its own layer |
| 4 | Cranpose host window | done: start, pair, settings, games, status |
| 4b | End-to-end harness: `scripts/e2e.sh` with the probe on the Mac, the probe on an Android phone (ALVR's MediaCodec path, over `adb forward`), or a Quest; the client's latency by stage in the host's stats | done on the Mac, a Galaxy Tab S7+ and a Pixel 9 Pro over USB, and on the Tab over 2.4 GHz Wi-Fi; the Quest leg is untested |
| 4c | Game sound: ScreenCaptureKit captures the Mac's sound; the server core sends it as ALVR's game audio | done; tested with the probe on the Mac and the Tab, not on a Quest |
| 4d | A virtual display for the headset: the desktop view shows it. Two switches, both off by default: it becomes the main display while a headset is connected; the cranvr window follows the headset | done; tested with the probe and the Tab app, not on a Quest |
| 4e | The desktop as a compositor layer in cranvr's Quest app: its own HEVC stream at the display's full pixel size, decoded straight into a surface layer; the cursor over UDP as its own layer | done; on a Quest 3, sharp 10-pixel text, 57 fps of a 60 fps video; the cursor delay is not measured |
| 5 | `cranpose-xr` with the OpenXR backend | needed by the store apps and by the cranvr client |
| 6 | Native mode: protocol, Quest client, items 3, 4 and 6 above | started: `quest/` builds cranvr's Quest app, ALVR's OpenXR client with its own package and the v20 protocol unchanged; not run on a Quest. On Android without OpenXR it shows the stream in a window with the gyroscope as the head, paced to the vsync, with a late warp of the newest decoded frame: 27.4 ms from motion to display and 49.6 ms round trip on a Galaxy Tab S7+ at 60 Hz over 2.4 GHz Wi-Fi. On a Pixel 9 Pro at 120 Hz over 5 GHz Wi-Fi: 18.5 ms and 41 ms. The protocol exists: the `proto/` crate (`cranvr-proto`, no ALVR dependency, builds for Android) holds the handshake, the control and datagram messages, media packets with Reed-Solomon parity, the clock sync and a TCP media path for USB. The host speaks it with `--protocol native` (ALVR stays the default) and `cranvr-probe --protocol native` is its first client. The Quest app in `quest/` still speaks ALVR's v20. `client/` is the first client of the native protocol that is a Cranpose app (`cranvr-client`, application id `dev.cranvr.client2`). It decodes with Cranpose's `VideoDecoder` in frames mode. A `StreamedScene` shows the decoded frames with the render poses of each frame (`FrameMeta::eye_poses`). The connect and stats panels are movable `SpatialPanel`s. A `SpatialFrameEffect` feeds the host once for each headset frame with the real head pose, the eye views, both controllers, their buttons and the predicted display time. The host's haptics play on the controllers. A recenter of the headset goes to the host as `ClientMsg::Recenter`. The host then puts its desktop screen in front of the user again. The Hello carries the headset's name, eye size, refresh rates and fields of view. The host locks the frames of its own scene to the headset's display (`host/src/headset/display_lock.rs`, plan item 3). Each tracking sample carries the display time that its poses are predicted for. The host builds the headset's display grid from those times, learns the compositor's lead and the time that a frame needs from its start to the end of its decode, plans each frame's start and target display time, renders for the pose that is valid at the target (the sample's pose moved by its velocities, at most 80 ms) and puts the target in the frame's metadata. The client passes the target through. The headset still takes the newest frame, until Cranpose's `StreamedScene` can choose frames by target. A headset pairs with the host by a code (`cranvr-host --protocol native --pair CODE`) and proves its key at each connection. On 17 November 2026 a Quest 3 on the desk ran against the host over Wi-Fi with UDP media and the host's test scene at 1248 by 1306 pixels per eye and 120 Hz (HEVC, no foveation). The headset's frame loop ran at 120.9 frames a second with at most 9.1 ms between two frames. Decode took 3.8 ms, the round trip 3.8 ms and the host's send to the last packet 2.8 ms. No packet was lost. Four arms ran in one session in the order lock off, on, off, on (`--no-display-lock` turns the lock off), 38 s each. The numbers are the medians of 14 intervals of 2 s after a warm-up of 8 s. The load average of the Mac was 14 to 35 from other work. Lock off: the host received 117.8 frames a second and the headset took 89.9 and 93.9 of them. The decoder dropped 27 and 24 a second, because a newer frame had come. The host measured 34.5 and 36.4 ms from the tracking sample to the display. Lock on: the host sent the same 117.8 frames a second and the headset took 113.8 and 110.8, which is 94 and 92 percent of its 121 frames. The decoder dropped 7.8 and 7.3 a second. The client counted 100 and 96 percent of the shown frames at the target that the host aimed for. The host counted 91 and 90 percent of its frames at their target, 6 percent replaced by a newer frame and 1 to 2 percent early. The host measured 40.7 and 40.6 ms from the tracking sample to the display, which is 4 to 6 ms more than with the lock off. The pose of a frame is the pose that is valid at its target, so the time from the pose's own time to the display fell from about 16 ms to about 0. The lock adds a margin of 2.6 to 3.0 ms to the 90th percentile of the need of a frame (19.1 to 19.5 ms in all). A first version of the planner sent a frame that started a little late to the next point. 14 points a second went unused. The planner now tests the earliest point with the median need. A fixed hold of 4 ms before each tracking send, measured on the same day with the lock off, took 112.8 and 104.9 of the frames at the same latency as the lock (41.0 ms). The lock matches it within the noise of the runs. With both on, the host got 56 frames a second. The hold is gone. Before the host chose a smaller eye for a client without foveation, the plain command sent 4128 by 2162 pixels. The host's encoder took 10.5 ms for a frame, so the host sent 85 to 95 frames a second. The Quest 3 showed green blocks in part of the picture. Its decoder advertises HEVC level 6.2, so the cause of the green blocks is not known. The picture is upright: the sky is brighter with elevation (`host/src/pattern.metal`) and the stream's sky is brighter at the top of the headset's screenshot. Not tested: head motion (the headset stayed on the desk, so the extrapolation moved a still pose and the reprojection had nothing to correct), a game (the lock plans only the host's own scene), the controllers, haptics on a hand, a recenter, a disconnect followed by a connect, the buttons of the panels, the USB path with the new scene, a foveated stream (Cranpose's `StreamedScene` has no foveation parameters, so the client takes none) |
| 7 | Native mode: items 1, 2 and 5 above | first frame-rate gains past the encoder limit |
