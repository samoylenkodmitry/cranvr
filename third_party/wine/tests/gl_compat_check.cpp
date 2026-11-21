// Asks Wine for the OpenGL context that Google Earth VR asks for and draws with a shader of GLSL 1.10. A check of
// cranvr's Wine build: on macOS, winemac must make a core context for a request of OpenGL 3.2 with the compatibility
// profile (patch 0002), and opengl32 must compile the legacy GLSL of such a program (patch 0003).
//
// It makes a hidden window, a context with the attributes of Earth VR (3.2, no flags, the compatibility profile), a
// texture of 64x64 as the target of a framebuffer, and a program of GLSL 1.10 that uses attribute, varying,
// texture2D and gl_FragColor. It draws one quad that samples a texture of one orange pixel and reads the result back.
//
// Usage: gl_compat_check.exe
// Prints "gl_compat_check: PASS" or "gl_compat_check: FAIL" with the reason.

#include <windows.h>
#include <GL/gl.h>
#include <cstddef>
#include <cstdio>

namespace {

typedef HGLRC(WINAPI *PFN_wglCreateContextAttribsARB)(HDC, HGLRC, const int *);
typedef void(APIENTRY *PFN_glGenObjects)(GLsizei, GLuint *);
typedef void(APIENTRY *PFN_glBindTarget)(GLenum, GLuint);
typedef void(APIENTRY *PFN_glFramebufferTexture2D)(GLenum, GLenum, GLenum, GLuint, GLint);
typedef void(APIENTRY *PFN_glBindVertexArray)(GLuint);
typedef void(APIENTRY *PFN_glBufferData)(GLenum, ptrdiff_t, const void *, GLenum);
typedef void(APIENTRY *PFN_glVertexAttribPointer)(GLuint, GLint, GLenum, GLboolean, GLsizei, const void *);
typedef void(APIENTRY *PFN_glEnableVertexAttribArray)(GLuint);
typedef GLuint(APIENTRY *PFN_glCreateShader)(GLenum);
typedef void(APIENTRY *PFN_glShaderSource)(GLuint, GLsizei, const char *const *, const GLint *);
typedef void(APIENTRY *PFN_glCompileShader)(GLuint);
typedef void(APIENTRY *PFN_glGetShaderiv)(GLuint, GLenum, GLint *);
typedef void(APIENTRY *PFN_glGetShaderInfoLog)(GLuint, GLsizei, GLsizei *, char *);
typedef GLuint(APIENTRY *PFN_glCreateProgram)();
typedef void(APIENTRY *PFN_glAttachShader)(GLuint, GLuint);
typedef void(APIENTRY *PFN_glBindAttribLocation)(GLuint, GLuint, const char *);
typedef void(APIENTRY *PFN_glLinkProgram)(GLuint);
typedef void(APIENTRY *PFN_glGetProgramiv)(GLuint, GLenum, GLint *);
typedef void(APIENTRY *PFN_glUseProgram)(GLuint);

int fail(const char *what)
{
    std::printf("gl_compat_check: FAIL %s\n", what);
    return 1;
}

template <typename T> bool load(T &function, const char *name)
{
    function = reinterpret_cast<T>(wglGetProcAddress(name));
    if (!function) std::printf("gl_compat_check: no %s\n", name);
    return function != nullptr;
}

// Two strings with a length each and no terminating zero, as a game may pass them
const char vertex_head[] = "#version 110\nattribute vec2 aPosition;\n";
const char vertex_body[] = "varying vec2 vCoord;\nvoid main() { vCoord = aPosition * 0.5 + 0.5; gl_Position = vec4(aPosition, 0.0, 1.0); }\n";
const char fragment_source[] =
    "// a comment before the version\n"
    "#version 110\n"
    "uniform sampler2D uTexture;\n"
    "varying vec2 vCoord;\n"
    "void main() { gl_FragColor = texture2D(uTexture, vCoord); }\n";

} // namespace

