#!/bin/zsh
# Checks the four looks of the gem that records (client/src/record_gem.rs) on a Quest on a desk,
# with no recording. A bare test host on PORT runs with no app: the script writes the Mac's state of
# the recording into its input (`recording {...}`), as the app does. The scripted build plays
# `scripted_pointer=gem` (client/src/scripted.rs) in the app's virtual room: the right controller
# points away, at the gem, presses it, points away and presses it again. The client shows its
# virtual room with nothing in it (`room=none`): a black background, with neither the room nor the
# host's picture, as for every screen capture of ui3d-check.sh and hologram-check.sh. The host's
# scene (`room=scene`) fills a capture with grey, which capture-check.py refuses. The script
#   - takes a screen capture of the idle gem and of the gem that the ray points at (green). The green
#     that the gem turned gives its place in the picture;
#   - answers the first press with a running recording and takes a capture of the red gem, twice, to
#     show the pulse;
#   - sends a state in which no recording can start, takes a capture of the dim gem and, after the
#     second press, one of the dim gem with the reason beside it.
# It measures the gem's color in each capture (scripts/gem-color.py), keeps a capture only when
# scripts/capture-check.py passes it, and exits 1 unless each look has its color (`--verdict`).
# The first start of the client after its install only warms up: the headset changes its USB
# functions about 28 s after that start, which ends the test port's link for 2 s.
#
# Usage: scripts/gem-looks.sh SERIAL WORKDIR [CAPTURES] [PORT]
#   CAPTURES  the folder for the captures that pass (none by default: every capture is deleted)
#   PORT      the test host's port (9961 by default)
#
# `cargo build --release` must have built cranvr-host, and the client must be a build of
# `scripts/build-client.sh --scripted-hand`. Hold the Quest lock. At the end the script puts the
# user's client back on 9950 (scripts/give-back.sh), also after a failure, unless CRANVR_NO_GIVE_BACK
# is set. CRANVR_GIVE_BACK_APK names a build that give-back.sh installs first.
set -u
serial=$1 work=$2
captures=${3:-} port=${4:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
source ${root}/scripts/client-start.zsh
export CLIENT_LINK_WAIT=${CLIENT_LINK_WAIT:-40}
home=${work}/host
mkdir -p ${home}
input=${work}/host-input
[[ -p ${input} ]] || mkfifo ${input}
host_log=${work}/host.out
host=""
finish() {
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    [[ -n ${host} ]] && kill ${host} 2> /dev/null
    exec 3>&-
    wait 2> /dev/null
    ${adb_cmd} reverse --remove tcp:${port} > /dev/null 2>&1
    # A batch of checks gives the client back once, after its last check
    [[ -n ${CRANVR_NO_GIVE_BACK:-} ]] || ${root}/scripts/give-back.sh ${serial} ${CRANVR_GIVE_BACK_APK:-}
}
trap finish EXIT
trap 'exit 130' INT TERM
client_waiting() {
    ${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
}
# Waits up to $2 seconds for a line of the client's log that holds $1
wait_log() {
    local i
    for i in {1..$(( $2 * 4 ))}; do
        ${adb_cmd} logcat -d -s Cranpose 2> /dev/null | grep -q -- "$1" && return 0
        sleep 0.25
    done
    return 1
}
# The gem's color in the capture $1 at the place `gem_at` holds, and the capture's check
measure() {
    python3 -I ${root}/scripts/gem-color.py ${work}/$1.png ${gem_at:-} | tee -a ${work}/colors.txt
    if [[ -n ${captures} ]]; then
        python3 -I ${root}/scripts/capture-check.py ${work}/$1.png ${captures}
    else
        python3 -I ${root}/scripts/capture-check.py ${work}/$1.png
    fi
}
# A change of the headset's USB functions takes adb away for a moment: the capture waits for it
shot() {
    quest_ready
    ${adb_cmd} exec-out screencap -p > ${work}/$1.png
    print -P "$1 taken at %D{%H:%M:%S.%.}"
}
state() {
    print -u 3 "recording $1"
    print "the Mac's state: $1"
}
lan=$(ipconfig getifaddr en0 || ipconfig getifaddr en1)
forget=127.0.0.1:${port}${lan:+,${lan}:${port}}
print "cut starts $(date +%T)"
${root}/target/release/cranvr-host --status --fps 120 --mbps 200 --native-port ${port} --config-dir ${home} --trust-all --tcp --foveation off < ${input} > ${host_log} 2>&1 &
host=$!
exec 3> ${input}
sleep 1
client_waiting
${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
theme=${THEME:-amber}
common=(--es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room none --es theme ${theme} --ez save_choices false \
    --es panels hidden --ez console false --es forget_host_key ${forget})
client_start ${host_log} ${common}
print "warm start: ${client_result}"
${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
sleep 1
${adb_cmd} logcat -c
client_start ${host_log} ${common} --es scripted_pointer gem --ei gem_wait_ms 12000
print "client: ${client_result} ${client_linked}"
[[ ${client_result} == alive ]] || exit 1
wait_log "points away: the gem shows its idle look" 10 || print "no idle step"
sleep 4
# The idle capture waits for the place that the green of the pointed gem shows
shot gem-idle
wait_log "points at the gem" 10 || print "no pointing step"
sleep 2.5
shot gem-pointed
gem_at=$(python3 -I ${root}/scripts/gem-color.py --find ${work}/gem-pointed.png ${work}/gem-idle.png)
print "the gem stands at ${gem_at:-an unknown place}"
measure gem-idle
measure gem-pointed
# The press reaches the host as the menu's request `recording.toggle`
for i in {1..32}; do
    grep -q "recording.toggle" ${host_log} && break
    sleep 0.25
done
grep -q "recording.toggle" ${host_log} && print "the host got the press" || print "no press reached the host"
state '{"running": true, "blocked": null, "note": null, "note_id": 0}'
wait_log "points away while the recording runs" 6 || print "no recording step"
sleep 2.5
# Two captures half of the pulse's 1.6 s apart: a capture takes about 0.8 s
shot gem-recording
shot gem-recording-later
measure gem-recording
measure gem-recording-later
state '{"running": false, "blocked": "Recording needs the headset on the USB cable with USB debugging", "note": null, "note_id": 0}'
sleep 1.5
shot gem-blocked
measure gem-blocked
wait_log "pulls its trigger again" 8 || print "no second press"
sleep 1.6
shot gem-blocked-press
measure gem-blocked-press
${adb_cmd} logcat -d -v time > ${work}/client.log
grep -E "Cranpose" ${work}/client.log | grep -E "scripted gem:|the gem (cannot|starts|stops)|pointer (enters|presses|rests on|leaves) the gem|the gem.s look" | sed -E 's/^[0-9-]+ ([0-9:.]+) .*cranvr_client::[a-z_:]+: /\1 /'
print "cut ends $(date +%T)"
python3 -I ${root}/scripts/gem-color.py --verdict ${work}/colors.txt ${theme}
