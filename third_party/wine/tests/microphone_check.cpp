// microphone_check.exe: records from the default capture device as a game does, through WASAPI in
// shared mode with an event, and says when it hears a sound.
//
// Usage: microphone_check.exe [--list] [--save PREFIX] [SECONDS]
//   --list    prints the name of the default capture device and ends.
//   --save PREFIX  also writes what it heard: PREFIX.f32 holds the first channel as 32-bit floats,
//             PREFIX.blocks a line "FRAME COUNT TIME" for each block, with the time at which the
//             program got it. A test finds a quiet sound in them that the onset level misses. The
//             files hold the sound of the room: delete them after the test.
//   SECONDS   how long to record (default 3).
//
// Each line starts with "microphone_check:". A sound that starts after 200 ms of quiet prints an
// "onset" line with the time at which the program got the block that holds it: the performance
// counter in units of 100 ns. Under Wine on macOS the counter is mach_continuous_time, so a
// program on the Mac can compare the time with its own clock. Every second prints the peak level
// and the blocks of that second. The end prints the frames a second and the blocks that came
// with a discontinuity.

#include <initguid.h>
#include <windows.h>
#include <mmdeviceapi.h>
#include <audioclient.h>
#include <functiondiscoverykeys_devpkey.h>

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

namespace {

// A level above this after this many frames below it is a sound that starts
constexpr float ONSET_LEVEL = 0.05f;
constexpr double QUIET_SECONDS = 0.2;
// KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, which MinGW's headers declare without defining it
constexpr GUID SUBTYPE_FLOAT = {0x00000003, 0x0000, 0x0010, {0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71}};

bool check(HRESULT hr, const char *what) {
    if (FAILED(hr)) {
        std::printf("microphone_check: FAIL %s: 0x%08lx\n", what, static_cast<unsigned long>(hr));
        return false;
    }
    return true;
}

long long now_100ns() {
    LARGE_INTEGER counter, frequency;
    QueryPerformanceCounter(&counter);
    QueryPerformanceFrequency(&frequency);
    // The counter runs for days: a plain product would overflow
    return counter.QuadPart / frequency.QuadPart * 10000000LL + counter.QuadPart % frequency.QuadPart * 10000000LL / frequency.QuadPart;
}

// One sample of a frame in the device's format, as a float from -1 to 1
float sample(const BYTE *frame, const WAVEFORMATEX *format) {
    auto extensible = reinterpret_cast<const WAVEFORMATEXTENSIBLE *>(format);
    bool is_float = format->wFormatTag == WAVE_FORMAT_IEEE_FLOAT ||
                    (format->wFormatTag == WAVE_FORMAT_EXTENSIBLE && IsEqualGUID(extensible->SubFormat, SUBTYPE_FLOAT));
    if (is_float && format->wBitsPerSample == 32) {
        float value;
        std::memcpy(&value, frame, 4);
        return value;
    }
    if (format->wBitsPerSample == 16) {
        short value;
        std::memcpy(&value, frame, 2);
        return value / 32768.0f;
    }
    if (format->wBitsPerSample == 32) {
        int value;
        std::memcpy(&value, frame, 4);
        return value / 2147483648.0f;
    }
    return 0.0f;
}

}  // namespace

