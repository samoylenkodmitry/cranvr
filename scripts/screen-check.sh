#!/bin/zsh
# Checks a game's loading screen in the headset (client/src/game_screen.rs) on a Quest on a desk. A bare
# test host on PORT runs with no game: the script writes a game's screen into its input (`screen {...}`),
# as a game's mailbox messages make it. The client shows its virtual room (`room=scene`). The script takes
# a screen capture
#   - before any screen (none),
#   - while the screen shows the loading words, two lines and the message in front of the head (shown),
#   - after the same screen came again turned 30 degrees toward the left (turned),
#   - after `screen null` (hidden).
# It counts the pixels of the message's color in the left eye of each capture and their middle
# (scripts/screen-color.py), keeps a capture only when scripts/capture-check.py passes it, and exits 1
# unless the message shows while the screen shows, stands more to the left when turned, and is gone
# before and after. The first start of the client only warms up. The headset changes its USB functions
# about 25 s after that start, which ends the test port's link and drops its `adb reverse`, so the
# measured start waits until 32 s after it and sets the reverse again. A run in which the link ended
# is void, and the script exits 2.
#
# Usage: scripts/screen-check.sh SERIAL WORKDIR [CAPTURES] [PORT]
#   CAPTURES  the folder for the captures that pass (none by default: every capture is deleted)
#   PORT      the test host's port (9961 by default)
#
# `cargo build --release` must have built cranvr-host, and the client must be a normal build of
# `scripts/build-client.sh`. Hold the Quest lock. At the end the script puts the user's client back on
# 9950 (scripts/give-back.sh), also after a failure, unless CRANVR_NO_GIVE_BACK is set.
# CRANVR_GIVE_BACK_APK names a build that give-back.sh installs first.
set -u
zmodload zsh/datetime
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
# Waits up to $2 seconds for a line of the client's log that holds $1
wait_log() {
    local i
    for i in {1..$(( $2 * 4 ))}; do
        ${adb_cmd} logcat -d -s Cranpose 2> /dev/null | grep -q -- "$1" && return 0
        sleep 0.25
    done
    return 1
}
# A change of the headset's USB functions takes adb away for a moment: the capture waits for it
shot() {
    quest_ready
    ${adb_cmd} exec-out screencap -p > ${work}/$1.png
    print -P "$1 taken at %D{%H:%M:%S.%.}"
    python3 -I ${root}/scripts/screen-color.py ${work}/$1.png $1 | tee -a ${work}/colors.txt
    if [[ -n ${captures} ]]; then
        python3 -I ${root}/scripts/capture-check.py ${work}/$1.png ${captures}
    else
        python3 -I ${root}/scripts/capture-check.py ${work}/$1.png
    fi
}
screen() {
    print -u 3 "screen $1"
    print "the game's screen: $1"
}
lines='"lines": ["A tip of the check, long enough to wrap on the panel in the headset.", "A second line of the check."]'
lan=$(ipconfig getifaddr en0 || ipconfig getifaddr en1)
forget=127.0.0.1:${port}${lan:+,${lan}:${port}}
print "cut starts $(date +%T)"
${root}/target/release/cranvr-host --status --fps 120 --mbps 200 --native-port ${port} --config-dir ${home} --trust-all --tcp --loopback --foveation off < ${input} > ${host_log} 2>&1 &
host=$!
exec 3> ${input}
sleep 1
${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
common=(--es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room scene --ez save_choices false \
    --es panels hidden --ez console false --es forget_host_key ${forget})
client_start ${host_log} ${common}
warm=${EPOCHSECONDS}
print "warm start: ${client_result}"
while (( EPOCHSECONDS < warm + 32 )); do
    sleep 1
done
${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
quest_ready
${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
${adb_cmd} logcat -c
client_start ${host_log} ${common}
print "client: ${client_result} ${client_linked}"
[[ ${client_result} == alive ]] || exit 1
sleep 2
shot none
screen "{\"loading\": \"LOADING\", ${lines}, \"message\": \"Press Trigger To Start\", \"facing_deg\": 0.0}"
wait_log "the game's screen: Some" 6 || print "the client did not log the screen"
sleep 1.5
shot shown
screen null
wait_log "the game's screen hides" 6 || print "the client did not log the end of the screen"
sleep 1
screen "{\"loading\": \"LOADING\", ${lines}, \"message\": \"Press Trigger To Start\", \"facing_deg\": 30.0}"
sleep 2
shot turned
screen null
sleep 1.5
shot hidden
${adb_cmd} logcat -d -v time > ${work}/client.log
grep -E "Cranpose" ${work}/client.log | grep -E "the game's screen|the frame needs" | sed -E 's/^[0-9-]+ ([0-9:.]+) .*(cranvr_client|cranpose_xr)[a-z_:]*: /\1 /'
print "cut ends $(date +%T)"
if (( $(grep -c "Headset connected" ${host_log}) > 2 )); then
    grep -E "link ended" ${host_log} | tail -1
    print "the run is void: the link ended during the check; run the script again"
    exit 2
fi
python3 -I ${root}/scripts/screen-color.py --verdict ${work}/colors.txt
