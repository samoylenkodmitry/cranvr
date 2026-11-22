#!/bin/zsh
# Checks the hand menu on a Quest on a desk (#119): where it stands for the controllers that a user
# has, that a button and the Mac open it, and what the app composes while it is closed. It reads the
# client's log and prints numbers (scripts/menu-check.py).
#
# Usage: scripts/menu-check.sh SERIAL WORKDIR CASE [SECONDS] [PORT]
#   CASE    right, left or both: the scripted controllers of `scripted_pointer=menu` (client/src/scripted.rs).
#           palm: the scripted hands of `scripted_pointer=palm`.
#           gaze: no controller and no hand (`scripted_pointer=none`), with the ball straight ahead.
#           ball: the scripted right hand of `scripted_point=ball` pinches the ball of `hand_anchor=view`.
#           mac: no scripted press; the run writes `menu open` to the host's input after 8 s.
#   SECONDS how long the run reads the log (18 by default, 22 for both)
#   PORT    the test host's port (9961 by default)
#
# The Quest must run a build from `scripts/build-client.sh --scripted-hand` (any build for mac). The
# script starts a test host with its own folder in WORKDIR and the standard mapping, so the user's
# host, its files and its mapping stay as they are. The client starts with `default_places` and
# `first_start`, so it ignores and keeps no carried place and saves no state of the menu's hint. It
# reaches the host through `adb reverse`, with its USB network off. Hold the Quest lock, start the
# user's client again afterwards and install the normal build.
set -u
serial=$1 work=$2 case=$3
default_seconds=18
[[ ${case} == both ]] && default_seconds=22
seconds=${4:-${default_seconds}} port=${5:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
mkdir -p ${work}/config
# The standard mapping: a menu with no game and no entry in `controls`
print '{"games": [], "fps": 120, "game_scale": 1.0, "msaa": 2, "controls": {}}' > ${work}/config/vr-menu.json
rm -f ${work}/input
mkfifo ${work}/input
# The host's input stays open for the run, so the host reads each line that the run writes
exec 3<>${work}/input
${root}/target/release/cranvr-host --status --config-dir ${work}/config --native-port ${port} --trust-all --no-game-audio --no-microphone < ${work}/input > ${work}/host.out 2> ${work}/host.log &
host=$!
${adb_cmd} logcat -c
${adb_cmd} logcat -v time > ${work}/logcat.txt 2>&1 &
logcat=$!
${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null
${adb_cmd} shell am force-stop dev.cranvr.client
${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
extras=(--es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room scene --ez default_places true --ez first_start true --ez log_poses true --es forget_host_key 127.0.0.1:${port})
case ${case} in
    mac) ;;
    palm) extras+=(--es scripted_pointer palm) ;;
    gaze) extras+=(--es scripted_pointer none --es gaze_pitch 0) ;;
    ball) extras+=(--es scripted_point ball --es hand_anchor view) ;;
    *) extras+=(--es scripted_pointer menu --es scripted_hands ${case}) ;;
esac
${adb_cmd} shell am start -n dev.cranvr.client/dev.cranpose.android.CranposeActivity ${extras} > /dev/null
if [[ ${case} == mac ]]; then
    sleep 8
    print "menu open" >&3
    sleep $(( seconds - 8 ))
else
    sleep ${seconds}
fi
kill ${host} ${logcat} 2> /dev/null
wait 2> /dev/null
exec 3>&-
${adb_cmd} reverse --remove tcp:${port} > /dev/null
python3 ${root}/scripts/menu-check.py ${work}/logcat.txt ${work}/host.log
