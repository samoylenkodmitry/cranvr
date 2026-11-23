#!/bin/zsh
# Counts crashes per start of the headset client against a test app. Each start begins from a
# stopped client and counts as held when the client still runs 3 s after its link
# (scripts/client-start.zsh). A crash counts when the device's crash log holds a fatal signal of
# the client; the script prints its top frames. A start that never linked counts apart.
#
# Usage: scripts/start-check.sh SERIAL WORKDIR STARTS [STOP_AFTER] [PORT]
#   STARTS      the most starts
#   STOP_AFTER  stop once this many crashes came (default: 0, which runs every start)
#   PORT        the test app's port (9961 by default)
#
# The client shows what the user's start shows: the 3D interface with the connection panel (the
# console's tray), the stats and the first display. It starts with `save_choices=false`, its own set
# of panels (`shown`), `default_places` and `first_start`, so it saves nothing over the user's
# choices, layout, places or menu hint. It reaches the test app through `adb reverse` with its USB
# network off. A test app with no window (`cranvr --headless`, from `cargo build --release`) has its
# own home in WORKDIR, so the user's app and its files stay as they are. Hold the Quest lock. At the
# end the script puts the user's client back on 9950 (scripts/give-back.sh), also after a failure.
# CRANVR_GIVE_BACK_APK names a build that give-back.sh installs first, such as the normal build.
set -u
serial=$1 work=$2 starts=$3
stop_after=${4:-0} port=${5:-9961}
root=${0:A:h:h}
adb_cmd=(adb -s ${serial})
source ${root}/scripts/client-start.zsh
home=${work}/home
mkdir -p ${home}
print '{"desktop": false, "game_audio": false, "autostart": true, "connection": "Automatic", "microphone": "Headset", "wine": {"mac_window": false}, "headset_room": "passthrough", "headset_interface": "3d", "launch": {}}' > ${home}/app.json
CRANVR_HOME=${home} CRANVR_NATIVE_PORT=${port} CRANVR_TEST_TRUST_ALL=1 CRANVR_HOST_BIN=${root}/target/release/cranvr-host \
    ${root}/target/release/cranvr --headless < /dev/null > ${work}/app.log 2>&1 &
app=$!
finish() {
    kill ${app} 2> /dev/null
    wait 2> /dev/null
    ${adb_cmd} reverse --remove tcp:${port} > /dev/null 2>&1
    ${root}/scripts/give-back.sh ${serial} ${CRANVR_GIVE_BACK_APK:-}
}
trap finish EXIT
trap 'exit 130' INT TERM
host_log=${home}/host.log
for i in {1..20}; do [[ -f ${host_log} ]] && break; sleep 0.5; done
[[ -f ${host_log} ]] || { print -u2 "the test app wrote no host log (${work}/app.log)"; exit 1; }
# A change of the headset's USB functions drops the test port's reverse: set it while the client waits
client_waiting() {
    ${adb_cmd} reverse tcp:${port} tcp:${port} > /dev/null 2>&1
}
client_waiting
extras=(--es host 127.0.0.1:${port} --ez connect true --ez usb_network false --es room passthrough --es interface 3d
    --ez save_choices false --ez first_start true --ez default_places true --es shown connection,stats,display1
    --es forget_host_key 127.0.0.1:${port})
held=0 crashed=0 unlinked=0 ended=0
for n in {1..${starts}}; do
    quest_ready
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    ${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
    # Horizon's shell crashed once after 31 stops and starts 4 s apart (2026-11-23), so it gets a
    # moment at home between two starts
    sleep 2
    client_waiting
    started=$(${adb_cmd} shell date "'+%m-%d %H:%M:%S.000'" | tr -d '\r')
    client_start ${host_log} ${extras}
    case ${client_result} in
        alive) (( held += 1 )) ;;
        crashed) (( crashed += 1 )) ;;
        ended) (( ended += 1 )) ;;
        *) (( unlinked += 1 )) ;;
    esac
    print "start ${n}: ${client_result}${client_linked:+, linked at ${client_linked}}"
    [[ -n ${client_crash} ]] && print -l -- "  "${(f)^client_crash}
    # A system dialog holds every launch back: more starts would measure nothing
    if [[ ${client_result} == unlinked ]] && ${adb_cmd} logcat -d -T "${started}" 2> /dev/null | grep -q "Launch is blocked because"; then
        print "Horizon OS holds the launch back: $(${adb_cmd} logcat -d -T "${started}" | grep -m1 -o 'Launch is blocked because: [^.]*')"
        break
    fi
    (( stop_after > 0 && crashed >= stop_after )) && break
done
print "starts ${n}: ${held} held, ${crashed} crashed, ${ended} ended with no crash, ${unlinked} did not link"
