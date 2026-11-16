#!/bin/zsh
# Starts a Windows program in cranvr's Wine stack (see setup-wine.sh), connected to a running
# cranvr-host through the host's runtime control file.
#
# Usage: scripts/run-game.sh [--mute] PROGRAM.exe [ARGS...]
#   PROGRAM.exe  a Windows path (C:\...) or a Mac path
#   --mute       start without sound (Wine gets no audio driver)
#
# Environment: CRANVR_MAX_MSAA caps the MSAA samples a game may use (default: the app's setting, else
# 2; 0 for no cap).
#
# Examples:
#   scripts/run-game.sh 'C:\basalt\runtime\d3d11_runtime_smoke.exe' --render-frames 7200 --visual
#   scripts/run-game.sh --steam
#   scripts/run-game.sh 'C:\Program Files (x86)\Steam\steamapps\common\Beat Saber\Beat Saber.exe'
#
# Start a Steam game by its .exe while Steam runs, not with steam.exe -applaunch: a game that
# Steam starts gets Steam's environment, not the one this script sets. For a game under
# steamapps\common the script finds the Steam app ID in Steam's appmanifest files and sets
# SteamAppId, which the game's Steam check needs.
#
# Environment: CRANVR_HOME (default ~/Library/Application Support/cranvr) is the host's config
# dir; the host writes runtime-control.txt there. CRANVR_CONTROL names another control file, for a
# host with its own config dir, such as the one of scripts/e2e.sh.

set -euo pipefail

home=${CRANVR_HOME:-$HOME/Library/Application Support/cranvr}
wine_root=${home}/wine/engine
control=${CRANVR_CONTROL:-${home}/runtime-control.txt}
mute=false
if [[ ${1:-} == --mute ]]; then
    mute=true
    shift
fi
# --steam: Steam with the flags its web helper shim needs (see setup-steam.sh)
if [[ ${1:-} == --steam ]]; then
    shift
    set -- 'C:\Program Files (x86)\Steam\steam.exe' -inhibitbootstrap -noverifyfiles -no-cef-sandbox "$@"
fi
if (( $# < 1 )); then
    print -u2 "usage: scripts/run-game.sh [--mute] PROGRAM.exe [ARGS...]"
    exit 2
fi
if [[ ! -x ${wine_root}/bin/wine ]]; then
    print -u2 "No Wine stack in ${home}/wine. Run scripts/setup-wine.sh first."
    exit 1
fi
if [[ ! -s ${control} ]]; then
    print -u2 "No ${control}. Start cranvr-host first."
    exit 1
fi

export WINEPREFIX=${home}/wine/prefix WINEARCH=win64 WINEDEBUG=${WINEDEBUG:--all}
# The runtime reads the control file through Wine's Z: drive, which maps the Mac's root
export BASALT_CONTROL="Z:${control//\//\\}"
export DXMT_BASALT_IOSURFACE=1
# Games may use at most this many MSAA samples (cranvr's DXMT patch 0006): CRANVR_MAX_MSAA, else
# the app's setting, else 2. 0 leaves it to the game.
msaa=${CRANVR_MAX_MSAA:-}
if [[ -z ${msaa} && -f ${home}/app.json ]]; then
    msaa=$(grep -oE '"game_msaa_limit": *[0-9]+' "${home}/app.json" | grep -oE '[0-9]+$' || true)
fi
msaa=${msaa:-2}
if (( msaa > 0 )); then
    export DXMT_MAX_SAMPLE_COUNT=${msaa}
fi
if ${mute}; then
    export WINEDLLOVERRIDES="winecoreaudio.drv=d${WINEDLLOVERRIDES:+;${WINEDLLOVERRIDES}}"
fi

program=$1
shift
# Run from the program's folder: many games load files relative to it
if [[ ${program} == [A-Za-z]:\\* ]]; then
    dir=$("${wine_root}/bin/wine" winepath -u "${program}" 2>/dev/null)
    dir=${dir:h}
else
    dir=${program:A:h}
fi
cd "${dir}"
if [[ ${dir} == */steamapps/common/* ]]; then
    steamapps=${dir%%/steamapps/common/*}/steamapps
    installdir=${${dir#*/steamapps/common/}%%/*}
    manifest=$(grep -l "\"installdir\"[[:space:]]*\"${installdir}\"" "${steamapps}"/appmanifest_*.acf 2>/dev/null | head -1 || true)
    if [[ -n ${manifest} ]]; then
        app_id=${${manifest:t:r}#appmanifest_}
        export SteamAppId=${app_id} SteamGameId=${app_id}
        print "Steam app ${app_id} (${installdir})"
    fi
fi
export MVK_CONFIG_LOG_LEVEL=0
exec "${wine_root}/bin/wine" "${program}" "$@"
