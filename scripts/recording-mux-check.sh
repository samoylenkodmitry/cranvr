#!/bin/zsh
# Checks the final file of a recording on this Mac with made-up parts (recording_mux.rs): a picture
# like the headset recorder's (HEVC at 60 fps) whose sound is the Mac's sound 1.3 s on and quieter,
# and the Mac's sound as recording_sound.rs writes it, with times that guess 0.85 s. The final file
# must have the picture as it was, AAC sound, the Mac's sound at the offset that the script put in,
# and pass its own check. Three passes:
#   - noise bursts that change their loudness (6 s);
#   - with SOUND=FILE, a real game sound: 40 s of silence, longer than the old search's 30 s, then
#     25 s of FILE from its 30th second on, as a game that sits in a silent menu first;
#   - a made-up microphone: a tone of 1 kHz for 2 s that starts 2 s after the Mac's sound. The final
#     file must have the tone from 0.68 s of the picture on (2 s, less the offset of 1.3 s and the
#     microphone's 0.02 s) and no tone after 2.68 s.
#
# Usage: [SOUND=FILE] scripts/recording-mux-check.sh WORKDIR   (needs ffmpeg and ffprobe, and
# `cargo build --release`)
set -eu
work=$1
root=${0:A:h:h}
mkdir -p ${work}
host=${root}/target/release/cranvr-host
# The Mac sound's time at the picture's start that each pass must find, and the slack in s
expected=1.3 slack=0.003
offset() {
    python3 -I -c 'import json,sys; line=[l for l in open(sys.argv[1]) if l.startswith("@cranvr ")][-1]; n=json.loads(line[8:])["numbers"]; s=n["sound"]; print(s["source"], s.get("offset_s"), str(n["checked"]).lower(), json.dumps(s.get("found_by"), separators=(",", ":")))' $1
}
pass() {
    local name=$1 seconds=$2 size=$3
    ffmpeg -v error -y -f lavfi -i "testsrc2=s=${size}:r=60:d=${seconds}" -ss ${expected} -i ${work}/sound.wav \
        -filter:a "volume=0.5" -t ${seconds} -c:v libx265 -preset ultrafast -tag:v hvc1 -c:a aac -b:a 128k -shortest ${work}/quest.mp4 2> /dev/null
    print '{"sound_first_ns": 1000000000, "microphone_first_ns": null, "start_mark_ns": 2000000000}' > ${work}/sound.json
    ${host} --mux-recording ${work}/quest.mp4 ${work} ${work}/out-${name}.mp4 > ${work}/mux-${name}.txt
    ffprobe -v error -show_entries stream=codec_type,codec_name,width,height,avg_frame_rate,duration -of compact ${work}/out-${name}.mp4
    local found=(${(s: :)$(offset ${work}/mux-${name}.txt)})
    print "${name}: the sound's source ${found[1]}, offset ${found[2]} s (put in ${expected} s), checked ${found[3]}, found by ${found[4]}"
    [[ ${found[1]} == mac && ${found[3]} == true ]]
    python3 -I -c 'import sys; sys.exit(abs(float(sys.argv[1]) - float(sys.argv[2])) > float(sys.argv[3]))' ${found[2]} ${expected} ${slack}
}
# The Mac's sound: 6 s of noise bursts that change their loudness, from 1.0 s before the mark
ffmpeg -v error -y -f lavfi -i "anoisesrc=d=6:c=pink:r=48000:a=0.5,volume='if(lt(mod(t,0.7),0.25),1,0.15)':eval=frame" -ac 2 -c:a pcm_s16le ${work}/sound.wav
pass noise 4 1920x1080
if [[ -n ${SOUND:-} ]]; then
    ffmpeg -v error -y -f lavfi -i "anullsrc=r=48000:cl=stereo:d=40" -ss 30 -t 25 -i ${SOUND} \
        -filter_complex "[1:a]aresample=48000,aformat=channel_layouts=stereo[g];[0:a][g]concat=n=2:v=0:a=1" -c:a pcm_s16le ${work}/sound.wav
    pass game 62 320x180
    ffmpeg -v error -y -f lavfi -i "anoisesrc=d=6:c=pink:r=48000:a=0.5,volume='if(lt(mod(t,0.7),0.25),1,0.15)':eval=frame" -ac 2 -c:a pcm_s16le ${work}/sound.wav
    pass noise 4 1920x1080
fi
# The microphone
ffmpeg -v error -y -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=2" -af volume=0.5 -ac 1 -c:a pcm_s16le ${work}/microphone.wav
print '{"sound_first_ns": 1000000000, "microphone_first_ns": 3000000000, "start_mark_ns": 2000000000}' > ${work}/sound.json
${host} --mux-recording ${work}/quest.mp4 ${work} ${work}/out-microphone.mp4 > ${work}/mux-microphone.txt
grep -q '"microphone":{"peak_db"' ${work}/mux-microphone.txt
# The tone's loudness in a narrow band about 1 kHz: while the tone plays and after it
tone() {
    ffmpeg -hide_banner -nostats -ss $2 -t $3 -i $1 -vn -af "bandpass=f=1000:width_type=h:w=40,volumedetect" -f null - 2>&1 | sed -nE 's/.*mean_volume: (-?[0-9.]+) dB/\1/p'
}
during=$(tone ${work}/out-microphone.mp4 0.9 1.5) after=$(tone ${work}/out-microphone.mp4 3.0 0.9) without=$(tone ${work}/out-noise.mp4 0.9 1.5)
print "the band of 1 kHz: ${during} dB while the tone plays, ${after} dB after it, ${without} dB with no microphone"
(( during > after + 15 && during > without + 15 ))
