# Wine patches

cranvr keeps Gcenx's macOS build of Wine 11.10 as its engine. Wine 11.10 has one more part that games reach: `wined3d.dll` holds vkd3d-shader, the HLSL compiler (LGPL 2.1 or later, like Wine). A game that compiles its shaders at run time calls `D3DCompile` in `d3dcompiler_47.dll`, which calls vkd3d-shader in `wined3d.dll`. A game ships its own `d3dcompiler_47.dll` too, but Wine takes its own copy first, so every game's HLSL goes through vkd3d-shader.

`scripts/build-wine-d3d.sh` clones Wine at the tag `wine-11.10` (commit `2cac6ccf33c0807f374dc96f5a20e35a2da86157`) into `third_party/wine/src` (not committed), applies the patches in this folder and builds `wined3d.dll` with MinGW-w64 into `third_party/wine/build/bin`. The build runs under Rosetta 2, because Wine's configure wants a 64-bit Intel host. `scripts/setup-wine.sh` copies the DLL into the engine and into the prefix's `system32`, where the loader takes it. The wine tree is not used otherwise. `package-game-support.sh` packs the pinned source tree and the patches as `SOURCE/wine-source.tar.xz` and as `cranvr-wine-source.tar.xz` in the release.

## patches

`0001-vkd3d-shader-hlsl-isnan-intrinsic.patch`

- Wine 11.10 carries vkd3d 2.0 (shader API 1.19). Its HLSL parser knows `isinf` and lacks `isnan`: a shader that calls it fails with `E5005: Function "isnan" is not defined`. The intrinsic is `x != x` after the usual conversion of the argument to float. The comparison is unordered, so it is true for NaN. `tests/isnan.hlsl` compiles to a shader with two `ne` instructions (opcode 0x39 of Direct3D 10's shader format), one for the vector and one for the scalar.
- A game whose pixel shaders end with a check for a NaN output (the shader paints it magenta) calls `isnan` in every pixel shader that has the check. Without the intrinsic those shaders fail to compile, and the game draws no picture.

## tests

`tests/hlsl_compile.cpp` loads a `d3dcompiler_47.dll` and compiles one HLSL file with `D3DCompile`. It prints `hlsl_compile: PASS` or `FAIL` with the compiler's messages. `tests/isnan.hlsl` calls `isnan` on a scalar and on a vector. `scripts/setup-wine.sh` runs the pair in the prefix after each install and stops when it fails.
