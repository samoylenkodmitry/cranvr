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
