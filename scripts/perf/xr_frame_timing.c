// An OpenXR API layer that measures a game's frame loop under Wine, for tests (issue #155). It
// sits between the game, or OpenComposite, and cranvr's runtime and times the three frame calls.
// Every 600 frames it writes one line to the game's stderr: the frame rate, the time that
// xrWaitFrame waited (OpenComposite's WaitGetPoses is xrWaitFrame and xrBeginFrame), the time from
// xrWaitFrame's return to the call of xrEndFrame (the game's work on the frame), the time of
// xrEndFrame, and the time from xrEndFrame's return to the next xrWaitFrame (the game's time
// between two frames, such as a frame cap of its own).
//
// Build and use: scripts/perf/xr-frame-timing.sh prints the two variables for a run.
//
// The file declares the few OpenXR types that it needs, so it builds without the OpenXR SDK. The
// layer passes every structure through as it is.

#include <windows.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int32_t XrResult;
typedef uint64_t XrVersion;
typedef void* XrInstance;
typedef void* XrSession;
typedef void (*PFN_xrVoidFunction)(void);
typedef XrResult (*PFN_xrGetInstanceProcAddr)(XrInstance, const char*, PFN_xrVoidFunction*);
typedef XrResult (*PFN_xrWaitFrame)(XrSession, const void*, void*);
typedef XrResult (*PFN_xrBeginFrame)(XrSession, const void*);
typedef XrResult (*PFN_xrEndFrame)(XrSession, const void*);

// The loader's interface for API layers (loader_interfaces.h of the OpenXR SDK, version 1)
typedef struct ApiLayerCreateInfo ApiLayerCreateInfo;
typedef XrResult (*PFN_xrCreateApiLayerInstance)(const void* info, const ApiLayerCreateInfo* layerInfo, XrInstance* instance);
typedef struct NegotiateLoaderInfo {
    int structType;
    uint32_t structVersion;
    size_t structSize;
    uint32_t minInterfaceVersion;
    uint32_t maxInterfaceVersion;
    XrVersion minApiVersion;
    XrVersion maxApiVersion;
} NegotiateLoaderInfo;
typedef struct NegotiateApiLayerRequest {
    int structType;
    uint32_t structVersion;
    size_t structSize;
    uint32_t layerInterfaceVersion;
    XrVersion layerApiVersion;
    PFN_xrGetInstanceProcAddr getInstanceProcAddr;
    PFN_xrCreateApiLayerInstance createApiLayerInstance;
} NegotiateApiLayerRequest;
typedef struct ApiLayerNextInfo ApiLayerNextInfo;
struct ApiLayerNextInfo {
    int structType;
    uint32_t structVersion;
    size_t structSize;
    char layerName[256];
    PFN_xrGetInstanceProcAddr nextGetInstanceProcAddr;
    PFN_xrCreateApiLayerInstance nextCreateApiLayerInstance;
    ApiLayerNextInfo* next;
};
struct ApiLayerCreateInfo {
    int structType;
    uint32_t structVersion;
    size_t structSize;
    void* loaderInstance;
    char settingsFileLocation[512];
    ApiLayerNextInfo* nextInfo;
};

#define WINDOW 600

static PFN_xrGetInstanceProcAddr nextGetInstanceProcAddr;
static PFN_xrWaitFrame nextWaitFrame;
static PFN_xrBeginFrame nextBeginFrame;
static PFN_xrEndFrame nextEndFrame;

static double ticksPerMs;
// The times of the frame in progress, in milliseconds
static double waitCalled, waitReturned, endReturned, windowStart;
static int count;
static double waits[WINDOW], works[WINDOW], ends[WINDOW], outsides[WINDOW], begins[WINDOW];
static unsigned long long frames;

static double now(void) {
    LARGE_INTEGER counter;
    QueryPerformanceCounter(&counter);
    return (double)counter.QuadPart / ticksPerMs;
}

static int compare(const void* a, const void* b) {
    double x = *(const double*)a, y = *(const double*)b;
    return x < y ? -1 : x > y;
}

// "mean p50 p90 max" of the window's values
static void describe(char* out, size_t size, double* values, int n) {
    double sum = 0;
    for (int i = 0; i < n; ++i) {
        sum += values[i];
    }
    qsort(values, n, sizeof(double), compare);
    snprintf(out, size, "%.2f (p50 %.2f, p90 %.2f, max %.2f)", sum / n, values[n / 2], values[n * 9 / 10], values[n - 1]);
}

