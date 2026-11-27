//! The final file of a recording: the picture of the headset's own recorder with the Mac's sound,
//! put where the user heard it (recording_align.rs), as an MP4 file.
//!
//! `cranvr-host --mux-recording VIDEO DIR OUT` reads the headset's file `VIDEO` and the sound of the
//! recording in `DIR` (recording_sound.rs) and writes `OUT`. AVFoundation does the work. The process
//! builds the sound of the whole picture in the picture's time, makes AAC of it and puts the
//! picture's track as it is, with no new encode, beside it. It then opens `OUT` again and checks it
//! against `VIDEO`. It prints one status line with the file's numbers, the loudness of both sounds
//! among them, and `checked` when the file passed.
//!
//! Which sound the file takes:
//! - the Mac's sound where it matched the headset's own track: it has no loss and nothing of the
//!   headset's own sounds. Where it does not cover the picture, as after a crash of the app that
//!   recorded it, the headset's track fills in;
//! - the headset's own track when the Mac's sound was silent while the headset heard sound, and when
//!   both had sound that did not match: the headset's track is in time with the picture for sure;
//! - the Mac's sound at the times of the start when the headset heard nothing to match;
//! - the headset's track as it is when the recording has no sound of the Mac's.

use crate::{
    recording_align::{self, Alignment},
    wav,
};
use block2::RcBlock;
use objc2::{rc::Retained, runtime::AnyObject, AnyThread};
use objc2_av_foundation::{
    AVAsset, AVAssetExportPresetAppleM4A, AVAssetExportPresetPassthrough, AVAssetExportSession, AVAssetExportSessionStatus,
    AVAssetReader, AVAssetReaderTrackOutput, AVAssetTrack, AVFileType, AVFileTypeAppleM4A, AVFileTypeMPEG4, AVMediaType,
    AVMediaTypeAudio, AVMediaTypeVideo, AVMutableComposition, AVURLAsset,
};
use objc2_core_media::{kCMPersistentTrackID_Invalid, CMTime, CMTimeRange};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

const RATE: f64 = wav::SAMPLE_RATE as f64;
/// The headset's recorder shows its first frame about this long after it answers that it started,
/// in s: the first frame came 0.15 s before the answer in a recording whose sound matched at 0.99
/// (2026-11-27). Only a recording with no sound to match falls back on it.
const FIRST_FRAME_AFTER_START: f64 = -0.15;
/// The headset's microphone reaches the Mac this much later than the Mac's sound reaches the ear in
/// the headset, in s: its sound goes this much earlier in the file
const MICROPHONE_LATER: f64 = 0.02;
/// The longest wait for one export of AVFoundation
const EXPORT_WAIT: Duration = Duration::from_secs(600);
/// The final file's picture may differ from the headset's by this much and pass its check, in s
const CHECK_SLACK: f64 = 0.05;

