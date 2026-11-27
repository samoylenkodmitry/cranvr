#!/bin/zsh
# Builds cranvr-quest-recorder.jar: the program that starts and stops the headset's own recorder for a
# recording of what the headset shows (quest-recorder/). The cranvr app pushes it to the headset over
# adb and runs it there with app_process. It needs a JDK (javac) and the Android SDK's d8 and android.jar.
#
# Usage: scripts/build-quest-recorder.sh   (writes target/quest-recorder/cranvr-quest-recorder.jar)
#
# Environment:
#   ANDROID_HOME  the Android SDK (default ~/Library/Android/sdk)
set -euo pipefail
root=${0:A:h:h}
sdk=${ANDROID_HOME:-${HOME}/Library/Android/sdk}
platform=$(print -l "${sdk}"/platforms/android-3[4-9]*(/N) | sort -V | head -1)
tools=$(print -l "${sdk}"/build-tools/*(/N) | sort -V | tail -1)
if [[ -z ${platform} || -z ${tools} ]]; then
    print -u2 "Missing the Android SDK's platform 34 or newer, or its build tools, in ${sdk}"
    exit 1
fi
work=${root}/target/quest-recorder
mkdir -p ${work}/classes
javac -source 11 -target 11 -Xlint:-options -classpath ${platform}/android.jar -d ${work}/classes ${root}/quest-recorder/src/dev/cranvr/recorder/*.java
${tools}/d8 --min-api 29 --lib ${platform}/android.jar --output ${work}/cranvr-quest-recorder.jar ${work}/classes/dev/cranvr/recorder/*.class
print "Built ${work}/cranvr-quest-recorder.jar"