int main()
{
    WNDCLASSA window_class{};
    window_class.lpfnWndProc = DefWindowProcA;
    window_class.hInstance = GetModuleHandleA(nullptr);
    window_class.lpszClassName = "gl_compat_check";
    RegisterClassA(&window_class);
    HWND window = CreateWindowA("gl_compat_check", "gl_compat_check", WS_OVERLAPPEDWINDOW, 0, 0, 64, 64, nullptr, nullptr,
                                window_class.hInstance, nullptr);
    HDC dc = window ? GetDC(window) : nullptr;
    if (!dc) return fail("no window");
    PIXELFORMATDESCRIPTOR format{};
    format.nSize = sizeof(format);
    format.nVersion = 1;
    format.dwFlags = PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER;
    format.iPixelType = PFD_TYPE_RGBA;
    format.cColorBits = 32;
    const int chosen = ChoosePixelFormat(dc, &format);
    if (!chosen || !SetPixelFormat(dc, chosen, &format)) return fail("no pixel format");
    HGLRC first = wglCreateContext(dc);
    if (!first || !wglMakeCurrent(dc, first)) return fail("no first context");
    auto create = reinterpret_cast<PFN_wglCreateContextAttribsARB>(wglGetProcAddress("wglCreateContextAttribsARB"));
    if (!create) return fail("no wglCreateContextAttribsARB");
    // WGL_CONTEXT_MAJOR_VERSION_ARB 3, WGL_CONTEXT_MINOR_VERSION_ARB 2, WGL_CONTEXT_FLAGS_ARB 0,
    // WGL_CONTEXT_PROFILE_MASK_ARB WGL_CONTEXT_COMPATIBILITY_PROFILE_BIT_ARB: the attributes of Earth VR
    const int attributes[] = { 0x2091, 3, 0x2092, 2, 0x2094, 0, 0x9126, 2, 0 };
    HGLRC context = create(dc, nullptr, attributes);
    wglMakeCurrent(nullptr, nullptr);
    wglDeleteContext(first);
    if (!context) return fail("no context of OpenGL 3.2 with the compatibility profile");
    if (!wglMakeCurrent(dc, context)) return fail("the context does not become current");
    std::printf("gl_compat_check: OpenGL %s, GLSL %s\n", reinterpret_cast<const char *>(glGetString(GL_VERSION)),
                reinterpret_cast<const char *>(glGetString(0x8B8C /* GL_SHADING_LANGUAGE_VERSION */)));

    PFN_glGenObjects GenFramebuffers, GenVertexArrays, GenBuffers;
    PFN_glBindTarget BindFramebuffer, BindBuffer;
    PFN_glFramebufferTexture2D FramebufferTexture2D;
    PFN_glBindVertexArray BindVertexArray;
    PFN_glBufferData BufferData;
    PFN_glVertexAttribPointer VertexAttribPointer;
    PFN_glEnableVertexAttribArray EnableVertexAttribArray;
    PFN_glCreateShader CreateShader;
    PFN_glShaderSource ShaderSource;
    PFN_glCompileShader CompileShader;
    PFN_glGetShaderiv GetShaderiv;
    PFN_glGetShaderInfoLog GetShaderInfoLog;
    PFN_glCreateProgram CreateProgram;
    PFN_glAttachShader AttachShader;
    PFN_glBindAttribLocation BindAttribLocation;
    PFN_glLinkProgram LinkProgram;
    PFN_glGetProgramiv GetProgramiv;
    PFN_glUseProgram UseProgram;
    if (!load(GenFramebuffers, "glGenFramebuffers") || !load(GenVertexArrays, "glGenVertexArrays") || !load(GenBuffers, "glGenBuffers")
        || !load(BindFramebuffer, "glBindFramebuffer") || !load(BindBuffer, "glBindBuffer")
        || !load(FramebufferTexture2D, "glFramebufferTexture2D") || !load(BindVertexArray, "glBindVertexArray")
        || !load(BufferData, "glBufferData") || !load(VertexAttribPointer, "glVertexAttribPointer")
        || !load(EnableVertexAttribArray, "glEnableVertexAttribArray") || !load(CreateShader, "glCreateShader")
        || !load(ShaderSource, "glShaderSource") || !load(CompileShader, "glCompileShader") || !load(GetShaderiv, "glGetShaderiv")
        || !load(GetShaderInfoLog, "glGetShaderInfoLog") || !load(CreateProgram, "glCreateProgram")
        || !load(AttachShader, "glAttachShader") || !load(BindAttribLocation, "glBindAttribLocation")
        || !load(LinkProgram, "glLinkProgram") || !load(GetProgramiv, "glGetProgramiv") || !load(UseProgram, "glUseProgram"))
        return fail("a function is missing");

    const auto compile = [&](GLenum type, GLsizei count, const char *const *strings, const GLint *lengths) -> GLuint {
        const GLuint shader = CreateShader(type);
        ShaderSource(shader, count, strings, lengths);
        CompileShader(shader);
        GLint ok = 0;
        GetShaderiv(shader, 0x8B81 /* GL_COMPILE_STATUS */, &ok);
        if (!ok) {
            char log[1024] = {};
            GetShaderInfoLog(shader, sizeof(log), nullptr, log);
            std::printf("gl_compat_check: %s\n", log);
            return 0;
        }
        return shader;
    };
    const char *vertex_strings[] = { vertex_head, vertex_body };
    const GLint vertex_lengths[] = { static_cast<GLint>(sizeof(vertex_head) - 1), static_cast<GLint>(sizeof(vertex_body) - 1) };
    const char *fragment_strings[] = { fragment_source };
    const GLuint vertex = compile(0x8B31 /* GL_VERTEX_SHADER */, 2, vertex_strings, vertex_lengths);
    const GLuint fragment = compile(0x8B30 /* GL_FRAGMENT_SHADER */, 1, fragment_strings, nullptr);
    if (!vertex || !fragment) return fail("a shader of GLSL 1.10 does not compile");
    const GLuint program = CreateProgram();
    AttachShader(program, vertex);
    AttachShader(program, fragment);
    BindAttribLocation(program, 0, "aPosition");
    LinkProgram(program);
    GLint linked = 0;
    GetProgramiv(program, 0x8B82 /* GL_LINK_STATUS */, &linked);
    if (!linked) return fail("the program of GLSL 1.10 does not link");

    // An orange texture of one pixel to sample, a 64x64 target and a quad over all of it
    const unsigned char orange[4] = { 230, 128, 26, 255 };
    GLuint textures[2];
    glGenTextures(2, textures);
    glBindTexture(GL_TEXTURE_2D, textures[0]);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexImage2D(GL_TEXTURE_2D, 0, 0x8058 /* GL_RGBA8 */, 1, 1, 0, GL_RGBA, GL_UNSIGNED_BYTE, orange);
    glBindTexture(GL_TEXTURE_2D, textures[1]);
    glTexImage2D(GL_TEXTURE_2D, 0, 0x8058, 64, 64, 0, GL_RGBA, GL_UNSIGNED_BYTE, nullptr);
    GLuint framebuffer, vao, buffer;
    GenFramebuffers(1, &framebuffer);
    BindFramebuffer(0x8D40 /* GL_FRAMEBUFFER */, framebuffer);
    FramebufferTexture2D(0x8D40, 0x8CE0 /* GL_COLOR_ATTACHMENT0 */, GL_TEXTURE_2D, textures[1], 0);
    const float quad[] = { -1, -1, 1, -1, -1, 1, 1, 1 };
    GenVertexArrays(1, &vao);
    BindVertexArray(vao);
    GenBuffers(1, &buffer);
    BindBuffer(0x8892 /* GL_ARRAY_BUFFER */, buffer);
    BufferData(0x8892, sizeof(quad), quad, 0x88E4 /* GL_STATIC_DRAW */);
    VertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, nullptr);
    EnableVertexAttribArray(0);
    glBindTexture(GL_TEXTURE_2D, textures[0]);
    UseProgram(program);
    glViewport(0, 0, 64, 64);
    glClearColor(0, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    unsigned char pixel[4] = {};
    glReadPixels(32, 32, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
    const GLenum error = glGetError();
    std::printf("gl_compat_check: the quad's middle is %u %u %u, GL error 0x%x\n", pixel[0], pixel[1], pixel[2], error);
    wglMakeCurrent(nullptr, nullptr);
    wglDeleteContext(context);
    ReleaseDC(window, dc);
    DestroyWindow(window);
    if (error != GL_NO_ERROR) return fail("a GL error");
    if (pixel[0] != orange[0] || pixel[1] != orange[1] || pixel[2] != orange[2]) return fail("the quad does not have the texture's color");
    std::printf("gl_compat_check: PASS\n");
    return 0;
}
