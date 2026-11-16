#!/bin/zsh
# Measures a game through the host that the cranvr app runs, with a Quest on the desk.
#
# Usage: scripts/perf/game-run.sh LABEL SECONDS [PROGRAM.exe]
#   LABEL      a name for this run's files
#   SECONDS    how long to record, 25 s after the game connects
#   PROGRAM    the game's Windows path (default: Beat Saber)
#
# The cranvr app must run. The script wakes the headset without a wearer (proximity sensor and
# guardian off), starts the Quest client and the game without sound, records the host's stats and
# the Mac's GPU use, then stops the game and gives the headset its sensor and guardian back.
#
# Environment:
#   ANDROID_SERIAL   the Quest, when adb sees more than one device
#   CRANVR_PERF_DIR  where the files go (default: target/perf in the repo)
#   HZ               paces the game at HZ instead of the headset's rate, to show its uncapped rate.
#                    The script writes the old rate back at its end.
#   Anything run-game.sh reads (CRANVR_MAX_MSAA) and DXMT's variables pass to the game.
#
# Output: perf-LABEL.log (the host's log lines of the run), gpu-LABEL.log (GPU use in percent every
# 2 s), game-LABEL.log (the game's Wine output), and a summary line from summarize.py.
label=$1 secs=$2
program=${3:-'C:\Program Files (x86)\Steam\steamapps\common\Beat Saber\Beat Saber.exe'}
[[ -n $label && -n $secs ]] || { sed -n '4,8p' $0; exit 2; }
repo=${0:A:h:h:h}
out=${CRANVR_PERF_DIR:-$repo/target/perf}
config="$HOME/Library/Application Support/cranvr"
log="$config/host.log"
mkdir -p $out
pgrep -f "cranvr.app/Contents/MacOS/cranvr-host" > /dev/null || { echo "the cranvr app is not running"; exit 1; }

# The control file holds the game's token: its copy stays private to this user (mode 0600).
set_pacing() {
    local f="$config/runtime-control.txt"
    (umask 077 && awk -v hz=$1 '{ $5 = hz; print }' "$f" > "$f.tmp") && mv "$f.tmp" "$f"
}

restore() {
    [[ -n $paced_hz ]] && set_pacing $paced_hz
    pkill -f "$(basename "${program//\\//}")" || true
    pkill -f UnityCrashHandler64 || true
    adb shell setprop debug.oculus.guardian_pause 0
    adb shell am broadcast -a com.oculus.vrpowermanager.automation_disable > /dev/null
}
trap restore EXIT

skip=$(grep -c "" "$log")
adb shell input keyevent KEYCODE_WAKEUP
adb shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
adb shell setprop debug.oculus.guardian_pause 1
adb shell am force-stop dev.cranvr.client
adb shell am start -n dev.cranvr.client/android.app.NativeActivity > /dev/null
for i in {1..120}; do tail -n +$skip "$log" | grep -q "Headset connected" && break; sleep 1; done
tail -n +$skip "$log" | grep -q "Headset connected" || { echo "no headset"; exit 1; }
sleep 3
if [[ -n $HZ ]]; then
    paced_hz=$(awk '{ print $5 }' "$config/runtime-control.txt")
    set_pacing $HZ
fi
(cd $repo && scripts/run-game.sh --mute "$program" > $out/game-$label.log 2>&1 &)
for i in {1..90}; do tail -n +$skip "$log" | grep -q "Game connected" && break; sleep 1; done
tail -n +$skip "$log" | grep -q "Game connected" || { echo "no game"; exit 1; }
sleep 25
for i in $(seq 1 $((secs / 2))); do
    ioreg -r -d 1 -c IOAccelerator | grep -oE '"Device Utilization %"=[0-9]+' | cut -d= -f2
    sleep 2
done > $out/gpu-$label.log
tail -n +$skip "$log" > $out/perf-$label.log
python3 -I ${0:A:h}/summarize.py $out/perf-$label.log $label