int main(int argc, char **argv) {
    bool list = false;
    double seconds = 3.0;
    const char *save = nullptr;
    for (int i = 1; i < argc; i++) {
        if (!std::strcmp(argv[i], "--list")) list = true;
        else if (!std::strcmp(argv[i], "--save") && i + 1 < argc) save = argv[++i];
        else seconds = std::atof(argv[i]);
    }
    FILE *samples_file = nullptr, *blocks_file = nullptr;
    if (save) {
        std::string prefix(save);
        samples_file = std::fopen((prefix + ".f32").c_str(), "wb");
        blocks_file = std::fopen((prefix + ".blocks").c_str(), "w");
        if (!samples_file || !blocks_file) {
            std::printf("microphone_check: FAIL cannot write %s.f32 and %s.blocks\n", save, save);
            return 1;
        }
    }
    if (!check(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "CoInitializeEx")) return 1;
    IMMDeviceEnumerator *enumerator = nullptr;
    if (!check(CoCreateInstance(CLSID_MMDeviceEnumerator, nullptr, CLSCTX_ALL, IID_IMMDeviceEnumerator, reinterpret_cast<void **>(&enumerator)),
               "the device enumerator"))
        return 1;
    IMMDevice *device = nullptr;
    HRESULT found = enumerator->GetDefaultAudioEndpoint(eCapture, eConsole, &device);
    if (found == E_NOTFOUND) {
        std::printf("microphone_check: no capture device\n");
        return list ? 0 : 1;
    }
    if (!check(found, "the default capture device")) return 1;
    IPropertyStore *properties = nullptr;
    PROPVARIANT name;
    PropVariantInit(&name);
    if (SUCCEEDED(device->OpenPropertyStore(STGM_READ, &properties)) && SUCCEEDED(properties->GetValue(PKEY_Device_FriendlyName, &name)) &&
        name.vt == VT_LPWSTR)
        std::printf("microphone_check: default capture device: %ls\n", name.pwszVal);
    else
        std::printf("microphone_check: default capture device has no name\n");
    if (list) return 0;

    IAudioClient *client = nullptr;
    if (!check(device->Activate(IID_IAudioClient, CLSCTX_ALL, nullptr, reinterpret_cast<void **>(&client)), "IAudioClient")) return 1;
    WAVEFORMATEX *format = nullptr;
    if (!check(client->GetMixFormat(&format), "GetMixFormat")) return 1;
    std::printf("microphone_check: format %lu Hz, %u channels, %u bits\n", static_cast<unsigned long>(format->nSamplesPerSec),
                format->nChannels, format->wBitsPerSample);
    // A buffer of 20 ms, as voice chat asks for
    if (!check(client->Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 200000, 0, format, nullptr), "Initialize"))
        return 1;
    REFERENCE_TIME latency = 0;
    client->GetStreamLatency(&latency);
    std::printf("microphone_check: stream latency %.1f ms\n", latency / 10000.0);
    HANDLE event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    if (!check(client->SetEventHandle(event), "SetEventHandle")) return 1;
    IAudioCaptureClient *capture = nullptr;
    if (!check(client->GetService(IID_IAudioCaptureClient, reinterpret_cast<void **>(&capture)), "IAudioCaptureClient")) return 1;
    if (!check(client->Start(), "Start")) return 1;

    const double rate = format->nSamplesPerSec;
    const long long start = now_100ns();
    long long frames = 0, quiet = 0, blocks = 0, second_blocks = 0, discontinuities = 0, onsets = 0;
    float peak = 0.0f;
    long long next_second = start + 10000000LL;
    while (now_100ns() - start < static_cast<long long>(seconds * 1e7)) {
        WaitForSingleObject(event, 100);
        UINT32 packet = 0;
        while (SUCCEEDED(capture->GetNextPacketSize(&packet)) && packet) {
            BYTE *data = nullptr;
            UINT32 count = 0;
            DWORD flags = 0;
            UINT64 position = 0, qpc = 0;
            if (FAILED(capture->GetBuffer(&data, &count, &flags, &position, &qpc))) break;
            const long long got = now_100ns();
            blocks++;
            second_blocks++;
            if (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY) discontinuities++;
            if (blocks_file) std::fprintf(blocks_file, "%lld %u %lld\n", frames, count, got);
            for (UINT32 i = 0; i < count; i++) {
                float value = (flags & AUDCLNT_BUFFERFLAGS_SILENT) ? 0.0f : sample(data + i * format->nBlockAlign, format);
                if (samples_file) std::fwrite(&value, sizeof(value), 1, samples_file);
                float level = std::fabs(value);
                if (level > peak) peak = level;
                if (level >= ONSET_LEVEL) {
                    if (quiet >= QUIET_SECONDS * rate) {
                        onsets++;
                        std::printf("microphone_check: onset at %lld (frame %lld, %u frames before the end of its block)\n", got, frames + i,
                                    count - i);
                    }
                    quiet = 0;
                } else {
                    quiet++;
                }
            }
            frames += count;
            capture->ReleaseBuffer(count);
        }
        if (now_100ns() >= next_second) {
            std::printf("microphone_check: second: peak %.3f, %lld blocks\n", peak, second_blocks);
            std::fflush(stdout);
            peak = 0.0f;
            second_blocks = 0;
            next_second += 10000000LL;
        }
    }
    client->Stop();
    if (samples_file) std::fclose(samples_file);
    if (blocks_file) std::fclose(blocks_file);
    const double elapsed = (now_100ns() - start) / 1e7;
    std::printf("microphone_check: %lld frames in %.2f s, %.1f frames a second, %lld blocks, %lld with a discontinuity, %lld onsets\n", frames,
                elapsed, frames / elapsed, blocks, discontinuities, onsets);
    std::printf("microphone_check: PASS\n");
    return 0;
}
