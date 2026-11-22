#!/bin/zsh
# Checks the pages of the Mac's settings on a Quest on a desk (#110). The scripted right controller
# opens the first page of the Mac's settings from the hand menu, presses a change there, goes back to
# the main page, opens a game's Options and presses a change there too (`scripted_pointer=pages`,
# client/src/scripted.rs). A test app with no window (`cranvr --headless`) answers the requests and
# saves each change in its own app.json. The script prints the entries of that app.json that changed,
# and the client's lines of the walk.
#
# Usage: scripts/pages-check.sh SERIAL WORKDIR [SECONDS] [PORT]
#   SECONDS how long the run lasts (20 by default)
#   PORT    the test app's port (9961 by default)
#
# The Quest must run a build from `scripts/build-client.sh --scripted-hand`, and `cargo build
# --release` must have built cranvr and cranvr-host. The test app has its own folder in WORKDIR with a
# made-up game whose program is an empty file, no desktop and no game sound, so the user's app, its
# files and the Mac's displays stay as they are. The client reaches the test app through `adb
# reverse`, with its USB network off. Hold the Quest lock, start the user's client again afterwards
# and install the normal build.
set -u
serial=$1 work=$2
seconds=${3:-20} port=${4:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
home=${work}/home
program=${home}/wine/prefix/drive_c/Games/Fake/fake.exe
mkdir -p ${program:h}
: > ${program}
# The entries that the walk may change stand in the file, so the check shows their change. A save
# writes every other entry too, with its standard value.
print "{\"desktop\": false, \"game_audio\": false, \"game_msaa_limit\": 2, \"launch\": {}, \"added_games\": [{\"name\": \"Fake\", \"program\": \"${program}\"}]}" > ${home}/app.json
cp ${home}/app.json ${work}/app.before.json
CRANVR_HOME=${home} CRANVR_NATIVE_PORT=${port} CRANVR_TEST_TRUST_ALL=1 CRANVR_HOST_BIN=${root}/target/release/cranvr-host \
    ${root}/target/release/cranvr --headless > ${work}/app.log 2>&1 &
app=$!
${adb_cmd} logcat -c
${adb_cmd} logcat -v time > ${work}/logcat.txt 2>&1 &
logcat=$!
${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null
${adb_cmd} shell am force-stop dev.cranvr.client
${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
extras=(--es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room scene --ez default_places true --es scripted_pointer pages --es forget_host_key 127.0.0.1:${port})
${adb_cmd} shell am start -n dev.cranvr.client/dev.cranpose.android.CranposeActivity ${extras} > /dev/null
sleep ${seconds}
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
print(f"  ({len(set(after) - set(before))} entries more, which the save wrote with their standard values)")
EOF
print "The client's walk:"
grep -E "scripted pages:|the console asks for|the hand menu opens the console|the hand menu opens|a button opens the hand menu" ${work}/logcat.txt | sed -E 's/^[0-9-]+ //'
