# cranvr plan

cranvr's host and the Cranpose client speak one protocol (`proto/`). Both ends are ours, so the protocol and the pipeline can change together. The client is a Cranpose XR app (see `cranpose-xr.md`). One APK can also run on Steam Frame and Pico.

## What exists

- The protocol: an encrypted and authenticated handshake, control messages over TCP, datagrams and media over UDP with Reed-Solomon parity or over TCP for a USB cable, the clock sync, the discovery and the pairing that the user allows on the Mac.
- The host: a Metal test scene, Windows OpenXR games under Wine through BasaltVR's runtime, the Mac desktop as a second stream, the Mac's sound as PCM, foveated HEVC encoding and the display lock.
- The client: a Cranpose app for the Quest and for Android phones and tablets.
- The `cranvr` window: it starts the host, shows the headsets that wait for the user's answer, sets up Wine and Steam and starts games.
- `scripts/e2e.sh`: a test client on the Mac or a Quest, with the latency by stage in the host's report.

## What the protocol allows next

Ordered by expected gain. The numbers are estimates unless the text says that they were measured.

1. **Get past the encoder limit.** Measured: with VideoToolbox's low-latency rate control the M5 HEVC encoder needed about 2 ms per megapixel and took one frame at a time. Without it and with two frames in flight, a foveated Quest 3 frame (2432 x 1152, 2.8 megapixels) takes 3.5 ms and the encoder keeps up with about 230 frames a second. Quest 3 shows about 9 megapixels per frame at native resolution.
   - Foveated encoding (done): full density in the center, less at the edges. It cuts the encoded pixels to about a third.
   - Super resolution on the Quest: encode at a lower size, then upscale with Snapdragon Game Super Resolution in one GPU pass. Virtual Desktop ships this.
   - Together these fit Quest 3 sharpness into the encoder's limit at 120 Hz.
2. **Frame generation on the Quest.** The Mac renders and encodes 60 or 72 frames per second. The Quest shows 120 through `XR_FB_space_warp`. The Quest needs motion vectors and depth for each frame:
   - Cranpose apps have exact motion vectors and depth.
   - Windows games give depth at best. The Mac can estimate motion vectors from the last two frames on the GPU.
   - This halves the encoder load.
3. **Encode, send and decode in slices.** The frame splits into slices, for example two per eye, each in its own encoder session. Each slice goes out as soon as it is encoded. The Quest decodes the slices in parallel. Encode, network and decode then overlap. Expected: 4 to 8 ms less latency. Two encoder sessions may also raise the encoder's throughput; this needs a measurement. The message format has the slice rectangles already.
4. **Recover from packet loss without a keyframe.**
   - UDP with forward error correction sized per frame (done).
   - Long-term reference frames: VideoToolbox has `kVTCompressionPropertyKey_EnableLTR` and an LTR refresh option. After a loss, the next frame can reference the last frame the Quest confirmed. A full keyframe is not needed, so no bitrate spike follows. Apple documents the acknowledgement flow poorly, so this needs an experiment first.
5. **Late poses for games.** The display lock plans the host's own scene only. A game's frames set their own pace. The host can read the pose right before it submits GPU work. Controllers are predicted on their own, because the sabers in Beat Saber show any error.
6. **No copies on the Quest.** MediaCodec decodes into an AHardwareBuffer. Vulkan uses that buffer directly as the layer image. Cranpose's `StreamedScene` does the composition today.
7. **Store builds.** `docs/cranpose-xr.md` covers the Cranpose apps for the VR stores.
