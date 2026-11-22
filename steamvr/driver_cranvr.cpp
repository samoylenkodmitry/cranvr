// cranvr's driver for SteamVR (the OpenVR driver API). SteamVR for Windows runs in cranvr's Wine prefix and
// loads this DLL into vrserver.exe as the driver "cranvr". The driver gives SteamVR a headset and two Touch
// controllers that follow the headset that cranvr-host serves, and it hands each frame of SteamVR's compositor
// to the host.
//
// The link to the host is the one of cranvr's OpenXR runtime: BasaltVR's control protocol, version 9, over
// loopback TCP (host/src/runtime_link.rs). The control file that BASALT_CONTROL names gives the port, the token,
// the size of an eye and the refresh rate. The host sends the head pose, the controllers' poses, the buttons
// and the field of view of each eye. The driver sends the frames and the haptic pulses.
//
// Frames. SteamVR runs in its direct mode with this driver (IVRDriverDirectModeComponent). Its compositor draws
// each layer of a frame (a game's eyes, then SteamVR's own panels such as the controller hints) into textures that
// the driver made for it, and calls Present. The driver draws the layers into one of its three pairs of eye images
// with one draw per layer and eye, waits for its GPU and tells the host which images hold the frame. The eye images are
// IOSurfaces: DXMT backs a SHARED_NTHANDLE BGRA texture with one when DXMT_BASALT_IOSURFACE=1, as it does for the
// OpenXR runtime. The pixels never leave the GPU. DXMT's keyed mutex (cranvr's DXMT patch 0014) returns from the
// compositor's ReleaseSync only after its GPU work is done, so a layer is complete when Present arrives.
//
// Poses. The head pose goes to SteamVR without velocity, so the compositor draws with the very pose that the host
// sent. The driver keeps the last poses with the host's timestamps and finds the one that a frame was drawn
// with, which the host needs for the headset's reprojection. The controllers' poses go in SteamVR's "raw" pose
// of the Touch controller: the host sends OpenXR's grip pose, and SteamVR's render model of the controller says
// where the grip pose lies in the raw pose ("openxr_grip"). Their input profile is SteamVR's own one of the
// Touch controller, so a game's Touch bindings apply.
//
// The only header of Valve here is openvr_driver.h (BSD 3-Clause, openvr/LICENSE).

#ifndef NOMINMAX
#define NOMINMAX
#endif
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>

#include <d3d11_1.h>
#include <d3dcompiler.h>
#include <dxgi1_2.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdarg>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <deque>
#include <functional>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "openvr_driver.h"

using namespace vr;

namespace {

// ---- the control protocol (basalt_control_protocol.h of BasaltVR, version 9) ----

constexpr uint16_t kProtocolVersion = 9;
constexpr size_t kTokenLength = 32;
constexpr uint32_t kMaxMessage = 4096;
enum : uint16_t {
    MSG_HELLO = 1,
    MSG_HELLO_ACK = 2,
    MSG_CREATE_SWAPCHAIN = 3,
    MSG_SWAPCHAIN_ACK = 4,
    MSG_DESTROY_SWAPCHAIN = 5,
    MSG_DESTROY_SWAPCHAIN_ACK = 6,
    MSG_FRAME_READY = 7,
    MSG_TEARDOWN = 8,
    MSG_HEAD_POSE = 9,
    MSG_CONTROLLER_MOTION = 10,
    MSG_BUTTON_STATE = 11,
    MSG_HAPTIC = 12,
    MSG_INTERACTION_PROFILE = 13,
    MSG_INPUT_RESET = 14,
    MSG_VIEW_CONFIG = 15,
    MSG_CONTROLLER_MASK = 16,
    MSG_FRAME_CONSUMED = 17,
    MSG_CONTROLLER_MOTION_AIM = 18,
};

// The ids of the controller inputs: the 64-bit hashes of OpenXR paths (common/src/inputs.rs)
enum : uint64_t {
    LEFT_X_CLICK = 0xb1a13c485685e33cull,
    LEFT_X_TOUCH = 0xc3d476432e89d9ddull,
    LEFT_Y_CLICK = 0x521496d23e3e9883ull,
    LEFT_Y_TOUCH = 0x117d0816b380e1f2ull,
    RIGHT_A_CLICK = 0xc72a0ba6ca52fba7ull,
    RIGHT_A_TOUCH = 0x07aedc78fdf1bf2aull,
    RIGHT_B_CLICK = 0x955047466e34669bull,
    RIGHT_B_TOUCH = 0xdd8323caa5e48729ull,
    LEFT_MENU_CLICK = 0x6268a669e87cf910ull,
    LEFT_THUMBSTICK_CLICK = 0xdfc8a61273cf2569ull,
    RIGHT_THUMBSTICK_CLICK = 0x00ac9a92f4453d61ull,
    LEFT_THUMBSTICK_TOUCH = 0x0966df80f4989bfcull,
    RIGHT_THUMBSTICK_TOUCH = 0xef43fe49dfc87556ull,
    LEFT_THUMBSTICK_X = 0xc3d709ed0d794ea1ull,
    RIGHT_THUMBSTICK_X = 0xd7c23648f060c6dfull,
    LEFT_THUMBSTICK_Y = 0xcb7d928706263554ull,
    RIGHT_THUMBSTICK_Y = 0xcee8c338e02e0131ull,
    LEFT_TRIGGER_TOUCH = 0x4fa57f59dbae033aull,
    RIGHT_TRIGGER_TOUCH = 0xe0a7e848dde340b0ull,
    LEFT_TRIGGER_VALUE = 0x0b66bbe3dc2dc49full,
    RIGHT_TRIGGER_VALUE = 0xede7e982d5eedf21ull,
    LEFT_SQUEEZE_VALUE = 0x01f1b9c689804b82ull,
    RIGHT_SQUEEZE_VALUE = 0xdd5935cc005136f1ull,
    LEFT_THUMBREST_TOUCH = 0x3ce1c9ed4d12f3c2ull,
    RIGHT_THUMBREST_TOUCH = 0x947e5a8f8780c92dull,
};

// DXMT names the IOSurface of a texture under this GUID (BasaltVR's DXMT patch 0001)
const GUID kIOSurfaceIdGuid = {0xdd807311, 0x529e, 0x4856, {0xa5, 0xc0, 0x48, 0xbe, 0xd2, 0x04, 0x81, 0x29}};

// The universe of every device of this driver. Its chaperone (below) names the same number.
constexpr uint64_t kUniverse = 0x6372616e7672ull;

// ---- log ----

std::mutex g_logLock;
FILE *g_logFile = nullptr;
std::chrono::steady_clock::time_point g_start = std::chrono::steady_clock::now();

void Log(const char *format, ...) {
    char text[1024];
    va_list args;
    va_start(args, format);
    vsnprintf(text, sizeof text, format, args);
    va_end(args);
    if (VRDriverLog()) {
        char line[1100];
        snprintf(line, sizeof line, "%s\n", text);
        VRDriverLog()->Log(line);
    }
    std::lock_guard<std::mutex> lock(g_logLock);
    if (g_logFile) {
        double seconds = std::chrono::duration<double>(std::chrono::steady_clock::now() - g_start).count();
        fprintf(g_logFile, "%9.3f %s\n", seconds, text);
        fflush(g_logFile);
    }
}

// ---- math ----

struct Vec3 {
    float x = 0, y = 0, z = 0;
};
struct Quat {
    float x = 0, y = 0, z = 0, w = 1;
};
struct Pose {
    Quat q;
    Vec3 p;
};

Vec3 Add(Vec3 a, Vec3 b) { return {a.x + b.x, a.y + b.y, a.z + b.z}; }
Vec3 Sub(Vec3 a, Vec3 b) { return {a.x - b.x, a.y - b.y, a.z - b.z}; }
Vec3 Cross(Vec3 a, Vec3 b) { return {a.y * b.z - a.z * b.y, a.z * b.x - a.x * b.z, a.x * b.y - a.y * b.x}; }
float Length(Vec3 a) { return std::sqrt(a.x * a.x + a.y * a.y + a.z * a.z); }
Quat Mul(Quat a, Quat b) {
    return {a.w * b.x + a.x * b.w + a.y * b.z - a.z * b.y, a.w * b.y - a.x * b.z + a.y * b.w + a.z * b.x,
            a.w * b.z + a.x * b.y - a.y * b.x + a.z * b.w, a.w * b.w - a.x * b.x - a.y * b.y - a.z * b.z};
}
Quat Conj(Quat q) { return {-q.x, -q.y, -q.z, q.w}; }
Vec3 Rotate(Quat q, Vec3 v) {
    Quat r = Mul(Mul(q, {v.x, v.y, v.z, 0}), Conj(q));
    return {r.x, r.y, r.z};
}
Pose Compose(const Pose &a, const Pose &b) { return {Mul(a.q, b.q), Add(a.p, Rotate(a.q, b.p))}; }
Pose Inverse(const Pose &a) {
    Quat c = Conj(a.q);
    Vec3 p = Rotate(c, a.p);
    return {c, {-p.x, -p.y, -p.z}};
}
// The angle between two orientations, in radians
float AngleBetween(Quat a, Quat b) {
    float d = std::fabs(a.x * b.x + a.y * b.y + a.z * b.z + a.w * b.w);
    return 2.0f * std::acos(std::min(1.0f, d));
}
Quat QuatFromMatrix(const HmdMatrix34_t &m) {
    float t = m.m[0][0] + m.m[1][1] + m.m[2][2];
    Quat q;
    if (t > 0) {
        float s = std::sqrt(t + 1.0f) * 2;
        q = {(m.m[2][1] - m.m[1][2]) / s, (m.m[0][2] - m.m[2][0]) / s, (m.m[1][0] - m.m[0][1]) / s, 0.25f * s};
    } else if (m.m[0][0] > m.m[1][1] && m.m[0][0] > m.m[2][2]) {
        float s = std::sqrt(1.0f + m.m[0][0] - m.m[1][1] - m.m[2][2]) * 2;
        q = {0.25f * s, (m.m[0][1] + m.m[1][0]) / s, (m.m[0][2] + m.m[2][0]) / s, (m.m[2][1] - m.m[1][2]) / s};
    } else if (m.m[1][1] > m.m[2][2]) {
        float s = std::sqrt(1.0f + m.m[1][1] - m.m[0][0] - m.m[2][2]) * 2;
        q = {(m.m[0][1] + m.m[1][0]) / s, 0.25f * s, (m.m[1][2] + m.m[2][1]) / s, (m.m[0][2] - m.m[2][0]) / s};
    } else {
        float s = std::sqrt(1.0f + m.m[2][2] - m.m[0][0] - m.m[1][1]) * 2;
        q = {(m.m[0][2] + m.m[2][0]) / s, (m.m[1][2] + m.m[2][1]) / s, 0.25f * s, (m.m[1][0] - m.m[0][1]) / s};
    }
    return q;
}
HmdMatrix34_t Translation(float x) {
    HmdMatrix34_t m = {};
    m.m[0][0] = m.m[1][1] = m.m[2][2] = 1;
    m.m[0][3] = x;
    return m;
}

// ---- the control file ----

struct Control {
    uint16_t port = 0;
    std::string token;
    uint32_t width = 0, height = 0, fps = 72;
};

std::string ControlPath() {
    char path[1024];
    DWORD n = GetEnvironmentVariableA("BASALT_CONTROL", path, sizeof path);
    return n > 0 && n < sizeof path ? std::string(path, n) : std::string();
}

bool ReadControl(Control &out) {
    std::string path = ControlPath();
    if (path.empty()) {
        return false;
    }
    FILE *file = fopen(path.c_str(), "rb");
    if (!file) {
        return false;
    }
    char line[256] = {};
    size_t n = fread(line, 1, sizeof line - 1, file);
    fclose(file);
    line[n] = 0;
    unsigned port = 0, width = 0, height = 0, fps = 0;
    char token[64] = {};
    int fields = sscanf(line, "%u %63s %u %u %u", &port, token, &width, &height, &fps);
    if (fields < 2 || port == 0 || port > 65535 || strlen(token) != kTokenLength) {
        return false;
    }
    out.port = static_cast<uint16_t>(port);
    out.token = token;
    out.width = fields >= 4 && width >= 256 ? width & ~1u : 1832;
    out.height = fields >= 4 && height >= 256 ? height & ~1u : 1920;
    out.fps = fields >= 5 && fps >= 60 && fps <= 240 ? fps : 72;
    return true;
}

// ---- packets ----

struct Writer {
    std::vector<uint8_t> bytes;
    template <typename T> void put(T value) {
        const uint8_t *p = reinterpret_cast<const uint8_t *>(&value);
        bytes.insert(bytes.end(), p, p + sizeof value);
    }
    void raw(const void *data, size_t size) {
        const uint8_t *p = static_cast<const uint8_t *>(data);
        bytes.insert(bytes.end(), p, p + size);
    }
};

struct Reader {
    const uint8_t *data;
    size_t size, at = 0;
    template <typename T> bool get(T &value) {
        if (at + sizeof value > size) {
            return false;
        }
        memcpy(&value, data + at, sizeof value);
        at += sizeof value;
        return true;
    }
    bool pose(Pose &p) { return get(p.q.x) && get(p.q.y) && get(p.q.z) && get(p.q.w) && get(p.p.x) && get(p.p.y) && get(p.p.z); }
    bool vec(Vec3 &v) { return get(v.x) && get(v.y) && get(v.z); }
};

// ---- what the host says about the headset ----

struct EyeFov {
    // tangents: left (negative), right, up, down (negative)
    float left, right, up, down;
};

// An eye's view as SteamVR takes it, at the start (GetProjectionRaw) and after a change (SetDisplayProjectionRaw).
// SteamVR builds its projection with the top as the lower edge of the view. The top therefore takes the tangent of
// the down angle and the bottom the tangent of the up angle, as in OpenComposite. With the other order on the Quest 3
// (44 degrees up, 55 down), SteamVR drew 55 degrees up and 44 down. The lower fifth of each eye then stayed black.
HmdRect2_t RawProjection(const EyeFov &fov) {
    HmdRect2_t rect;
    rect.vTopLeft = {{fov.left, fov.down}};
    rect.vBottomRight = {{fov.right, fov.up}};
    return rect;
}

struct Motion {
    Pose pose;
    Vec3 linear, angular;
};

// The vtables that SteamVR calls, method for method as in openvr_driver.h. SteamVR is built with MSVC. A
// method that returns a struct takes the address of the result after `this` in MSVC's ABI and before it in
// GCC's: these mirrors declare that address as an explicit argument after `this`, so GCC passes it where
// SteamVR puts it, and return it as MSVC does.
class DeviceDriverAbi {
public:
    virtual EVRInitError Activate(uint32_t objectId) = 0;
    virtual void Deactivate() = 0;
    virtual void EnterStandby() = 0;
    virtual void *GetComponent(const char *nameAndVersion) = 0;
    virtual void DebugRequest(const char *request, char *response, uint32_t responseSize) = 0;
    virtual DriverPose_t *GetPose(DriverPose_t *result) = 0;
};

class DisplayComponentAbi {
public:
    virtual void GetWindowBounds(int32_t *x, int32_t *y, uint32_t *width, uint32_t *height) = 0;
    virtual bool IsDisplayOnDesktop() = 0;
    virtual bool IsDisplayRealDisplay() = 0;
    virtual void GetRecommendedRenderTargetSize(uint32_t *width, uint32_t *height) = 0;
    virtual void GetEyeOutputViewport(EVREye eye, uint32_t *x, uint32_t *y, uint32_t *width, uint32_t *height) = 0;
    virtual void GetProjectionRaw(EVREye eye, float *left, float *right, float *top, float *bottom) = 0;
    virtual DistortionCoordinates_t *ComputeDistortion(DistortionCoordinates_t *result, EVREye eye, float u, float v) = 0;
    virtual bool ComputeInverseDistortion(HmdVector2_t *result, EVREye eye, uint32_t channel, float u, float v) = 0;
};

class Driver;
Driver *g_driver = nullptr;

// ---- the link to cranvr-host ----

class HostLink {
public:
    using Handler = std::function<void(uint16_t type, Reader &payload)>;

