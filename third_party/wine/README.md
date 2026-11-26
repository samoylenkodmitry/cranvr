# Wine patches

cranvr keeps Gcenx's macOS build of Wine 11.10 as its engine. It replaces nine files of it with files built from the same Wine tag and cranvr's patches:

- `wined3d.dll` holds vkd3d-shader, the HLSL compiler (LGPL 2.1 or later, like Wine). A game that compiles its shaders at run time calls `D3DCompile` in `d3dcompiler_47.dll`, which calls vkd3d-shader in `wined3d.dll`. A game ships its own `d3dcompiler_47.dll` too. Wine loads its own copy first, so every game's HLSL goes through vkd3d-shader.
- `winemac.so` is the Mac side of Wine's Mac driver. It makes the OpenGL contexts of a Windows program.
- `opengl32.so` is the Mac side of Wine's `opengl32.dll`. Every OpenGL call of a program goes through it.
- `wininet.dll` is Wine's HTTP client of the WinINet API.
- `wtsapi32.dll` answers questions about the Windows session, such as whether the screen is locked.
- `user32.dll` holds the window functions of Windows.
- `ntdll.so` is the Mac side of `ntdll.dll`. It manages the memory of a Windows program, among other things.
- `winecoreaudio.so` is the Mac side of Wine's CoreAudio driver. It gives a Windows program the Mac's sound devices.
- `winevulkan.so` is the Mac side of Wine's `vulkan-1.dll`. It passes each Vulkan call of a Windows program to MoltenVK, which runs Vulkan on Metal.

`scripts/build-wine.sh` clones Wine at the tag `wine-11.10` (commit `2cac6ccf33c0807f374dc96f5a20e35a2da86157`) into `third_party/wine/src` (not committed), applies the patches in this folder and builds the nine files into `third_party/wine/build/bin`: the DLLs with MinGW-w64, the Mac libraries with Xcode's clang for x86_64 and macOS 10.15 and newer, as Gcenx's build has. The build runs under Rosetta 2, because Wine's configure wants a 64-bit Intel host. `scripts/setup-wine.sh` copies the DLLs into the engine and into the prefix's `system32`, where the loader takes them, and the libraries into the engine. The wine tree is not used otherwise. `package-game-support.sh` packs the pinned source tree and the patches as `SOURCE/wine-source.tar.xz` and as `cranvr-wine-source.tar.xz` in the release.

## patches

`0001-vkd3d-shader-hlsl-isnan-intrinsic.patch`

