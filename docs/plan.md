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
| 6 | Native mode: protocol, Quest client, items 3, 4 and 6 above | started: `quest/` builds cranvr's Quest app, ALVR's OpenXR client with its own package and the v20 protocol unchanged; not run on a Quest. On Android without OpenXR it shows the stream in a window with the gyroscope as the head, paced to the vsync, with a late warp of the newest decoded frame: 27.4 ms from motion to display and 49.6 ms round trip on a Galaxy Tab S7+ at 60 Hz over 2.4 GHz Wi-Fi. On a Pixel 9 Pro at 120 Hz over 5 GHz Wi-Fi: 18.5 ms and 41 ms. The protocol exists: the `proto/` crate (`cranvr-proto`, no ALVR dependency, builds for Android) holds the handshake, the control and datagram messages, media packets with Reed-Solomon parity, the clock sync and a TCP media path for USB. The host speaks it with `--protocol native` (ALVR stays the default) and `cranvr-probe --protocol native` is its first client. The Quest app still speaks ALVR's v20 |
| 7 | Native mode: items 1, 2 and 5 above | first frame-rate gains past the encoder limit |