    void Start(Handler onMessage, std::function<void()> onConnect, std::function<void()> onDisconnect) {
        onMessage_ = std::move(onMessage);
        onConnect_ = std::move(onConnect);
        onDisconnect_ = std::move(onDisconnect);
        WSADATA wsa;
        WSAStartup(MAKEWORD(2, 2), &wsa);
        thread_ = std::thread([this] { Run(); });
    }

    void Stop() {
        stop_ = true;
        {
            std::lock_guard<std::mutex> lock(write_);
            if (socket_ != INVALID_SOCKET) {
                Writer w;
                w.put<uint32_t>(1);
                SendLocked(MSG_TEARDOWN, w.bytes);
                shutdown(socket_, SD_BOTH);
            }
        }
        if (thread_.joinable()) {
            thread_.join();
        }
    }

    bool Connected() const { return connected_; }

    // Ends the link: the link connects again at once and the host takes the next HELLO as a new game
    void Reconnect() {
        std::lock_guard<std::mutex> lock(write_);
        if (socket_ != INVALID_SOCKET) {
            Writer w;
            w.put<uint32_t>(1);
            SendLocked(MSG_TEARDOWN, w.bytes);
            shutdown(socket_, SD_BOTH);
        }
    }

    bool Send(uint16_t type, const std::vector<uint8_t> &payload) {
        std::lock_guard<std::mutex> lock(write_);
        return socket_ != INVALID_SOCKET && connected_ && SendLocked(type, payload);
    }

private:
    bool SendLocked(uint16_t type, const std::vector<uint8_t> &payload) {
        Writer w;
        w.put<uint32_t>(static_cast<uint32_t>(2 + payload.size()));
        w.put<uint16_t>(type);
        w.raw(payload.data(), payload.size());
        const char *at = reinterpret_cast<const char *>(w.bytes.data());
        size_t left = w.bytes.size();
        while (left > 0) {
            int sent = send(socket_, at, static_cast<int>(left), 0);
            if (sent <= 0) {
                return false;
            }
            at += sent;
            left -= static_cast<size_t>(sent);
        }
        return true;
    }

    bool ReadAll(SOCKET s, void *buffer, size_t size) {
        char *at = static_cast<char *>(buffer);
        while (size > 0) {
            int got = recv(s, at, static_cast<int>(size), 0);
            if (got <= 0) {
                return false;
            }
            at += got;
            size -= static_cast<size_t>(got);
        }
        return true;
    }

    void Run() {
        bool told = false;
        while (!stop_) {
            Control control;
            if (!ReadControl(control)) {
                if (!told) {
                    Log("no control file: BASALT_CONTROL is '%s'. SteamVR waits for cranvr's host.", ControlPath().c_str());
                    told = true;
                }
                Sleep(1000);
                continue;
            }
            SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
            DWORD timeout = 5000;
            setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, reinterpret_cast<const char *>(&timeout), sizeof timeout);
            setsockopt(s, SOL_SOCKET, SO_SNDTIMEO, reinterpret_cast<const char *>(&timeout), sizeof timeout);
            BOOL noDelay = TRUE;
            setsockopt(s, IPPROTO_TCP, TCP_NODELAY, reinterpret_cast<const char *>(&noDelay), sizeof noDelay);
            sockaddr_in address = {};
            address.sin_family = AF_INET;
            address.sin_port = htons(control.port);
            address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
            if (connect(s, reinterpret_cast<sockaddr *>(&address), sizeof address) != 0) {
                closesocket(s);
                if (!told) {
                    Log("cranvr's host does not answer on port %u; trying again each second", control.port);
                    told = true;
                }
                Sleep(1000);
                continue;
            }
            told = false;
            {
                std::lock_guard<std::mutex> lock(write_);
                socket_ = s;
                Writer w;
                w.put<uint16_t>(kProtocolVersion);
                w.raw(control.token.data(), kTokenLength);
                w.put<uint64_t>(GetCurrentProcessId());
                const char renderer[] = "SteamVR (cranvr driver)";
                w.put<uint16_t>(sizeof renderer - 1);
                w.raw(renderer, sizeof renderer - 1);
                SendLocked(MSG_HELLO, w.bytes);
            }
            Receive(s);
            {
                std::lock_guard<std::mutex> lock(write_);
                closesocket(s);
                socket_ = INVALID_SOCKET;
            }
            if (connected_.exchange(false)) {
                Log("cranvr's host closed the link");
                onDisconnect_();
            }
            if (!stop_) {
                Sleep(500);
            }
        }
    }

    void Receive(SOCKET s) {
        std::vector<uint8_t> buffer;
        while (!stop_) {
            uint32_t length = 0;
            uint16_t type = 0;
            if (!ReadAll(s, &length, sizeof length) || length < 2 || length > kMaxMessage || !ReadAll(s, &type, sizeof type)) {
                return;
            }
            buffer.resize(length - 2);
            if (!buffer.empty() && !ReadAll(s, buffer.data(), buffer.size())) {
                return;
            }
            Reader r{buffer.data(), buffer.size()};
            if (type == MSG_HELLO_ACK) {
                uint16_t version = 0;
                r.get(version);
                if (version != kProtocolVersion) {
                    Log("the host speaks protocol %u, this driver %u", version, kProtocolVersion);
                    return;
                }
                connected_ = true;
                Log("connected to cranvr's host");
                onConnect_();
                continue;
            }
            if (type == MSG_TEARDOWN) {
                return;
            }
            onMessage_(type, r);
        }
    }

    Handler onMessage_;
    std::function<void()> onConnect_, onDisconnect_;
    std::thread thread_;
    std::mutex write_;
    SOCKET socket_ = INVALID_SOCKET;
    std::atomic<bool> stop_{false};
    std::atomic<bool> connected_{false};
};

// ---- the compositor's textures and the eye images ----

const char kShader[] = R"(
cbuffer Layer : register(b0) {
    float4 out_tan;  // the output eye: left, right, up, down tangents
    float4 row0;     // rows 0, 1 and 3 of the layer's projection
    float4 row1;
    float4 row3;
    float4 bounds;   // uMin, vMin, uMax, vMax of the layer's texture
    float4 mode;     // x: decode sRGB in the shader; y: base layer; z: no projection, the layer fills the eye
};
Texture2D layer_texture : register(t0);
SamplerState layer_sampler : register(s0);
struct VsOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VsOut vs_main(uint id : SV_VertexID) {
    VsOut o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.uv = uv;
    o.pos = float4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    return o;
}
float3 to_linear(float3 c) {
    float3 low = c / 12.92;
    float3 high = pow((c + 0.055) / 1.055, 2.4);
    return lerp(low, high, step(0.04045, c));
}
float4 ps_main(VsOut i) : SV_Target {
    float2 luv = i.uv;
    if (mode.z < 0.5) {
        float4 d = float4(lerp(out_tan.x, out_tan.y, i.uv.x), lerp(out_tan.z, out_tan.w, i.uv.y), -1.0, 1.0);
        float w = dot(row3, d);
        float2 ndc = float2(dot(row0, d), dot(row1, d)) / w;
        luv = float2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    }
    if (luv.x < 0.0 || luv.y < 0.0 || luv.x > 1.0 || luv.y > 1.0) {
        return mode.y > 0.5 ? float4(0.0, 0.0, 0.0, 1.0) : float4(0.0, 0.0, 0.0, 0.0);
    }
    float4 c = layer_texture.SampleLevel(layer_sampler, lerp(bounds.xy, bounds.zw, luv), 0);
    if (mode.x > 0.5) {
        c.rgb = to_linear(c.rgb);
    }
    if (mode.y > 0.5) {
        c.a = 1.0;
    }
    return c;
}
)";

