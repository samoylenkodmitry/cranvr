#!/bin/zsh
# Makes a short recording with the gem on the left hand (client/src/record_gem.rs) on a Quest on a
# desk, in the app's virtual room, and measures what the recording costs the stream. A test app with
# no window runs the host on PORT with its own folder, and the scripted build plays
# `scripted_pointer=gem` (client/src/scripted.rs): the right controller points away, at the gem,
# presses it, points away for 8 s while the recording runs and presses it again. The script
#   - reads the numbers of the recording that the two presses made (ffprobe) and deletes it;
#   - with GAME=1, where the host's fake game streams at 120 fps through the run, prints the
#     headset's frame rate and GPU times (VrApi) and the host's numbers (frame rate, pose to display)
#     in the 8 s before the first press and in the 6 s while the recording runs.
# scripts/gem-looks.sh checks the gem's looks.
#
# Usage: [GAME=1] scripts/gem-check.sh SERIAL WORKDIR [PORT]
#   PORT  the test app's port (9961 by default)
#
# `cargo build --release` must have built cranvr, cranvr-host and cranvr-fake-game,
# scripts/build-quest-recorder.sh the recorder, and the client must be a build of
# `scripts/build-client.sh --scripted-hand`. Hold the Quest lock. At the end the script stops a
# recording that still runs, deletes the headset's files that it made and puts the user's client
# back on 9950 (scripts/give-back.sh), also after a failure, unless CRANVR_NO_GIVE_BACK is set.
# CRANVR_GIVE_BACK_APK names a build that give-back.sh installs first.
set -u
serial=$1 work=$2
port=${3:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
source ${root}/scripts/client-start.zsh
export CLIENT_LINK_WAIT=${CLIENT_LINK_WAIT:-40}
home=${work}/host
records=${work}/recordings
mkdir -p ${home} ${records}
videos=/sdcard/Oculus/VideoShots
# The owner's stream settings: 200 Mbps with no foveation
print '{"fps": 120, "mbps": 200, "foveation_level": "Off", "desktop": false, "game_audio": true, "hand_controllers": false, "microphone": "Headset", "connection": "Automatic", "launch": {}}' > ${home}/app.json
app="" game=""
before=(${(f)"$(${adb_cmd} shell ls ${videos} 2> /dev/null | tr -d '\r')"})
finish() {
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    [[ -n ${app} ]] && kill ${app} 2> /dev/null
    [[ -n ${game} ]] && kill ${game} 2> /dev/null
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
lan=$(ipconfig getifaddr en0 || ipconfig getifaddr en1)
forget=127.0.0.1:${port}${lan:+,${lan}:${port}}
print "cut starts $(date +%T)"
CRANVR_HOME=${home} CRANVR_NATIVE_PORT=${port} CRANVR_TEST_TRUST_ALL=1 CRANVR_TEST_LOOPBACK=1 CRANVR_HOST_BIN=${root}/target/release/cranvr-host CRANVR_RECORDINGS=${records} \
    ${root}/target/release/cranvr --headless < /dev/null > ${work}/app.log 2>&1 &
app=$!
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
client_start ${host_log} --es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room scene --es theme amber --ez save_choices false \
    --es panels hidden --ez console false --es forget_host_key ${forget} --es scripted_pointer gem --ei gem_wait_ms 12000
print "client: ${client_result} ${client_linked}"
[[ ${client_result} == alive ]] || exit 1
wait_log "points away: the gem shows its idle look" 10 || print "no idle step"
sleep 3
mark_idle=$(date +%s)
wait_log "points at the gem" 14 || print "no pointing step"
mark_pointed=$(date +%s)
# The headset's log keeps a few seconds of lines while the recorder runs: the idle lines go out now
${adb_cmd} logcat -d -v epoch > ${work}/logcat-idle.txt
wait_log "points away while the recording runs" 10 || print "no recording step"
sleep 1
mark_recording=$(date +%s)
sleep 6
mark_end=$(date +%s)
wait_log "pulls its trigger again" 6 || print "no second press"
# The app saves the file in its folder after the stop
for i in {1..60}; do
    saved=$(print -l ${records}/cranvr-*.mp4(N) | head -1)
    [[ -n ${saved} ]] && break
    sleep 1
done
tail -4 ${home}/recordings.log 2> /dev/null | cut -c1-400
if [[ -n ${saved} ]]; then
    ffprobe -v error -count_frames -show_entries stream=codec_type,codec_name,width,height,avg_frame_rate,nb_read_frames,duration,sample_rate,channels -of compact ${saved}
    ffmpeg -hide_banner -nostats -i ${saved} -vn -af volumedetect -f null - 2>&1 | grep -E "mean_volume|max_volume"
    print "the recording ${saved:t} is deleted after its numbers"
    rm ${saved}
else
    print "no recording was saved"
fi
${adb_cmd} logcat -d -v epoch > ${work}/logcat.txt
print "cut ends $(date +%T)"
# The costs: the headset's VrApi lines and the host's lines in each window
for window in "idle ${mark_idle} ${mark_pointed}" "recording ${mark_recording} ${mark_end}"; do
    parts=(${=window})
    print "== ${parts[1]} ($(( parts[3] - parts[2] )) s)"
    log=${work}/logcat.txt
    [[ ${parts[1]} == idle ]] && log=${work}/logcat-idle.txt
    awk -v from=${parts[2]} -v to=${parts[3]} '$1 >= from && $1 < to' ${log} | grep -E 'VrApi.*FPS=' | python3 -I ${root}/scripts/vrapi-summary.py
    from=$(date -r ${parts[2]} +%T) to=$(date -r ${parts[3]} +%T)
    awk -v from=${from} -v to=${to} '$2 >= from && $2 < to' ${host_log} | grep -E "fps \([0-9]+ game frames\)|pose to display" | sed -E 's/.*INFO\] //' | cut -c1-160
done
