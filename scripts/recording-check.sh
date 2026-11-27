#!/bin/zsh
# Checks the paths of a recording of what the headset shows (app/src/recorder.rs) on a Quest on a
# desk, in the app's virtual room, with real game sound. A test app with no window runs the host on
# PORT with its own folder, on loopback alone (CRANVR_TEST_LOOPBACK), so macOS asks for no Local
# Network access. A test host has no System Audio Recording permission, so CRANVR_TEST_SOUND plays a
# game's sound in place of the tap, to the headset and to the recording alike (host/src/test_sound.rs):
# 30 s of silence after the app's start, then the files of SOUNDS one after another.
#
# The checks, in this order (CHECKS names some of them, as a comma list):
#   row      the hand menu's row "Record what I see": the scripted build presses the row's Record on
#            the menu in front of the user and 9 s later its Stop (`scripted_pointer=record_row`)
#   busy     a start while the headset's own recorder records refuses, and touches nothing of it
#   mic      the setting "Microphone in recordings" on, a recording of 5 s, the setting off
#   disk     a recording stops by itself when the free disk reads under 5 GB (CRANVR_TEST_FREE_DISK)
#   cable    the cable reads as pulled during a recording (CRANVR_TEST_CABLE_OUT): the app says so,
#            a press says so again, and the return of the cable saves the recording
#   term     SIGTERM to the app during a recording: the headset's recorder stops, the parts stay,
#            the next start of the app saves the recording
#   kill     SIGKILL to the app during a recording: the headset records on, the next start of the app
#            stops it and saves the recording
# Presses other than the row's come on the app's input, as the window's button and the headset's
# requests reach it. For each saved file the script prints the app's numbers (the sound's source,
# offset, the windows that matched, both sounds' loudness) and ffprobe's, then deletes the file. At
# the end it deletes every headset video that the run made and gives the client back.
#
# Usage: SOUNDS="FILE..." [CHECKS=row,busy,...] scripts/recording-check.sh SERIAL WORKDIR [PORT]
#   PORT  the test app's port (9962 by default)
#
# `cargo build --release` must have built cranvr and cranvr-host, scripts/build-quest-recorder.sh the
# recorder, and the client must be a build of `scripts/build-client.sh --scripted-hand`. Hold the
# Quest lock. CRANVR_GIVE_BACK_APK names the build that give-back.sh installs at the end;
# CRANVR_NO_GIVE_BACK skips the give-back.
set -u
serial=$1 work=$2
port=${3:-9962}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
source ${root}/scripts/client-start.zsh
export CLIENT_LINK_WAIT=${CLIENT_LINK_WAIT:-40}
checks=,${CHECKS:-row,busy,mic,disk,cable,term,kill},
home=${work}/host records=${work}/recordings
mkdir -p ${home} ${records}
videos=/sdcard/Oculus/VideoShots
host_log=${home}/host.log recordings_log=${home}/recordings.log
free_file=${work}/free-disk cable_file=${work}/cable-out
print 100000000000 > ${free_file}
rm -f ${cable_file}
print '{"fps": 120, "mbps": 200, "foveation_level": "High", "game_scale": 0.7, "desktop": false, "game_audio": true, "hand_controllers": false, "microphone": "Headset", "connection": "Automatic", "recording_microphone": false, "launch": {}}' > ${home}/app.json
# The game's sound: 30 s of silence from the app's start, then each file of SOUNDS
sound=${work}/test-sound.wav
inputs=(-f lavfi -i "anullsrc=r=48000:cl=stereo:d=1")
filters="" count=0
for file in ${(z)SOUNDS}; do
    file=${(Q)file}
    inputs+=(-i ${file})
    filters+="[$(( count + 1 )):a]aresample=48000,aformat=channel_layouts=stereo[s${count}];"
    count=$(( count + 1 ))