struct LayerConstants {
    float outTan[4];
    float row0[4], row1[4], row3[4];
    float bounds[4];
    float mode[4];
};

// A shader view's format for a texture's format, and whether the shader must decode sRGB itself
DXGI_FORMAT ViewFormat(DXGI_FORMAT format, bool *decode) {
    *decode = false;
    switch (format) {
    case DXGI_FORMAT_R8G8B8A8_TYPELESS:
    case DXGI_FORMAT_R8G8B8A8_UNORM:
        *decode = true;
        return DXGI_FORMAT_R8G8B8A8_UNORM;
    case DXGI_FORMAT_B8G8R8A8_TYPELESS:
    case DXGI_FORMAT_B8G8R8A8_UNORM:
        *decode = true;
        return DXGI_FORMAT_B8G8R8A8_UNORM;
    case DXGI_FORMAT_B8G8R8X8_TYPELESS:
    case DXGI_FORMAT_B8G8R8X8_UNORM:
        *decode = true;
        return DXGI_FORMAT_B8G8R8X8_UNORM;
    case DXGI_FORMAT_R10G10B10A2_TYPELESS:
    case DXGI_FORMAT_R10G10B10A2_UNORM:
        *decode = true;
        return DXGI_FORMAT_R10G10B10A2_UNORM;
    case DXGI_FORMAT_R16G16B16A16_TYPELESS:
        return DXGI_FORMAT_R16G16B16A16_FLOAT;
    default:
        return format;
    }
}

struct SwapSet {
    uint32_t pid = 0;
    DXGI_FORMAT format = DXGI_FORMAT_UNKNOWN;
    uint32_t width = 0, height = 0, samples = 1;
    SharedTextureHandle_t handles[3] = {};
    ID3D11Texture2D *textures[3] = {};
    ID3D11ShaderResourceView *views[3] = {};
    ID3D11Texture2D *resolved = nullptr; // for a multisampled set
    ID3D11ShaderResourceView *resolvedView = nullptr;
    bool decode = false;
    uint32_t index = 0;
};

using SubmitLayerPerEye = IVRDriverDirectModeComponent::SubmitLayerPerEye_t;
struct Layer {
    SubmitLayerPerEye eye[2];
};

class Frames : public IVRDriverDirectModeComponent {
public:
    // Called by the driver before SteamVR asks for anything
    void Configure(uint32_t width, uint32_t height) {
        width_ = width;
        height_ = height;
    }

    void SetFov(const EyeFov fov[2]) {
        std::lock_guard<std::mutex> lock(fovLock_);
        fov_[0] = fov[0];
        fov_[1] = fov[1];
    }

    void HostConnected() { register_ = true; }

    void HostDisconnected() {
        std::lock_guard<std::mutex> lock(slotLock_);
        for (auto &slot : slots_) {
            slot.pending = 0;
        }
        slotFree_.notify_all();
    }

    void FrameConsumed(uint64_t number) {
        std::lock_guard<std::mutex> lock(slotLock_);
        for (auto &slot : slots_) {
            if (slot.pending == number) {
                slot.pending = 0;
            }
        }
        slotFree_.notify_all();
    }

    void Shutdown() {
        std::lock_guard<std::mutex> lock(lock_);
        for (auto &set : sets_) {
            ReleaseSet(set);
        }
        sets_.clear();
        ReleaseOutputs();
        auto release = [](auto *&p) {
            if (p) {
                p->Release();
                p = nullptr;
            }
        };
        release(vs_);
        release(ps_);
        release(constants_);
        release(sampler_);
        release(opaque_);
        release(blend_);
        release(raster_);
        release(query_);
        release(context_);
        release(device_);
    }

    // IVRDriverDirectModeComponent

    void CreateSwapTextureSet(uint32_t pid, const SwapTextureSetDesc_t *desc, SwapTextureSet_t *out) override {
        std::lock_guard<std::mutex> lock(lock_);
        memset(out, 0, sizeof *out);
        if (!Device()) {
            return;
        }
        SwapSet set;
        set.pid = pid;
        set.format = static_cast<DXGI_FORMAT>(desc->nFormat);
        set.width = desc->nWidth;
        set.height = desc->nHeight;
        set.samples = std::max(1u, desc->nSampleCount);
        D3D11_TEXTURE2D_DESC td = {};
        td.Width = desc->nWidth;
        td.Height = desc->nHeight;
        td.MipLevels = 1;
        td.ArraySize = 1;
        td.Format = set.format;
        td.SampleDesc.Count = set.samples;
        td.Usage = D3D11_USAGE_DEFAULT;
        td.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
        td.MiscFlags = D3D11_RESOURCE_MISC_SHARED;
        DXGI_FORMAT viewFormat = ViewFormat(set.format, &set.decode);
        for (int i = 0; i < 3; i++) {
            HRESULT hr = device_->CreateTexture2D(&td, nullptr, &set.textures[i]);
            IDXGIResource *resource = nullptr;
            HANDLE handle = nullptr;
            if (SUCCEEDED(hr)) {
                hr = set.textures[i]->QueryInterface(__uuidof(IDXGIResource), reinterpret_cast<void **>(&resource));
            }
            if (SUCCEEDED(hr)) {
                hr = resource->GetSharedHandle(&handle);
                resource->Release();
            }
            if (FAILED(hr) || !handle) {
                Log("a swap texture of %ux%u, format %u, %u samples for process %u was not made (0x%08lx)", desc->nWidth,
                    desc->nHeight, desc->nFormat, set.samples, pid, static_cast<unsigned long>(hr));
                ReleaseSet(set);
                return;
            }
            set.handles[i] = reinterpret_cast<SharedTextureHandle_t>(handle);
            out->rSharedTextureHandles[i] = set.handles[i];
            if (set.samples == 1) {
                D3D11_SHADER_RESOURCE_VIEW_DESC vd = {};
                vd.Format = viewFormat;
                vd.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D;
                vd.Texture2D.MipLevels = 1;
                device_->CreateShaderResourceView(set.textures[i], &vd, &set.views[i]);
            }
        }
        if (set.samples > 1) {
            D3D11_TEXTURE2D_DESC rd = td;
            rd.SampleDesc.Count = 1;
            rd.MiscFlags = 0;
            rd.BindFlags = D3D11_BIND_SHADER_RESOURCE;
            rd.Format = viewFormat;
            if (SUCCEEDED(device_->CreateTexture2D(&rd, nullptr, &set.resolved))) {
                device_->CreateShaderResourceView(set.resolved, nullptr, &set.resolvedView);
            }
        }
        if (++setsMade_ <= 12) {
            Log("swap texture set for process %u: %ux%u, format %u, %u samples", pid, desc->nWidth, desc->nHeight,
                desc->nFormat, set.samples);
        }
        sets_.push_back(set);
    }

    void DestroySwapTextureSet(SharedTextureHandle_t handle) override {
        std::lock_guard<std::mutex> lock(lock_);
        for (auto it = sets_.begin(); it != sets_.end(); ++it) {
            if (std::find(std::begin(it->handles), std::end(it->handles), handle) != std::end(it->handles)) {
                ReleaseSet(*it);
                sets_.erase(it);
                return;
            }
        }
    }

    void DestroyAllSwapTextureSets(uint32_t pid) override {
        std::lock_guard<std::mutex> lock(lock_);
        for (auto it = sets_.begin(); it != sets_.end();) {
            if (it->pid == pid) {
                ReleaseSet(*it);
                it = sets_.erase(it);
            } else {
                ++it;
            }
        }
    }

    void GetNextSwapTextureSetIndex(SharedTextureHandle_t handles[2], uint32_t (*indices)[2]) override {
        std::lock_guard<std::mutex> lock(lock_);
        for (int e = 0; e < 2; e++) {
            SwapSet *set = Find(handles[e], nullptr);
            if (set) {
                set->index = (set->index + 1) % 3;
                (*indices)[e] = set->index;
            } else {
                (*indices)[e] = 0;
            }
        }
    }

    void SubmitLayer(const SubmitLayerPerEye (&perEye)[2]) override {
        std::lock_guard<std::mutex> lock(lock_);
        Layer layer;
        layer.eye[0] = perEye[0];
        layer.eye[1] = perEye[1];
        layers_.push_back(layer);
    }

    void Present(SharedTextureHandle_t syncTexture) override;

    void PostPresent(const Throttling_t *) override;

    void GetFrameTiming(DriverDirectMode_FrameTiming *timing) override { timing->m_nReprojectionFlags = 0; }

    // The vsync clock that PostPresent waits for
    void SetPeriod(double seconds) { period_ = seconds; }
    double Period() const { return period_; }

private:
    struct Slot {
        ID3D11Texture2D *eye[2] = {};
        ID3D11RenderTargetView *target[2] = {};
        uint32_t surface[2] = {};
        uint64_t pending = 0; // the frame that the host still reads, 0 when free
    };
    static constexpr int kSlots = 3;

    bool Device();
    bool MakeOutputs();
    void ReleaseOutputs();
    void ReleaseSet(SwapSet &set) {
        for (int i = 0; i < 3; i++) {
            if (set.views[i]) {
                set.views[i]->Release();
                set.views[i] = nullptr;
            }
            if (set.textures[i]) {
                set.textures[i]->Release();
                set.textures[i] = nullptr;
            }
        }
        if (set.resolvedView) {
            set.resolvedView->Release();
            set.resolvedView = nullptr;
        }
        if (set.resolved) {
            set.resolved->Release();
            set.resolved = nullptr;
        }
    }
    SwapSet *Find(SharedTextureHandle_t handle, int *index) {
        for (auto &set : sets_) {
            for (int i = 0; i < 3; i++) {
                if (set.handles[i] == handle) {
                    if (index) {
                        *index = i;
                    }
                    return &set;
                }
            }
        }
        return nullptr;
    }
    void Draw(const Layer &layer, int eye, bool base, const EyeFov &fov);

    std::mutex lock_;
    std::vector<SwapSet> sets_;
    std::vector<Layer> layers_;
    uint32_t setsMade_ = 0;
    uint32_t width_ = 1832, height_ = 1920;

    ID3D11Device *device_ = nullptr;
    ID3D11DeviceContext *context_ = nullptr;
    ID3D11VertexShader *vs_ = nullptr;
    ID3D11PixelShader *ps_ = nullptr;
    ID3D11Buffer *constants_ = nullptr;
    ID3D11SamplerState *sampler_ = nullptr;
    ID3D11BlendState *opaque_ = nullptr, *blend_ = nullptr;
    ID3D11RasterizerState *raster_ = nullptr;
    ID3D11Query *query_ = nullptr;
    bool deviceFailed_ = false;

    Slot slots_[kSlots];
    uint32_t outWidth_ = 0, outHeight_ = 0;
    uint64_t swapchainId_ = 0;
    std::atomic<bool> register_{false};
    std::mutex slotLock_;
    std::condition_variable slotFree_;
    uint64_t frameNumber_ = 0;

