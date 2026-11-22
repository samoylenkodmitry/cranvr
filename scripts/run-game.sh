#!/bin/zsh
# Starts a Windows program in cranvr's Wine stack (see setup-wine.sh), connected to a running
# cranvr-host through the host's runtime control file.
#
# Usage: scripts/run-game.sh [--mute] PROGRAM.exe [ARGS...]
#   PROGRAM.exe  a Windows path (C:\...) or a Mac path
#   --mute       start without sound (Wine gets no audio driver)
#
# Environment: CRANVR_OPENVR=opencomposite, steamvr or off chooses the OpenVR runtime for this start
# (default: the app's setting "OpenVR games" in app.json, which is OpenComposite until the user changes
# it). The script writes the prefix's OpenVR path file (%LOCALAPPDATA%\\openvr\\openvrpaths.vrpath), as the
# app does (app/src/openvr.rs). OpenComposite sends an OpenVR game's calls to the OpenXR runtime of cranvr.
# SteamVR runs the game in SteamVR for Windows from the prefix's Steam library, with cranvr's driver in
# C:\\cranvr\\steamvr: the game starts SteamVR, and SteamVR ends with the game. Off removes a file that
# cranvr wrote. No file of a game changes. An OpenXR game ignores the file.
#
# Environment: CRANVR_MAX_MSAA caps the MSAA samples a game may use (default: the app's setting, else
# 2; 0 for no cap). The script applies no option of a particular game: set them yourself, as the
# app's launch options do, for example BASALT_MAX_ARRAY_SIZE=1 for a game whose right eye stays
# black, or WINEDLLOVERRIDES='winhttp=n,b' for a mod loader that does not start without its winhttp.dll
# (BSIPA, which patches the game's files, does start without it and starts twice with it).
#
# Examples:
#   scripts/run-game.sh 'C:\basalt\runtime\d3d11_runtime_smoke.exe' --render-frames 7200 --visual
#   scripts/run-game.sh --steam
#   scripts/run-game.sh 'C:\Program Files (x86)\Steam\steamapps\common\Beat Saber\Beat Saber.exe'
#
# Start a Steam game by its .exe while Steam runs, not with steam.exe -applaunch: a game that
# Steam starts gets Steam's environment, not the one this script sets. SteamAppId, which the game's
# Steam check needs, comes from the first of these: CRANVR_STEAM_APP_ID; the "Steam app ID" in the
# program's launch options in app.json (the Play tab fills it in for a game added from a folder, such
# as a copy of a Steam game); for a game under steamapps\common, the app ID in Steam's appmanifest files.
#
# Environment: CRANVR_HOME (default ~/Library/Application Support/cranvr) is the host's config
# dir; the host writes runtime-control.txt there. CRANVR_CONTROL names another control file, for a
# host with its own config dir, such as the one of scripts/e2e.sh.

set -euo pipefail

home=${CRANVR_HOME:-$HOME/Library/Application Support/cranvr}
wine_root=${home}/wine/engine
control=${CRANVR_CONTROL:-${home}/runtime-control.txt}
mute=false
steam=false
if [[ ${1:-} == --mute ]]; then
    mute=true
    shift
fi
# --steam: Steam with the flags its web helper shim needs (see setup-steam.sh)
if [[ ${1:-} == --steam ]]; then
    steam=true
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
# A game's mirror window on the Mac stays empty and costs no GPU time (DXMT patch 0008);
# CRANVR_MAC_MIRROR=1 draws it. Steam's own window draws with DXMT too, so it keeps drawing.
if ! ${steam} && [[ ${CRANVR_MAC_MIRROR:-0} != 1 ]]; then
    export DXMT_SKIP_PRESENT=1
