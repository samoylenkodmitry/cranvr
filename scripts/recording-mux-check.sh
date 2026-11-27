#!/bin/zsh
# Checks the final file of a recording on this Mac with made-up parts (recording_mux.rs): a picture
# like the headset recorder's (HEVC 1920x1080 at 60 fps) whose sound is the Mac's sound 0.3 s
# later and quieter, and the Mac's sound as recording_sound.rs writes it. The final file must have
# the picture as it was, AAC sound, and the lag that the script put in.
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