    std::mutex fovLock_;
    EyeFov fov_[2] = {{-1.0f, 1.0f, 1.0f, -1.0f}, {-1.0f, 1.0f, 1.0f, -1.0f}};

    std::atomic<double> period_{1.0 / 72};
    std::chrono::steady_clock::time_point lastPresent_ = std::chrono::steady_clock::now();

    // Counts for the log line every 5 seconds
    uint32_t presents_ = 0, sent_ = 0, dropped_ = 0, layerCount_ = 0, unknown_ = 0;
    // Which processes drew the layers of the last frame, as a log line when it changes
    std::string sources_;
    double waitMs_ = 0, composeMs_ = 0;
    std::chrono::steady_clock::time_point statsSince_ = std::chrono::steady_clock::now();
};

// ---- the devices ----

class Hmd;
class Controller;

class Driver : public IServerTrackedDeviceProvider {
public:
    EVRInitError Init(IVRDriverContext *context) override;
    void Cleanup() override;
    const char *const *GetInterfaceVersions() override { return k_InterfaceVersions; }
    void RunFrame() override;
    bool ShouldBlockStandbyMode() override { return false; }
    void EnterStandby() override {}
    void LeaveStandby() override {}

    void OnMessage(uint16_t type, Reader &r);
    void OnConnect();
    void OnDisconnect();

    // The host's timestamp of the head pose that a frame was drawn with
    uint64_t TimestampOf(const HmdMatrix34_t &pose, float *errorMeters, float *errorRadians);
    Pose LatestHead() {
        std::lock_guard<std::mutex> lock(headLock_);
        return heads_.empty() ? Pose{} : heads_.back().pose;
    }
    float Ipd() const { return ipd_; }
    EyeFov Fov(int eye) {
        std::lock_guard<std::mutex> lock(fovLock_);
        return fov_[eye];
    }
    uint32_t Width() const { return control_.width; }
    uint32_t Height() const { return control_.height; }
    uint32_t Fps() const { return fps_; }

    // A process whose layers reach a frame. The first one that is not SteamVR's compositor is the game.
    void NoteLayerProcess(uint32_t pid);

    HostLink link;
    Frames frames;

private:
    struct HeadSample {
        uint64_t timestamp;
        Pose pose;
    };
    void SendHaptic(uint32_t hand, float seconds, float frequency, float amplitude);

    Control control_;
    std::atomic<uint32_t> fps_{72};
    std::atomic<float> ipd_{0.063f};
    std::mutex fovLock_;
    EyeFov fov_[2];
    std::atomic<bool> fovChanged_{false};
    std::mutex headLock_;
    std::deque<HeadSample> heads_;
    Hmd *hmd_ = nullptr;
    Controller *controllers_[2] = {};
    std::chrono::steady_clock::time_point lastControlCheck_ = std::chrono::steady_clock::now();
    // The game that SteamVR runs for cranvr, and whether a process is SteamVR's compositor
    void WatchGame();
    std::mutex gameLock_;
    std::map<uint32_t, bool> compositorProcess_;
    uint32_t gamePid_ = 0;
    HANDLE game_ = nullptr;
    bool quitAsked_ = false;
};

class Display : public DisplayComponentAbi {
public:
    void GetWindowBounds(int32_t *x, int32_t *y, uint32_t *width, uint32_t *height) override {
        *x = 0;
        *y = 0;
        *width = g_driver->Width() * 2;
        *height = g_driver->Height();
    }
    bool IsDisplayOnDesktop() override { return false; }
    bool IsDisplayRealDisplay() override { return false; }
    void GetRecommendedRenderTargetSize(uint32_t *width, uint32_t *height) override {
        *width = g_driver->Width();
        *height = g_driver->Height();
    }
    void GetEyeOutputViewport(EVREye eye, uint32_t *x, uint32_t *y, uint32_t *width, uint32_t *height) override {
        *x = eye == Eye_Left ? 0 : g_driver->Width();
        *y = 0;
        *width = g_driver->Width();
        *height = g_driver->Height();
    }
    void GetProjectionRaw(EVREye eye, float *left, float *right, float *top, float *bottom) override {
        HmdRect2_t rect = RawProjection(g_driver->Fov(eye == Eye_Left ? 0 : 1));
        *left = rect.vTopLeft.v[0];
        *top = rect.vTopLeft.v[1];
        *right = rect.vBottomRight.v[0];
        *bottom = rect.vBottomRight.v[1];
    }
    DistortionCoordinates_t *ComputeDistortion(DistortionCoordinates_t *result, EVREye, float u, float v) override {
        // No lens here: the headset corrects its own
        result->rfRed[0] = result->rfGreen[0] = result->rfBlue[0] = u;
        result->rfRed[1] = result->rfGreen[1] = result->rfBlue[1] = v;
        return result;
    }
    bool ComputeInverseDistortion(HmdVector2_t *result, EVREye, uint32_t, float u, float v) override {
        result->v[0] = u;
        result->v[1] = v;
        return true;
    }
};

// A play area of 2 x 2 meters at the origin of the host's room, whose floor is at height 0. SteamVR then needs
// no room setup.
// The LUID of the first DXGI adapter, as SteamVR's Prop_GraphicsAdapterLuid_Uint64 holds it
static bool AdapterLuid(uint64_t *luid) {
    IDXGIFactory1 *factory = nullptr;
    if (FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), reinterpret_cast<void **>(&factory)))) {
        return false;
    }
    IDXGIAdapter1 *adapter = nullptr;
    bool found = false;
    if (SUCCEEDED(factory->EnumAdapters1(0, &adapter))) {
        DXGI_ADAPTER_DESC1 desc;
        if (SUCCEEDED(adapter->GetDesc1(&desc))) {
            *luid = (static_cast<uint64_t>(static_cast<uint32_t>(desc.AdapterLuid.HighPart)) << 32) | desc.AdapterLuid.LowPart;
            found = true;
        }
        adapter->Release();
    }
    factory->Release();
    return found;
}

std::string ChaperoneJson() {
    char json[1200];
    snprintf(json, sizeof json,
             "{\"jsonid\":\"chaperone_info\",\"universes\":[{\"collision_bounds\":["
             "[[-1,0,-1],[-1,2.43,-1],[-1,2.43,1],[-1,0,1]],[[-1,0,1],[-1,2.43,1],[1,2.43,1],[1,0,1]],"
             "[[1,0,1],[1,2.43,1],[1,2.43,-1],[1,0,-1]],[[1,0,-1],[1,2.43,-1],[-1,2.43,-1],[-1,0,-1]]],"
             "\"play_area\":[2,2],\"seated\":{\"translation\":[0,0,0],\"yaw\":0},"
             "\"standing\":{\"translation\":[0,0,0],\"yaw\":0},\"universeID\":\"%llu\"}],\"version\":5}",
             static_cast<unsigned long long>(kUniverse));
    return json;
}

class Hmd : public DeviceDriverAbi {
public:
    EVRInitError Activate(uint32_t id) override {
        id_ = id;
        PropertyContainerHandle_t c = VRProperties()->TrackedDeviceToPropertyContainer(id);
        VRProperties()->SetStringProperty(c, Prop_ModelNumber_String, "cranvr headset");
        VRProperties()->SetStringProperty(c, Prop_ManufacturerName_String, "cranvr");
        VRProperties()->SetStringProperty(c, Prop_TrackingSystemName_String, "cranvr");
        VRProperties()->SetStringProperty(c, Prop_RenderModelName_String, "generic_hmd");
        VRProperties()->SetFloatProperty(c, Prop_UserIpdMeters_Float, g_driver->Ipd());
        VRProperties()->SetFloatProperty(c, Prop_UserHeadToEyeDepthMeters_Float, 0.0f);
        VRProperties()->SetFloatProperty(c, Prop_DisplayFrequency_Float, static_cast<float>(g_driver->Fps()));
        VRProperties()->SetBoolProperty(c, Prop_DisplaySupportsRuntimeFramerateChange_Bool, true);
        VRProperties()->SetFloatProperty(c, Prop_SecondsFromVsyncToPhotons_Float, 0.0f);
        VRProperties()->SetUint64Property(c, Prop_CurrentUniverseId_Uint64, kUniverse);
        VRProperties()->SetBoolProperty(c, Prop_IsOnDesktop_Bool, false);
        VRProperties()->SetBoolProperty(c, Prop_DisplayDebugMode_Bool, false);
        VRProperties()->SetBoolProperty(c, Prop_HasDisplayComponent_Bool, true);
        VRProperties()->SetBoolProperty(c, Prop_HasDriverDirectModeComponent_Bool, true);
        VRProperties()->SetBoolProperty(c, Prop_HasVirtualDisplayComponent_Bool, false);
        VRProperties()->SetBoolProperty(c, Prop_HasCameraComponent_Bool, false);
        VRProperties()->SetBoolProperty(c, Prop_DriverDirectModeSendsVsyncEvents_Bool, true);
        VRProperties()->SetStringProperty(c, Prop_DriverProvidedChaperoneJson_String, ChaperoneJson().c_str());
        VRProperties()->SetBoolProperty(c, Prop_DriverProvidedChaperoneVisibility_Bool, false);
        VRProperties()->SetInt32Property(c, Prop_ExpectedControllerCount_Int32, 2);
        VRProperties()->SetStringProperty(c, Prop_ExpectedControllerType_String, "oculus_touch");
        VRProperties()->SetBoolProperty(c, Prop_DeviceProvidesBatteryStatus_Bool, false);
        // SteamVR's own input profile of an Oculus headset: the headset of the Touch controllers' bindings
        VRProperties()->SetStringProperty(c, Prop_InputProfilePath_String, "{oculus}/input/rift_profile.json");
        // The headset is on while the host streams: SteamVR looks for /proximity
        VRProperties()->SetBoolProperty(c, Prop_ContainsProximitySensor_Bool, true);
        VRDriverInput()->CreateBooleanComponent(c, "/proximity", &proximity_);
        VRDriverInput()->UpdateBooleanComponent(proximity_, true, 0);
        // SteamVR's dashboard and web panels make their Direct3D device on the headset's adapter, which they find
        // by this LUID. The Mac has one GPU, so the first adapter is the headset's.
        uint64_t luid = 0;
        if (AdapterLuid(&luid)) {
            VRProperties()->SetUint64Property(c, Prop_GraphicsAdapterLuid_Uint64, luid);
        }
        Log("headset active as device %u: %ux%u an eye at %u Hz", id, g_driver->Width(), g_driver->Height(), g_driver->Fps());
        return VRInitError_None;
    }
    void Deactivate() override { id_ = k_unTrackedDeviceIndexInvalid; }
    void EnterStandby() override {}
    void *GetComponent(const char *name) override {
        if (!strcmp(name, IVRDisplayComponent_Version)) {
            return &display_;
        }
        if (!strcmp(name, IVRDriverDirectModeComponent_Version)) {
            return static_cast<IVRDriverDirectModeComponent *>(&g_driver->frames);
        }
        return nullptr;
    }
    void DebugRequest(const char *, char *response, uint32_t size) override {
        if (size) {
            response[0] = 0;
        }
    }
    DriverPose_t *GetPose(DriverPose_t *result) override {
        *result = MakePose(g_driver->LatestHead());
        return result;
    }

    uint32_t Id() const { return id_; }

