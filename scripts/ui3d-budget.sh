#!/bin/zsh
# Measures what the 3D menus cost on a Quest on a desk (#111): the headset's frame rate, the app's
# GPU time, the compositor's time and the layer count from the runtime's VrApi line once a second,
# and the client's own count of layers. A test host on PORT streams its scene at 120 Hz, which fills
# the view as a game's stream does. The client runs four times: the Flat and the 3D interface, each
# with the hand menu closed and open (`hand_menu=open`). No panel shows besides the ball and the menu.
#
# Usage: scripts/ui3d-budget.sh SERIAL WORKDIR [SECONDS] [PORT]
#   SECONDS  the VrApi lines read of each run, after 6 s of start (6 by default)
#   PORT     the test host's port (9961 by default)
#
# The client must be a build of `scripts/build-client.sh`. It starts with `save_choices=false`, its
# own set of panels (`shown`) and `first_start`, so it saves nothing of the user's. Hold the Quest lock
# and start the user's client again afterwards.
set -u
serial=$1 work=$2
secs=${3:-6} port=${4:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
mkdir -p ${work}/host
${root}/target/release/cranvr-host --native-port ${port} --trust-all --fps 120 --config-dir ${work}/host --discovery-port 0 --no-cable --no-game-audio --no-microphone --no-hand-controllers > ${work}/host.log 2>&1 &
host=$!
sleep 1
${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null
for interface in flat 3d; do
    for menu in closed open; do
        extras=(--es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room scene --es interface ${interface} --ez save_choices false --ez first_start true --es shown none --es forget_host_key 127.0.0.1:${port})
        [[ ${menu} == open ]] && extras+=(--es hand_menu open)
        ${adb_cmd} shell am force-stop dev.cranvr.client
        ${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
        ${adb_cmd} logcat -c
        ${adb_cmd} shell am start -n dev.cranvr.client/dev.cranpose.android.CranposeActivity ${extras} > /dev/null
        sleep $((secs + 6))
        ${adb_cmd} logcat -d -v time > ${work}/logcat-${interface}-${menu}.txt
        print "== ${interface}, menu ${menu}"
        grep -E 'VrApi.*FPS=' ${work}/logcat-${interface}-${menu}.txt | tail -n ${secs} | python3 -I ${root}/scripts/vrapi-summary.py
        grep -E 'the frame has' ${work}/logcat-${interface}-${menu}.txt | tail -1 | sed -E 's/.*cranpose xr: //'
    done
done
${adb_cmd} shell am force-stop dev.cranvr.client
kill ${host}
wait 2> /dev/null
${adb_cmd} reverse --remove tcp:${port} > /dev/null 2>&1
