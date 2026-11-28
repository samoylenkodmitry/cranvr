#!/bin/zsh
# Measures the sharpness of a display's stream on this Mac, with the probe as the headset (issue #222).
# A test host on PORT makes a virtual display of 1920 x 1080 points (3840 x 2160 pixels) that only the
# stream shows. show.swift puts the test pattern on it. For each width the probe asks for the display
# at that width, decodes the stream with VideoToolbox and saves the luma of the first key picture and of
# the last picture. measure.swift then reads the pattern in both. The pointer of the probe moves no mouse
# (CRANVR_POINTER_DRY_RUN=1). The Mac's cursor stays out of the pictures.
#
# macOS adds the virtual display to the right of the Mac's displays while the host runs. No window
# moves. The host gives each display its old mode back.
#
# Usage: scripts/sharpness/mac-check.sh WORKDIR [SECONDS] [WIDTH...]
#   SECONDS  the length of each probe run (3 by default: the key picture and the refresh 250 ms later)
#   WIDTH    the widths to ask for (3840 2560 1920 by default)
# `cargo build --release -p cranvr-host -p cranvr-probe` must have built both programs.
set -u
work=$1
seconds=${2:-3}
shift $(( $# < 2 ? $# : 2 ))
widths=(3840 2560 1920)
(( $# )) && widths=("$@")
root=${0:A:h:h:h}
here=${0:A:h}
port=${CRANVR_SHARPNESS_PORT:-9962}
mkdir -p ${work}/host ${work}/bin
host="" show=""
finish() {
    [[ -n ${show} ]] && kill ${show} 2> /dev/null
    [[ -n ${host} ]] && kill ${host} 2> /dev/null
    wait 2> /dev/null
    print "the virtual display ends $(date +%T)"
}
trap finish EXIT
trap 'exit 130' INT TERM
swift ${here}/pattern.swift ${work}/pattern > /dev/null || exit 1
for tool in show measure; do
    swiftc -O ${here}/${tool}.swift -o ${work}/bin/${tool} || exit 1
done
print "the virtual display starts $(date +%T)"
CRANVR_POINTER_DRY_RUN=1 CRANVR_DESKTOP_CURSOR_IN_VIDEO=0 ${root}/target/release/cranvr-host --status --native-port ${port} \
    --config-dir ${work}/host --trust-all --tcp --loopback --no-game-audio --desktop --virtual-display 1920x1080 > ${work}/host.out 2>&1 &
host=$!
display=""
for i in {1..40}; do
    display=$(sed -nE 's/.*Virtual display ([0-9]+):.*/\1/p' ${work}/host.out | head -1)
    [[ -n ${display} ]] && break
    sleep 0.25
done
[[ -n ${display} ]] || { print -u2 "the host made no virtual display"; tail -5 ${work}/host.out; exit 1 }
${work}/bin/show ${work}/pattern/pattern.png ${display} > ${work}/show.out 2>&1 &
show=$!
sleep 1
print "$(cat ${work}/show.out)"
for width in ${widths}; do
    ${root}/target/release/cranvr-probe --connect 127.0.0.1:${port} --tcp --desktop --display-width ${width} --display-fps 30 \
        --seconds ${seconds} --save-display ${work}/w${width} > ${work}/probe-${width}.out 2>&1
    grep -E "probe: desktop: [0-9]+ pictures" ${work}/probe-${width}.out | sed -E 's/; network.*//'
    for picture in key last; do
        print "${width} ${picture}: $(${work}/bin/measure ${work}/w${width}-${picture}.pgm ${work}/pattern --frame --json ${work}/w${width}-${picture}.json | tr '\n' ' ')"
    done
done