    static DriverPose_t MakePose(const Pose &head) {
        DriverPose_t pose = {};
        pose.qWorldFromDriverRotation.w = 1;
        pose.qDriverFromHeadRotation.w = 1;
        pose.qRotation = {head.q.w, head.q.x, head.q.y, head.q.z};
        pose.vecPosition[0] = head.p.x;
        pose.vecPosition[1] = head.p.y;
        pose.vecPosition[2] = head.p.z;
        // No velocity: SteamVR then draws with this very pose, which the host can name
        pose.result = TrackingResult_Running_OK;
        pose.poseIsValid = true;
        pose.deviceIsConnected = true;
        return pose;
    }

    void Update(const Pose &head) {
        if (id_ != k_unTrackedDeviceIndexInvalid) {
            DriverPose_t pose = MakePose(head);
            VRServerDriverHost()->TrackedDevicePoseUpdated(id_, pose, sizeof pose);
        }
    }

private:
    uint32_t id_ = k_unTrackedDeviceIndexInvalid;
    Display display_;
    VRInputComponentHandle_t proximity_ = k_ulInvalidInputComponentHandle;
};

// Reads "origin" and "rotate_xyz" of one component of a SteamVR render model's JSON: where that pose lies in the
// controller's raw pose
bool ReadModelComponent(const std::string &json, const char *component, Pose *out) {
    std::string key = std::string("\"") + component + "\"";
    size_t at = json.find(key);
    if (at == std::string::npos) {
        return false;
    }
    size_t end = json.find('}', at);
    auto numbers = [&](const char *field, float v[3]) {
        size_t f = json.find(field, at);
        if (f == std::string::npos || f > end) {
            return false;
        }
        size_t open = json.find('[', f);
        return open != std::string::npos && sscanf(json.c_str() + open, "[ %f , %f , %f", &v[0], &v[1], &v[2]) == 3;
    };
    float origin[3], rotate[3];
    if (!numbers("\"origin\"", origin) || !numbers("\"rotate_xyz\"", rotate)) {
        return false;
    }
    auto axis = [](float degrees, float x, float y, float z) {
        float half = degrees * 3.14159265f / 360.0f;
        float s = std::sin(half);
        return Quat{x * s, y * s, z * s, std::cos(half)};
    };
    out->q = Mul(Mul(axis(rotate[0], 1, 0, 0), axis(rotate[1], 0, 1, 0)), axis(rotate[2], 0, 0, 1));
    out->p = {origin[0], origin[1], origin[2]};
    return true;
}

std::string SteamVrRoot() {
    char path[MAX_PATH];
    DWORD n = GetModuleFileNameA(nullptr, path, MAX_PATH);
    std::string p(path, n);
    // ...\SteamVR\bin\win64\vrserver.exe
    for (int i = 0; i < 3; i++) {
        size_t slash = p.find_last_of("\\/");
        if (slash == std::string::npos) {
            return std::string();
        }
        p.resize(slash);
    }
    return p;
}

class Controller : public DeviceDriverAbi {
public:
    explicit Controller(int hand) : hand_(hand) {}

    EVRInitError Activate(uint32_t id) override {
        id_ = id;
        bool left = hand_ == 0;
        container_ = VRProperties()->TrackedDeviceToPropertyContainer(id);
        PropertyContainerHandle_t c = container_;
        char model[128] = {};
        VRSettings()->GetString("driver_cranvr", "controllerRenderModel", model, sizeof model);
        std::string renderModel = std::string(model[0] ? model : "oculus_quest_plus_controller") + (left ? "_left" : "_right");
        VRProperties()->SetStringProperty(c, Prop_ModelNumber_String, left ? "Oculus Quest2 (Left Controller)" : "Oculus Quest2 (Right Controller)");
        VRProperties()->SetStringProperty(c, Prop_ManufacturerName_String, "Oculus");
        VRProperties()->SetStringProperty(c, Prop_TrackingSystemName_String, "oculus");
        VRProperties()->SetStringProperty(c, Prop_RegisteredDeviceType_String, left ? "cranvr/cranvr_left_controller" : "cranvr/cranvr_right_controller");
        VRProperties()->SetStringProperty(c, Prop_RenderModelName_String, renderModel.c_str());
        VRProperties()->SetStringProperty(c, Prop_ControllerType_String, "oculus_touch");
        VRProperties()->SetStringProperty(c, Prop_InputProfilePath_String, "{oculus}/input/touch_profile.json");
        VRProperties()->SetInt32Property(c, Prop_ControllerRoleHint_Int32, left ? TrackedControllerRole_LeftHand : TrackedControllerRole_RightHand);
        VRProperties()->SetUint64Property(c, Prop_CurrentUniverseId_Uint64, kUniverse);
        VRProperties()->SetBoolProperty(c, Prop_DeviceIsWireless_Bool, true);
        VRProperties()->SetBoolProperty(c, Prop_DeviceProvidesBatteryStatus_Bool, false);
        VRProperties()->SetInt32Property(c, Prop_Axis0Type_Int32, k_eControllerAxis_Joystick);
        VRProperties()->SetInt32Property(c, Prop_Axis1Type_Int32, k_eControllerAxis_Trigger);
        VRProperties()->SetInt32Property(c, Prop_Axis2Type_Int32, k_eControllerAxis_Trigger);

        IVRDriverInput *in = VRDriverInput();
        auto boolean = [&](uint64_t id, const char *path) {
            VRInputComponentHandle_t h = k_ulInvalidInputComponentHandle;
            in->CreateBooleanComponent(c, path, &h);
            booleans_[id] = h;
        };
        auto scalar = [&](uint64_t id, const char *path, EVRScalarUnits units) {
            VRInputComponentHandle_t h = k_ulInvalidInputComponentHandle;
            in->CreateScalarComponent(c, path, &h, VRScalarType_Absolute, units);
            scalars_[id] = h;
        };
        if (left) {
            boolean(LEFT_X_CLICK, "/input/x/click");
            boolean(LEFT_X_TOUCH, "/input/x/touch");
            boolean(LEFT_Y_CLICK, "/input/y/click");
            boolean(LEFT_Y_TOUCH, "/input/y/touch");
            // The left menu button opens SteamVR's dashboard, as on a Rift: the right one belongs to the headset
            boolean(LEFT_MENU_CLICK, "/input/system/click");
            boolean(LEFT_THUMBSTICK_CLICK, "/input/joystick/click");
            boolean(LEFT_THUMBSTICK_TOUCH, "/input/joystick/touch");
            scalar(LEFT_THUMBSTICK_X, "/input/joystick/x", VRScalarUnits_NormalizedTwoSided);
            scalar(LEFT_THUMBSTICK_Y, "/input/joystick/y", VRScalarUnits_NormalizedTwoSided);
            scalar(LEFT_TRIGGER_VALUE, "/input/trigger/value", VRScalarUnits_NormalizedOneSided);
            boolean(LEFT_TRIGGER_TOUCH, "/input/trigger/touch");
            scalar(LEFT_SQUEEZE_VALUE, "/input/grip/value", VRScalarUnits_NormalizedOneSided);
            boolean(LEFT_THUMBREST_TOUCH, "/input/thumbrest/touch");
        } else {
            boolean(RIGHT_A_CLICK, "/input/a/click");
            boolean(RIGHT_A_TOUCH, "/input/a/touch");
            boolean(RIGHT_B_CLICK, "/input/b/click");
            boolean(RIGHT_B_TOUCH, "/input/b/touch");
            boolean(RIGHT_THUMBSTICK_CLICK, "/input/joystick/click");
            boolean(RIGHT_THUMBSTICK_TOUCH, "/input/joystick/touch");
            scalar(RIGHT_THUMBSTICK_X, "/input/joystick/x", VRScalarUnits_NormalizedTwoSided);
            scalar(RIGHT_THUMBSTICK_Y, "/input/joystick/y", VRScalarUnits_NormalizedTwoSided);
            scalar(RIGHT_TRIGGER_VALUE, "/input/trigger/value", VRScalarUnits_NormalizedOneSided);
            boolean(RIGHT_TRIGGER_TOUCH, "/input/trigger/touch");
            scalar(RIGHT_SQUEEZE_VALUE, "/input/grip/value", VRScalarUnits_NormalizedOneSided);
            boolean(RIGHT_THUMBREST_TOUCH, "/input/thumbrest/touch");
        }
        // The Touch controller's grip has no touch sensor: a grip that is held counts as touched
        in->CreateBooleanComponent(c, "/input/grip/touch", &gripTouch_);
        in->CreateHapticComponent(c, "/output/haptic", &haptic_);

        // Where OpenXR's grip pose lies in the controller's raw pose, from SteamVR's render model
        std::string path = SteamVrRoot() + "\\resources\\rendermodels\\" + renderModel + "\\" + renderModel + ".json";
        std::string json;
        if (FILE *file = fopen(path.c_str(), "rb")) {
            char buffer[4096];
            size_t n;
            while ((n = fread(buffer, 1, sizeof buffer, file)) > 0) {
                json.append(buffer, n);
            }
            fclose(file);
        }
        Pose grip, aim;
        if (ReadModelComponent(json, "openxr_grip", &grip) && ReadModelComponent(json, "openxr_aim", &aim)) {
            gripInRaw_ = grip;
            aimInRaw_ = aim;
            hasModel_ = true;
            Log("%s controller: device %u, render model %s, grip pose at (%.4f %.4f %.4f) of the raw pose", left ? "left" : "right",
                id, renderModel.c_str(), grip.p.x, grip.p.y, grip.p.z);
        } else {
            Log("%s controller: device %u, no openxr_grip in %s; the raw pose is the grip pose", left ? "left" : "right", id,
                path.c_str());
        }
        Publish();
        return VRInitError_None;
    }
    void Deactivate() override { id_ = k_unTrackedDeviceIndexInvalid; }
    void EnterStandby() override {}
    void *GetComponent(const char *) override { return nullptr; }
    void DebugRequest(const char *, char *response, uint32_t size) override {
        if (size) {
            response[0] = 0;
        }
    }
    DriverPose_t *GetPose(DriverPose_t *result) override {
        std::lock_guard<std::mutex> lock(lock_);
        *result = MakePose();
        return result;
    }

    PropertyContainerHandle_t Container() const { return container_; }

    void OnMotion(const ::Motion &grip, const ::Motion *aim) {
        {
            std::lock_guard<std::mutex> lock(lock_);
            grip_ = grip;
            seen_ = true;
            if (aim && hasModel_ && checks_ < 1) {
                // The host's aim pose against the aim pose of SteamVR's model: a wrong reading of the model shows here
                Pose predicted = Compose(Compose(grip.pose, Inverse(gripInRaw_)), aimInRaw_);
                checks_++;
                Log("%s controller: the model's aim pose is %.1f degrees and %.1f mm from the headset's", hand_ == 0 ? "left" : "right",
                    AngleBetween(predicted.q, aim->pose.q) * 57.29578f, Length(Sub(predicted.p, aim->pose.p)) * 1000.0f);
            }
        }
        Publish();
    }

    void SetTracked(bool tracked) {
        {
            std::lock_guard<std::mutex> lock(lock_);
            if (tracked_ == tracked) {
                return;
            }
            tracked_ = tracked;
        }
        Publish();
    }

    void Button(uint64_t path, float value) {
        auto b = booleans_.find(path);
        if (b != booleans_.end()) {
            VRDriverInput()->UpdateBooleanComponent(b->second, value > 0.5f, 0);
            return;
        }
        auto s = scalars_.find(path);
        if (s != scalars_.end()) {
            VRDriverInput()->UpdateScalarComponent(s->second, value, 0);
            if (path == LEFT_SQUEEZE_VALUE || path == RIGHT_SQUEEZE_VALUE) {
                VRDriverInput()->UpdateBooleanComponent(gripTouch_, value > 0.1f, 0);
            }
        }
    }