/// Writes the final file and returns its numbers for the app.
pub fn run(video: &Path, dir: &Path, out: &Path) -> Result<Value, String> {
    let times: Value = std::fs::read_to_string(dir.join("sound.json")).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or(Value::Null);
    let at = |key: &str| times[key].as_u64();
    let quest = asset(video);
    let video_track = track(&quest, kind(unsafe { AVMediaTypeVideo }))?.ok_or("the headset's file has no picture")?;
    let heard_track = track(&quest, kind(unsafe { AVMediaTypeAudio }))?;
    let video_range = unsafe { video_track.timeRange() };
    let picture_s = seconds(video_range.duration);
    let composition = unsafe { AVMutableComposition::composition() };
    insert(&composition, unsafe { AVMediaTypeVideo }, &video_track, video_range, zero())?;
    let mut numbers = json!({});
    // The tracks in the composition need their assets alive until the export of the composition ends
    let mut sources = Vec::new();
    let mac = wav::read(&dir.join("sound.wav")).ok().filter(|(channels, samples)| *channels == 2 && samples.len() > 2 * wav::SAMPLE_RATE as usize);
    let with_sound = match (mac, at("sound_first_ns")) {
        (Some((_, mac)), Some(sound_first)) => {
            let heard = heard_track.as_ref().map(|track| read_pcm(&quest, track)).transpose()?;
            let guess = at("start_mark_ns").map(|mark| (mark as f64 - sound_first as f64) * 1e-9 + FIRST_FRAME_AFTER_START);
            let microphone = match (wav::read(&dir.join("microphone.wav")), at("microphone_first_ns")) {
                (Ok((1, voice)), Some(first)) => Some((voice, (first as f64 - sound_first as f64) * 1e-9 - MICROPHONE_LATER)),
                _ => None,
            };
            let (stereo, sound) = final_sound(&mac, heard.as_ref(), microphone.as_ref(), guess, (picture_s * RATE).round() as usize);
            // The microphone's loudness, or false for a recording without it
            numbers["microphone"] = microphone.as_ref().map_or(json!(false), |(voice, _)| recording_align::level(voice, 1));
            numbers["sound"] = sound;
            let mixed = dir.join("final-sound.wav");
            let mut writer = wav::WavWriter::create(&mixed, 2).map_err(|e| format!("the sound: {e}"))?;
            writer.write_samples(&stereo).map_err(|e| format!("the sound: {e}"))?;
            writer.finish().map_err(|e| format!("the sound: {e}"))?;
            let aac = dir.join("final-sound.m4a");
            export(&asset(&mixed), unsafe { AVAssetExportPresetAppleM4A }, &aac, unsafe { AVFileTypeAppleM4A })?;
            let aac_asset = asset(&aac);
            let sound_track = track(&aac_asset, kind(unsafe { AVMediaTypeAudio }))?.ok_or("the AAC sound has no track")?;
            sources.push(aac_asset);
            let length = seconds(unsafe { sound_track.timeRange() }.duration).min(picture_s);
            insert(&composition, unsafe { AVMediaTypeAudio }, &sound_track, unsafe { CMTimeRange::new(zero(), time(length)) }, zero())?;
            true
        }
        _ => {
            let with_sound = heard_track.is_some();
            if let Some(heard) = heard_track {
                insert(&composition, unsafe { AVMediaTypeAudio }, &heard, unsafe { heard.timeRange() }, zero())?;
                numbers["sound"] = json!({"source": "headset", "why": "no sound of the Mac's: System Audio Recording may be off"});
            } else {
                numbers["sound"] = json!({"source": "none", "why": "neither the Mac nor the headset recorded sound"});
            }
            with_sound
        }
    };
    export(&composition, unsafe { AVAssetExportPresetPassthrough }, out, unsafe { AVFileTypeMPEG4 })?;
    drop(sources);
    let size = unsafe { video_track.naturalSize() };
    numbers["width"] = json!(size.width);
    numbers["height"] = json!(size.height);
    numbers["fps"] = json!(unsafe { video_track.nominalFrameRate() });
    numbers["seconds"] = json!(picture_s);
    numbers["tap"] = json!({"starts": times["tap_starts"], "gaps": times["gaps"], "filled_s": times["filled_s"], "first_sound_after_ms": times["first_sound_after_ms"], "ended": times["ended"]});
    numbers["checked"] = json!(check(out, picture_s, with_sound)?);
    Ok(numbers)
}

