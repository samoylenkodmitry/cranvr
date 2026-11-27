#!/bin/zsh
# Checks the final file of a recording on this Mac with made-up parts (recording_mux.rs): a picture
# like the headset recorder's (HEVC 1920x1080 at 60 fps) whose sound is the Mac's sound 0.3 s
# later and quieter, and the Mac's sound as recording_sound.rs writes it. The final file must have
# the picture as it was, AAC sound, and the lag that the script put in. A second pass adds a made-up
# microphone: a tone of 1 kHz for 2 s that starts 2 s after the Mac's sound. The final file must
# have the tone from 0.68 s of the picture on (2 s, less the lag of 1.3 s and the microphone's
# 0.02 s) and no tone after 2.68 s.
#
# Usage: scripts/recording-mux-check.sh WORKDIR   (needs ffmpeg and ffprobe, and `cargo build --release`)
set -eu
work=$1
root=${0:A:h:h}
mkdir -p ${work}
# The Mac's sound: 6 s of noise bursts that change their loudness, from 1.0 s before the mark
ffmpeg -v error -y -f lavfi -i "anoisesrc=d=6:c=pink:r=48000:a=0.5,volume='if(lt(mod(t,0.7),0.25),1,0.15)':eval=frame" -ac 2 -c:a pcm_s16le ${work}/sound.wav
# The headset's recording: from 1.3 s of the Mac's sound on (the mark plus 0.3 s), quieter
ffmpeg -v error -y -f lavfi -i "testsrc2=s=1920x1080:r=60:d=4" -ss 1.3 -i ${work}/sound.wav \
    -filter:a "volume=0.5" -t 4 -c:v libx265 -preset ultrafast -tag:v hvc1 -c:a aac -b:a 128k -shortest ${work}/quest.mp4 2> /dev/null
print '{"sound_first_ns": 1000000000, "microphone_first_ns": null, "start_mark_ns": 2000000000}' > ${work}/sound.json
${root}/target/release/cranvr-host --mux-recording ${work}/quest.mp4 ${work} ${work}/out.mp4 | tee ${work}/mux.txt
ffprobe -v error -show_entries stream=codec_type,codec_name,width,height,avg_frame_rate,duration -of compact ${work}/out.mp4
grep -q '"ok":true' ${work}/mux.txt
# The microphone
ffmpeg -v error -y -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=2" -af volume=0.5 -ac 1 -c:a pcm_s16le ${work}/microphone.wav
print '{"sound_first_ns": 1000000000, "microphone_first_ns": 3000000000, "start_mark_ns": 2000000000}' > ${work}/sound.json
${root}/target/release/cranvr-host --mux-recording ${work}/quest.mp4 ${work} ${work}/out-microphone.mp4 | tee ${work}/mux-microphone.txt
grep -q '"microphone":true' ${work}/mux-microphone.txt
# The tone's loudness in a narrow band about 1 kHz: while the tone plays and after it
tone() {
    ffmpeg -hide_banner -nostats -ss $2 -t $3 -i $1 -vn -af "bandpass=f=1000:width_type=h:w=40,volumedetect" -f null - 2>&1 | sed -nE 's/.*mean_volume: (-?[0-9.]+) dB/\1/p'
}
during=$(tone ${work}/out-microphone.mp4 0.9 1.5) after=$(tone ${work}/out-microphone.mp4 3.0 0.9) without=$(tone ${work}/out.mp4 0.9 1.5)
print "the band of 1 kHz: ${during} dB while the tone plays, ${after} dB after it, ${without} dB with no microphone"
(( during > after + 15 && during > without + 15 ))