static void report(double at) {
    char wait[96], begin[96], work[96], end[96], outside[96];
    describe(wait, sizeof wait, waits, count);
    describe(begin, sizeof begin, begins, count);
    describe(work, sizeof work, works, count);
    describe(end, sizeof end, ends, count);
    describe(outside, sizeof outside, outsides, count);
    fprintf(stderr,
        "cranvr frame timing: %llu frames, the last %d in %.2f s (%.1f fps); ms: xrWaitFrame %s, xrBeginFrame %s, "
        "xrWaitFrame's return to xrEndFrame %s, xrEndFrame %s, xrEndFrame's return to the next xrWaitFrame %s\n",
        frames, count, (at - windowStart) / 1000.0, count * 1000.0 / (at - windowStart), wait, begin, work, end, outside);
    fflush(stderr);
    count = 0;
    windowStart = at;
}

static XrResult layerWaitFrame(XrSession session, const void* info, void* state) {
    waitCalled = now();
    XrResult result = nextWaitFrame(session, info, state);
    waitReturned = now();
    return result;
}

static XrResult layerBeginFrame(XrSession session, const void* info) {
    double called = now();
    XrResult result = nextBeginFrame(session, info);
    if (count < WINDOW) {
        begins[count] = now() - called;
    }
    return result;
}

static XrResult layerEndFrame(XrSession session, const void* info) {
    double called = now();
    XrResult result = nextEndFrame(session, info);
    double returned = now();
    // A frame counts from its xrWaitFrame, and the time outside a frame from the last xrEndFrame
    if (waitReturned > 0 && endReturned > 0 && count < WINDOW) {
        waits[count] = waitReturned - waitCalled;
        works[count] = called - waitReturned;
        ends[count] = returned - called;
        outsides[count] = waitCalled - endReturned;
        ++count;
        ++frames;
    }
    if (windowStart == 0) {
        windowStart = returned;
    }
    endReturned = returned;
    if (count == WINDOW) {
        report(returned);
    }
    return result;
}

static XrResult layerGetInstanceProcAddr(XrInstance instance, const char* name, PFN_xrVoidFunction* function) {
    if (strcmp(name, "xrGetInstanceProcAddr") == 0) {
        *function = (PFN_xrVoidFunction)layerGetInstanceProcAddr;
        return 0;
    }
    if (strcmp(name, "xrWaitFrame") == 0 && nextWaitFrame != NULL) {
        *function = (PFN_xrVoidFunction)layerWaitFrame;
        return 0;
    }
    if (strcmp(name, "xrBeginFrame") == 0 && nextBeginFrame != NULL) {
        *function = (PFN_xrVoidFunction)layerBeginFrame;
        return 0;
    }
    if (strcmp(name, "xrEndFrame") == 0 && nextEndFrame != NULL) {
        *function = (PFN_xrVoidFunction)layerEndFrame;
        return 0;
    }
    return nextGetInstanceProcAddr(instance, name, function);
}

static XrResult layerCreateApiLayerInstance(const void* info, const ApiLayerCreateInfo* layerInfo, XrInstance* instance) {
    nextGetInstanceProcAddr = layerInfo->nextInfo->nextGetInstanceProcAddr;
    ApiLayerCreateInfo nextLayerInfo = *layerInfo;
    nextLayerInfo.nextInfo = layerInfo->nextInfo->next;
    XrResult result = layerInfo->nextInfo->nextCreateApiLayerInstance(info, &nextLayerInfo, instance);
    if (result < 0) {
        return result;
    }
    nextGetInstanceProcAddr(*instance, "xrWaitFrame", (PFN_xrVoidFunction*)&nextWaitFrame);
    nextGetInstanceProcAddr(*instance, "xrBeginFrame", (PFN_xrVoidFunction*)&nextBeginFrame);
    nextGetInstanceProcAddr(*instance, "xrEndFrame", (PFN_xrVoidFunction*)&nextEndFrame);
    LARGE_INTEGER frequency;
    QueryPerformanceFrequency(&frequency);
    ticksPerMs = (double)frequency.QuadPart / 1000.0;
    fprintf(stderr, "cranvr frame timing: the layer runs; a line every %d frames\n", WINDOW);
    fflush(stderr);
    return result;
}

__declspec(dllexport) XrResult xrNegotiateLoaderApiLayerInterface(
    const NegotiateLoaderInfo* loaderInfo, const char* layerName, NegotiateApiLayerRequest* request) {
    (void)layerName;
    if (loaderInfo == NULL || request == NULL || loaderInfo->minInterfaceVersion > 1 || loaderInfo->maxInterfaceVersion < 1) {
        return -1;
    }
    request->layerInterfaceVersion = 1;
    // OpenXR 1.0
    request->layerApiVersion = (XrVersion)1 << 48;
    request->getInstanceProcAddr = layerGetInstanceProcAddr;
    request->createApiLayerInstance = layerCreateApiLayerInstance;
    return 0;
}