/// The sound of the whole picture, `frames` stereo frames from its start, and what the log says of
/// it. `mac` is the Mac's sound, `heard` the headset's own track with the picture's time of its first
/// sample, `microphone` the headset's microphone with its time in the Mac's sound, and `guess` the
/// Mac sound's time at the picture's start from the times of the start.
fn final_sound(mac: &[i16], heard: Option<&(f64, Vec<i16>)>, microphone: Option<&(Vec<i16>, f64)>, guess: Option<f64>, frames: usize) -> (Vec<i16>, Value) {
    let mac_level = recording_align::level(mac, 2);
    let heard_level = heard.map(|(_, stereo)| recording_align::level(stereo, 2));
    let has = |level: &Value| level["sound_s"].as_u64().unwrap_or(0) > 0;
    let heard_has = heard_level.as_ref().is_some_and(has);
    let alignment = heard.and_then(|(start, stereo)| recording_align::align(&recording_align::mono(stereo), *start, &recording_align::mono(mac)));
    let mut mixed = mac.to_vec();
    if let Some((voice, at)) = microphone {
        mix(&mut mixed, voice, (at * RATE).round() as i64);
    }
    let mut sound = json!({"mac": mac_level, "headset": heard_level, "guess_s": guess});
    let use_headset = match &alignment {
        Some(_) => None,
        None if heard_has && !has(&mac_level) => Some("the Mac's sound was silent while the headset heard sound"),
        None if heard_has => Some("the Mac's sound and the headset's did not match"),
        None => None,
    };
    let stereo = match (use_headset, heard) {
        (Some(why), Some(heard)) => {
            sound["source"] = json!("headset");
            sound["why"] = json!(why);
            let mut stereo = picture_timed(heard, frames);
            // The microphone has the Mac's time; the times of the start put it in the picture's
            if let (Some((voice, at)), Some(guess)) = (microphone, guess) {
                mix(&mut stereo, voice, ((at - guess) * RATE).round() as i64);
            }
            stereo
        }
        _ => {
            let placement = match &alignment {
                Some(Alignment { offset_s, rate, windows, tried, matched, spread_ms }) => {
                    sound["found_by"] = json!({"windows": windows, "tried": tried, "matched": matched, "spread_ms": spread_ms});
                    sound["drift_ppm"] = json!((rate * 1e6 * 10.0).round() / 10.0);
                    (*offset_s, *rate)
                }
                None => {
                    let why = match heard {
                        None => "the start's times: the headset's file has no sound",
                        Some(_) => "the start's times: the headset heard no sound to match",
                    };
                    sound["found_by"] = json!(why);
                    (guess.unwrap_or(0.0), 0.0)
                }
            };
            sound["source"] = json!("mac");
            sound["offset_s"] = json!(placement.0);
            let (mut stereo, covered) = recording_align::place(&mixed, placement, frames);
            // Where the Mac's sound ends early, as after a crash of the app, the headset's track fills in
            if let Some(heard) = heard {
                let filler = picture_timed(heard, frames);
                let mut filled = 0;
                for (frame, _) in covered.iter().enumerate().filter(|(_, covered)| !**covered) {
                    stereo[frame * 2..frame * 2 + 2].copy_from_slice(&filler[frame * 2..frame * 2 + 2]);
                    filled += 1;
                }
                sound["from_headset_s"] = json!((f64::from(filled) / RATE * 10.0).round() / 10.0);
            }
            stereo
        }
    };
    (stereo, sound)
}

/// The headset's own track in the picture's time: `frames` stereo frames from the picture's start.
fn picture_timed((start, stereo): &(f64, Vec<i16>), frames: usize) -> Vec<i16> {
    let first = (start * RATE).round() as i64;
    let mut out = vec![0_i16; frames * 2];
    for frame in 0..frames {
        let Some(at) = usize::try_from(frame as i64 - first).ok().filter(|at| at * 2 + 1 < stereo.len()) else { continue };
        out[frame * 2] = stereo[at * 2];
        out[frame * 2 + 1] = stereo[at * 2 + 1];
    }
    out
}

/// Opens the final file again and checks it against the headset's picture of `picture_s` seconds:
/// its picture has the same length, and it has sound when the recording had sound.
fn check(out: &Path, picture_s: f64, with_sound: bool) -> Result<bool, String> {
    let made = asset(out);
    let video = track(&made, kind(unsafe { AVMediaTypeVideo }))?.ok_or("the final file has no picture")?;
    let length = seconds(unsafe { video.timeRange() }.duration);
    if (length - picture_s).abs() > CHECK_SLACK {
        return Err(format!("the final file's picture lasts {length:.3} s, the headset's {picture_s:.3} s"));
    }
    if with_sound && track(&made, kind(unsafe { AVMediaTypeAudio }))?.is_none() {
        return Err("the final file has no sound".into());
    }
    Ok(true)
}

fn kind(media: Option<&'static AVMediaType>) -> &'static AVMediaType {
    media.expect("AVFoundation names its media types")
}

fn asset(path: &Path) -> Retained<AVURLAsset> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.display().to_string()));
    unsafe { AVURLAsset::URLAssetWithURL_options(&url, None) }
}

/// The first track of `media` of `asset`.
#[allow(deprecated)]
fn track(asset: &AVAsset, media: &AVMediaType) -> Result<Option<Retained<AVAssetTrack>>, String> {
    Ok(unsafe { asset.tracksWithMediaType(media) }.firstObject())
}

fn insert(composition: &AVMutableComposition, media: Option<&'static AVMediaType>, track: &AVAssetTrack, range: CMTimeRange, at: CMTime) -> Result<(), String> {
    let added = unsafe { composition.addMutableTrackWithMediaType_preferredTrackID(kind(media), kCMPersistentTrackID_Invalid) }.ok_or("the composition took no track")?;
    unsafe { added.insertTimeRange_ofTrack_atTime_error(range, track, at) }.map_err(|e| {
        format!("the composition took no {:.3} s from {:.3} s at {:.3} s: {} ({:?})", seconds(range.duration), seconds(range.start), seconds(at), e.localizedDescription(), e)
    })
}

