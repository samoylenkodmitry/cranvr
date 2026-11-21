# Wine patches

cranvr keeps Gcenx's macOS build of Wine 11.10 as its engine. It replaces four files of it with files built from the same Wine tag and cranvr's patches:

- `wined3d.dll` holds vkd3d-shader, the HLSL compiler (LGPL 2.1 or later, like Wine). A game that compiles its shaders at run time calls `D3DCompile` in `d3dcompiler_47.dll`, which calls vkd3d-shader in `wined3d.dll`. A game ships its own `d3dcompiler_47.dll` too. Wine loads its own copy first, so every game's HLSL goes through vkd3d-shader.
- `winemac.so` is the Mac side of Wine's Mac driver. It makes the OpenGL contexts of a Windows program.
- `opengl32.so` is the Mac side of Wine's `opengl32.dll`. Every OpenGL call of a program goes through it.
- `wininet.dll` is Wine's HTTP client of the WinINet API.

`scripts/build-wine.sh` clones Wine at the tag `wine-11.10` (commit `2cac6ccf33c0807f374dc96f5a20e35a2da86157`) into `third_party/wine/src` (not committed), applies the patches in this folder and builds the four files into `third_party/wine/build/bin`: the DLLs with MinGW-w64, the Mac libraries with Xcode's clang for x86_64 and macOS 10.15 and newer, as Gcenx's build has. The build runs under Rosetta 2, because Wine's configure wants a 64-bit Intel host. `scripts/setup-wine.sh` copies the DLLs into the engine and into the prefix's `system32`, where the loader takes them, and the libraries into the engine. The wine tree is not used otherwise. `package-game-support.sh` packs the pinned source tree and the patches as `SOURCE/wine-source.tar.xz` and as `cranvr-wine-source.tar.xz` in the release.

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

## tests

`tests/hlsl_compile.cpp` loads a `d3dcompiler_47.dll` and compiles one HLSL file with `D3DCompile`. It prints `hlsl_compile: PASS` or `FAIL` with the compiler's messages. `tests/isnan.hlsl` calls `isnan` on a scalar and on a vector.

`tests/gl_compat_check.cpp` asks for the context that Earth VR asks for, compiles a vertex and a fragment shader of GLSL 1.10 (the vertex shader in two strings with lengths and no terminating zero), links them, draws a quad that samples a texture into a framebuffer and reads the middle back. It prints `gl_compat_check: PASS` when the pixel has the texture's color and no OpenGL error is left.

`scripts/setup-wine.sh` runs both programs in the prefix after each install and stops when one fails.
