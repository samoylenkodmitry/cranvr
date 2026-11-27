#!/bin/zsh
# A leak test of DXMT's shared textures and shared fences. C:\basalt\runtime\shared_churn.exe runs in Wine. In each
# of --cycles cycles (default 200) it makes six textures of DXMT's stock shared path (R16G16B16A16_FLOAT, B8G8R8A8 with
# a legacy handle, mipmaps, two slices with an NT handle, 4x MSAA, a keyed mutex), one IOSurface texture of cranvr's
# path and one shared fence. A second process of the program opens each one, checks it and signals the fence. Then
# both release everything. At the end the program checks that the handle of a released IOSurface texture does not
# open once its ID names a surface of another size. The run needs no host, no headset and no game. It takes about
# 5 seconds.
#
# Usage: scripts/shared-churn.sh [--cycles N] [--width W] [--height H]
#
# The run counts the names in launchd of the program's two processes (scripts/launchd-names.py): while both hold the
# first cycle's objects (live), after the first cycle (warm), after the last one (churned) and after their end. It
# reads the IOSurface part of each process's footprint (`footprint`) after the first and the last cycle. It counts the
# global IOSurfaces of the test's height (scripts/iosurfaces.py) before and after. It fails when a name of a released
# object stays while the processes live or after their end, when a global surface of the test stays, or when the
# processes' surfaces grew by more than 32 MB and a twentieth of what the cycles made.
#
# Before cranvr's DXMT patch 0020 each cycle left 8 names in launchd (6 textures, the keyed mutex's event and the
# fence) with the memory of the textures until the user logged out. Run the old stack only with --cycles 2 --width 64
# --height 64. Set CRANVR_HOME to the stack to test (default: the app's, ~/Library/Application Support/cranvr).

set -euo pipefail

root=${0:A:h:h}
home=${CRANVR_HOME:-$HOME/Library/Application Support/cranvr}
cycles=200 width=640 height=333
while (( $# )); do
    case $1 in
        --cycles) cycles=$2; shift 2 ;;
        --width) width=$2; shift 2 ;;
        --height) height=$2; shift 2 ;;
        *) print -u2 "usage: scripts/shared-churn.sh [--cycles N] [--width W] [--height H]"; exit 2 ;;
    esac
done
wine=${home}/wine/engine/bin/wine
if [[ ! -x ${wine} || ! -f ${home}/wine/prefix/drive_c/basalt/runtime/shared_churn.exe ]]; then
    print -u2 "No shared_churn.exe in ${home}/wine. Run scripts/setup-wine.sh with a runtime that has it."
    exit 1
fi
work=$(mktemp -d "${TMPDIR:-/tmp}/shared-churn.XXXXXX")
signal=${work}/signal
names() { python3 -I "${root}/scripts/launchd-names.py" "$@"; }
surfaces() { python3 -I "${root}/scripts/iosurfaces.py" "${height}"; }
# The IOSurface part of a process's footprint in bytes: "IOSurface" for the surfaces mapped into the process and
# "Owned physical footprint (unmapped)" for the rest (DXMT's textures, which Metal never maps)
surface_footprint() {
    footprint -p "$1" -f bytes 2>/dev/null | awk '$2 == "B" && $4 == "B" && $6 == "B" {
        category = $8; for (i = 9; i <= NF; i++) category = category " " $i
        if (category == "IOSurface" || category == "Owned physical footprint (unmapped)") sum += $1
    } END { print sum + 0 }'
}

print "Before: launchd $(names --save "${work}/before.json"), IOSurfaces ${height} high: $(surfaces)"
before_surfaces=$(surfaces)
print -r "Starting shared_churn.exe, ${cycles} cycles of ${width}x${height} (log: ${work}/program.log)"
WINEPREFIX=${home}/wine/prefix WINEARCH=win64 WINEDEBUG=-all DXMT_BASALT_IOSURFACE=1 DXMT_LOG_PATH=none \
    "${wine}" 'C:\basalt\runtime\shared_churn.exe' --cycles "${cycles}" --width "${width}" --height "${height}" \
    --signal "Z:${signal//\//\\}" > "${work}/program.log" 2>&1 &
program=$!

