#!/bin/zsh
# Checks the direct touch of a tracked hand on a Quest on a desk: the tip of the index finger presses
# the panels with no pinch (Cranpose's `PointerSettings::direct_touch`). A test app with no window
# (`cranvr --headless`) answers on PORT with its own folder, app.json and recordings. Its host shows
# the Mac's main display and only logs the mouse events of the pointer (CRANVR_POINTER_DRY_RUN), so
# nothing clicks on the Mac. The scripted build plays `scripted_point=touch` (client/src/scripted.rs)
# in the app's virtual room: the right fingertip touches a hologram object of the console's Stream
# page and the object that puts the value back, the hand menu's ball twice, the gem that records
# twice and the display's panel. The script then prints
#   - the entries of the test app's app.json that changed, which must be none after the way back,
#     and the client's lines of the walk, with each value that the menu showed after a touch;
#   - the pixels of screen captures in the left eye (scripts/touch-colors.py): the hover and the
#     changed object of the console, the open menu against the hover over the ball, the green gem of
#     the hover and the red gem of the recording;
#   - the host's lines of the recording and of the display's dry-run clicks.
# Each capture is deleted once its numbers are out (scripts/capture-check.py keeps none).
#
# Usage: scripts/touch-check.sh SERIAL WORKDIR [PORT]
#   PORT  the test app's port (9961 by default)
#
# `cargo build --release` must have built cranvr and cranvr-host, scripts/build-quest-recorder.sh
# the recorder, and the client must be a build of `scripts/build-client.sh --scripted-hand`. Hold
# the Quest lock. A headset on a desk loses its place in the room, and Guardian's dialog then holds
# every start of the app, so the script pauses Guardian for the run (scripts/guardian.sh). At the
# end it stops a recording that still runs, deletes the headset's files that it made, puts the
# user's client back on 9950 (scripts/give-back.sh) and sets Guardian's exact old value back, also
# after a failure. CRANVR_GIVE_BACK_APK names a build that give-back.sh installs first, such as the
# normal build.
set -u
serial=$1 work=$2
port=${3:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
source ${root}/scripts/client-start.zsh
source ${root}/scripts/guardian.sh
export CLIENT_LINK_WAIT=${CLIENT_LINK_WAIT:-40}
home=${work}/home
records=${work}/recordings
mkdir -p ${home} ${records}
videos=/sdcard/Oculus/VideoShots
before=(${(f)"$(${adb_cmd} shell ls ${videos} 2> /dev/null | tr -d '\r')"})
app="" logcat=""
finish() {
    [[ -n ${logcat} ]] && kill ${logcat} 2> /dev/null
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    [[ -n ${app} ]] && kill ${app} 2> /dev/null
    wait 2> /dev/null
    # A recording that still runs stops, and the headset keeps none of the files that this run made
    ${adb_cmd} shell CLASSPATH=/data/local/tmp/cranvr-quest-recorder.jar app_process / dev.cranvr.recorder.Main stop > /dev/null 2>&1
    local file
    for file in ${(f)"$(${adb_cmd} shell ls ${videos} 2> /dev/null | tr -d '\r')"}; do
        [[ ${file} == dev.cranvr.client-*.mp4 && ${before[(Ie)${file}]} == 0 ]] || continue
        print "the headset's file ${file} is deleted"
        ${adb_cmd} shell rm ${videos}/${file}
    done
    ${adb_cmd} reverse --remove tcp:${port} > /dev/null 2>&1
    print "cut ends $(date +%T)"
    ${root}/scripts/give-back.sh ${serial} ${CRANVR_GIVE_BACK_APK:-}
    guardian_restore
}
trap finish EXIT
guardian_traps
guardian_pause ${adb_cmd} || { print -u2 "Cannot read Guardian's value on the Quest"; exit 1; }
# The entries that the walk changes stand in the file, so the check shows any change that stayed
print '{"fps": 120, "mbps": 100, "foveation_level": "Medium", "game_scale": 0.7, "game_msaa_limit": 2, "unscale_displays": true, "hand_controllers": true, "autostart": true, "desktop": true, "game_audio": false, "connection": "Automatic", "microphone": "Headset", "wine": {"mac_window": false}, "headset_room": "scene", "headset_volume": 150, "headset_interface": null, "headset_theme": null, "launch": {}}' > ${home}/app.json
cp ${home}/app.json ${work}/app.before.json
print "cut starts $(date +%T)"
CRANVR_HOME=${home} CRANVR_RECORDINGS=${records} CRANVR_NATIVE_PORT=${port} CRANVR_TEST_TRUST_ALL=1 CRANVR_TEST_LOOPBACK=1 \
    CRANVR_POINTER_DRY_RUN=1 CRANVR_HOST_BIN=${root}/target/release/cranvr-host \
    ${root}/target/release/cranvr --headless < /dev/null > ${work}/app.log 2>&1 &
app=$!
${adb_cmd} logcat -c
: > ${work}/logcat.txt
# The client's log. A change of the headset's USB functions ends the stream of adb, so a wait starts
# it again from the device's time.
listen() {
    [[ -n ${logcat} ]] && kill -0 ${logcat} 2> /dev/null && return
    local now=$(${adb_cmd} shell date "'+%m-%d %H:%M:%S.000'" 2> /dev/null | tr -d '\r')
    local since=()
    [[ -n ${now} ]] && since=(-T "${now}")
    ${adb_cmd} logcat -v time ${since} >> ${work}/logcat.txt 2>&1 &
    logcat=$!
}
listen
client_waiting() {
    ${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
}
client_waiting
host_log=${home}/host.log
lan=$(ipconfig getifaddr en0 || ipconfig getifaddr en1)
forget=127.0.0.1:${port}${lan:+,${lan}:${port}}
# Waits up to `seconds` for a client log line that holds `text`
wait_line() {
    local text=$1 seconds=$2 i
    for i in {1..$((seconds * 5))}; do
        grep -q -- "${text}" ${work}/logcat.txt && return 0
        listen
        sleep 0.2
    done
    print "no line \"${text}\" in ${seconds} s"
    return 1
}
# A capture named $1, taken now
shot() {
    quest_ready
    ${adb_cmd} exec-out screencap -p > ${work}/$1.png
    print -P "$1 taken at %D{%H:%M:%S.%.}"
}
# The pixels of the captures $@ (scripts/touch-colors.py), and then each capture is deleted
measure() {
    python3 -I ${root}/scripts/touch-colors.py ${work}/$^@.png
    local name
    for name in "$@"; do
        python3 -I ${root}/scripts/capture-check.py ${work}/${name}.png > /dev/null
    done
}
${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
client_start ${host_log} --es room scene --es host 127.0.0.1:${port} --ez connect true --ez usb_network false \
    --es interface 3d --ez default_places true --ez save_choices false --ez first_start true --es forget_host_key ${forget} \
    --ez console true --es console_page stream --es hand_anchor view --es shown display1 --es scripted_point touch
print "client: ${client_result} ${client_linked:-}"
[[ ${client_result} == alive ]] || exit 1
step="the fingertip hovers 3 cm in front of"
wait_line "${step} the console's change" 60 && sleep 0.5 && shot console-hover
wait_line "touches the console's change" 5 && sleep 1.2 && shot console-changed
measure console-hover console-changed
wait_line "${step} the ball, which opens" 15 && sleep 0.5 && shot ball-hover
wait_line "touches the ball, which opens" 5 && sleep 1.2 && shot menu-open
measure ball-hover menu-open
wait_line "${step} the gem, which starts" 15 && sleep 0.5 && shot gem-hover
wait_line "touches the gem, which starts" 5 && sleep 2 && shot gem-recording
measure gem-hover gem-recording
wait_line "scripted touch: done" 30
sleep 1
kill ${logcat} 2> /dev/null
logcat=""
print "The test app's app.json, entries that changed:"
python3 -I - ${work}/app.before.json ${home}/app.json <<'EOF'
import json, sys
before, after = (json.load(open(path)) for path in sys.argv[1:3])
changed = [key for key in sorted(set(before) | set(after)) if before.get(key) != after.get(key)]
for key in changed:
    print(f"  {key}: {json.dumps(before.get(key))} -> {json.dumps(after.get(key))}")
print("  none" if not changed else "")
EOF
print "The client's walk:"
grep -E "scripted touch:|the hand menu (opens|closes)|the gem (starts|stops|cannot)|the gem's look|pointer (enters|presses) (the console|the hand ball|the gem)|a panel holds back" ${work}/logcat.txt | sed -E 's/^[0-9-]+ //' | cut -c1-240
print "The host:"
grep -E "Desktop \\(dry run\\)|recording runs|Desktop: capturing" ${host_log} | grep -v "mouse moved" | head -20
grep -E "[Rr]ecord|saved" ${work}/app.log | tail -8
print "Recordings of the test app:"
ls -la ${records}
