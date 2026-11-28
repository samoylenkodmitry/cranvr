#!/bin/zsh
# Logs the poses of the real controllers that lie awake and still on a desk, then checks the
# headset's models against them (scripts/desk-check.py).
#
# Usage: scripts/desk-check.sh SERIAL WORKDIR [SECONDS] [PORT]
#
# The Quest must run a build from `scripts/build-client.sh --scripted-hand`. The script starts a test
# host on PORT (9961 by default) with its own folder in WORKDIR, so the user's host, its files and its
# panel layout stay as they are. It starts the client against that host with log_poses, keeps the
# headset awake (prox_close), reads the log for SECONDS (20 by default) and stops the host.
# Hold the Quest lock. At the end the script puts the user's
# client back on 9950 (scripts/give-back.sh), also after a failure. CRANVR_GIVE_BACK_APK names a
# build that give-back.sh installs first, such as the normal build.
set -u
serial=$1 work=$2 seconds=${3:-20} port=${4:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
# The user's client goes back on its host at the end, also when the run fails or stops early
finish() {
    kill ${host:-} ${logcat:-} 2> /dev/null
    ${adb_cmd} shell am force-stop dev.cranvr.client
    ${root}/scripts/give-back.sh ${serial} ${CRANVR_GIVE_BACK_APK:-}
}
trap finish EXIT
trap 'exit 130' INT TERM
mkdir -p ${work}/config
${root}/target/release/cranvr-host --status --config-dir ${work}/config --native-port ${port} --trust-all --no-game-audio > ${work}/host.out 2> ${work}/host.log &
host=$!
${adb_cmd} logcat -v time -T 1 > ${work}/logcat.txt 2>&1 &
logcat=$!
${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null
${adb_cmd} shell am force-stop dev.cranvr.client
${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
${adb_cmd} shell am start -n dev.cranvr.client/dev.cranvr.client.AdbStart \
    --es host 127.0.0.1:${port} --ez connect true --ez log_poses true \
    --es forget_host_key 127.0.0.1:${port} > /dev/null
sleep ${seconds}
kill ${host} ${logcat} 2> /dev/null
wait 2> /dev/null
${adb_cmd} reverse --remove tcp:${port} > /dev/null
python3 ${root}/scripts/desk-check.py ${work}/logcat.txt
