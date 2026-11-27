#!/bin/zsh
# Checks the gem on the left hand that records what the headset shows, on a Quest on a desk. A test
# app with no window runs the host on PORT with its own folder, and the scripted build plays
# `scripted_pointer=gem` (client/src/scripted.rs): the left controller holds the gem, and the right
# controller points away, at the gem, presses it, points away while the recording runs and presses it
# again. The script
#   - takes a screen capture of each look of the gem: idle, pointed at, recording and, after a new
#     start of the test app without adb (`CRANVR_ADB=none`), not possible. It measures the gem's
#     color in each one and keeps a capture only when capture-check.py passes it;
#   - reads the numbers of the recording that the two presses made (ffprobe) and deletes it.
#
# With GAME=1 the host's fake game streams at 120 fps through the run, and the script prints the
# headset's frame rate and GPU times and the host's numbers (frame rate, pose to display) before the
# first press and while the recording runs.
#
# Usage: [GAME=1] scripts/gem-check.sh SERIAL WORKDIR [CAPTURES] [ROOM] [PORT]
#   CAPTURES  the folder for the captures that pass (none by default: every capture is deleted)
#   ROOM      the client's room for this launch: none (the default) or scene. Use scene for a
#             recording whose picture counts: the headset's recorder shows the cameras' picture
#             behind a room of none.
#   PORT      the test host's port (9961 by default)
#
# `cargo build --release` must have built cranvr and cranvr-host, scripts/build-quest-recorder.sh the
# recorder, and the client must be a build of `scripts/build-client.sh --scripted-hand`. Hold the
# Quest lock. At the end the script puts the user's client back on 9950 (scripts/give-back.sh), also
# after a failure. CRANVR_GIVE_BACK_APK names a build that give-back.sh installs first.
set -u
serial=$1 work=$2
captures=${3:-} room=${4:-none} port=${5:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
source ${root}/scripts/client-start.zsh
export CLIENT_LINK_WAIT=${CLIENT_LINK_WAIT:-40}
home=${work}/host
records=${work}/recordings
mkdir -p ${home} ${records}
# The owner's stream settings: 200 Mbps with no foveation
print '{"fps": 120, "mbps": 200, "foveation_level": "Off", "desktop": false, "game_audio": true, "hand_controllers": false, "microphone": "Headset", "connection": "Automatic", "launch": {}}' > ${home}/app.json
app="" game=""
start_app() {
    CRANVR_HOME=${home} CRANVR_NATIVE_PORT=${port} CRANVR_TEST_TRUST_ALL=1 CRANVR_HOST_BIN=${root}/target/release/cranvr-host CRANVR_RECORDINGS=${records} \
        "$@" ${root}/target/release/cranvr --headless < /dev/null >> ${work}/app.log 2>&1 &
    app=$!
}
finish() {
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    [[ -n ${app} ]] && kill ${app} 2> /dev/null
    [[ -n ${game} ]] && kill ${game} 2> /dev/null
    wait 2> /dev/null
    ${adb_cmd} reverse --remove tcp:${port} > /dev/null 2>&1
    # A batch of checks gives the client back once, after its last check
    [[ -n ${CRANVR_NO_GIVE_BACK:-} ]] || ${root}/scripts/give-back.sh ${serial} ${CRANVR_GIVE_BACK_APK:-}
}
trap finish EXIT
trap 'exit 130' INT TERM
host_log=${home}/host.log
client_waiting() {
    ${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
}
# Waits up to $2 seconds for a line of the client's log that holds $1
wait_log() {
    local i
    for i in {1..$(( $2 * 4 ))}; do
        ${adb_cmd} logcat -d 2> /dev/null | grep -q -- "$1" && return 0
        sleep 0.25
    done
    return 1
}
# Takes a screen capture named $1 and prints the gem's color at the place `gem_at` holds. With a
# second argument `find`, the capture's green gem gives that place first.
capture() {
    ${adb_cmd} exec-out screencap -p > ${work}/$1.png
    if [[ ${2:-} == find ]]; then
        gem_at=$(python3 -I ${root}/scripts/gem-color.py --find ${work}/$1.png)
        print "the gem stands at ${gem_at:-an unknown place}"
    fi
    python3 -I ${root}/scripts/gem-color.py ${work}/$1.png ${gem_at:-} | tee -a ${work}/colors.txt
    if [[ -n ${captures} ]]; then
        python3 -I ${root}/scripts/capture-check.py ${work}/$1.png ${captures}
    else
        python3 -I ${root}/scripts/capture-check.py ${work}/$1.png
    fi
}
lan=$(ipconfig getifaddr en0 || ipconfig getifaddr en1)
forget=127.0.0.1:${port}${lan:+,${lan}:${port}}
print "cut starts $(date +%T)"
start_app
sleep 2
if [[ ${GAME:-0} == 1 ]]; then
    ${root}/target/release/cranvr-fake-game --control ${home}/runtime-control.txt --fps 120 --seconds 120 > ${work}/game.log 2>&1 &
    game=$!
    sleep 2
fi
client_waiting
${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
${adb_cmd} logcat -c
client_start ${host_log} --es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room ${room} --es theme amber --ez save_choices false \
    --es panels hidden --ez console false --es forget_host_key ${forget} --es scripted_pointer gem --ei gem_wait_ms 25000
print "client: ${client_result} ${client_linked}"
[[ ${client_result} == alive ]] || exit 1
wait_log "points away: the gem shows its idle look" 10 || print "no idle step"
sleep 5
mark_idle=$(date +%s)
sleep 4
# The idle capture waits for the place that the green of the pointed gem shows
${adb_cmd} exec-out screencap -p > ${work}/gem-idle-raw.png
wait_log "points at the gem" 20 || print "no pointing step"
sleep 2.5
capture gem-pointed find
mv ${work}/gem-idle-raw.png ${work}/gem-idle.png
python3 -I ${root}/scripts/gem-color.py ${work}/gem-idle.png ${gem_at:-} | tee -a ${work}/colors.txt
python3 -I ${root}/scripts/capture-check.py ${work}/gem-idle.png ${captures}
wait_log "points away while the recording runs" 10 || print "no recording step"
mark_recording=$(date +%s)
sleep 4
capture gem-recording
sleep 3
mark_end=$(date +%s)
wait_log "pulls its trigger again" 12 || print "no second press"
# The app saves the file in its folder after the stop
for i in {1..60}; do
    saved=$(print -l ${records}/cranvr-*.mp4(N) | head -1)
    [[ -n ${saved} ]] && break
    sleep 1
done
grep -E "Recording: " ${work}/app.log | tail -6
if [[ -n ${saved} ]]; then
    ffprobe -v error -count_frames -show_entries stream=codec_type,codec_name,codec_tag_string,width,height,avg_frame_rate,nb_read_frames,duration,sample_rate,channels -of compact ${saved}
    ffmpeg -v error -i ${saved} -vn -af volumedetect -f null - 2>&1 | grep -E "mean_volume|max_volume"
    print "the recording ${saved:t} is deleted after its numbers"
    rm ${saved}
else
    print "no recording was saved"
fi
# The costs: the headset's VrApi lines and the host's lines in the 4 s before the first press and in
# the 7 s while the recording runs
${adb_cmd} logcat -d -v epoch > ${work}/logcat.txt
for window in "idle ${mark_idle} $(( mark_idle + 4 ))" "recording ${mark_recording} ${mark_end}"; do
    parts=(${=window})
    print "== ${parts[1]}"
    awk -v from=${parts[2]} -v to=${parts[3]} '$1 >= from && $1 < to' ${work}/logcat.txt | grep -E 'VrApi.*FPS=' | python3 -I ${root}/scripts/vrapi-summary.py
done
grep -E "fps \([0-9]+ game frames\)|pose to display" ${host_log} | tail -10 | cut -c1-200
# A test app without adb: the gem shows that it cannot record
kill ${app} 2> /dev/null
wait ${app} 2> /dev/null
links=$(grep -c "Headset connected" ${host_log})
start_app env CRANVR_ADB=none
for i in {1..40}; do
    client_waiting
    (( $(grep -c "Headset connected" ${host_log}) > links )) && break
    sleep 1
done
sleep 4
capture gem-blocked
print "cut ends $(date +%T)"
