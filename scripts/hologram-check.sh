#!/bin/zsh
# Checks the hologram form of the 3D interface on a Quest on a desk (#111). A test app with no window
# (`cranvr --headless`) answers on PORT and saves each change in its own app.json. The script:
#   - walks the Stream page as a hologram: the scripted right controller changes every choice and
#     toggle of the page (`scripted_pointer=hologram`, client/src/scripted.rs), and the script prints
#     the entries of the test app's app.json that changed and the client's lines of the walk;
#   - takes a screen capture of the Stream page and of the open hand menu in each theme, with the
#     client's virtual room and nothing in it (`room=none`). Each capture goes through
#     scripts/capture-check.py, which keeps it in CAPTURES only when it shows no room.
#
# Usage: [PARTS="walk captures"] [THEMES="cyan amber"] scripts/hologram-check.sh SERIAL WORKDIR [CAPTURES] [PORT]
#   CAPTURES where the captures that pass go (none are kept without it)
#   PORT     the test app's port (9961 by default)
#
# The client starts with `interface=3d`, `save_choices=false`, its own set of panels (`shown`) and
# `first_start`, so it saves nothing over the user's choices, layout or menu hint. The Quest must run
# a build from `scripts/build-client.sh --scripted-hand`, and `cargo build --release` must have built
# cranvr and cranvr-host. Hold the Quest lock. At the end the script puts the user's client back on
# 9950 (scripts/give-back.sh), also after a failure. CRANVR_GIVE_BACK_APK names a build that
# give-back.sh installs first, such as the normal build.
set -u
serial=$1 work=$2
captures=${3:-} port=${4:-9961}
parts=(${=PARTS:-walk captures})
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
source ${root}/scripts/client-start.zsh
finish() {
    kill ${app:-} ${logcat:-} 2> /dev/null
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    ${adb_cmd} reverse --remove tcp:${port} > /dev/null 2>&1
    ${root}/scripts/give-back.sh ${serial} ${CRANVR_GIVE_BACK_APK:-}
}
trap finish EXIT
trap 'exit 130' INT TERM
home=${work}/home
mkdir -p ${home}
# The entries that the walk changes stand in the file, so the check shows their change
print '{"fps": 120, "mbps": 100, "foveation_level": "Medium", "game_scale": 0.7, "game_msaa_limit": 2, "unscale_displays": true, "hand_controllers": true, "autostart": true, "desktop": false, "game_audio": false, "connection": "Automatic", "microphone": "Headset", "wine": {"mac_window": false}, "headset_room": "scene", "headset_volume": 150, "headset_interface": null, "headset_theme": null, "launch": {}}' > ${home}/app.json
cp ${home}/app.json ${work}/app.before.json
CRANVR_HOME=${home} CRANVR_NATIVE_PORT=${port} CRANVR_TEST_TRUST_ALL=1 CRANVR_HOST_BIN=${root}/target/release/cranvr-host \
    ${root}/target/release/cranvr --headless < /dev/null > ${work}/app.log 2>&1 &
app=$!
${adb_cmd} logcat -c
: > ${work}/logcat.txt
# The client's log. A change of the headset's USB functions ends the stream of adb, so a wait starts
# it again from the device's time.
listen() {
    [[ -n ${logcat:-} ]] && kill -0 ${logcat} 2> /dev/null && return
    local now=$(${adb_cmd} shell date "'+%m-%d %H:%M:%S.000'" 2> /dev/null | tr -d '\r')
    # The time is one argument of its own, as `-T` takes it
    local since=()
    [[ -n ${now} ]] && since=(-T "${now}")
    ${adb_cmd} logcat -v time ${since} >> ${work}/logcat.txt 2>&1 &
    logcat=$!
}
listen
# A change of the headset's USB functions drops the test port's reverse: set it while the client waits
client_waiting() {
    ${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
}
client_waiting
host_log=${home}/host.log
# A test app has a new key at each run. After a restart of its host the client finds it on the
# Mac's Wi-Fi address too, so the client forgets the keys of both addresses.
lan=$(ipconfig getifaddr en0 || ipconfig getifaddr en1)
forget=127.0.0.1:${port}${lan:+,${lan}:${port}}
# A client start after a restart of the test host can take more than 20 s to link
export CLIENT_LINK_WAIT=40
common=(--es room none --es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es interface 3d --ez default_places true --ez save_choices false --ez first_start true --es shown none --es forget_host_key ${forget})

# Starts the client with `extras` and waits for its link; returns 1 when the start did not hold
start() {
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    ${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
    client_start ${host_log} ${common} "$@"
    [[ ${client_result} == alive ]] && return 0
    print "the start did not hold (${client_result})"
    [[ -n ${client_crash} ]] && print -l -- "  "${(f)^client_crash}
    return 1
}

# Waits up to `seconds` for a client log line that matches `pattern`, counted from `from` lines on
wait_line() {
    local pattern=$1 seconds=$2 from=$3 i
    for i in {1..$((seconds * 5))}; do
        tail -n +${from} ${work}/logcat.txt | grep -q -- "${pattern}" && return 0
        listen
        sleep 0.2
    done
    return 1
}

# A capture named `name`, kept in CAPTURES only when it shows no room
capture() {
    ${adb_cmd} exec-out screencap -p > ${work}/${1}.png
    if [[ -n ${captures} ]]; then
        python3 -I ${root}/scripts/capture-check.py ${work}/${1}.png ${captures}
    else
        python3 -I ${root}/scripts/capture-check.py ${work}/${1}.png
    fi
}

if (( ${parts[(Ie)walk]} )); then
    from=$(($(wc -l < ${work}/logcat.txt) + 1))
    # The first press waits until the start's check of 3 s is over, as the press restarts the host
    if start --ez console true --es console_page stream --es theme cyan --es scripted_pointer hologram --ei walk_wait_ms 4000; then
        wait_line "scripted hologram: done" 120 ${from} || print "the walk did not end in 120 s"
    fi
fi
if (( ${parts[(Ie)captures]} )); then
    for theme in ${=THEMES:-cyan amber}; do
        from=$(($(wc -l < ${work}/logcat.txt) + 1))
        # The page stands a meter ahead, and the left controller stands still out of view, so no ball
        # for a look shows
        if start --ez console true --es console_page stream --es console_place ahead --es theme ${theme} --es scripted_pointer away; then
            # The page rises out of its projector in 420 ms; a system toast blacks the screen for a few
            # seconds after a start
            wait_line "the console shows the page stream" 10 ${from} && sleep 4
            capture hologram-stream-${theme}
        fi
        from=$(($(wc -l < ${work}/logcat.txt) + 1))
        # The menu rides the still left controller, and the console stays closed
        if start --ez console false --es hand_menu open --es theme ${theme} --es scripted_pointer holding; then
            wait_line "the hand menu opens" 10 ${from}
            sleep 4
            capture hologram-menu-${theme}
        fi
    done
fi
kill ${app} ${logcat} 2> /dev/null
wait 2> /dev/null
print "The test app's app.json, entries that changed:"
python3 -I - ${work}/app.before.json ${home}/app.json <<'EOF'
import json, sys
before, after = (json.load(open(path)) for path in sys.argv[1:3])
for key in sorted(set(before) | set(after)):
    if before.get(key) != after.get(key):
        print(f"  {key}: {json.dumps(before.get(key))} -> {json.dumps(after.get(key))}")
EOF
print "The client's walk:"
grep -E "scripted hologram:|the console shows|the hand menu (opens|closes)|the Mac sets|press ended as" ${work}/logcat.txt | sed -E 's/^[0-9-]+ //' | cut -c1-220