    void Reset() {
        for (auto &b : booleans_) {
            VRDriverInput()->UpdateBooleanComponent(b.second, false, 0);
        }
        for (auto &s : scalars_) {
            VRDriverInput()->UpdateScalarComponent(s.second, 0.0f, 0);
        }
        VRDriverInput()->UpdateBooleanComponent(gripTouch_, false, 0);
        SetTracked(false);
    }

private:
    DriverPose_t MakePose() {
        DriverPose_t pose = {};
        pose.qWorldFromDriverRotation.w = 1;
        pose.qDriverFromHeadRotation.w = 1;
        Pose raw = hasModel_ ? Compose(grip_.pose, Inverse(gripInRaw_)) : grip_.pose;
        pose.qRotation = {raw.q.w, raw.q.x, raw.q.y, raw.q.z};
        pose.vecPosition[0] = raw.p.x;
        pose.vecPosition[1] = raw.p.y;
        pose.vecPosition[2] = raw.p.z;
        // The raw pose's origin moves with the grip and turns around it
        Vec3 v = Add(grip_.linear, Cross(grip_.angular, Sub(raw.p, grip_.pose.p)));
        pose.vecVelocity[0] = v.x;
        pose.vecVelocity[1] = v.y;
        pose.vecVelocity[2] = v.z;
        pose.vecAngularVelocity[0] = grip_.angular.x;
        pose.vecAngularVelocity[1] = grip_.angular.y;
        pose.vecAngularVelocity[2] = grip_.angular.z;
        bool valid = seen_ && tracked_;
        pose.result = valid ? TrackingResult_Running_OK : TrackingResult_Running_OutOfRange;
        pose.poseIsValid = valid;
        pose.deviceIsConnected = true;
        return pose;
    }

    void Publish() {
        if (id_ == k_unTrackedDeviceIndexInvalid) {
            return;
        }
        DriverPose_t pose;
        {
            std::lock_guard<std::mutex> lock(lock_);
            pose = MakePose();
        }
        VRServerDriverHost()->TrackedDevicePoseUpdated(id_, pose, sizeof pose);
    }

    int hand_;
    uint32_t id_ = k_unTrackedDeviceIndexInvalid;
    PropertyContainerHandle_t container_ = k_ulInvalidPropertyContainer;
    std::map<uint64_t, VRInputComponentHandle_t> booleans_, scalars_;
    VRInputComponentHandle_t gripTouch_ = k_ulInvalidInputComponentHandle;
    VRInputComponentHandle_t haptic_ = k_ulInvalidInputComponentHandle;
    std::mutex lock_;
    ::Motion grip_ = {};
    bool seen_ = false, tracked_ = true, hasModel_ = false;
    Pose gripInRaw_, aimInRaw_;
    int checks_ = 0;
};

// ---- the driver ----

EVRInitError Driver::Init(IVRDriverContext *context) {
    VR_INIT_SERVER_DRIVER_CONTEXT(context);
    std::string control = ControlPath();
    if (!control.empty()) {
        // The log sits next to the control file, in cranvr's folder on the Mac
        size_t slash = control.find_last_of("\\/");
        std::string log = (slash == std::string::npos ? std::string() : control.substr(0, slash + 1)) + "steamvr-driver.log";
        g_logFile = fopen(log.c_str(), "w");
    }
    char gate[16] = {};
    GetEnvironmentVariableA("DXMT_BASALT_IOSURFACE", gate, sizeof gate);
    Log("driver 'cranvr' starts in process %lu: BASALT_CONTROL='%s', DXMT_BASALT_IOSURFACE='%s'", GetCurrentProcessId(),
        control.c_str(), gate);
    if (strcmp(gate, "1") != 0) {
        Log("DXMT_BASALT_IOSURFACE is not 1: DXMT makes no IOSurfaces, so the host gets no frames");
    }

    // Until the host says otherwise: a Quest 3's view (left eye; the right eye mirrors it)
    fov_[0] = {std::tan(-0.942478f), std::tan(0.698132f), std::tan(0.733038f), std::tan(-0.942478f)};
    fov_[1] = {std::tan(-0.698132f), std::tan(0.942478f), std::tan(0.733038f), std::tan(-0.942478f)};
    if (ReadControl(control_)) {
        fps_ = control_.fps;
    } else {
        control_.width = 1832;
        control_.height = 1920;
    }
    frames.Configure(control_.width, control_.height);
    frames.SetPeriod(1.0 / fps_);

    link.Start([this](uint16_t type, Reader &r) { OnMessage(type, r); }, [this] { OnConnect(); }, [this] { OnDisconnect(); });
    // The view, the size and the rate of the display cannot change once SteamVR has the headset: wait a little
    // for the host's view of the eyes
    for (int i = 0; i < 30 && !fovChanged_; i++) {
        Sleep(100);
    }
    fovChanged_ = false;
    {
        EyeFov fov[2] = {fov_[0], fov_[1]};
        frames.SetFov(fov);
    }

    hmd_ = new Hmd();
    controllers_[0] = new Controller(0);
    controllers_[1] = new Controller(1);
    VRServerDriverHost()->TrackedDeviceAdded("cranvr_headset", TrackedDeviceClass_HMD, reinterpret_cast<ITrackedDeviceServerDriver *>(hmd_));
    VRServerDriverHost()->TrackedDeviceAdded("cranvr_left_controller", TrackedDeviceClass_Controller,
                                             reinterpret_cast<ITrackedDeviceServerDriver *>(controllers_[0]));
    VRServerDriverHost()->TrackedDeviceAdded("cranvr_right_controller", TrackedDeviceClass_Controller,
                                             reinterpret_cast<ITrackedDeviceServerDriver *>(controllers_[1]));

    // SteamVR's vsync events pace its compositor
    std::thread([this] {
        auto next = std::chrono::steady_clock::now();
        while (g_driver) {
            next += std::chrono::duration_cast<std::chrono::steady_clock::duration>(std::chrono::duration<double>(frames.Period()));
            std::this_thread::sleep_until(next);
            auto now = std::chrono::steady_clock::now();
            if (now - next > std::chrono::milliseconds(50)) {
                next = now;
            }
            if (g_driver && hmd_->Id() != k_unTrackedDeviceIndexInvalid) {
                VRServerDriverHost()->VsyncEvent(0.0);
            }
        }
    }).detach();
    return VRInitError_None;
}

void Driver::Cleanup() {
    Log("driver stops");
    link.Stop();
    frames.Shutdown();
    VR_CLEANUP_SERVER_DRIVER_CONTEXT();
}

void Driver::OnConnect() { frames.HostConnected(); }

void Driver::OnDisconnect() {
    frames.HostDisconnected();
    for (auto *c : controllers_) {
        if (c) {
            c->Reset();
        }
    }
}

void Driver::OnMessage(uint16_t type, Reader &r) {
    switch (type) {
    case MSG_HEAD_POSE: {
        uint64_t timestamp = 0;
        Pose pose;
        if (!r.get(timestamp) || !r.pose(pose)) {
            return;
        }
        {
            std::lock_guard<std::mutex> lock(headLock_);
            heads_.push_back({timestamp, pose});
            while (heads_.size() > 256) {
                heads_.pop_front();
            }
        }
        if (hmd_) {
            hmd_->Update(pose);
        }
        break;
    }
    case MSG_CONTROLLER_MOTION:
    case MSG_CONTROLLER_MOTION_AIM: {
        uint64_t timestamp = 0;
        uint32_t hand = 0, reserved = 0;
        ::Motion grip, aim;
        if (!r.get(timestamp) || !r.get(hand) || !r.get(reserved) || !r.pose(grip.pose) || !r.vec(grip.linear) || !r.vec(grip.angular) || hand > 1) {
            return;
        }
        bool hasAim = type == MSG_CONTROLLER_MOTION_AIM && r.pose(aim.pose) && r.vec(aim.linear) && r.vec(aim.angular);
        if (controllers_[hand]) {
            controllers_[hand]->OnMotion(grip, hasAim ? &aim : nullptr);
        }
        break;
    }
    case MSG_CONTROLLER_MASK: {
        uint64_t timestamp = 0;
        uint32_t mask = 0;
        if (r.get(timestamp) && r.get(mask)) {
            for (int hand = 0; hand < 2; hand++) {
                if (controllers_[hand]) {
                    controllers_[hand]->SetTracked((mask >> hand) & 1);
                }
            }
        }
        break;
    }
    case MSG_BUTTON_STATE: {
        uint64_t path = 0;
        uint32_t scalar = 0;
        float value = 0;
        if (r.get(path) && r.get(scalar) && r.get(value)) {
            for (auto *c : controllers_) {
                if (c) {
                    c->Button(path, value);
                }
            }
        }
        break;
    }
    case MSG_INPUT_RESET:
        // The host sends it after a second without poses, as a keepalive too
        for (auto *c : controllers_) {
            if (c) {
                c->Reset();
            }
        }
        break;
    case MSG_VIEW_CONFIG: {
        float ipd = 0, angles[8];
        if (!r.get(ipd)) {
            return;
        }
        for (float &a : angles) {
            if (!r.get(a)) {
                return;
            }
        }
        EyeFov fov[2];
        for (int e = 0; e < 2; e++) {
            fov[e] = {std::tan(angles[e * 4]), std::tan(angles[e * 4 + 1]), std::tan(angles[e * 4 + 2]), std::tan(angles[e * 4 + 3])};
        }
        {
            std::lock_guard<std::mutex> lock(fovLock_);
            fov_[0] = fov[0];
            fov_[1] = fov[1];
        }
        ipd_ = ipd;
        frames.SetFov(fov);
        Log("view from the host: ipd %.1f mm, left eye %.1f %.1f %.1f %.1f, right eye %.1f %.1f %.1f %.1f degrees", ipd * 1000,
            angles[0] * 57.2958f, angles[1] * 57.2958f, angles[2] * 57.2958f, angles[3] * 57.2958f, angles[4] * 57.2958f,
            angles[5] * 57.2958f, angles[6] * 57.2958f, angles[7] * 57.2958f);
        if (hmd_ && hmd_->Id() != k_unTrackedDeviceIndexInvalid) {
            VRServerDriverHost()->SetDisplayProjectionRaw(hmd_->Id(), RawProjection(fov[0]), RawProjection(fov[1]));
            VRServerDriverHost()->SetDisplayEyeToHead(hmd_->Id(), Translation(-ipd / 2), Translation(ipd / 2));
            VRProperties()->SetFloatProperty(VRProperties()->TrackedDeviceToPropertyContainer(hmd_->Id()), Prop_UserIpdMeters_Float, ipd);
        }
        fovChanged_ = true;
        break;
    }
    case MSG_FRAME_CONSUMED: {
        uint64_t number = 0;
        if (r.get(number)) {
            frames.FrameConsumed(number);
        }
        break;
    }
    default:
        // Acks, profiles and the tracking of hands, body, face and eyes: nothing for SteamVR here
        break;
    }
}

