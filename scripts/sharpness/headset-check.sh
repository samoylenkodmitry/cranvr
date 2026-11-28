#!/bin/zsh
# Measures the sharpness of a display's panel in the headset (issue #222). A test host on PORT makes a
# virtual display of 1920 x 1080 points (3840 x 2160 pixels) that only the stream shows. show.swift
# puts the test pattern on it (pattern.swift). For each CASE the client starts in its virtual room
# (`room=scene`) with the display's panel at a known width and distance (`display_place`). The script
# takes one screen capture. measure.swift reads the pattern in the left eye and prints the
# contrast of the lines and the smallest text whose letters stay apart. capture-check.py then keeps the
# capture in CAPTURES or deletes it.
#
# A capture counts only while the headset tracks its head (`head_pose` in scripts/client-start.zsh):
# with no frame of the host in the client's last second the script takes no capture, prints why and
# exits 2.
#
# Usage: scripts/sharpness/headset-check.sh SERIAL WORKDIR [CAPTURES] CASE...
#   CAPTURES  a folder for the captures that pass capture-check.py, or `-` to delete every capture
#   CASE      WIDTH,DISTANCE in meters, then `:PIXELS` for the widest picture to ask for, such as
#             1.6,1.5 (the panel's default size and distance) or 3.0,1.5:2560
# Environment: CRANVR_SHARPNESS_PORT (9961 by default). `cargo build --release -p cranvr-host` must
# have built the host. The client must be a build of scripts/build-client.sh from this tree. Hold the
# Quest lock. At the end the script puts the user's client back on 9950 (scripts/give-back.sh), also
# after a failure. CRANVR_GIVE_BACK_APK names a build that give-back.sh installs first.
set -u
zmodload zsh/datetime
serial=$1 work=$2 captures=$3
shift 3
cases=("$@")
root=${0:A:h:h:h}
here=${0:A:h}
port=${CRANVR_SHARPNESS_PORT:-9961}
adb_cmd=(adb -s ${serial})
source ${root}/scripts/client-start.zsh
export CLIENT_LINK_WAIT=${CLIENT_LINK_WAIT:-40}
mkdir -p ${work}/host ${work}/bin
host_log=${work}/host.out
host="" show=""
finish() {
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    [[ -n ${show} ]] && kill ${show} 2> /dev/null
    [[ -n ${host} ]] && kill ${host} 2> /dev/null
    wait 2> /dev/null
    ${adb_cmd} reverse --remove tcp:${port} > /dev/null 2>&1
    print "cut ends at the give-back $(date +%T)"
    ${root}/scripts/give-back.sh ${serial} ${CRANVR_GIVE_BACK_APK:-}
}
trap finish EXIT
trap 'exit 130' INT TERM

swift ${here}/pattern.swift ${work}/pattern > /dev/null || exit 1
for tool in show measure; do
    swiftc -O ${here}/${tool}.swift -o ${work}/bin/${tool} || exit 1
done
CRANVR_POINTER_DRY_RUN=1 CRANVR_DESKTOP_CURSOR_IN_VIDEO=0 ${root}/target/release/cranvr-host --status --fps 120 --mbps 200 \
    --native-port ${port} --config-dir ${work}/host --trust-all --tcp --loopback --foveation off --no-game-audio \
    --desktop --virtual-display 1920x1080 > ${host_log} 2>&1 &
host=$!
display=""
for i in {1..40}; do
    display=$(sed -nE 's/.*Virtual display ([0-9]+):.*/\1/p' ${host_log} | head -1)
    [[ -n ${display} ]] && break
    sleep 0.25
done
[[ -n ${display} ]] || { print -u2 "the host made no virtual display"; exit 1 }
${work}/bin/show ${work}/pattern/pattern.png ${display} > ${work}/show.out 2>&1 &
show=$!

# The headset changes its USB functions about 25 s after the client's first start, which ends the
# test port's link and drops its `adb reverse` (#227): the measured starts wait until 32 s after it
lan=$(ipconfig getifaddr en0 || ipconfig getifaddr en1)
forget=127.0.0.1:${port}${lan:+,${lan}:${port}}
common=(--es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room scene --ez save_choices false
    --ez default_places true --es shown display1 --ez console false --es forget_host_key ${forget})
print "cut starts $(date +%T)"
${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
${adb_cmd} logcat -c
client_start ${host_log} ${common}
warm=${EPOCHSECONDS}
print "warm start: ${client_result}"
# A headset with no head pose shows no capture of the app: the run ends here
sleep 3
head_pose || exit 2
while (( EPOCHSECONDS < warm + 32 )); do
    sleep 1
done

void=0
for case in ${cases}; do
    place=${case%%:*}
    limit=(); [[ ${case} == *:* ]] && limit=(--ei desktop_width ${case##*:})
    name=${${case//,/x}//:/-}
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    quest_ready
    ${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
    ${adb_cmd} logcat -c
    client_start ${host_log} ${common} --es display_place ${place} ${limit}
    [[ ${client_result} == alive ]] || { print "${case}: the client did not start (${client_result})"; void=1; continue }
    # The display's first pictures and the refresh 250 ms after them
    for i in {1..20}; do
        ${adb_cmd} logcat -d -s Cranpose 2> /dev/null | grep -qE "displays: 1 wished; in the last 5 s: [1-9]" && break
        sleep 0.5
    done
    sleep 1
    if ! head_pose; then
        void=1
        continue
    fi
    ${adb_cmd} exec-out screencap -p > ${work}/${name}.png
    print "${case}: captured at $(date +%T), the client received ${client_fps} frames a second"
    grep -E "Native display 1: capturing" ${host_log} | tail -1 | sed -E 's/.*capturing/  host: capturing/'
    if ${work}/bin/measure ${work}/${name}.png ${work}/pattern --eye left --json ${work}/${name}.json | sed 's/^/  /'; then
        :
    else
        print "  void: the capture holds no pattern"
        void=1
    fi
    python3 -I ${root}/scripts/capture-check.py ${work}/${name}.png ${captures:#-} | sed 's/^/  /'
done
exit $(( void ? 2 : 0 ))
