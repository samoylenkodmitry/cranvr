//! The final file of a recording: the picture of the headset's own recorder with the Mac's sound,
//! put where the user heard it (recording_align.rs), as an MP4 file.
//!
//! `cranvr-host --mux-recording VIDEO DIR OUT` reads the headset's file `VIDEO` and the sound of the
//! recording in `DIR` (recording_sound.rs) and writes `OUT`. AVFoundation does the work. An export
//! makes AAC of the Mac's sound, with the headset's microphone mixed in when the recording took it.
//! A composition then puts the picture's track as it is, with no new encode, beside that sound.
//! The process prints one status line with the file's numbers.
//!
//! The Mac's sound replaces the headset's track: it has no loss and nothing of the headset's own
//! sounds. A recording without the Mac's sound keeps the headset's track.

use crate::{recording_align, wav};
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
/// The search for the lag spans this far on each side of the guess from the start's times, in s
const SEARCH_SPAN: f64 = 2.0;
/// The headset's recorder shows its first frame about this long after it answers that it started,
/// in s. Only a recording with no sound to match falls back on it.
const FIRST_FRAME_AFTER_START: f64 = 0.0;
/// The headset's microphone reaches the Mac this much later than the Mac's sound reaches the ear in
/// the headset, in s: its sound goes this much earlier in the file
const MICROPHONE_LATER: f64 = 0.02;
/// The longest wait for one export of AVFoundation
const EXPORT_WAIT: Duration = Duration::from_secs(600);

/// Writes the final file and returns its numbers for the app.
pub fn run(video: &Path, dir: &Path, out: &Path) -> Result<Value, String> {
    let times: Value = std::fs::read_to_string(dir.join("sound.json")).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or(Value::Null);
    let at = |key: &str| times[key].as_u64();
    let quest = asset(video);
    let video_track = track(&quest, kind(unsafe { AVMediaTypeVideo }))?.ok_or("the headset's file has no picture")?;
    let heard = track(&quest, kind(unsafe { AVMediaTypeAudio }))?;
    let sound = wav::read(&dir.join("sound.wav")).ok().filter(|(channels, samples)| *channels == 2 && samples.len() > 2 * wav::SAMPLE_RATE as usize);
    let composition = unsafe { AVMutableComposition::composition() };
    let video_range = unsafe { video_track.timeRange() };
    insert(&composition, unsafe { AVMediaTypeVideo }, &video_track, video_range, zero())?;
    let mut numbers = json!({});
    // The tracks in the composition need their assets alive until the export of the composition ends
    let mut sources = Vec::new();
    match (sound, at("sound_first_ns")) {
        (Some((_, mut stereo)), Some(sound_first)) => {
            let guess = at("start_mark_ns").map_or(0.0, |mark| (mark as f64 - sound_first as f64) * 1e-9 + FIRST_FRAME_AFTER_START);
            let (lag, method) = match heard.as_ref().map(|track| read_pcm(&quest, track)).transpose()? {
                Some((start, samples)) => match recording_align::find(&samples, &recording_align::mono(&stereo), guess + start, SEARCH_SPAN) {
                    Some(found) => (found.seconds() - start, json!({"matched": found.matched})),
                    None => (guess, json!("the start's times: the headset heard no sound to match")),
                },
                None => (guess, json!("the start's times: the headset's file has no sound")),
            };
            if let (Ok((1, voice)), Some(first)) = (wav::read(&dir.join("microphone.wav")), at("microphone_first_ns")) {
                let offset = (first as f64 - sound_first as f64) * 1e-9 - MICROPHONE_LATER;
                mix(&mut stereo, &voice, (offset * RATE).round() as i64);
                numbers["microphone"] = json!(true);
            }
            let mixed = dir.join("final-sound.wav");
            let mut writer = wav::WavWriter::create(&mixed, 2).map_err(|e| format!("the sound: {e}"))?;
            writer.write_samples(&stereo).map_err(|e| format!("the sound: {e}"))?;
            writer.finish().map_err(|e| format!("the sound: {e}"))?;
            let aac = dir.join("final-sound.m4a");
            export(&asset(&mixed), unsafe { AVAssetExportPresetAppleM4A }, &aac, unsafe { AVFileTypeAppleM4A })?;
            let aac_asset = asset(&aac);
            let sound_track = track(&aac_asset, kind(unsafe { AVMediaTypeAudio }))?.ok_or("the AAC sound has no track")?;
            sources.push(aac_asset);
            let sound_range = unsafe { sound_track.timeRange() };
            // The sound's time `lag` plays at the picture's start
            let skip = lag.max(0.0);
            let length = (seconds(sound_range.duration) - skip).min(seconds(video_range.duration) + lag.min(0.0));
            if length > 0.0 {
                let range = unsafe { CMTimeRange::new(time(skip), time(length)) };
                insert(&composition, unsafe { AVMediaTypeAudio }, &sound_track, range, time((-lag).max(0.0)))?;
            }
            numbers["sound"] = json!({"source": "mac", "lag_s": lag, "guess_s": guess, "found_by": method});
        }
        _ => {
            if let Some(heard) = heard {
                insert(&composition, unsafe { AVMediaTypeAudio }, &heard, unsafe { heard.timeRange() }, zero())?;
            }
            numbers["sound"] = json!({"source": "headset", "why": "no sound of the Mac's: System Audio Recording may be off"});
        }
    }
    export(&composition, unsafe { AVAssetExportPresetPassthrough }, out, unsafe { AVFileTypeMPEG4 })?;
    drop(sources);
    let size = unsafe { video_track.naturalSize() };
    numbers["width"] = json!(size.width);
    numbers["height"] = json!(size.height);
    numbers["fps"] = json!(unsafe { video_track.nominalFrameRate() });
    numbers["seconds"] = json!(seconds(video_range.duration));
    Ok(numbers)
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

/// The sound of `track` as one channel of 16-bit samples at 48 kHz, with the time of its first
/// sample in the file, in s.
fn read_pcm(asset: &AVAsset, track: &AVAssetTrack) -> Result<(f64, Vec<i16>), String> {
    let reader = unsafe { AVAssetReader::assetReaderWithAsset_error(asset) }.map_err(|e| format!("the headset's sound: {}", e.localizedDescription()))?;
    let keys = ["AVFormatIDKey", "AVSampleRateKey", "AVNumberOfChannelsKey", "AVLinearPCMBitDepthKey", "AVLinearPCMIsFloatKey", "AVLinearPCMIsBigEndianKey", "AVLinearPCMIsNonInterleaved"].map(NSString::from_str);
    let values: [Retained<AnyObject>; 7] = [
        number(NSNumber::new_u32(u32::from_be_bytes(*b"lpcm"))),
        number(NSNumber::new_f64(RATE)),
        number(NSNumber::new_u32(1)),
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
