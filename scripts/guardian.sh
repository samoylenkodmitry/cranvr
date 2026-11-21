# Pauses the headset's Guardian for a run with the headset on a desk, then sets the exact old value
# back. A headset that lies still loses its place in the room, and the boundary then covers the app
# with a dialog. A paused Guardian on a headset that someone then wears shows no boundary, so the
# old value must come back on every way out of the run.
#
# A script sources this file, then calls `guardian_pause ADB_COMMAND...` and `guardian_restore` from
# its EXIT trap. `guardian_traps` makes INT, TERM and HUP end the script through the EXIT trap: zsh
# skips the EXIT trap on a TERM without a trap of its own. A trapped signal waits for the command in
# the foreground, so a long wait in the script runs as `sleep N & wait $!`. Only SIGKILL skips the
# restore, as no script can catch it.
#
# The old value comes back exactly: an empty value stays empty.

typeset -ga guardian_adb=()
typeset -g guardian_saved=0 guardian_old=""

guardian_traps() {
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap 'exit 129' HUP
}

# Reads the old value, then pauses Guardian. Nothing is paused when the read fails.
guardian_pause() {
    guardian_adb=("$@")
    guardian_old=$("${guardian_adb[@]}" shell getprop debug.oculus.guardian_pause | tr -d '\r') || return 1
    guardian_saved=1
    "${guardian_adb[@]}" shell setprop debug.oculus.guardian_pause 1
}

# Sets the old value back and reads it again. It tries three times and warns when the value
# stays wrong. A second call does nothing.
guardian_restore() {
    (( guardian_saved )) || return 0
    local attempt now
    for attempt in 1 2 3; do
        # The device's shell parses the line again: the quotes keep an empty value as one argument
        "${guardian_adb[@]}" shell setprop debug.oculus.guardian_pause "${(qq)guardian_old}" >/dev/null 2>&1 || true
        now=$("${guardian_adb[@]}" shell getprop debug.oculus.guardian_pause 2>/dev/null | tr -d '\r') || now="(unread)"
        if [[ ${now} == "${guardian_old}" ]]; then
            guardian_saved=0
            print "Guardian is back at its old value '${guardian_old}'"
            return 0
        fi
        sleep 1 || true
    done
    print -u2 "WARNING: Guardian reads '${now}' instead of '${guardian_old}'. Set it back before anyone wears the headset:"
    print -u2 "  ${guardian_adb[*]} shell setprop debug.oculus.guardian_pause ${(qq)guardian_old}"
    return 1
}
