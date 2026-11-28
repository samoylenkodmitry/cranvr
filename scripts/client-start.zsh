# Starts the headset client (dev.cranvr.client) and tells whether the start held. A start holds when
# the client links to its host and the same process still runs CLIENT_HOLD seconds (3 by default)
# after the link. A link line alone proves nothing: on 2026-11-23 (#145) a client linked and
# crashed 1 s later, a script took the link as success and the user's stream stayed dark for 16
# minutes.
#
# Source this file from a zsh script that sets `adb_cmd` (adb with the Quest's -s).
#
# client_start HOST_LOG EXTRAS...
#   Starts the client with the `am start` EXTRAS and waits up to CLIENT_LINK_WAIT seconds (20 by
#   default) for a new "Headset connected" line in HOST_LOG. It calls the function
#   `client_waiting` once a second while it waits, when the caller defines one (a test port sets its
#   adb reverse again there). It sets:
#     client_result  alive, crashed, ended (the link or the process ended with no crash), unlinked or
#                    old (the installed client has no entry for adb: install a build from main)
#     client_crash   the fatal signal line and the top frames of a crash
#     client_linked  the host's time of the new link line
# quest_ready
#   Waits up to 40 s until adb lists the Quest as a device again: a change of the headset's USB
#   functions takes adb away for a moment.
# client_has_entry
#   Succeeds when the installed client has the entry for adb, so a start with extras can work.

readonly CLIENT_PACKAGE=dev.cranvr.client
# The client takes launch extras only through this alias, which only adb's shell may start
# (client/src/launch.rs)
readonly CLIENT_ACTIVITY=dev.cranvr.client/dev.cranvr.client.AdbStart

quest_ready() {
    local i
    for i in {1..40}; do
        ${adb_cmd} get-state 2> /dev/null | grep -q '^device' && return 0
        sleep 1
    done
    return 1
}

# Whether the installed client has the entry for adb. A query of the package manager starts nothing.
client_has_entry() {
    ${adb_cmd} shell cmd package resolve-activity --brief -n ${CLIENT_ACTIVITY} 2> /dev/null | grep -q AdbStart
}

client_pid() {
    ${adb_cmd} shell pidof ${CLIENT_PACKAGE} 2> /dev/null | tr -d '\r'
}

# The crash log of the client since the device time $1: the fatal signal line and the frames
client_crash_since() {
    ${adb_cmd} logcat -b crash -d -T "$1" 2> /dev/null |
        grep -E "Fatal signal .*\(v\.cranvr\.client\)|^.*DEBUG.*(#0[0-4] pc|Cmdline: dev\.cranvr\.client|Cause: )" |
        sed -E 's/^[0-9-]+ [0-9:.]+ +[0-9]+ +[0-9]+ [A-Z] //' | head -9
}

client_start() {
    local host_log=$1
    shift
    local wait_s=${CLIENT_LINK_WAIT:-20} hold=${CLIENT_HOLD:-3}
    local links ends since pid later i
    client_result=unlinked client_crash="" client_linked=""
    quest_ready || return 0
    links=$(grep -c "Headset connected" ${host_log})
    since=$(${adb_cmd} shell date "'+%m-%d %H:%M:%S.000'" | tr -d '\r')
    if ${adb_cmd} shell am start -n ${CLIENT_ACTIVITY} "$@" 2>&1 | grep -q "does not exist"; then
        print -u2 "client-start: the installed client has no ${CLIENT_ACTIVITY#*/}: install a build from main"
        client_result=old
        return 0
    fi
    for i in {1..${wait_s}}; do
        sleep 1
        (( $(grep -c "Headset connected" ${host_log}) > links )) && break
        (( ${+functions[client_waiting]} )) && client_waiting
    done
    if (( $(grep -c "Headset connected" ${host_log}) > links )); then
        client_linked=$(grep "Headset connected" ${host_log} | tail -1 | awk '{ print $2 }')
        ends=$(grep -c "Headset disconnected" ${host_log})
        pid=$(client_pid)
        later=${pid}
        for i in {1..${hold}}; do
            sleep 1
            later=$(client_pid)
            [[ ${later} == ${pid} ]] || break
        done
        if [[ -n ${pid} && ${later} == ${pid} ]] && (( $(grep -c "Headset disconnected" ${host_log}) == ends )); then
            client_result=alive
            return 0
        fi
        client_result=ended
    fi
    # The crash dump reaches the log a moment after the fault
    client_crash=$(client_crash_since "${since}")
    if [[ -z ${client_crash} ]]; then
        sleep 2
        client_crash=$(client_crash_since "${since}")
    fi
    [[ -n ${client_crash} ]] && client_result=crashed
    return 0
}
