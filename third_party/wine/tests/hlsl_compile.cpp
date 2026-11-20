// Compiles HLSL through a d3dcompiler_47.dll and prints what it says. A check of cranvr's
// Wine build: its HLSL compiler (vkd3d-shader) must accept the intrinsics that games call.
//
// Usage: hlsl_compile.exe [--dll PATH] [--target ps_5_0] [--entry main] [--out FILE.dxbc] FILE.hlsl
//   --dll     the compiler to load (default d3dcompiler_47.dll, which is Wine's own under Wine)
//   --target  the shader profile (default ps_5_0)
//   --entry   the entry point (default main)
//   --out     write the compiled shader here
// Prints "hlsl_compile: PASS (N bytes)" or "hlsl_compile: FAIL" with the compiler's messages.

#include <windows.h>
#include <d3dcompiler.h>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <iterator>
#include <string>

int main(int argc, char **argv)
{
    std::string dll = "d3dcompiler_47.dll", target = "ps_5_0", entry = "main", out, file;
    for (int i = 1; i < argc; ++i) {
        std::string a = argv[i];
        if (a == "--dll" && i + 1 < argc) dll = argv[++i];
        else if (a == "--target" && i + 1 < argc) target = argv[++i];
        else if (a == "--entry" && i + 1 < argc) entry = argv[++i];
        else if (a == "--out" && i + 1 < argc) out = argv[++i];
        else file = a;
    }
    if (file.empty()) {
        std::fprintf(stderr, "usage: hlsl_compile.exe [--dll PATH] [--target T] [--entry E] [--out F] FILE.hlsl\n");
        return 2;
    }
    std::ifstream in(file, std::ios::binary);
    if (!in) {
        std::fprintf(stderr, "hlsl_compile: cannot read %s\n", file.c_str());
        return 2;
    }
    std::string source((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());

    HMODULE module = LoadLibraryA(dll.c_str());
    if (!module) {
        std::fprintf(stderr, "hlsl_compile: cannot load %s (error %lu)\n", dll.c_str(), GetLastError());
        return 2;
    }
    char path[MAX_PATH] = {};
    GetModuleFileNameA(module, path, MAX_PATH);
    std::printf("hlsl_compile: compiler %s\n", path);
    auto compile = reinterpret_cast<decltype(&D3DCompile)>(GetProcAddress(module, "D3DCompile"));
    if (!compile) {
        std::fprintf(stderr, "hlsl_compile: %s has no D3DCompile\n", dll.c_str());
        return 2;
    }

    ID3DBlob *code = nullptr, *messages = nullptr;
    HRESULT hr = compile(source.data(), source.size(), file.c_str(), nullptr, nullptr, entry.c_str(),
                         target.c_str(), 0, 0, &code, &messages);
    if (messages && messages->GetBufferSize())
        std::printf("%.*s\n", static_cast<int>(messages->GetBufferSize()),
                    static_cast<const char *>(messages->GetBufferPointer()));
    if (FAILED(hr) || !code) {
        std::printf("hlsl_compile: FAIL (hr 0x%08lx)\n", static_cast<unsigned long>(hr));
        return 1;
    }
    if (!out.empty()) {
        std::ofstream(out, std::ios::binary).write(static_cast<const char *>(code->GetBufferPointer()),
                                                   static_cast<std::streamsize>(code->GetBufferSize()));
    }
    std::printf("hlsl_compile: PASS (%zu bytes)\n", static_cast<size_t>(code->GetBufferSize()));
    return 0;
}