/// The sound of `track` as interleaved stereo of 16-bit samples at 48 kHz, with the time of its first
/// sample in the file, in s.
fn read_pcm(asset: &AVAsset, track: &AVAssetTrack) -> Result<(f64, Vec<i16>), String> {
    let reader = unsafe { AVAssetReader::assetReaderWithAsset_error(asset) }.map_err(|e| format!("the headset's sound: {}", e.localizedDescription()))?;
    let keys = ["AVFormatIDKey", "AVSampleRateKey", "AVNumberOfChannelsKey", "AVLinearPCMBitDepthKey", "AVLinearPCMIsFloatKey", "AVLinearPCMIsBigEndianKey", "AVLinearPCMIsNonInterleaved"].map(NSString::from_str);
    let values: [Retained<AnyObject>; 7] = [
        number(NSNumber::new_u32(u32::from_be_bytes(*b"lpcm"))),
        number(NSNumber::new_f64(RATE)),
        number(NSNumber::new_u32(2)),
        number(NSNumber::new_u32(16)),
        number(NSNumber::new_bool(false)),
        number(NSNumber::new_bool(false)),
        number(NSNumber::new_bool(false)),
    ];
    let key_refs: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
    let settings = NSDictionary::<NSString, AnyObject>::from_retained_objects(&key_refs, &values);
    let output = unsafe { AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(track, Some(&settings)) };
    unsafe { reader.addOutput(&output) };
    if !unsafe { reader.startReading() } {
        return Err("the headset's sound does not read".into());
    }
    let mut samples = Vec::new();
    let mut start = None;
    while let Some(buffer) = unsafe { output.copyNextSampleBuffer() } {
        start.get_or_insert_with(|| seconds(unsafe { buffer.presentation_time_stamp() }));
        let Some(block) = (unsafe { buffer.data_buffer() }) else { continue };
        let length = unsafe { block.data_length() };
        let mut bytes = vec![0u8; length];
        if unsafe { block.copy_data_bytes(0, length, std::ptr::NonNull::new(bytes.as_mut_ptr()).expect("a buffer").cast()) } == 0 {
            samples.extend(bytes.as_chunks::<2>().0.iter().map(|pair| i16::from_le_bytes(*pair)));
        }
    }
    Ok((start.unwrap_or(0.0), samples))
}

fn number(value: Retained<NSNumber>) -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(Retained::into_super(value)))
}

/// Exports `asset` with `preset` to `out` as `file_type` and waits for the end.
fn export(asset: &AVAsset, preset: &NSString, out: &Path, file_type: Option<&'static AVFileType>) -> Result<(), String> {
    std::fs::remove_file(out).ok();
    let session = unsafe { AVAssetExportSession::initWithAsset_presetName(AVAssetExportSession::alloc(), asset, preset) }.ok_or("AVFoundation has no such export")?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(&out.display().to_string()));
    unsafe {
        session.setOutputURL(Some(&url));
        session.setOutputFileType(file_type);
    }
    let (done, wait) = mpsc::channel();
    let handler = RcBlock::new(move || {
        done.send(()).ok();
    });
    unsafe { session.exportAsynchronouslyWithCompletionHandler(&handler) };
    wait.recv_timeout(EXPORT_WAIT).map_err(|_| format!("the export of {} took too long", out.display()))?;
    if unsafe { session.status() } != AVAssetExportSessionStatus::Completed {
        let why = unsafe { session.error() }.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
        return Err(format!("the export of {} failed: {why}", out.display()));
    }
    Ok(())
}

/// Adds one channel of `voice` to both channels of `stereo`, its first frame at frame `at`.
fn mix(stereo: &mut [i16], voice: &[i16], at: i64) {
    for (index, sample) in voice.iter().enumerate() {
        let Some(frame) = usize::try_from(at + index as i64).ok() else { continue };
        let Some(pair) = stereo.get_mut(frame * 2..frame * 2 + 2) else { break };
        for channel in pair {
            *channel = channel.saturating_add(*sample);
        }
    }
}

fn time(seconds: f64) -> CMTime {
    unsafe { CMTime::with_seconds(seconds, 48_000) }
}

fn zero() -> CMTime {
    time(0.0)
}

fn seconds(time: CMTime) -> f64 {
    unsafe { time.seconds() }
}

/// The paths of `--mux-recording VIDEO DIR OUT`
pub fn paths(args: &[String]) -> Option<(PathBuf, PathBuf, PathBuf)> {
    match args {
        [video, dir, out] => Some((video.into(), dir.into(), out.into())),
        _ => None,
    }
}
