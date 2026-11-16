#!/bin/zsh
# Records a Metal System Trace of every process on this Mac and exports its GPU intervals.
#
# Usage: scripts/perf/metal-trace.sh NAME [SECONDS]
#   NAME     the trace's name; NAME.trace and NAME.xml go to CRANVR_PERF_DIR (default: target/perf)
#   SECONDS  how long to record (default: 5)
#
# Then: gpu-by-pass.py NAME.xml SECONDS FRAMES [PROCESS] and gpu-by-kind.py NAME.xml [PROCESS] sum
# the GPU time per process, per pass and per kind of pass. A 5 s trace is about 500 MB.
set -e
name=$1 secs=${2:-5}
[[ -n $name ]] || { sed -n '4,6p' $0; exit 2; }
out=${CRANVR_PERF_DIR:-${0:A:h:h:h}/target/perf}
mkdir -p $out
uptime
xcrun xctrace record --template 'Metal System Trace' --all-processes --time-limit ${secs}s --no-prompt \
    --output $out/$name.trace > /dev/null
xcrun xctrace export --input $out/$name.trace \
    --xpath '/trace-toc/run[@number="1"]/data/table[@schema="metal-gpu-intervals"]' > $out/$name.xml
echo "$out/$name.xml"