fi
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
app_id=${CRANVR_STEAM_APP_ID:-}
if [[ -z ${app_id} && -f ${home}/app.json ]]; then
    # The launch options are saved by the program's Windows path (C:\ inside the prefix, Z:\ elsewhere)
    if [[ ${program} == [A-Za-z]:\\* ]]; then option_key=${program}; else option_key=${program:A}; fi
    app_id=$(python3 -I -c '
import json, sys
app_json, program, prefix = sys.argv[1:4]
def windows(path):
    if len(path) > 1 and path[1] == ":":
        return path
    drive_c = prefix.rstrip("/") + "/drive_c/"
    if path.startswith(drive_c):
        return "C:\\" + path[len(drive_c):].replace("/", "\\")
    return "Z:" + path.replace("/", "\\")
want = windows(program).lower()
for key, options in json.load(open(app_json)).get("launch", {}).items():
    if key.lower() == want:
        print(str(options.get("steam_app_id", "")).strip())
        break
' "${home}/app.json" "${option_key}" "${home}/wine/prefix" 2>/dev/null || true)
    if [[ -n ${app_id} ]]; then
        print "Steam app ${app_id} (the program's launch options)"
    fi
fi
if [[ -n ${app_id} ]]; then
    export SteamAppId=${app_id} SteamGameId=${app_id}
elif [[ ${dir} == */steamapps/common/* ]]; then
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

# OpenVR runtime: the same choice as the app makes (app/src/openvr.rs)
openvr=${CRANVR_OPENVR:-}
if [[ -z ${openvr} && -f ${home}/app.json ]]; then
    openvr=$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1])).get("wine", {}).get("openvr", "opencomposite"))' "${home}/app.json" 2>/dev/null || true)
fi
openvr=${openvr:-opencomposite}
[[ ${openvr} == (opencomposite|steamvr|off) ]] || { print -u2 "CRANVR_OPENVR takes opencomposite, steamvr or off, not ${openvr}"; exit 2; }
prefix=${home}/wine/prefix
user_dir=${prefix}/drive_c/users/$(id -un)
[[ -d ${user_dir} ]] || user_dir=$(print -rl -- "${prefix}"/drive_c/users/*(/N) | grep -v '/Public$' | head -1)
if [[ -n ${user_dir} ]]; then
    vrpath=${user_dir}/AppData/Local/openvr/openvrpaths.vrpath
    case ${openvr} in
        opencomposite)
            [[ -f ${prefix}/drive_c/opencomposite/bin/vrclient_x64.dll ]] || { print -u2 "The game support has no OpenComposite"; exit 1; }
            mkdir -p "${vrpath:h}"
            print -r -- '{"jsonid": "vrpathreg", "runtime": ["C:\\opencomposite"], "version": 1}' > "${vrpath}"
            print "OpenVR games use OpenComposite (${vrpath})" ;;
        steamvr)
            [[ -f ${prefix}/drive_c/cranvr/steamvr/driver.vrdrivermanifest ]] || { print -u2 "The game support has no SteamVR driver"; exit 1; }
            mkdir -p "${vrpath:h}"
            # SteamVR's folder from its manifest (app 250820) in one of the Steam libraries of the prefix
            python3 -I - "${prefix}" "${vrpath}" <<'PYTHON' || exit 1
import json, os, re, sys
prefix, vrpath = sys.argv[1], sys.argv[2]
steam = os.path.join(prefix, "drive_c", "Program Files (x86)", "Steam")
def windows(path):
    rest = os.path.relpath(path, os.path.join(prefix, "drive_c"))
    return ("C:\\" + rest if not rest.startswith("..") else "Z:" + path).replace("/", "\\")
libraries = [os.path.join(steam, "steamapps")]
try:
    text = open(os.path.join(steam, "steamapps", "libraryfolders.vdf"), encoding="utf-8", errors="replace").read()
    for found in re.findall(r'"path"\s+"([^"]+)"', text):
        found = found.replace("\\\\", "\\")
        drive, rest = found[0].upper(), found[3:].replace("\\", "/")
        root = os.path.join(prefix, "drive_c", rest) if drive == "C" else "/" + rest
        libraries.append(os.path.join(root, "steamapps"))
except OSError:
    pass
for steamapps in libraries:
    try:
        manifest = open(os.path.join(steamapps, "appmanifest_250820.acf"), encoding="utf-8", errors="replace").read()
    except OSError:
        continue
    installdir = re.search(r'"installdir"\s+"([^"]+)"', manifest)
    folder = os.path.join(steamapps, "common", installdir.group(1)) if installdir else ""
    if folder and os.path.isfile(os.path.join(folder, "bin", "win64", "vrserver.exe")):
        paths = {"config": [windows(os.path.join(steam, "config"))], "external_drivers": ["C:\\cranvr\\steamvr"],
                 "jsonid": "vrpathreg", "log": [windows(os.path.join(steam, "logs"))], "runtime": [windows(folder)], "version": 1}
        open(vrpath, "w").write(json.dumps(paths))
        print("OpenVR games use SteamVR in " + windows(folder) + " (" + vrpath + ")")
        sys.exit(0)
print("SteamVR is not installed: install SteamVR from the Steam library of the prefix", file=sys.stderr)
sys.exit(1)
PYTHON
            ;;
        off)
            if [[ -f ${vrpath} ]] && grep -qE 'C:\\\\(opencomposite|cranvr\\\\steamvr)' "${vrpath}"; then
                rm -f "${vrpath}"
                print "OpenVR games find no runtime (OpenVR is off)"
            fi ;;
    esac
fi
exec "${wine_root}/bin/wine" "${program}" "$@"
