#!/bin/zsh
# Installs the Windows Steam client into cranvr's Wine prefix and prepares it, so its first start
# only asks for the Steam login. Run scripts/setup-wine.sh first.
#
# Usage: scripts/setup-steam.sh [SteamSetup.exe]
# Without an installer the script downloads SteamSetup.exe from Valve's CDN.
#
# Steps:
#   1. silent install of SteamSetup.exe;
#   2. one start of Steam, so it downloads its current client (about 1.6 GB);
#   3. Steam's web helper (the Chromium process behind Steam's window) gets BasaltVR's shim.
#      Under Wine on macOS the helper cannot show its GPU surface; the shim starts it with
#      --disable-gpu --in-process-gpu --no-sandbox (the setup CrossOver uses);
#   4. Steam no longer starts with Wine by itself.
#
# Run it again after a Steam update if Steam's window stays black: it puts the shim back.

set -euo pipefail

home=${CRANVR_HOME:-$HOME/Library/Application Support/cranvr}
wine_root=${home}/wine/engine
prefix=${home}/wine/prefix
steam_dir="${prefix}/drive_c/Program Files (x86)/Steam"
shim=${prefix}/drive_c/basalt/runtime/steamwebhelper_compat.exe
export WINEPREFIX=${prefix} WINEARCH=win64 WINEDEBUG=-all MVK_CONFIG_LOG_LEVEL=0

# Steam's 64-bit web helper: in cef.win64 in newer clients, in cef.win7x64 in older ones
find_helper() {
    local dir
    for dir in cef.win64 cef.win7x64; do
        if [[ -f "${steam_dir}/bin/cef/${dir}/steamwebhelper.exe" ]]; then
            print -r -- "${steam_dir}/bin/cef/${dir}/steamwebhelper.exe"
            return
        fi
    done
}

if [[ ! -x ${wine_root}/bin/wine ]]; then
    print -u2 "No Wine stack in ${home}/wine. Run scripts/setup-wine.sh first."
    exit 1
fi
if [[ ! -f ${shim} ]]; then
    print -u2 "Missing ${shim}. Run scripts/setup-wine.sh again."
    exit 1
fi
# A crash should end the program, not wait in Wine's debugger
"${wine_root}/bin/wine" reg.exe add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion\AeDebug' /v Debugger /t REG_SZ /d '' /f >/dev/null

# 1. Install. The installer copies Steam.exe, then may crash under Wine on its last step: wait
# for the file instead of the installer, then end the installer.
if [[ ! -f ${steam_dir}/steam.exe ]]; then
    installer=${1:-}
    if [[ -z ${installer} ]]; then
        download=$(mktemp -d)
        installer=${download}/SteamSetup.exe
        print "Downloading SteamSetup.exe from Valve"
        curl -fsSL -o "${installer}" https://cdn.akamai.steamstatic.com/client/installer/SteamSetup.exe
    fi
    if [[ $(head -c 2 "${installer}") != MZ ]] || ! grep -q "Valve Corp" "${installer}"; then
        print -u2 "${installer} does not look like Valve's signed Steam installer."
        exit 1
    fi
    print "Installing Steam into the Wine prefix"
    "${wine_root}/bin/wine" "${installer}" /S >/dev/null 2>&1 &
    for i in {1..60}; do
        sleep 2
        [[ -f ${steam_dir}/steam.exe ]] && break
    done
    sleep 5
    "${wine_root}/bin/wineserver" -k 2>/dev/null || true
    wait 2>/dev/null || true
    if [[ ! -f ${steam_dir}/steam.exe ]]; then
        print -u2 "FAIL: the installer finished without steam.exe"
        exit 1
    fi
fi

# 2. First start: Steam updates itself, then gets stopped. The update is done when Steam's log
# says so, the web helper exists and Steam's folder stops growing. Steam waits about 40 seconds before it starts
# to download. A first start can also sit idle for good: its log stops after "Verification
# complete", or at "Downloading manifest" with no request, and the folder stays at a few MB. A
# second start asks for its update at once. So a start with no growth for two minutes is stopped
# and started again.
start_steam() {
    (cd "${steam_dir}" && "${wine_root}/bin/wine" ./steam.exe -noverifyfiles -no-cef-sandbox >/dev/null 2>&1 &)
}
# The web helper appears early in the update. Steam's own log says when the update ended, so a run
# that stopped halfway starts Steam again instead of putting the shim into a half-updated client.
update_done() {
    grep -qs "Update complete, launching Steam" "${steam_dir}/logs/bootstrap_log.txt"
}
helper=$(find_helper)
if [[ -z ${helper} ]] || ! update_done; then
    print "Starting Steam once to download its current client"
    start_steam
    last="" stable=0 idle=0 restarts=0
    for i in {1..180}; do
        sleep 5
        # du fails when Steam deletes a file while it counts: keep the last size then
        size=$(du -sk "${steam_dir}" 2>/dev/null | cut -f1) || size=${last}
        size=${size:-0}
        helper=$(find_helper)
        ended=0
        update_done && ended=1
        if [[ -n ${helper} && ${ended} == 1 && ${size} == "${last}" ]]; then
            (( stable += 1 ))
            (( stable >= 4 )) && break
        else
            stable=0
        fi
        if [[ ${ended} == 0 && ${size} == "${last}" ]]; then
            (( idle += 1 ))
        else
            idle=0
        fi
        if (( idle >= 24 && restarts < 5 )); then
            print "Steam did not start its download. Starting it again"
            "${wine_root}/bin/wineserver" -k 2>/dev/null || true
            sleep 3
            start_steam
            (( restarts += 1 ))
            idle=0
        fi
        last=${size}
        (( i % 6 == 0 )) && print "  Steam folder: $(( size / 1024 )) MB"
    done
    if [[ -z ${helper} ]] || ! update_done; then
        "${wine_root}/bin/wineserver" -k 2>/dev/null || true
        print -u2 "FAIL: Steam did not finish its update in 15 minutes. Run this script again."
        exit 1
    fi
fi

# 3. The web helper shim; the newest original Steam helper stays beside it
"${wine_root}/bin/wineserver" -k 2>/dev/null || true
backup=${helper:r}.basalt-original.exe
if ! cmp -s "${helper}" "${shim}"; then
    # Only Valve's helper goes to the backup, which the shim starts. After a rebuild of BasaltVR the
    # installed helper is an older shim: a copy of it over the backup would leave Steam without
    # Valve's helper.
    if LC_ALL=C grep -q -a "Valve Corp" "${helper}"; then
        cp "${helper}" "${backup}"
    elif [[ ! -f ${backup} ]]; then
        print -u2 "FAIL: ${helper:t} is not Valve's helper and no backup of Valve's helper exists."
        print -u2 "      Let Steam repair its files (start it once without the shim), then run this again."
        exit 1
    fi
    cp "${shim}" "${helper}"
    print "Installed the web helper shim in ${helper:h:t}"
fi

# 4. No autorun
"${wine_root}/bin/wine" reg.exe add 'HKCU\Software\Valve\Steam' /v SuppressAutoRun /t REG_DWORD /d 1 /f >/dev/null
"${wine_root}/bin/wine" reg.exe delete 'HKCU\Software\Microsoft\Windows\CurrentVersion\Run' /v Steam /f >/dev/null 2>&1 || true

print "Steam is ready. Open it from the cranvr app, or: scripts/run-game.sh --steam"