uint64_t Driver::TimestampOf(const HmdMatrix34_t &m, float *errorMeters, float *errorRadians) {
    Vec3 p = {m.m[0][3], m.m[1][3], m.m[2][3]};
    Quat q = QuatFromMatrix(m);
    std::lock_guard<std::mutex> lock(headLock_);
    uint64_t best = heads_.empty() ? 0 : heads_.back().timestamp;
    float bestScore = 1e9f, bestMeters = 0, bestRadians = 0;
    for (auto it = heads_.rbegin(); it != heads_.rend(); ++it) {
        float meters = Length(Sub(it->pose.p, p));
        float radians = AngleBetween(it->pose.q, q);
        float score = meters + radians * 0.1f;
        if (score < bestScore) {
            bestScore = score;
            best = it->timestamp;
            bestMeters = meters;
            bestRadians = radians;
            if (score < 1e-5f) {
                break;
            }
        }
    }
    *errorMeters = bestMeters;
    *errorRadians = bestRadians;
    return best;
}

void Driver::SendHaptic(uint32_t hand, float seconds, float frequency, float amplitude) {
    // SteamVR asks for a single short pulse with a duration of zero. The OpenXR runtime gives its shortest pulse
    // 20 ms, so the driver does the same.
    uint64_t ns = static_cast<uint64_t>(std::max(0.0f, seconds) * 1e9);
    if (ns < 20000000) {
        ns = 20000000;
    }
    Writer w;
    w.put<uint32_t>(hand);
    w.put<uint64_t>(ns);
    w.put<float>(frequency);
    w.put<float>(amplitude);
    link.Send(MSG_HAPTIC, w.bytes);
}

// The Windows path of a process's program, as UTF-8
static std::string ProgramOf(HANDLE process) {
    wchar_t wide[1024];
    DWORD size = 1024;
    if (!QueryFullProcessImageNameW(process, 0, wide, &size)) {
        return std::string();
    }
    char path[4096];
    int n = WideCharToMultiByte(CP_UTF8, 0, wide, static_cast<int>(size), path, sizeof path, nullptr, nullptr);
    return n > 0 ? std::string(path, static_cast<size_t>(n)) : std::string();
}

void Driver::NoteLayerProcess(uint32_t pid) {
    std::lock_guard<std::mutex> lock(gameLock_);
    if (pid == 0 || pid == gamePid_) {
        return;
    }
    auto known = compositorProcess_.find(pid);
    if (known != compositorProcess_.end() && known->second) {
        return;
    }
    HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, FALSE, pid);
    if (!process) {
        return;
    }
    std::string program = ProgramOf(process);
    std::string lower = program;
    std::transform(lower.begin(), lower.end(), lower.begin(), [](unsigned char c) { return static_cast<char>(tolower(c)); });
    // SteamVR's compositor draws its own layers: its room, its panels and the fade of a game that stalls
    bool compositor = lower.size() >= 16 && lower.compare(lower.size() - 16, 16, "vrcompositor.exe") == 0;
    compositorProcess_[pid] = compositor;
    if (compositor) {
        CloseHandle(process);
        return;
    }
    if (game_) {
        CloseHandle(game_);
    }
    game_ = process;
    gamePid_ = pid;
    quitAsked_ = false;
    Log("the game is process %u: %s", pid, program.c_str());
    // The host learns a game's program from game-program.txt beside the control file, read at the next HELLO
    std::string control = ControlPath();
    size_t slash = control.find_last_of("\\/");
    if (slash != std::string::npos && !program.empty()) {
        std::string file = control.substr(0, slash + 1) + "game-program.txt";
        if (FILE *out = fopen(file.c_str(), "wb")) {
            fwrite(program.data(), 1, program.size(), out);
            fclose(out);
            link.Reconnect();
        }
    }
}

// cranvr starts SteamVR for one game. When that game ends, SteamVR ends too, so the host sees no game and the
// next game starts with a SteamVR of its own.
void Driver::WatchGame() {
    std::lock_guard<std::mutex> lock(gameLock_);
    if (!game_ || quitAsked_ || WaitForSingleObject(game_, 0) != WAIT_OBJECT_0) {
        return;
    }
    quitAsked_ = true;
    Log("the game (process %u) has ended: SteamVR quits", gamePid_);
    VREvent_Data_t data = {};
    VRServerDriverHost()->VendorSpecificEvent(k_unTrackedDeviceIndex_Hmd, VREvent_DriverRequestedQuit, data, 0.0);
}

void Driver::RunFrame() {
    WatchGame();
    VREvent_t event;
    while (VRServerDriverHost()->PollNextEvent(&event, sizeof event)) {
        if (event.eventType == VREvent_Input_HapticVibration) {
            const auto &h = event.data.hapticVibration;
            for (uint32_t hand = 0; hand < 2; hand++) {
                if (controllers_[hand] && controllers_[hand]->Container() == h.containerHandle) {
                    SendHaptic(hand, h.fDurationSeconds, h.fFrequency, h.fAmplitude);
                }
            }
        }
    }
    // The host writes a new rate into the control file when the headset changes it
    auto now = std::chrono::steady_clock::now();
    if (now - lastControlCheck_ > std::chrono::seconds(1)) {
        lastControlCheck_ = now;
        Control control;
        if (ReadControl(control) && control.fps != fps_) {
            Log("the host's refresh rate is now %u Hz", control.fps);
            fps_ = control.fps;
            frames.SetPeriod(1.0 / control.fps);
            if (hmd_ && hmd_->Id() != k_unTrackedDeviceIndexInvalid) {
                VRProperties()->SetFloatProperty(VRProperties()->TrackedDeviceToPropertyContainer(hmd_->Id()), Prop_DisplayFrequency_Float,
                                                 static_cast<float>(control.fps));
            }
        }
    }
}

// ---- frames ----

bool Frames::Device() {
    if (device_) {
        return true;
    }
    if (deviceFailed_) {
        return false;
    }
    deviceFailed_ = true;
    D3D_FEATURE_LEVEL level = D3D_FEATURE_LEVEL_11_0;
    HRESULT hr = D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT, &level, 1,
                                   D3D11_SDK_VERSION, &device_, nullptr, &context_);
    if (FAILED(hr)) {
        Log("no Direct3D 11 device (0x%08lx)", static_cast<unsigned long>(hr));
        return false;
    }
    HMODULE compiler = LoadLibraryA("d3dcompiler_47.dll");
    auto compile = compiler ? reinterpret_cast<pD3DCompile>(reinterpret_cast<void *>(GetProcAddress(compiler, "D3DCompile"))) : nullptr;
    if (!compile) {
        Log("no D3DCompile in d3dcompiler_47.dll");
        return false;
    }
    ID3DBlob *vs = nullptr, *ps = nullptr, *errors = nullptr;
    hr = compile(kShader, sizeof kShader - 1, "cranvr_layers", nullptr, nullptr, "vs_main", "vs_5_0", 0, 0, &vs, &errors);
    if (SUCCEEDED(hr)) {
        hr = compile(kShader, sizeof kShader - 1, "cranvr_layers", nullptr, nullptr, "ps_main", "ps_5_0", 0, 0, &ps, &errors);
    }
    if (FAILED(hr)) {
        Log("the layer shader does not compile: %s", errors ? static_cast<const char *>(errors->GetBufferPointer()) : "?");
        return false;
    }
    device_->CreateVertexShader(vs->GetBufferPointer(), vs->GetBufferSize(), nullptr, &vs_);
    device_->CreatePixelShader(ps->GetBufferPointer(), ps->GetBufferSize(), nullptr, &ps_);
    vs->Release();
    ps->Release();
    D3D11_BUFFER_DESC bd = {sizeof(LayerConstants), D3D11_USAGE_DEFAULT, D3D11_BIND_CONSTANT_BUFFER, 0, 0, 0};
    device_->CreateBuffer(&bd, nullptr, &constants_);
    D3D11_SAMPLER_DESC sd = {};
    sd.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR;
    sd.AddressU = sd.AddressV = sd.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
    sd.MaxLOD = D3D11_FLOAT32_MAX;
    device_->CreateSamplerState(&sd, &sampler_);
    D3D11_BLEND_DESC blend = {};
    blend.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL;
    device_->CreateBlendState(&blend, &opaque_);
    auto &rt = blend.RenderTarget[0];
    rt.BlendEnable = TRUE;
    rt.SrcBlend = D3D11_BLEND_SRC_ALPHA;
    rt.DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
    rt.BlendOp = D3D11_BLEND_OP_ADD;
    rt.SrcBlendAlpha = D3D11_BLEND_ONE;
    rt.DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA;
    rt.BlendOpAlpha = D3D11_BLEND_OP_ADD;
    device_->CreateBlendState(&blend, &blend_);
    D3D11_RASTERIZER_DESC rd = {};
    rd.FillMode = D3D11_FILL_SOLID;
    rd.CullMode = D3D11_CULL_NONE;
    rd.DepthClipEnable = TRUE;
    device_->CreateRasterizerState(&rd, &raster_);
    D3D11_QUERY_DESC qd = {D3D11_QUERY_EVENT, 0};
    device_->CreateQuery(&qd, &query_);
    deviceFailed_ = false;
    Log("Direct3D 11 device ready, the layer shader compiled");
    return true;
}

void Frames::ReleaseOutputs() {
    for (auto &slot : slots_) {
        for (int e = 0; e < 2; e++) {
            if (slot.target[e]) {
                slot.target[e]->Release();
                slot.target[e] = nullptr;
            }
            if (slot.eye[e]) {
                slot.eye[e]->Release();
                slot.eye[e] = nullptr;
            }
        }
        slot.pending = 0;
    }
    outWidth_ = outHeight_ = 0;
}

// The eye images that the host reads: BGRA with sRGB, which DXMT backs with IOSurfaces
bool Frames::MakeOutputs() {
    ReleaseOutputs();
    D3D11_TEXTURE2D_DESC td = {};
    td.Width = width_;
    td.Height = height_;
    td.MipLevels = 1;
    td.ArraySize = 1;
    td.Format = DXGI_FORMAT_B8G8R8A8_UNORM_SRGB;
    td.SampleDesc.Count = 1;
    td.Usage = D3D11_USAGE_DEFAULT;
    td.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
    td.MiscFlags = D3D11_RESOURCE_MISC_SHARED_NTHANDLE;
    for (auto &slot : slots_) {
        for (int e = 0; e < 2; e++) {
            if (FAILED(device_->CreateTexture2D(&td, nullptr, &slot.eye[e])) ||
                FAILED(device_->CreateRenderTargetView(slot.eye[e], nullptr, &slot.target[e]))) {
                Log("an eye image of %ux%u was not made", width_, height_);
                ReleaseOutputs();
                return false;
            }
            UINT size = sizeof slot.surface[e];
            if (FAILED(slot.eye[e]->GetPrivateData(kIOSurfaceIdGuid, &size, &slot.surface[e])) || !slot.surface[e]) {
                Log("DXMT gave the eye image no IOSurface: is DXMT_BASALT_IOSURFACE=1 in vrserver's environment?");
                ReleaseOutputs();
                return false;
            }
        }
    }
    outWidth_ = width_;
    outHeight_ = height_;
    Log("eye images ready: %d frames of %ux%u", kSlots, width_, height_);
    return true;
}

