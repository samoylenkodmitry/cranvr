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
