#!/bin/zsh
# Measures what the 3D menus cost on a Quest on a desk (#111): the headset's frame rate, the app's
# GPU time, the compositor's time and the layer count from the runtime's VrApi line once a second,
# and the client's own count of layers. A test host on PORT streams its scene at 120 Hz, which fills
# the view as a game's stream does. Each case starts the client anew:
#   - the Flat and the 3D interface with the hand menu closed and open (`hand_menu=open`), with no
#     controller (`scripted_pointer=none`), as the first table of #111 had them;
#   - the closed menu of both interfaces with the left controller held still (`scripted_pointer=holding`).
#     While the stream runs with no panel, the beams hide and the model with them, so the 3D ball
#     stays flat. Before the client links, its own panels show, the model shows and the 3D ball rides
#     the controller;
#   - the closed menu of both interfaces with that controller while a game runs: the host's fake game
#     (`cranvr-fake-game`) sends frames through the runtime's protocol.
# A fourth part of a case names the panels that show, as `shown` takes them: with
# `flat:closed:holding:stats` the stats panel shows, so the beams and the model show while the stream
# runs. No panel of the user's shows.
#
# Usage: [CASES="flat:open:none ..."] scripts/ui3d-budget.sh SERIAL WORKDIR [SECONDS] [PORT]
#   SECONDS  the VrApi lines read of each case, 3 s after the client links (3 by default)
#   PORT     the test host's port (9961 by default)
#
# The client must be a build of `scripts/build-client.sh --scripted-hand`, and `cargo build --release`
# must have built cranvr-host and cranvr-fake-game. It starts with `save_choices=false`, its own set
# of panels (`shown`) and `first_start`, so it saves nothing of the user's. A case counts only when
# the client still runs 3 s after its link (scripts/client-start.zsh). Hold the Quest lock. At the
# end the script puts the user's client back on 9950 (scripts/give-back.sh), also after a failure.
# CRANVR_GIVE_BACK_APK names a build that give-back.sh installs first, such as the normal build.
set -u
serial=$1 work=$2
secs=${3:-3} port=${4:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
source ${root}/scripts/client-start.zsh
mkdir -p ${work}/host
${root}/target/release/cranvr-host --native-port ${port} --trust-all --fps 120 --config-dir ${work}/host --discovery-port 0 --no-game-audio --no-microphone --no-hand-controllers > ${work}/host.log 2>&1 &
host=$!
game=""
finish() {
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    [[ -n ${game} ]] && kill ${game} 2> /dev/null
    kill ${host} 2> /dev/null
    wait 2> /dev/null
    ${adb_cmd} reverse --remove tcp:${port} > /dev/null 2>&1
    ${root}/scripts/give-back.sh ${serial} ${CRANVR_GIVE_BACK_APK:-}
}
trap finish EXIT
trap 'exit 130' INT TERM
sleep 1
# A change of the headset's USB functions drops the test port's reverse: set it while the client waits
client_waiting() {
    ${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
}
client_waiting
# The first start after an install shows the system's notice for a few seconds, so a start that
# nobody measures comes first.
cases=(${=CASES:-flat:closed:none 3d:closed:none flat:open:none 3d:open:none flat:closed:holding 3d:closed:holding game:flat:holding game:3d:holding})
for case in warm:closed:none ${cases}; do
    parts=(${(s.:.)case})
    panels=none
    if [[ ${parts[1]} == game ]]; then
        interface=${parts[2]} menu=closed pointer=${parts[3]}
        if [[ -z ${game} ]]; then
            ${root}/target/release/cranvr-fake-game --control ${work}/host/runtime-control.txt --fps 120 --seconds 120 > ${work}/game.log 2>&1 &
            game=$!
            sleep 2
        fi
    else
        interface=${parts[1]} menu=${parts[2]} pointer=${parts[3]} panels=${parts[4]:-none}
    fi
    [[ ${interface} == warm ]] && interface=flat
    extras=(--es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room scene --es interface ${interface} --ez save_choices false --ez first_start true --es shown ${panels} --es forget_host_key 127.0.0.1:${port} --es scripted_pointer ${pointer})
    [[ ${menu} == open ]] && extras+=(--es hand_menu open)
    # A change of the headset's USB functions drops the test port's `adb reverse` and ends the link.
    # The client counts as started when it still runs 3 s after its link. A case runs once more when
    # its start did not hold or its link ended.
    for attempt in 1 2; do
        ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
        ${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
        ${adb_cmd} logcat -c
        client_start ${work}/host.log ${extras}
        ended=$(grep -c "Headset disconnected" ${work}/host.log)
        streamed=0
        if [[ ${client_result} == alive ]]; then
            sleep ${secs}
            (( $(grep -c "Headset disconnected" ${work}/host.log) == ended )) && streamed=1
        else
            print "the start did not hold (${client_result})"
            [[ -n ${client_crash} ]] && print -l -- "  "${(f)^client_crash}
        fi
        (( streamed )) && break
    done
    [[ ${parts[1]} == warm ]] && continue
    ${adb_cmd} logcat -d -v time > ${work}/logcat-${case//:/-}.txt
    print "== ${case}"
    grep -E 'VrApi.*FPS=' ${work}/logcat-${case//:/-}.txt | tail -n ${secs} | python3 -I ${root}/scripts/vrapi-summary.py
    grep -E 'the frame has' ${work}/logcat-${case//:/-}.txt | tail -1 | sed -E 's/.*cranpose xr: //'
    grep -E 'fps received' ${work}/logcat-${case//:/-}.txt | tail -1 | sed -E 's/.*connection: //' | cut -c1-90
    (( streamed )) || print "the stream did not run through the case"
done
print "The host's game frames while the fake game ran:"
grep -E '[0-9.]+ fps \([0-9]+ game frames\)' ${work}/host.log | tail -2 | sed -E 's/.*INFO\] //' | cut -c1-80
