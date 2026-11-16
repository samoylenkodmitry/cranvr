# Cranpose XR

Goal: Cranpose apps that sell in VR stores. The first stores are the Meta Horizon Store and Steam. More can follow. One codebase should cover them.

## Stores and what each needs

| Store | Devices | Package | XR API | Graphics | Store SDK |
|---|---|---|---|---|---|
| Meta Horizon Store | Quest 3, 3S, Pro | Android APK (arm64) | OpenXR, Meta runtime | Vulkan | Meta Platform SDK (entitlement check required) |
| Steam, PC VR | any SteamVR headset | Windows x64, Linux x64 | OpenXR through SteamVR | Vulkan (or D3D12) | Steamworks |
| Steam, Steam Frame | Steam Frame (standalone) | Android APK (runs through Lepton) or Linux arm64 | OpenXR | Vulkan | Steamworks; "Verified" needs 90 fps |
| Pico Store | Pico 4 Ultra | Android APK | OpenXR | Vulkan | Pico entitlement |
| Google Play | Android XR headsets (Galaxy XR) | Android APK or AAB | OpenXR | Vulkan | Play |
| visionOS App Store | Vision Pro | app bundle | Compositor Services (no OpenXR) | Metal | — |

Two facts make one codebase practical:
- OpenXR with Vulkan covers every row except visionOS.
- One Android APK covers four stores: Meta, Pico, Google Play, and Steam Frame (Steam accepts an Android depot with an APK). Only the entitlement check differs per store.

Sources: [Steam Frame on Steamworks](https://partner.steamgames.com/doc/steamhardware/steamframe), [PC Gamer on Android builds on Steam](https://www.pcgamer.com/hardware/vr-hardware/first-official-android-game-spotted-on-steam-suggests-you-wont-need-to-buy-games-twice-to-run-them-natively-on-the-steam-frames-own-chip), [VR.org on the Verified bar](https://vr.org/articles/valve-lepton-fex-steam-frame-translation-layers-2026).

## A `cranpose-xr` layer

Cranpose renders with wgpu. OpenXR hands out its own swapchain images. wgpu-hal can wrap those images as wgpu textures (Vulkan on Android, Windows and Linux). bevy_openxr uses the same method and is a working reference.

The layer would own:
- **Session lifecycle:** focus, visibility, pause and resume. Store reviews test these first.
- **Frame loop:** wait, begin and end frame, with the predicted display time.
- **Views:** the pose and field of view of each eye.
- **Input:** controllers and hands through OpenXR actions.
- **Layers:** a projection layer for the 3D scene. Quad layers for flat Cranpose panels; text in a quad layer stays sharper than text drawn into the scene.
- **AR:** passthrough (`XR_FB_passthrough` on Quest, environment blend modes on Android XR).
- **Frame generation:** `XR_FB_space_warp`. Cranpose knows the exact motion vectors and depth of everything it draws.

Backends behind one API:
1. **OpenXR:** all store builds.
2. **cranvr stream:** the app runs on the Mac and shows on the Quest through cranvr. Development needs no APK install for each change. This backend serves development only.
3. **visionOS:** later, through Compositor Services and Metal.

Store modules sit behind Cargo features: Meta Platform SDK (through JNI), Steamworks (`steamworks` crate), Pico.

## Where cranvr uses Cranpose

- The host window on the Mac: game library, pairing, quality settings.
- The cranvr Quest client (see `plan.md`). It is a Cranpose XR app itself. It can be the first Cranpose app in the Meta store and on Steam Frame. It tests the store path before paid apps depend on it.

## What sets the frame rate when streaming

The encoder sets it more than the renderer. On an M5 the HEVC encoder needs about 2 ms per megapixel. At 120 Hz a frame has 8.3 ms. The renderer gets 2 to 4 ms of it. A faster renderer helps only within that window. `plan.md` lists the ways around the encoder limit.