done
joined=$(for i in {0..$(( count - 1 ))}; do print -n "[s${i}]"; done)
ffmpeg -v error -y ${inputs} -filter_complex "${filters}${joined}concat=n=${count}:v=0:a=1" -c:a pcm_s16le ${sound} || exit 1
print "the test sound: $(ffprobe -v error -show_entries format=duration -of csv=p=0 ${sound}) s of ${count} files"
input=${work}/app-input
[[ -p ${input} ]] || mkfifo ${input}
app=""
headset_videos() {
    ${adb_cmd} shell "cd ${videos} 2> /dev/null && for f in *.mp4; do [ -e \"\$f\" ] && stat -c '%s %n' \"\$f\"; done" 2> /dev/null | tr -d '\r'
}
before=(${(f)"$(headset_videos | awk '{ print $2 }')"})
print "the headset holds ${#before} videos before the run"
finish() {
    exec 3>&- 2> /dev/null
    [[ -n ${app} ]] && kill ${app} 2> /dev/null
    wait 2> /dev/null
    pkill -TERM -f "${root}/target/release/cranvr-host --status.*--native-port ${port}"
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    # A recording that still runs stops, and the headset keeps none of the files that this run made
    ${adb_cmd} shell CLASSPATH=/data/local/tmp/cranvr-quest-recorder.jar app_process / dev.cranvr.recorder.Main stop > /dev/null 2>&1
    local file
    for file in ${(f)"$(headset_videos | awk '{ print $2 }')"}; do
        [[ -n ${file} && ${before[(Ie)${file}]} == 0 ]] || continue
        print "the headset's file ${file} of this run is deleted"
        ${adb_cmd} shell rm ${videos}/${file}
    done
    for file in ${records}/cranvr-*.mp4(N); do
        print "the recording ${file:t} is deleted"
        rm ${file}
    done
    ${adb_cmd} reverse --remove tcp:${port} > /dev/null 2>&1
    print "cut ends $(date +%T)"
    [[ -n ${CRANVR_NO_GIVE_BACK:-} ]] || ${root}/scripts/give-back.sh ${serial} ${CRANVR_GIVE_BACK_APK:-}
}
trap finish EXIT
trap 'exit 130' INT TERM
start_app() {
    CRANVR_HOME=${home} CRANVR_NATIVE_PORT=${port} CRANVR_TEST_TRUST_ALL=1 CRANVR_TEST_LOOPBACK=1 \
        CRANVR_HOST_BIN=${root}/target/release/cranvr-host CRANVR_RECORDINGS=${records} \
        CRANVR_TEST_SOUND=${sound}@${sound_start} CRANVR_TEST_FREE_DISK=${free_file} CRANVR_TEST_CABLE_OUT=${cable_file} \
        ${root}/target/release/cranvr --headless < ${input} >> ${work}/app.log 2>&1 &
    app=$!
    exec 3> ${input}
    print "the test app ${app} starts at $(date +%T)"
}
press() {
    print -u 3 '{"action": "run", "id": "recording.toggle"}'
    print "press at $(date +%T)"
}
# Waits up to $2 seconds for a new line of recordings.log that holds $1, after line $3
wait_line() {
    local i
    for i in {1..$(( $2 * 4 ))}; do
        tail -n +$(( $3 + 1 )) ${recordings_log} 2> /dev/null | grep -q -- "$1" && return 0
        sleep 0.25
    done
    print "no line \"$1\" in $2 s"
    return 1
}
lines() {
    [[ -f ${recordings_log} ]] && wc -l < ${recordings_log} | tr -d ' ' || print 0
}
# The numbers of the last saved file, then the file goes
report() {
    local saved=$(print -l ${records}/cranvr-*.mp4(N) | head -1)
    grep "saved ${records}" ${recordings_log} | tail -1 | sed -E 's/^[0-9-]+ saved [^ ]+ //' | python3 -I -c '
import json, sys
n = json.loads(sys.stdin.read().strip()[1:-1])
s = n.get("sound", {})
print("  sound:", s.get("source"), "offset", s.get("offset_s"), "found by", json.dumps(s.get("found_by")), "drift", s.get("drift_ppm"), "ppm; why", s.get("why"))
print("  mac", json.dumps(s.get("mac")), "headset", json.dumps(s.get("headset")), "from the headset", s.get("from_headset_s"), "s; microphone", json.dumps(n.get("microphone")))
print("  picture", n.get("width"), "x", n.get("height"), "at", n.get("fps"), "fps,", n.get("seconds"), "s; tap", json.dumps(n.get("tap")), "; checked", n.get("checked"))'
    if [[ -n ${saved} ]]; then
        ffprobe -v error -count_frames -show_entries stream=codec_type,codec_name,width,height,avg_frame_rate,nb_read_frames,duration,sample_rate,channels -of compact ${saved} | sed 's/^/  /'
        print "  the recording ${saved:t} is deleted after its numbers"
        rm ${saved}
    else
        print "  no recording was saved"
    fi
}
# Whether the headset's recorder writes a file now: the file's boxes hold no mdat yet (recorder.rs)
headset_grows() {
    local line
    line=$(${adb_cmd} shell "cd ${videos} 2> /dev/null && for f in *.mp4; do [ -e \"\$f\" ] && head -c 48 \"\$f\" | od -An -tx1 | tr -d ' \\n' && echo; done" 2> /dev/null | tr -d '\r' | grep -v '^$' | grep -cv '6d646174')
    (( line > 0 )) && print yes || print no
}
lan=$(ipconfig getifaddr en0 || ipconfig getifaddr en1)
forget=127.0.0.1:${port}${lan:+,${lan}:${port}}
sound_start=$(( $(date +%s) * 1000 + 30000 ))
print "cut starts $(date +%T)"
start_app
sleep 2
client_waiting() {
    ${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
}
client_waiting
${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
${adb_cmd} logcat -c
scripted=()
[[ ${checks} == *,row,* ]] && scripted=(--es scripted_pointer record_row --ei record_ms 9000)
client_start ${host_log} --es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room scene --es theme amber --ez save_choices false \
    --es panels hidden --ez console false --es interface flat --es hand_menu open --ez default_places true --es forget_host_key ${forget} ${scripted}
print "client: ${client_result} ${client_linked}"
[[ ${client_result} == alive ]] || exit 1

if [[ ${checks} == *,row,* ]]; then
    print "== row: the hand menu's row"
    at=$(lines)
    wait_line "started" 40 ${at} && wait_line "saved " 40 ${at}
    ${adb_cmd} logcat -d -v time > ${work}/client-row.log
    grep -E "scripted record row|hand menu|the Mac's recording" ${work}/client-row.log | sed -E 's/^[0-9-]+ ([0-9:.]+) .*cranvr_client::[a-z_:]+: /  \1 /' | cut -c1-220
    report
fi
if [[ ${checks} == *,busy,* ]]; then
    print "== busy: a start while the headset's own recorder records"
    ${adb_cmd} shell CLASSPATH=/data/local/tmp/cranvr-quest-recorder.jar app_process / dev.cranvr.recorder.Main start
    sleep 1
    at=$(lines)
    press
    wait_line "records already" 10 ${at}
    tail -n +$(( at + 1 )) ${recordings_log} | sed 's/^/  /'
    ${adb_cmd} shell CLASSPATH=/data/local/tmp/cranvr-quest-recorder.jar app_process / dev.cranvr.recorder.Main stop
    sleep 2
    print "  the headset's own recording stays until the run's end, which deletes it"
fi
if [[ ${checks} == *,mic,* ]]; then
    print "== mic: the microphone in a recording"
    print -u 3 '{"action": "set", "id": "recording_microphone", "value": "on"}'
    sleep 0.5
    at=$(lines)
    press
    wait_line "started" 15 ${at} && sleep 5 && press && wait_line "saved " 40 ${at}
    report
    print -u 3 '{"action": "set", "id": "recording_microphone", "value": "off"}'
fi
if [[ ${checks} == *,disk,* ]]; then
    print "== disk: the free disk falls under 5 GB"
    at=$(lines)
    press
    wait_line "started" 15 ${at} && sleep 3
    print 4500000000 > ${free_file}
    print "  the free disk reads 4.5 GB from $(date +%T)"
    wait_line "free: the recording stops" 8 ${at} && wait_line "saved " 40 ${at}
    print 100000000000 > ${free_file}
    tail -n +$(( at + 1 )) ${recordings_log} | grep -v "saved /" | sed 's/^/  /'
    report
fi
if [[ ${checks} == *,cable,* ]]; then
    print "== cable: the cable reads as pulled during a recording"
    at=$(lines)
    press
    wait_line "started" 15 ${at} && sleep 2
    touch ${cable_file}
    print "  the cable reads as out from $(date +%T)"
    wait_line "The cable is out" 12 ${at}
    print "  the headset records on: $(headset_grows)"
    press
    sleep 1
    rm -f ${cable_file}
    print "  the cable reads as back from $(date +%T)"
    wait_line "The cable is back" 8 ${at} && wait_line "saved " 40 ${at}
    tail -n +$(( at + 1 )) ${recordings_log} | grep -v "saved /" | sed 's/^/  /'
    report
fi
if [[ ${checks} == *,term,* ]]; then
    print "== term: SIGTERM to the app during a recording"
    at=$(lines)
    press
    wait_line "started" 15 ${at} && sleep 4
    kill -TERM ${app}
    print "  SIGTERM at $(date +%T)"
    wait ${app} 2> /dev/null
    print "  the app ended at $(date +%T); the headset records on: $(headset_grows)"
    print "  the parts: $(ls -A ${records} | tr '\n' ' ')"
    start_app
    wait_line "saved " 40 ${at}
    tail -n +$(( at + 1 )) ${recordings_log} | grep -v "saved /" | sed 's/^/  /'
    report
fi
if [[ ${checks} == *,kill,* ]]; then
    print "== kill: SIGKILL to the app during a recording"
    at=$(lines)
    sleep 3
    press
    wait_line "started" 15 ${at} && sleep 4
    kill -KILL ${app}
    print "  SIGKILL at $(date +%T)"
    wait ${app} 2> /dev/null
    # The host of a killed app runs on with no app: this run's own host goes
    pkill -TERM -f "${root}/target/release/cranvr-host --status.*--native-port ${port}"
    sleep 2
    print "  the headset records on: $(headset_grows)"
    start_app
    wait_line "saved " 40 ${at}
    tail -n +$(( at + 1 )) ${recordings_log} | grep -v "saved /" | sed 's/^/  /'
    report
fi