- Wine 11.10 carries vkd3d 2.0 (shader API 1.19). Its HLSL parser knows `isinf` and lacks `isnan`: a shader that calls it fails with `E5005: Function "isnan" is not defined`. The intrinsic is `x != x` after the usual conversion of the argument to float. The comparison is unordered, so it is true for NaN. `tests/isnan.hlsl` compiles to a shader with two `ne` instructions (opcode 0x39 of Direct3D 10's shader format), one for the vector and one for the scalar.
- A game whose pixel shaders end with a check for a NaN output (the shader paints it magenta) calls `isnan` in every pixel shader that has the check. Without the intrinsic those shaders fail to compile. The game then draws no picture.

`0002-winemac-core-context-for-a-compatibility-request.patch`

- macOS offers OpenGL 3.2 and newer as a forward-compatible core profile only (4.1 on Apple Silicon). Google Earth VR asks `wglCreateContextAttribsARB` for 3.2 with the compatibility profile and no flags. winemac refused the request ("OS X only supports forward-compatible 3.2+ contexts"), and the game quit 1.2 seconds after `VR_Init`.
- winemac makes the core context for such a request now and logs a warning. A program runs as long as it makes no call that the core profile removed. Patch 0003 compiles its legacy GLSL.
- A program that falls back to an older context after a refusal gets the core context instead. Such a program now runs or fails on the core profile, where it ran on the legacy context of OpenGL 2.1 before.

`0003-opengl32-legacy-glsl-in-a-core-context.patch`

- The core profile of macOS compiles GLSL 1.40, 1.50 and 3.30 to 4.10, and `#version 100` of OpenGL ES without its extensions. Earth VR passes GLSL 1.10 without a version line and GLSL ES 1.00 that calls `dFdx`: 46 of 52 shaders failed in the first second.
- `glShaderSource` in a core context of OpenGL 3.2 or newer gives such a shader a preamble of GLSL 1.50: `attribute` and `varying` become `in` and `out`, `texture2D` and its kin become `texture`, and `gl_FragColor` or `gl_FragData` become an output of the shader. The `#version` line becomes a blank one and `#line 1` follows the preamble, so the compiler's line numbers stay those of the program. GLSL 1.30, which macOS does not compile, and GLSL 1.40 or 1.50 that uses those names get the same preamble. GLSL ES 3.00 becomes GLSL 3.30. The `#extension` lines of OpenGL ES extensions that desktop GLSL has built in become blanks.
- A semicolon after the body of a function at the outer level becomes a blank: `void main() { ... };`, also after a function-like macro whose replacement ends with a body. GLSL ES 1.00 and the compilers of Windows take it, GLSL 1.50 refuses it. 16 shaders of Earth VR had it. The semicolon after a struct stays.
- Built-in variables of the fixed-function pipeline (`gl_ModelViewMatrix`, `gl_Vertex`) have no mapping: such a shader still fails.
- A shader that does not compile and a program that does not link give a warning with the compiler's message (`WINEDEBUG=warn+opengl`). Without the warning channel the calls cost nothing more.
- With the patch, Earth VR compiles 155 shaders and links all of its programs in a run of 150 seconds. No OpenGL error waits at its frames.

`0004-wininet-child-handles-under-the-lock.patch`

- wininet keeps a list of the child handles of each handle: the requests of a connection, the connections of a session. A request was added to the list, removed from it and the list was walked without a lock. Earth VR loads its pictures with many requests on several threads and crashed after 30 seconds in `invalidate_handle`: it read a next pointer of 0 in the list of a handle that it closed (Earth VR's crash dump, `WININET.dll+0x1d394`).
- The list changes under `WININET_cs` now. `invalidate_handle` takes a reference to each child under the lock and invalidates the children after it, so another thread may release a child meanwhile. A child whose last reference is already gone is left out. A handle is invalidated once, also by two threads at a time.
- One crash in the run before the patch. No crash in 23 runs of 60 to 140 seconds after it: the game made frames to the end of each run.

`0005-ntdll-high-entropy-memory-above-4-gb-on-macos.patch`

- Unity mods detour methods with Harmony, which uses MonoMod. When a detour's target lies more than 2 GB from its source, MonoMod.Core writes `jmp qword ptr [rip+disp32]` and keeps the target in a cell of 8 bytes within 2 GB of the jump. It finds the cell's page with `VirtualQuery`: it walks from the jump down and up, nearest page first. Version 1.3.3 (in BSIPA's `Libs` for Beat Saber 1.44.1) counts page 0 as free. `VirtualAlloc` at address 0 places the page anywhere. The cell then lies more than 2 GB away, the 32-bit displacement wraps and the jump reads another address. MonoMod's main branch checks both since its pull request 308 (June 2026). A mod pack keeps the version that it ships. cranvr changes no file of a game or its mods.
- The walk reaches page 0 only from code in the low 2 GB with no free page between. Windows gives a 64-bit program that is linked with high-entropy addresses (`IMAGE_DLLCHARACTERISTICS_HIGH_ENTROPY_VA` and `DYNAMIC_BASE`, as Unity's players are) memory above 4 GB. Wine on Linux frees its low reserved area for such a program, so the host places the memory high. Wine on macOS keeps the low area (the zerofill section of the loader), so the memory came from the bottom of the address space. In Beat Saber 1.44.1 Unity reserves 512 MB blocks at 0x01320000, 0x21320000, 0x41320000 and 0x7fff0000. Mono's code fills the gaps. When the low 2 GB ran full before the mods' detours, Mono put the patched methods above 4 GB and the trampolines stayed near 0x65000000. MonoMod's walk then took page 0.
- In a run with `WINEDEBUG=+seh` and MonoMod's own log (`MONOMOD_LogToFile`): the detour of `TextMeshProUGUI.GenerateTextMesh` went from the trampoline at 0x65077650 to 0x12baf03a0. The jump read 0x2cbe0000 and faulted (`EXCEPTION_ACCESS_VIOLATION`, rip 0x65077650, a read). The cell was at 0x12cbe0000, 4 GB higher. Mono turns the fault into a `NullReferenceException` in the frame of the trampoline, so every text of BeatSaberMarkupLanguage's patched `GenerateTextMesh`, `CalculatePreferredValues` and `VerticalScrollIndicator.RefreshHandle` failed: the menus showed their buttons without labels. It happened in 2 of 28 starts of the game on the Mac and in the owner's start on 2026-11-21.
- With the patch, `map_view` places memory at no fixed address above 4 GB for such a program: first in the reserved area above 4 GB, then in the host's free space, never in the low area. A request with an address, with address limits (`zero_bits`, `MEM_ADDRESS_REQUIREMENTS`) or with `MEM_TOP_DOWN` behaves as before. A program without the two flags behaves as before.
- Thread stacks stay in the low area. Wine's Mac side gives each thread a stack of its own for system calls, above 4 GB. A thread that is suspended on its way out of a system call (`__wine_syscall_dispatcher_return`, which on macOS makes a system call of its own) reports a context whose `Rsp` points into that stack: the syscall frame plus 0x70. Chromium's sampling profiler, which Steam's web helper runs with `--start-stack-profiler`, copies the stack from `Rsp` up to `StackBase`. In a first version of the patch the main thread's stack started at 0x100110000, right above the Mac-side stack of the thread at 0x100000000. The copy in `libcef.dll` (RVA 0x922cc1a) started at 0x10010fd70, read the no-access page at 0x100110000 and faulted. The web helper crashed 5 to 8 times a minute. Steam restarted it each time, so its store page stayed empty. With the thread stack below 4 GB the reported `Rsp` lies above `StackBase`. The profiler then drops the sample, as it did before the patch.

`0006-wtsapi32-session-of-the-console-is-unlocked.patch`

- SteamVR's compositor asks `WTSQuerySessionInformationA` for `WTSSessionInfoEx` and `WTSIsRemoteSession`, to learn whether the screen of the computer is locked. Wine 11.10 answered neither class ("Unimplemented class 25" and "29"). The compositor took the session as locked. In that state SteamVR marks the headset's pose of each frame invalid (`TrackingResult_Uninitialized`) and fades the picture out to a plain color. The pose that `WaitGetPoses` returned to a game for the head was invalid in every frame, with SteamVR's own null driver too. The setting `steamvr.allowDisplayLockedMode` hid the symptom.
- `WTSSessionInfoEx` now gives level 1 of `WTSINFOEX` with the console's session: active, `WTS_SESSIONSTATE_UNLOCK`, the station "Console", the user and the computer's name. `WTSIsRemoteSession` gives one byte, 0. The ANSI call converts the strings of the first one and passes the byte of the second. The patch adds the structures to `include/wtsapi32.h`.
- With the patch, an OpenVR program under SteamVR got a valid head pose in 1056 of 1056 frames (none before), and SteamVR showed its room instead of the fade.

`0007-user32-dwmgetdxsharedsurface-for-vrdesktop.patch`

- SteamVR's dashboard (`vrdashboard.exe`) loads `vrdesktop.dll`, which shows the computer's desktop in the dashboard. The DLL imports `DwmGetDxSharedSurface` from `user32.dll`. Wine 11.10 lists the function as a comment only, so the import failed and the dashboard logged "Failed to initialize vrdesktop". Without the dashboard, the menu button of a controller opened nothing.
- `user32.dll` now exports `DwmGetDxSharedSurface`. It fails with `ERROR_CALL_NOT_IMPLEMENTED`, because Wine has no DWM that composes windows from shared surfaces. `vrdesktop.dll` then takes the desktop's picture another way.
- With the patch, vrdesktop initializes, and the menu button opens the dashboard over the game in the headset's picture.

`0008-winecoreaudio-cranvr-microphone.patch`

- Wine 11.10 gives a program the Mac's input devices as capture devices. A game records from the default one. The headset's microphone is no device of the Mac. macOS adds a virtual input device only through an audio driver in `/Library/Audio/Plug-Ins/HAL`, which needs an install with an administrator's password.
- While `microphone.txt` exists in cranvr's config dir (`$CRANVR_HOME`, else `~/Library/Application Support/cranvr`), the driver lists the capture device "cranvr microphone" first and makes it the default one. Windows programs see it as "Microphone (cranvr microphone)". A stream of it reads the file when it opens. `headset` reads the ring `microphone.pcm`, which cranvr's host writes. `mac <UID>` records from the Mac input device with that Core Audio UID, as Wine records from any other device. `off`, other text and a Mac device that is not connected record silence.
- The ring holds a header of 64 bytes and then 16-bit samples of one channel at 48 kHz. The header has the magic `CRMC`, the version 1, the rate, the capacity in frames (a power of two) and the host's count of frames written. The reader adds a heartbeat (`mach_continuous_time` in nanoseconds) in each period while the stream runs, with its counts of frames read, frames missing and skips. The host asks the headset to record only while the heartbeat is younger than 500 ms.
- The reader takes the frames that are due at 48 kHz by the Mac's clock. It stays 720 frames (15 ms) behind the newest frame of the ring, since the host writes 10 ms at a time. When the least lag over half a second is more than 1200 frames, the reader skips forward to 720 frames. The difference between the headset's clock and the Mac's clock and a late datagram thus add no lasting delay. A frame that is missing gives silence. `GetStreamLatency` reports the 15 ms and the period.
- The device's mix format is 48 kHz, one channel, 32-bit float. A stream in another linear PCM format gets Wine's converter, as a stream of a Mac device does.
- Wine's capture path moved its read offset past the end of the buffer when a full buffer took more frames. `GetBuffer` then copied from outside the buffer, and the program crashed with a page fault. The read offset now follows the write offset.
- With the patch and a test writer that put a click into the ring every 500 ms, `microphone_check.exe` got each click 16 to 19 ms after the writer had written it, 48000 frames a second, with no discontinuity.

`0009-winevulkan-metal-texture-of-an-image.patch`

- cranvr's OpenXR runtime gives a Vulkan game swapchain images that lie on IOSurfaces, so that cranvr-host encodes the game's picture with no copy (runtime patch 0013). The runtime makes a Metal texture of each IOSurface with DXMT's `winemetal.dll` and creates the Vulkan image on that texture: `VkImportMetalTextureInfoEXT` of `VK_EXT_metal_objects` in the `pNext` chain of `vkCreateImage`. MoltenVK then binds the image to the texture.
- Wine 11.10 converts the `pNext` chain of a Windows program's `VkImageCreateInfo` into the Mac's form in `dlls/winevulkan/vulkan_thunks.c`. It knows only the structures of the extensions that it offers, and Wine offers no `VK_EXT_metal_objects` to Windows programs: it dropped the structure. The image then got memory of its own, and the IOSurface stayed empty.
- The patch passes the structure on to MoltenVK. The Metal texture in it is the Mac's own pointer, which `winemetal.dll` gives to Windows programs as a 64-bit number, so the structure needs no conversion. The patch handles it for 64-bit programs only. Win32u still prints a FIXME line with "Unhandled sType 1000311007" for the structure and passes it on.
- `vulkan_thunks.c` is a generated file (by `dlls/winevulkan/make_vulkan`). The patch edits the generated file, as a new generation would drop the change.
- With the patch, `metal_texture_check.exe` reads the pattern of the IOSurface back through the Vulkan image. Without it the image reads zeros.

## tests

`tests/hlsl_compile.cpp` loads a `d3dcompiler_47.dll` and compiles one HLSL file with `D3DCompile`. It prints `hlsl_compile: PASS` or `FAIL` with the compiler's messages. `tests/isnan.hlsl` calls `isnan` on a scalar and on a vector.

`tests/gl_compat_check.cpp` asks for the context that Earth VR asks for, compiles a vertex and a fragment shader of GLSL 1.10 (the vertex shader in two strings with lengths and no terminating zero), links them, draws a quad that samples a texture into a framebuffer and reads the middle back. It prints `gl_compat_check: PASS` when the pixel has the texture's color and no OpenGL error is left.

`tests/far_jump_check.cpp` builds MonoMod's case. It allocates code as Mono does, reserves every free page within 2 GB of it, allocates a target beyond, finds the cell with MonoMod 1.3.3's walk, writes the jump and calls it. It prints `far_jump_check: PASS` when the call reaches the target. It prints `FAIL` with the address that the jump read otherwise. Under Wine without patch 0005 the code lands at 0x7e0000, the walk takes page 0, the cell lands at 0x80800000 and the jump faults. The program is linked with high-entropy addresses, as Unity's players are.

`tests/stack_sample_check.cpp` samples a thread as a profiler does. A second thread makes system calls in a loop. The program suspends it 20000 times, reads `Rsp` and checks that every page from there up to `StackBase` is readable, when that range is shorter than 8 MB. It prints `stack_sample_check: PASS`, or `FAIL` with the count of copies that would fault. Under Wine without patch 0005 about a quarter of the samples report an `Rsp` above the thread's stack. With the first version of the patch the same share lay just below the stack. Every one of those copies would fault.

`tests/session_check.cpp` asks for `WTSSessionInfoEx` and `WTSIsRemoteSession` as SteamVR's compositor does. It prints `session_check: PASS` when the session is active, unlocked and local. Under Wine without patch 0006 it prints `FAIL: WTSSessionInfoEx has no answer`.

`tests/microphone_check.cpp` records from the default capture device as a voice chat does: WASAPI in shared mode with an event and a buffer of 20 ms. With `--list` it prints the device's name and ends. Otherwise it prints an `onset` line for each sound that starts after 200 ms of quiet, with the time at which it got the block. The time is the performance counter in units of 100 ns, which is `mach_continuous_time` under Wine on macOS, so a program on the Mac can compare it with its own clock. At the end it prints the frames a second and the blocks with a discontinuity.

`tests/metal_texture_check.cpp` does what the runtime does for a Vulkan game. It makes an IOSurface with a Metal texture through `winemetal.dll`, writes a pattern into the texture, creates a Vulkan image on the texture with `VkImportMetalTextureInfoEXT` and copies the image into a buffer with Vulkan. It prints `metal_texture_check: PASS` when the buffer holds the pattern. Under Wine without patch 0009 it prints `FAIL` with the first pixel that differs.

`scripts/setup-wine.sh` runs the seven programs in the prefix after each install and stops when one fails. It runs `microphone_check.exe --list` with a config dir of its own that chooses `off`.
