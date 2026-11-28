#!/bin/zsh
# Probes the headset's USB network (NCM) without the cranvr client: issue #59, item 3.
#
# Usage: scripts/ncm-probe/run.sh SERIAL [HOLD_SECONDS]
#
# The script builds a small test app (dev.cranvr.ncmprobe) with the SDK's build tools, installs it,
# starts the Mac's end (mac_side.py) and opens the app in the headset. The app asks the system for
# the USB network. It holds the network for HOLD_SECONDS (120 by default) and gives it back. The
# headset then returns to its old USB functions about 30 s later. The script prints both logs and
# removes the app.
#
# The headset changes its USB functions at the request, so USB adb is gone until it changes them
# back. The app's log lines reach the Mac over Wi-Fi in that time. The headset and the Mac must be
# on the same Wi-Fi network for them.
#
# The first time, macOS asks "Allow accessory to connect?" for the headset's new USB functions.
# Click Allow on the Mac. Without that answer the link never comes up. The headset may then ask
# its own question in the app's window.
#
# Afterwards a running cranvr client may have moved to Wi-Fi. Start it again on the cable:
#   adb -s SERIAL shell am force-stop dev.cranvr.client
#   adb -s SERIAL shell am start -n dev.cranvr.client/dev.cranvr.client.AdbStart --es host 127.0.0.1:9950 --ez connect true
set -euo pipefail

serial=$1 hold=${2:-120}
here=${0:A:h}
root=${here:h:h}
work=${root}/target/ncm-probe
sdk=${ANDROID_HOME:-${HOME}/Library/Android/sdk}
bt=$(print -l "${sdk}"/build-tools/*(/N) | sort -V | tail -1)
jar=$(print -l "${sdk}"/platforms/android-34*(/N) | sort -V | head -1)/android.jar
adb_cmd=(adb -s ${serial})

mkdir -p ${work}/classes ${work}/dex
javac -source 8 -target 8 -Xlint:-options -bootclasspath ${jar} -classpath ${jar}:${bt}/core-lambda-stubs.jar \
    -d ${work}/classes ${here}/src/dev/cranvr/ncmprobe/*.java
${bt}/d8 --min-api 29 --lib ${jar} --output ${work}/dex ${work}/classes/dev/cranvr/ncmprobe/*.class
${bt}/aapt2 link -I ${jar} --manifest ${here}/AndroidManifest.xml -o ${work}/base.apk
(cd ${work}/dex && zip -q ${work}/base.apk classes.dex)
${bt}/zipalign -f 4 ${work}/base.apk ${work}/aligned.apk
${bt}/apksigner sign --ks ${HOME}/.android/debug.keystore --ks-pass pass:android --key-pass pass:android \
    --out ${work}/ncmprobe.apk ${work}/aligned.apk

# The Mac's address on the route to the headset's Wi-Fi address takes the app's log lines
quest_ip=$(${adb_cmd} shell ip -4 -o addr show wlan0 | awk '{print $4}' | cut -d/ -f1)
mac_if=$(route -n get ${quest_ip} | awk '/interface:/ {print $2}')
mac_ip=$(ipconfig getifaddr ${mac_if})
print "Headset ${quest_ip}, Mac ${mac_ip} on ${mac_if}"

${adb_cmd} install -r ${work}/ncmprobe.apk > /dev/null
python3 -I ${here}/mac_side.py $(( hold + 60 )) ${serial} > ${work}/mac_side.log 2>&1 &
mac_side=$!
sleep 1
${adb_cmd} shell am start -n dev.cranvr.ncmprobe/.ProbeActivity --es mac ${mac_ip} --ei hold ${hold} > /dev/null
print "The probe runs for ${hold} s. Click Allow if macOS asks about the accessory."
wait ${mac_side} || true
cat ${work}/mac_side.log
# USB adb returns once the headset has its old USB functions again
for _ in {1..30}; do
    ${adb_cmd} get-state > /dev/null 2>&1 && break
    sleep 2
done
${adb_cmd} logcat -d -v time | grep -E "NcmProbe|UsbDeviceManager" > ${work}/headset.log || true
${adb_cmd} uninstall dev.cranvr.ncmprobe > /dev/null || true
print "Headset log: ${work}/headset.log"