pids=""
typeset -A at
for stage in live warm churned; do
    for i in {1..600}; do
        grep -q "shared_churn: measure ${stage}" "${work}/program.log" 2>/dev/null && break
        grep -qE "shared_churn: (PASS|FAIL)" "${work}/program.log" 2>/dev/null && break
        sleep 0.05
    done
    grep -q "shared_churn: measure ${stage}" "${work}/program.log" || break
    # Both processes run the program's .exe; Wine names a process by its Windows path
    pids=$(pgrep -f '(^|")C:.basalt.runtime.shared_churn\.exe' | paste -sd, -)
    line="$(names --pids "${pids}")"
    if [[ ${stage} != live ]]; then
        bytes=0
        for pid in ${(s:,:)pids}; do
            bytes=$(( bytes + $(surface_footprint "${pid}") ))
        done
        line+=" footprint ${bytes}"
    fi
    at[${stage}]=${line}
    print "${stage}: processes ${pids}: launchd ${line}"
    : > "${signal}.${stage}"
done
wait ${program} || true
grep -h "shared_churn:" "${work}/program.log" | grep -v "measure" || true
grep -h "the ID names another surface now" "${work}/program.log" | sed 's/^/DXMT: /' || true
# The surfaces go a few milliseconds after their last owner
for i in {1..20}; do
    after_surfaces=$(surfaces)
    (( ${after_surfaces%% *} <= ${before_surfaces%% *} )) && break
    sleep 0.05
done
after_pids=$(names --pids "${pids:-0}")
print "After: launchd $(names --since "${work}/before.json"), IOSurfaces ${height} high: ${after_surfaces}"
print "After: the program's processes ${pids}: launchd ${after_pids}"

bad=()
grep -q "shared_churn: PASS" "${work}/program.log" || bad+=("the program did not pass")
grep -q "refused it" "${work}/program.log" && ! grep -q "the ID names another surface now" "${work}/program.log" &&
    bad+=("the second process refused the forged ID, but DXMT logged no size check")
field() { print -r -- "$1" | awk -v key="$2" '{ for (i = 1; i < NF; i++) if ($i == key) { print $(i + 1); exit } }'; }
if [[ -n ${at[live]:-} ]]; then
    (( $(field "${at[live]}" names) > 0 )) || bad+=("no name in launchd while both processes held the objects: the count sees nothing")
fi
if [[ -n ${at[warm]:-} && -n ${at[churned]:-} ]]; then
    warm_names=$(field "${at[warm]}" names) churned_names=$(field "${at[churned]}" names)
    (( warm_names == 0 )) || bad+=("${warm_names} names of released objects stay in launchd after the first cycle")
    (( churned_names <= warm_names )) || bad+=("$(( churned_names - warm_names )) names of released objects stay in launchd after the last cycle")
    # What the cycles made, in bytes a pixel: R16G16B16A16 8, B8G8R8A8 4, mipmaps 5 (4 and a third), two slices 8,
    # 4 samples 16, the keyed mutex's 4, the IOSurface texture 4
    made=$(( (cycles - 1) * width * height * (8 + 4 + 5 + 8 + 16 + 4 + 4) ))
    grown=$(( $(field "${at[churned]}" footprint) - $(field "${at[warm]}" footprint) ))
    print "The processes' IOSurfaces grew by $(( grown / 1048576 )) MB while the cycles made $(( made / 1048576 )) MB"
    # Metal and DXMT keep up to about 12 MB of their own after the first cycles; a leak grows with the cycles
    (( grown <= 32 * 1048576 || grown * 20 <= made )) ||
        bad+=("the processes' IOSurfaces grew by more than 32 MB and a twentieth of what the cycles made")
fi
(( $(field "${after_pids}" names) == 0 )) || bad+=("$(field "${after_pids}" names) names of the program stay in launchd after its end")
(( ${after_surfaces%% *} <= ${before_surfaces%% *} )) || bad+=("$(( ${after_surfaces%% *} - ${before_surfaces%% *} )) IOSurfaces ${height} high stay")
if (( ${#bad} )); then
    for problem in "${bad[@]}"; do print -u2 "FAIL: ${problem}"; done
    print -u2 "Log: ${work}/program.log"
    exit 1
fi
print "PASS: shared textures and fences leave no name in launchd and no surface"
rm -r "${work}"
