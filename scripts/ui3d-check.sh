#!/bin/zsh
# Checks the 3D menus on a Quest on a desk (#111). The scripted right controller presses the 3D ball,
# which opens the hand menu, and the menu's key Stream, which opens the console there. It presses a
# 3D key and flips a 3D switch on that page, then drags a 3D slider on another page
# (`scripted_pointer=ui3d`, client/src/scripted.rs). A test app with no window (`cranvr --headless`)
# answers the requests and saves each change in its own app.json. The script prints the entries of
# that app.json that changed, the client's lines of the walk, and takes a screen capture while the
# menu is open, while the right controller's model stands in front of the open menu, and while the
# console shows each page. The client starts with `room=none`: its virtual room with nothing behind
# the panels, so the captures are black around them. Each capture goes through
# scripts/capture-check.py, which keeps it in CAPTURES only when it shows no room.
#
# Usage: scripts/ui3d-check.sh SERIAL WORKDIR [CAPTURES] [SECONDS] [PORT]
#   CAPTURES where the captures that pass go (none are kept without it)
#   SECONDS  how long the run lasts (32 by default; the walk takes about 29 s after the link)
#   PORT     the test app's port (9961 by default)
#
# The client starts with `interface=3d`, `save_choices=false`, its own set of panels (`shown`) and
# `first_start`, so it saves nothing over the user's choices, layout or menu hint. The Quest must run a
# build from `scripts/build-client.sh --scripted-hand`, and `cargo build --release` must have built
# cranvr and cranvr-host. Hold the Quest lock. At the end the script puts the user's
# client back on 9950 (scripts/give-back.sh), also after a failure. CRANVR_GIVE_BACK_APK names a
# build that give-back.sh installs first, such as the normal build.
set -u
serial=$1 work=$2
captures=${3:-} seconds=${4:-32} port=${5:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
# The user's client goes back on its host at the end, also when the run fails or stops early
finish() {
    kill ${app:-} ${logcat:-} 2> /dev/null
    ${adb_cmd} shell am force-stop dev.cranvr.client
    ${root}/scripts/give-back.sh ${serial} ${CRANVR_GIVE_BACK_APK:-}
}
trap finish EXIT
trap 'exit 130' INT TERM
home=${work}/home
mkdir -p ${home}
# The entries that the walk changes stand in the file, so the check shows their change
print '{"desktop": false, "game_audio": false, "game_msaa_limit": 2, "autostart": true, "connection": "Automatic", "microphone": "Headset", "wine": {"mac_window": false}, "headset_room": "scene", "headset_volume": 150, "headset_interface": null, "launch": {}}' > ${home}/app.json
cp ${home}/app.json ${work}/app.before.json
CRANVR_HOME=${home} CRANVR_NATIVE_PORT=${port} CRANVR_TEST_TRUST_ALL=1 CRANVR_HOST_BIN=${root}/target/release/cranvr-host \
    ${root}/target/release/cranvr --headless < /dev/null > ${work}/app.log 2>&1 &
app=$!
${adb_cmd} logcat -c
${adb_cmd} logcat -v time > ${work}/logcat.txt 2>&1 &
logcat=$!
${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null
${adb_cmd} shell am force-stop dev.cranvr.client
${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
extras=(--es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room none --es interface 3d --ei walk_wait_ms 5000 --ez log_poses true --ez default_places true --ez save_choices false --ez first_start true --es shown none --es scripted_pointer ui3d --es forget_host_key 127.0.0.1:${port})
${adb_cmd} shell am start -n dev.cranvr.client/dev.cranpose.android.CranposeActivity ${extras} > /dev/null
# A capture a moment after each of these lines: the menu has shown, the console has shown its page
marks=("the hand menu opens:0.7:menu" "the hand menu opens:0.6:front" "the console shows the page stream:1.4:stream" "the console shows the page headset:1.4:headset")
end=$((SECONDS + seconds))
taken=()
while (( SECONDS < end )); do
    for mark in ${marks}; do
        line=${mark%%:*} rest=${mark#*:}
        wait_s=${rest%%:*} name=${rest#*:}
        if [[ ${taken[(Ie)$name]} -eq 0 ]] && grep -q "${line}" ${work}/logcat.txt; then
            taken+=(${name})
            sleep ${wait_s}
            ${adb_cmd} exec-out screencap -p > ${work}/ui3d-${name}.png
            if [[ -n ${captures} ]]; then
                python3 -I ${root}/scripts/capture-check.py ${work}/ui3d-${name}.png ${captures}
            else
                python3 -I ${root}/scripts/capture-check.py ${work}/ui3d-${name}.png
            fi
        fi
    done
    sleep 0.2
done
kill ${app} ${logcat} 2> /dev/null
wait 2> /dev/null
${adb_cmd} reverse --remove tcp:${port} > /dev/null 2>&1
print "The test app's app.json, entries that changed:"
python3 -I - ${work}/app.before.json ${home}/app.json <<'EOF'
import json, sys
before, after = (json.load(open(path)) for path in sys.argv[1:3])
for key in sorted(before):
    if before.get(key) != after.get(key):
        print(f"  {key}: {json.dumps(before.get(key))} -> {json.dumps(after.get(key))}")
EOF
print "The client's walk:"
grep -E "scripted ui3d:|the console asks for|the console shows|the hand menu (opens|closes)|the Mac sets|the user picks|press ended as|the frame needs" ${work}/logcat.txt | sed -E 's/^[0-9-]+ //' | cut -c1-220