void Frames::Draw(const Layer &layer, int eye, bool base, const EyeFov &fov) {
    const SubmitLayerPerEye &e = layer.eye[eye];
    int index = 0;
    SwapSet *set = Find(e.hTexture, &index);
    if (!set) {
        unknown_++;
        return;
    }
    ID3D11ShaderResourceView *view = set->views[index];
    if (set->samples > 1) {
        if (!set->resolved) {
            return;
        }
        bool decode;
        context_->ResolveSubresource(set->resolved, 0, set->textures[index], 0, ViewFormat(set->format, &decode));
        view = set->resolvedView;
    }
    if (!view) {
        return;
    }
    LayerConstants k = {};
    k.outTan[0] = fov.left;
    k.outTan[1] = fov.right;
    k.outTan[2] = fov.up;
    k.outTan[3] = fov.down;
    const HmdMatrix44_t &m = e.mProjection;
    bool projected = std::fabs(m.m[0][0]) > 1e-6f && std::fabs(m.m[1][1]) > 1e-6f;
    for (int c = 0; c < 4; c++) {
        k.row0[c] = m.m[0][c];
        k.row1[c] = m.m[1][c];
        k.row3[c] = m.m[3][c];
    }
    k.bounds[0] = e.bounds.uMin;
    k.bounds[1] = e.bounds.vMin;
    k.bounds[2] = e.bounds.uMax;
    k.bounds[3] = e.bounds.vMax;
    k.mode[0] = set->decode ? 1.0f : 0.0f;
    k.mode[1] = base ? 1.0f : 0.0f;
    k.mode[2] = projected ? 0.0f : 1.0f;
    context_->UpdateSubresource(constants_, 0, nullptr, &k, 0, 0);
    float factor[4] = {};
    context_->OMSetBlendState(base ? opaque_ : blend_, factor, 0xffffffff);
    context_->PSSetShaderResources(0, 1, &view);
    context_->Draw(3, 0);
    ID3D11ShaderResourceView *none = nullptr;
    context_->PSSetShaderResources(0, 1, &none);
}

void Frames::Present(SharedTextureHandle_t) {
    auto started = std::chrono::steady_clock::now();
    std::vector<Layer> layers;
    {
        std::lock_guard<std::mutex> lock(lock_);
        layers.swap(layers_);
    }
    presents_++;
    layerCount_ += static_cast<uint32_t>(layers.size());
    {
        std::lock_guard<std::mutex> lock(lock_);
        std::string sources;
        for (const Layer &layer : layers) {
            SwapSet *set = Find(layer.eye[0].hTexture, nullptr);
            if (set) {
                g_driver->NoteLayerProcess(set->pid);
            }
            // The vertical tangents of the left eye's projection: the part of the view that the layer covers
            const HmdMatrix44_t &m = layer.eye[0].mProjection;
            bool projected = std::fabs(m.m[1][1]) > 1e-6f;
            char part[160];
            snprintf(part, sizeof part, "%s[process %u, %ux%u, bounds %.2f %.2f %.2f %.2f, view up %.3f down %.3f]", sources.empty() ? "" : " ",
                     set ? set->pid : 0, set ? set->width : 0, set ? set->height : 0, layer.eye[0].bounds.uMin, layer.eye[0].bounds.vMin,
                     layer.eye[0].bounds.uMax, layer.eye[0].bounds.vMax, projected ? (m.m[1][2] + 1) / m.m[1][1] : 0.0f,
                     projected ? (m.m[1][2] - 1) / m.m[1][1] : 0.0f);
            sources += part;
        }
        if (sources != sources_) {
            sources_ = sources;
            Log("the frame's layers now come from: %s", sources.empty() ? "nothing" : sources.c_str());
        }
    }
    bool sent = false;
    if (g_driver->link.Connected() && !layers.empty()) {
        std::lock_guard<std::mutex> lock(lock_);
        if (Device() && (outWidth_ == width_ && outHeight_ == height_ ? true : MakeOutputs())) {
            if (register_.exchange(false) || swapchainId_ == 0) {
                if (swapchainId_) {
                    Writer d;
                    d.put<uint64_t>(swapchainId_);
                    g_driver->link.Send(MSG_DESTROY_SWAPCHAIN, d.bytes);
                }
                swapchainId_++;
                Writer w;
                w.put<uint64_t>(swapchainId_);
                w.put<uint32_t>(outWidth_);
                w.put<uint32_t>(outHeight_);
                w.put<uint32_t>(DXGI_FORMAT_B8G8R8A8_UNORM_SRGB);
                w.put<uint32_t>(kSlots * 2);
                for (auto &slot : slots_) {
                    w.put<uint32_t>(slot.surface[0]);
                    w.put<uint32_t>(slot.surface[1]);
                }
                g_driver->link.Send(MSG_CREATE_SWAPCHAIN, w.bytes);
                std::lock_guard<std::mutex> slotLock(slotLock_);
                for (auto &slot : slots_) {
                    slot.pending = 0;
                }
            }
            // A slot that the host still reads waits; the host gives each frame back after its encoder read it
            int free = -1;
            {
                std::unique_lock<std::mutex> slotLock(slotLock_);
                auto pick = [&] {
                    for (int i = 0; i < kSlots; i++) {
                        if (slots_[i].pending == 0) {
                            free = i;
                            return true;
                        }
                    }
                    return false;
                };
                slotFree_.wait_for(slotLock, std::chrono::milliseconds(50), pick);
            }
            auto composeStart = std::chrono::steady_clock::now();
            waitMs_ += std::chrono::duration<double, std::milli>(composeStart - started).count();
            if (free >= 0) {
                Slot &slot = slots_[free];
                EyeFov fov[2];
                {
                    std::lock_guard<std::mutex> fovLock(fovLock_);
                    fov[0] = fov_[0];
                    fov[1] = fov_[1];
                }
                context_->VSSetShader(vs_, nullptr, 0);
                context_->PSSetShader(ps_, nullptr, 0);
                context_->IASetInputLayout(nullptr);
                context_->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                context_->RSSetState(raster_);
                context_->PSSetConstantBuffers(0, 1, &constants_);
                context_->PSSetSamplers(0, 1, &sampler_);
                D3D11_VIEWPORT viewport = {0, 0, static_cast<float>(outWidth_), static_cast<float>(outHeight_), 0, 1};
                context_->RSSetViewports(1, &viewport);
                for (int eye = 0; eye < 2; eye++) {
                    context_->OMSetRenderTargets(1, &slot.target[eye], nullptr);
                    for (size_t i = 0; i < layers.size(); i++) {
                        Draw(layers[i], eye, i == 0, fov[eye]);
                    }
                }
                ID3D11RenderTargetView *none = nullptr;
                context_->OMSetRenderTargets(1, &none, nullptr);
                context_->End(query_);
                context_->Flush();
                // The host reads the images as soon as it hears of them: wait for this GPU work first
                auto until = std::chrono::steady_clock::now() + std::chrono::milliseconds(100);
                while (context_->GetData(query_, nullptr, 0, 0) == S_FALSE && std::chrono::steady_clock::now() < until) {
                    SwitchToThread();
                }
                composeMs_ += std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - composeStart).count();

                float meters = 0, radians = 0;
                uint64_t timestamp = g_driver->TimestampOf(layers[0].eye[0].mHmdPose, &meters, &radians);
                Pose head;
                {
                    HmdMatrix34_t hp = layers[0].eye[0].mHmdPose;
                    head.q = QuatFromMatrix(hp);
                    head.p = {hp.m[0][3], hp.m[1][3], hp.m[2][3]};
                }
                if (sent_ < 3 || meters > 0.01f || radians > 0.02f) {
                    static int told = 0;
                    if (told++ < 10) {
                        Log("frame %llu: %zu layers, drawn with the head pose of %llu (off by %.1f mm and %.2f degrees)",
                            static_cast<unsigned long long>(frameNumber_ + 1), layers.size(), static_cast<unsigned long long>(timestamp),
                            meters * 1000, radians * 57.2958f);
                        const HmdMatrix44_t &m = layers[0].eye[0].mProjection;
                        if (std::fabs(m.m[0][0]) > 1e-6f && std::fabs(m.m[1][1]) > 1e-6f) {
                            // What the layer's projection covers, for a check against the host's view
                            Log("  layer 0, left eye: tangents left %.3f right %.3f up %.3f down %.3f (the host's: %.3f %.3f %.3f %.3f)",
                                (m.m[0][2] - 1) / m.m[0][0], (m.m[0][2] + 1) / m.m[0][0], (m.m[1][2] + 1) / m.m[1][1],
                                (m.m[1][2] - 1) / m.m[1][1], fov[0].left, fov[0].right, fov[0].up, fov[0].down);
                        }
                    }
                }
                Writer w;
                uint64_t number = ++frameNumber_;
                w.put<uint64_t>(number);
                w.put<int64_t>(0);
                w.put<uint64_t>(timestamp);
                w.put<uint32_t>(2);
                float ipd = g_driver->Ipd();
                for (int eye = 0; eye < 2; eye++) {
                    w.put<uint64_t>(swapchainId_);
                    w.put<uint32_t>(static_cast<uint32_t>(free * 2 + eye));
                    w.put<uint32_t>(0);
                    w.put<int32_t>(0);
                    w.put<int32_t>(0);
                    w.put<int32_t>(static_cast<int32_t>(outWidth_));
                    w.put<int32_t>(static_cast<int32_t>(outHeight_));
                    Vec3 offset = Rotate(head.q, {eye == 0 ? -ipd / 2 : ipd / 2, 0, 0});
                    w.put<float>(head.q.x);
                    w.put<float>(head.q.y);
                    w.put<float>(head.q.z);
                    w.put<float>(head.q.w);
                    w.put<float>(head.p.x + offset.x);
                    w.put<float>(head.p.y + offset.y);
                    w.put<float>(head.p.z + offset.z);
                    w.put<float>(std::atan(fov[eye].left));
                    w.put<float>(std::atan(fov[eye].right));
                    w.put<float>(std::atan(fov[eye].up));
                    w.put<float>(std::atan(fov[eye].down));
                }
                {
                    std::lock_guard<std::mutex> slotLock(slotLock_);
                    slot.pending = number;
                }
                if (g_driver->link.Send(MSG_FRAME_READY, w.bytes)) {
                    sent = true;
                    sent_++;
                } else {
                    std::lock_guard<std::mutex> slotLock(slotLock_);
                    slot.pending = 0;
                }
            }
        }
    }
    if (!sent) {
        dropped_++;
    }
    auto now = std::chrono::steady_clock::now();
    double seconds = std::chrono::duration<double>(now - statsSince_).count();
    if (seconds >= 5.0) {
        Log("in %.1f s: %u presents, %u frames sent, %u not sent, %.1f layers a frame, %u unknown textures, wait for a free image %.2f ms, "
            "draw and GPU %.2f ms a frame",
            seconds, presents_, sent_, dropped_, presents_ ? static_cast<double>(layerCount_) / presents_ : 0.0, unknown_,
            presents_ ? waitMs_ / presents_ : 0.0, sent_ ? composeMs_ / sent_ : 0.0);
        presents_ = sent_ = dropped_ = layerCount_ = unknown_ = 0;
        waitMs_ = composeMs_ = 0;
        statsSince_ = now;
    }
}

void Frames::PostPresent(const Throttling_t *) {
    // The compositor's next frame starts at the next vsync of the headset's rate
    auto period = std::chrono::duration_cast<std::chrono::steady_clock::duration>(std::chrono::duration<double>(period_.load()));
    auto next = lastPresent_ + period;
    auto now = std::chrono::steady_clock::now();
    if (next < now || next > now + period) {
        next = now;
    }
    std::this_thread::sleep_until(next);
    lastPresent_ = next;
}

Driver g_instance;

} // namespace

extern "C" __declspec(dllexport) void *HmdDriverFactory(const char *interfaceName, int *returnCode) {
    if (!strcmp(interfaceName, IServerTrackedDeviceProvider_Version)) {
        g_driver = &g_instance;
        return static_cast<IServerTrackedDeviceProvider *>(&g_instance);
    }
    if (returnCode) {
        *returnCode = VRInitError_Init_InterfaceNotFound;
    }
    return nullptr;
}
