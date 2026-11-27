# How cranvr works

cranvr streams VR from an Apple Silicon Mac to a Meta Quest. The Mac side is Rust. It renders and composes with Metal, encodes HEVC with VideoToolbox and sends the frames with cranvr's own protocol (`proto/`, crate `cranvr-proto`). The headset side is a Cranpose app (`client/`, package `cranvr-client`, application id `dev.cranvr.client`). It uses only Cranpose's public API and plain Rust crates. It has no ndk, JNI, GL, OpenXR or platform code of its own. The same crate also builds as a flat app for an Android phone or tablet.

The goal is to play Windows SteamVR games on a Quest from a Mac. Windows games run under Wine with the OpenXR runtime and the DXMT fork from [BasaltVR](https://github.com/Stooovie/BasaltVR) (MIT). A game that speaks OpenVR runs through [OpenComposite](https://gitlab.com/znixian/OpenOVR) (GPL 3), which turns its OpenVR calls into OpenXR calls to that runtime. The game's frames reach the host as shared IOSurfaces. No frame of a Direct3D 11 or Vulkan game passes through the CPU. An OpenGL game's frames do: OpenComposite copies them into the runtime's images (see "OpenVR games").

## What works

- A head-tracked test scene for a headset on a desk.
- Foveated encoding at four levels: Off, Low, Medium and High. The host keeps the full pixel density around the direction straight ahead of each eye and squeezes the edges. The client unwarps them. The default level Medium encodes about a third of the pixels of an eye. It makes the full Quest 3 eye size fit at 120 Hz (see "Foveated encoding").
- Windows OpenXR games under Wine. Head pose, controller poses, buttons and haptics pass both ways.
- Windows OpenVR games under Wine, through OpenComposite. The app has a choice for it in the Settings tab, in the Wine card and in each game's options. OpenGL games too: Google Earth VR draws its picture in a test host (see "Not tested yet").
- Vulkan games under Wine, through MoltenVK: OpenXR games with `XR_KHR_vulkan_enable` or `XR_KHR_vulkan_enable2` and OpenVR games through OpenComposite. Each game's options have a Graphics choice for a game that draws with Vulkan or Direct3D 11 as an argument tells it (see "Vulkan games").
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
| `quest-recorder` | The program that starts and stops the headset's own recorder over adb, for a recording of what the headset shows. |
| `third_party/basaltvr` | cranvr's patches to BasaltVR's runtime and DXMT fork. |
| `third_party/wine` | cranvr's patch to Wine's HLSL compiler (`wined3d.dll`) and its test program. |
