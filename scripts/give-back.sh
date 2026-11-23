#!/bin/zsh
# Puts the user's client back on the cranvr app's host after a test and checks that it holds: the
# client links and the same process still runs 3 s later (scripts/client-start.zsh). It stops the
# client before each start, tries three times and exits 1 when no start held, so the caller can say
# that the headset has no stream.
#
# Usage: scripts/give-back.sh SERIAL [APK] [-- EXTRAS...]
#   APK     a build to install first, such as the normal build after a test build
#   EXTRAS  more launch extras, such as `--es forget_host_key ADDRESS`
#
# Environment:
#   CRANVR_HOST_LOG    the app's host log (default: ~/Library/Application Support/cranvr/host.log)
#   CRANVR_OWNER_HOST  the address that the client links to (default: 127.0.0.1:9950, the cable)
#   CLIENT_HOLD        the seconds that the client must run after its link (default: 3)
set -u
serial=$1
shift
apk=""
if (( $# )) && [[ $1 != -- ]]; then
    apk=$1
    shift
fi
[[ ${1:-} == -- ]] && shift
extras=("$@")
adb_cmd=(adb -s ${serial})
host_log=${CRANVR_HOST_LOG:-${HOME}/Library/Application Support/cranvr/host.log}
owner=${CRANVR_OWNER_HOST:-127.0.0.1:9950}
source ${0:A:h}/client-start.zsh

if [[ -n ${apk} ]]; then
    installed=0
    for attempt in 1 2 3; do
        quest_ready && ${adb_cmd} install -r ${apk} 2>&1 | grep -q Success && { installed=1; break; }
        sleep 2
    done
    (( installed )) || print -u2 "give-back: ${apk:t} did not install"
fi
for attempt in 1 2 3; do
    quest_ready
    ${adb_cmd} shell am force-stop ${CLIENT_PACKAGE}
    ${adb_cmd} shell am broadcast -a com.oculus.vrpowermanager.prox_close > /dev/null
    client_start ${host_log} --es host ${owner} --ez connect true ${extras}
    if [[ ${client_result} == alive ]]; then
        print "give-back: the client linked to ${owner} at ${client_linked} and still runs ${CLIENT_HOLD:-3} s later"
        exit 0
    fi
    print -u2 "give-back: start ${attempt} did not hold (${client_result})"
    [[ -n ${client_crash} ]] && print -u2 -l -- ${(f)client_crash}
done
print -u2 "give-back: the client does not run on ${owner}"
exit 1
