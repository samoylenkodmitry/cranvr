# OpenComposite patches

cranvr runs Windows OpenVR games under Wine with [OpenComposite](https://gitlab.com/znixian/OpenOVR) (GPL 3), the OpenVR runtime that makes OpenXR calls. The pinned commit is `cff07db75c4823afe93ed7027b03d5f7bc86f164` (branch `openxr`, revision 1.0.1521 plus 11 commits). `scripts/build-opencomposite.sh` clones it into `third_party/opencomposite/src` (not committed), takes the submodules `libs/openxr-sdk` and `libs/glm`, applies the patches and builds `vrclient_x64.dll` into `third_party/opencomposite/build/bin` with the MinGW-w64 toolchain that builds BasaltVR's runtime. The files in this folder are under the GPL 3 like OpenComposite, since the patches change its code. The folder `tests/` is not: `openvr_smoke.cpp` is cranvr's own program (MIT) that uses Valve's OpenVR headers.

- `mingw-x86_64.cmake` is the CMake toolchain file. It defines `WIN32` (OpenComposite tests it, and GCC does not define it in strict C++20 mode) and includes `<cstdint>` for the OpenXR loader's sources, which GCC 16 no longer gets from other headers.
- `shim/atlbase.h` is the part of ATL that OpenComposite uses, for MinGW-w64.
- `tests/openvr_smoke.cpp` is the OpenVR test program (see "End-to-end test" in the README).

## patches

`0001-mingw-w64-and-direct3d-only-build.patch`

- The CMake option `OC_D3D_ONLY` builds the Windows DLL with Direct3D compositors only. The Vulkan SDK of the upstream build is a Windows download of import libraries, and the runtime offers Direct3D 11 only.
- Symbol visibility `hidden` goes to C and C++ compiles only, since the resource compiler takes no such flag.
- GNU ld needs OpenComposite's two static libraries in a group, and the Direct3D, DXGI, shell and debug libraries by name, which MSVC takes from `#pragma comment(lib)`.

`0002-mingw-w64-source-fixes.patch`

- `std::ifstream` and `std::ofstream` take a `std::filesystem::path` for a wide file name. `fsync` is `_commit`. `PathStripPathA` needs `<shlwapi.h>`. The Vulkan code of `XrBackend.cpp` and `CD3D11_VIEWPORT`, a helper of Microsoft's headers, are guarded or written out. A macro with no variable arguments becomes `OOVR_ABORT`.
- The log stream is a function-local static. MinGW's order of static initialisation runs the constructor of the global configuration, which logs, before the stream's constructor. The DLL then crashed while it loaded.
- The interface methods that return a struct by value (`HmdMatrix34_t`, `HmdMatrix44_t`, `HiddenAreaMesh_t`, `HmdColor_t`, `DistortionCoordinates_t`) take the address of the result as the first parameter after `this` and return it. MSVC passes `this` first and the address second, GCC the other way round: a game that MSVC built called `GetHiddenAreaMesh`, and the DLL wrote the result over the vtable pointer of its own interface object. `scripts/split_headers.py` rewrites the generated interface headers, and `scripts/stubs/codegen.py` writes the stubs and the FnTable functions to match. A FnTable function returns the struct by value, as C callers expect.
- `DX11Compositor` copies a game's image into the swapchain image with `CopySubresourceRegion`, which cannot change the order of the channels. The runtime offers B8G8R8A8 formats only. When it offers no format of the game's order, the compositor takes the same channels in the order that it offers (sRGB only for a game image that is sRGB), copies the image into an intermediate texture of the game's own format, and draws a full-screen quad into the swapchain image. It restores the state of the game's pipeline after the draw.

`0003-faster-session-restarts-and-trackpad-on-touch.patch`

- `DrvOpenXR::ShutdownSession` and `XrBackend::OnSessionCreated` slept for 250 ms between two looks at the session's state. The OpenXR runtime of cranvr has the state at once. A game restarts the session twice at its start: when it first asks for input and when it submits its first frame. The two sleeps stalled the first frames of every OpenVR game for half a second, and the average frame rate of a short run lost about 7 percent. The loops now look every 2 ms.
- The Touch controller profile translates `trackpad` to `thumbstick`: the position, the click and the touch. A game that ships action bindings for the Vive wand only used the trackpad for walking or turning. OpenComposite built no Touch binding for it, and the action never fired.

`0004-display-frequency-from-the-runtime.patch`

- `GetFloatTrackedDeviceProperty(Prop_DisplayFrequency_Float)` answered 90 Hz at every refresh rate, with a TODO. It asks the runtime for the rate through `XR_FB_display_refresh_rate` when the runtime offers the extension (cranvr's does) and answers 90 otherwise. The header `RuntimeExtensions/XR_FB_display_refresh_rate.h` declares the one function, since the OpenXR SDK that OpenComposite bundles is older than the extension. `openvr_smoke.exe --expect-hz N` checks the answer.

`0005-skybox-mip-chain-and-upright-converting-copy.patch`

- The converting copy (0002) drew a game's R8G8B8A8 image into the runtime's B8G8R8A8 image with OpenComposite's inverting quad, which turns the picture over. Every OpenVR game whose eye textures are not B8G8R8A8 was upside down in the headset. The copy draws the same quad without the flip (`vs_plain`). `scripts/e2e.sh --openvr-wine` draws a picture with a bright upper half and checks the host's copy of it. It fails with OpenComposite 0004 and passes with 0005.
- The swapchain has one mip level, whatever the game's texture has. The runtime takes images of one level, and the copy fills level 0 only. A game's loading screen (2048x1024 with a chain of mip levels) stopped the game with "Cannot create DX texture swap chain".
- `SetSkyboxOverride` does nothing. The runtime ends a frame with projection layers only, and the override is a quad layer. The Unity plugin of SteamVR sets such a skybox for a loading screen; the game now goes on to submit its frames.

`0006-controller-pose-log-and-hold.patch`

- `XrController::GetPose` writes a line to OpenComposite's log when the source of a controller's pose changes: the runtime's grip pose action ("valid, from the runtime's grip pose action"), a held pose ("held at its last place"), the hand tracking fallback ("valid, from hand tracking"), no pose ("invalid"), or no action space yet. A game that gets no valid pose for a controller draws it at the origin, which is the floor. The line tells which hand, and when. At most 60 lines.
- A controller whose pose action drops out stays where it was. The zero matrix of an invalid pose put it on the floor. The pose is held for 0.25 s before the hand tracking pose (the palm lies elsewhere, and a switch back and forth moved the controller by 10 cm), and for up to 1 s when hand tracking has no pose either. Then the pose is invalid, as before. The held pose has no velocity.

`0007-distinct-serial-numbers.patch`

- `GetStringTrackedDeviceProperty(Prop_SerialNumber_String)` answered `<unknown>` for every device (a TODO of OpenComposite). Serious Sam VR queries the serial number of both controllers in every frame (14175 times each in 2 minutes) and tells its controllers apart by it (its cvars `vr_strForceLeftHandControllerSN` and `vr_strForceRightHandControllerSN` name serial numbers). Two controllers with one serial number counted as one: the first showed and worked, the second stayed without a pose and lay on the floor, and no button of it reached the game. The controllers answer `oculus_touch_left` and `oculus_touch_right` now, the headset `oculus_hmd`. With the probe as the headset the game's menu shows both controllers with their laser pointers (before: the left one only), and the right one moves with the probe's. `openvr_smoke.exe` fails when the three serial numbers do not differ.

`0008-no-quad-layers-for-overlays.patch`

- `BaseOverlay::_BuildLayers` put the quad layer of each visible overlay and of the keyboard into the layers of the frame. The runtime ends a frame with projection layers only: `xrEndFrame` failed with `XR_ERROR_LAYER_INVALID`, and the headset got no frame at all for as long as an overlay was up. A game that keeps one up (the loading screen of the Unity plugin of SteamVR, a game's menu) froze the picture. The scene layer goes alone now and the overlays are not shown. `openvr_smoke.exe --overlay` keeps an overlay up for the run, and `scripts/e2e.sh --openvr-overlay` checks the host's count of game frames: 0.0 a second with patch 0007, 72.1 a second at 72 Hz with 0008.

`0009-opengl-games-through-direct3d-11.patch`

- Google Earth VR draws with OpenGL and submits `TextureType_OpenGL` textures: one texture of 2888x1546 with both eyes side by side, each eye with its half in the bounds. The build of patch 0001 has no OpenGL compositor, and the runtime offers Direct3D 11 images only.
- The build defines `SUPPORT_GL_D3D11`. At an OpenGL game's first frame the session stays on the Direct3D 11 device that OpenComposite made for its first session (`TemporaryD3D11`), and `GLD3D11Compositor` serves each eye. It reads the game's rectangle of the texture with `glReadPixels` in the game's own context (as BGRA, the runtime's order) into a pixel buffer with a fence. Then it maps the buffer, turns the rows over (GL rows go up from the bottom, Direct3D rows go down from the top), writes them into a staging texture and copies that into the swapchain image. A multisampled texture is resolved with a blit first. The bytes keep their encoding: an sRGB texture, or 8-bit colour that OpenVR takes as gamma, goes to a `B8G8R8A8_UNORM_SRGB` image. The copy puts back every binding and pixel-store value of the game's context that it touches.
- Each eye has two pixel buffers. A frame starts the read of the game's new picture into one buffer and shows the picture of the frame before from the other one, whose fence has passed by then. The view of that frame goes with it (`Compositor::AdjustView`), so the runtime places the picture for the pose that it was drawn for. The first frame and a frame after a change of size wait for their own read. The game's CPU and GPU work overlap as before, and the headset shows each picture one frame later.
- With a wait for the game's frame in each copy, Earth VR at 1444x1546 per eye made 45 frames a second at 72 Hz: the copy of the first eye took 13.3 ms (most of it the wait), the second 2.6 ms. With the two buffers it makes 72 frames a second at 72 Hz. A copy takes 0.7 to 0.95 ms on average per eye (the read 0.1 ms, the wait 0.01 to 0.04 ms, the copy into the swapchain image 0.5 to 0.7 ms). `openvr_smoke.exe --gl` makes 72.0 at 72 Hz. The log of OpenComposite gives the times of every 600 copies.
- `Compositor::Invoke` is virtual, so the copy can take the rectangle of the bounds itself and give the frame the whole image.
- `IVRCompositor::GetFrameTimeRemaining` was a stub that stops the game. Earth VR calls it in its first seconds. It answers the frame period of the last `xrWaitFrame` less the time since that call returned.

`0010-legacy-button-events.patch`

- A game without an action manifest reads the legacy controller state (`GetControllerState`) and the button events of it: `VREvent_ButtonPress`, `VREvent_ButtonUnpress`, `VREvent_ButtonTouch` and `VREvent_ButtonUntouch`. OpenComposite had the code that makes the events from the state (`BaseSystem::CheckControllerEvents`). The OpenXR port stopped calling it, so no such event reached a game.
- Google Earth VR takes a click from these events. A trigger press on "Click to start welcome tour" reached the runtime and the legacy state, and the game did nothing: 13 presses on the caption in 4 minutes. With the patch the welcome tour starts 4 seconds after the press.
- After each frame, once the game's legacy input is loaded, OpenComposite compares the state of each hand with the state at the frame before and queues an event for each button that changed. A game with its own action manifest gets none: the check would load the empty manifest before a game that sets its manifest a few frames late.
- `openvr_smoke.exe --expect-input` counts the press and the release events of the probe's left grip and A, and fails without them.

`0011-plain-error-for-other-application-types.patch`

- OpenComposite runs scene applications only. For any other application type `VR_Init` showed a message box ("Cannot init VR: unsupported apptype 6"), and the box stopped the caller. Steam starts OpenVR as its watchdog (`VRApplication_SteamWatchdog`, type 6) when SteamVR is installed. When the prefix's OpenVR path file named OpenComposite at Steam's start, the box blocked Steam's main loop, and Steam's own watchdog ended Steam after 15 s ("Fatal assert; application exiting").
- `VR_InitInternal2` now refuses every type but a scene with `VRInitError_Init_InvalidApplicationType` and one line in OpenComposite's log ("VR_Init refused: OpenComposite runs scene applications only, and this program asked for application type 6"). This covers the background, utility, overlay and bootstrapper types too: the bootstrapper `vrstartup.exe` ends at the error, as before, and a utility program got a token and then no interface. The check comes before OpenComposite's crash handler, so a refused program such as Steam keeps its own handler.
- `VR_GetVRInitErrorAsSymbol` was a stub that showed a box too. Valve's `openvr_api.dll` asks the runtime for the symbol of an error, and a caller that logs the refusal by its symbol stopped there. It answers the symbols of the errors that OpenComposite returns.
- `openvr_smoke.exe --app-type watchdog|background|utility|overlay|other --runtime DIR` calls `VR_Init` with that type on a thread of its own and passes on an init error within 10 s. A box keeps the call from returning, so the program then fails and ends itself. `scripts/setup-wine.sh` runs it for the watchdog after each install. Each type returned error 130 in 0.5 s with this patch. Steam's call stayed in the box before it, on 2026-11-22 at 13:07:55.
