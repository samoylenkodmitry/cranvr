// Asks for the state of the program's session as SteamVR's compositor does. A check of cranvr's Wine build: the
// session of a Wine program is the console, unlocked and local (patch 0006).
//
// SteamVR's compositor asks WTSQuerySessionInformationA for WTSSessionInfoEx and WTSIsRemoteSession. It takes a
// session that it cannot read as locked. It then marks the headset's pose of each frame invalid and fades the
// picture out, as on a Windows computer whose screen is locked. Wine 11.10 answered neither class.
//
// Usage: session_check.exe
// Prints "session_check: PASS" or "session_check: FAIL" with the reason.

#include <windows.h>
#include <wtsapi32.h>
#include <cstdio>

namespace {

// MinGW's header lacks the extended session info of Windows 7, so the program declares its first level here, as the
// Windows SDK does
struct InfoExLevel1 {
    ULONG SessionId;
    WTS_CONNECTSTATE_CLASS SessionState;
    LONG SessionFlags;
    char WinStationName[33];
    char UserName[21];
    char DomainName[18];
    LARGE_INTEGER LogonTime, ConnectTime, DisconnectTime, LastInputTime, CurrentTime;
    DWORD IncomingBytes, OutgoingBytes, IncomingFrames, OutgoingFrames, IncomingCompressedBytes, OutgoingCompressedBytes;
};
struct InfoEx {
    DWORD Level;
    InfoExLevel1 Level1;
};
const WTS_INFO_CLASS kSessionInfoEx = static_cast<WTS_INFO_CLASS>(25);
const WTS_INFO_CLASS kIsRemoteSession = static_cast<WTS_INFO_CLASS>(29);
const LONG kSessionUnlocked = 1;

int Fail(const char *why) {
    std::printf("session_check: FAIL: %s\n", why);
    return 1;
}

}  // namespace

int main() {
    char *buffer = nullptr;
    DWORD size = 0;
    if (!WTSQuerySessionInformationA(WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, kSessionInfoEx, &buffer, &size)) {
        return Fail("WTSSessionInfoEx has no answer");
    }
    const InfoEx *info = reinterpret_cast<const InfoEx *>(buffer);
    if (size < sizeof(InfoEx) || info->Level != 1) {
        WTSFreeMemory(buffer);
        return Fail("WTSSessionInfoEx gives no first level");
    }
    std::printf("session_check: session %lu, state %d, flags %ld, station \"%s\"\n", info->Level1.SessionId,
                static_cast<int>(info->Level1.SessionState), info->Level1.SessionFlags, info->Level1.WinStationName);
    const bool unlocked = info->Level1.SessionFlags == kSessionUnlocked && info->Level1.SessionState == WTSActive;
    WTSFreeMemory(buffer);
    if (!unlocked) {
        return Fail("the session is locked or not active");
    }
    if (!WTSQuerySessionInformationA(WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, kIsRemoteSession, &buffer, &size)) {
        return Fail("WTSIsRemoteSession has no answer");
    }
    // Windows answers with one byte, and SteamVR refuses another length
    const bool local = size == 1 && buffer[0] == 0;
    WTSFreeMemory(buffer);
    if (!local) {
        return Fail("the session is remote, or the answer is not one byte");
    }
    std::printf("session_check: PASS\n");
    return 0;
}
