//! The game sound in the headset. A Core Audio process tap captures what this Mac plays, from every
//! process but cranvr's own. The host converts it to interleaved 16-bit stereo at 48 kHz and hands it
//! to the headset link (`headset/native/audio.rs`).
//!
//! The tap takes each app's sound before the Mac's volume, so the Mac can stay muted while the
//! headset plays it.
//!
//! The tap needs no screen stream. The sound came from ScreenCaptureKit before, and its stream was
//! bound to the main display. WindowServer then worked for every change of the screen, even with
//! every display hidden in the headset: one such stream added about a third to its GPU time
//! (issue #118).
//!
//! The tap needs the System Audio Recording permission (`cranvr_common::system_audio`). The app asks
//! for it. The host starts the tap only when the permission is there, so the host never shows a
//! prompt of its own: a test host started from a terminal would ask in the terminal's name.
//!
//! The tap's sound arrives through a private aggregate device. The default output device is its
//! clock and the tap is its input. The device hands each buffer to an IO block on the host's own
//! dispatch queue and waits for it. The block converts the buffer and sends its datagrams, which
//! takes well under a millisecond. A late block costs only this device a buffer: the log counts such
//! gaps. The host builds the device again when the default output device or its sample rate changes.

use dispatch2::{DispatchQoS, DispatchQueue, DispatchQueueAttr, DispatchRetained};
use block2::RcBlock;
use objc2::{msg_send, rc::Retained, runtime::AnyClass, runtime::AnyObject, Message};
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyBufferFrameSize,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectSystemObject, kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat, AudioConvertHostTimeToNanos, AudioConvertNanosToHostTime, AudioDeviceCreateIOProcIDWithBlock,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop, AudioGetCurrentHostTime,
    AudioHardwareCreateAggregateDevice, AudioHardwareDestroyAggregateDevice, AudioObjectAddPropertyListenerBlock,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, AudioObjectRemovePropertyListenerBlock,
    AudioObjectSetPropertyData,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp, AudioTimeStampFlags};
use objc2_core_foundation::{CFDictionary, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};
use std::{
    cell::RefCell,
    ffi::{c_void, CStr},
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::Sender,
        Mutex,
    },
};

const CHANNELS: usize = 2;
const SAMPLE_RATE: u32 = cranvr_proto::message::AudioFormat::HOST_CAPTURE.sample_rate;
/// The device's buffer: 10 ms, two datagrams of 5 ms. ScreenCaptureKit handed over 20 ms at once.
const BUFFER_SECONDS: f64 = 0.01;

// Stereo frames captured since the last `take_counts`. Buffers that the host could not read.
static FRAMES: AtomicU64 = AtomicU64::new(0);
static UNREADABLE: AtomicU64 = AtomicU64::new(0);

/// Seconds of sound captured and buffers in a format the host cannot read, since the last call.
pub fn take_counts() -> (f64, u64) {
    let frames = FRAMES.swap(0, Ordering::Relaxed);
    (frames as f64 / SAMPLE_RATE as f64, UNREADABLE.swap(0, Ordering::Relaxed))
}

/// What the capture measured since the last `take_stats`, for the log
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// How old the newest sample of a buffer was when the host had it: average and largest, in ms
    pub age_ms: Option<(f64, f64)>,
    /// The loudest sample, 1 for full scale
    pub peak: f64,
    /// Buffers whose first sample did not follow the last one: sound the host never got
    pub gaps: u64,
    /// The capture's clock against the Mac's clock since the capture started, in parts per million
    pub clock_ppm: Option<f64>,
}

#[derive(Default)]
struct Measure {
    ages_ns: u64,
    age_count: u64,
    age_max_ns: u64,
    peak: u16,
    gaps: u64,
    /// The host time and the device's sample time of the first buffer, and of the newest one
    first: Option<(u64, f64)>,
    newest: Option<(u64, f64)>,
    next_sample_time: Option<f64>,
    rate: f64,
}

static MEASURE: Mutex<Option<Measure>> = Mutex::new(None);

/// The capture's numbers since the last call
pub fn take_stats() -> Stats {
    let mut guard = MEASURE.lock().unwrap();
    let Some(m) = guard.as_mut() else { return Stats::default() };
    let stats = Stats {
        age_ms: (m.age_count > 0).then(|| (m.ages_ns as f64 / m.age_count as f64 / 1e6, m.age_max_ns as f64 / 1e6)),
        peak: f64::from(m.peak) / 32768.0,
        gaps: m.gaps,
        clock_ppm: clock_ppm(m.first, m.newest, m.rate),
    };
    (m.ages_ns, m.age_count, m.age_max_ns, m.peak, m.gaps) = (0, 0, 0, 0, 0);
    stats
}

/// The frames that the device counted against the host time between two buffers, as the device's
/// rate against its nominal rate. A reading needs ten seconds.
fn clock_ppm(first: Option<(u64, f64)>, newest: Option<(u64, f64)>, rate: f64) -> Option<f64> {
    let ((t0, f0), (t1, f1)) = (first?, newest?);
    let seconds = unsafe { AudioConvertHostTimeToNanos(t1.checked_sub(t0)?) } as f64 / 1e9;
    (seconds >= 10.0 && rate > 0.0).then(|| ((f1 - f0) / rate / seconds - 1.0) * 1e6)
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
struct StreamDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

const FORMAT_FLAG_FLOAT: u32 = 1;
const FORMAT_FLAG_NON_INTERLEAVED: u32 = 1 << 5;

/// One buffer of an AudioBufferList, as Core Audio lays it out
#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    channels: u32,
    bytes: u32,
    data: *const c_void,
}

/// The buffers of a list: at most 8, one per channel when the samples are not interleaved
const MAX_BUFFERS: usize = 8;

/// Reads float or 16-bit samples, planar or interleaved, as interleaved 16-bit stereo. A mono
/// source plays on both sides; channels past the second are dropped.
unsafe fn interleave(description: &StreamDescription, buffers: &[AudioBuffer]) -> Option<Vec<u8>> {
    let float = description.format_flags & FORMAT_FLAG_FLOAT != 0;
    let bytes_per_sample = (description.bits_per_channel / 8) as usize;
    if !(float && bytes_per_sample == 4 || !float && bytes_per_sample == 2) {
        return None;
    }
    let read = |buffer: &AudioBuffer, index: usize| -> i16 {
        if float {
            let value = *(buffer.data as *const f32).add(index);
            (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
        } else {
            *(buffer.data as *const i16).add(index)
        }
    };
    let planar = description.format_flags & FORMAT_FLAG_NON_INTERLEAVED != 0;
    let mut out = Vec::new();
    if planar {
        let first = buffers.first()?;
        let second = buffers.get(1).unwrap_or(first);
        let frames = first.bytes as usize / bytes_per_sample;
        out.reserve(frames * CHANNELS * 2);
        for i in 0..frames {
            out.extend_from_slice(&read(first, i).to_ne_bytes());
            out.extend_from_slice(&read(second, i).to_ne_bytes());
        }
    } else {
        let buffer = buffers.first()?;
        let channels = (buffer.channels as usize).max(1);
        let frames = buffer.bytes as usize / bytes_per_sample / channels;
        out.reserve(frames * CHANNELS * 2);
        for i in 0..frames {
            let left = read(buffer, i * channels);
            let right = if channels > 1 { read(buffer, i * channels + 1) } else { left };
            out.extend_from_slice(&left.to_ne_bytes());
            out.extend_from_slice(&right.to_ne_bytes());
        }
    }
    Some(out)
}

/// Turns interleaved 16-bit stereo at the device's rate into 48 kHz by linear interpolation. It keeps
/// the last frame and the position between buffers, so the buffers join without a seam. Most output
/// devices run at 48 kHz, and then the samples pass as they are.
struct Resampler {
    /// Input frames per output frame
    step: f64,
    /// The position of the next output frame in the next input buffer. -1 is `last`.
    position: f64,
    last: [i16; 2],
}

impl Resampler {
    fn new(rate: f64) -> Self {
        Self { step: rate / f64::from(SAMPLE_RATE), position: 0.0, last: [0, 0] }
    }

    fn run(&mut self, input: Vec<u8>) -> Vec<u8> {
        if self.step == 1.0 {
            return input;
        }
        let frames: Vec<[i16; 2]> = input
            .as_chunks::<4>()
            .0
            .iter()
            .map(|f| [i16::from_ne_bytes([f[0], f[1]]), i16::from_ne_bytes([f[2], f[3]])])
            .collect();
        let n = frames.len();
        if n == 0 {
            return input;
        }
        let at = |i: isize| if i < 0 { self.last } else { frames[i as usize] };
        let mut out = Vec::with_capacity(((n as f64 / self.step) as usize + 2) * 4);
        while self.position < (n - 1) as f64 {
            let i = self.position.floor();
            let t = self.position - i;
            let (a, b) = (at(i as isize), at(i as isize + 1));
            for c in 0..CHANNELS {
                let value = f64::from(a[c]) + (f64::from(b[c]) - f64::from(a[c])) * t;
                out.extend_from_slice(&(value.round() as i16).to_ne_bytes());
            }
            self.position += self.step;
        }
        self.position -= n as f64;
        self.last = frames[n - 1];
        out
    }
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioHardwareCreateProcessTap(description: *mut AnyObject, tap: *mut AudioObjectID) -> i32;
    fn AudioHardwareDestroyProcessTap(tap: AudioObjectID) -> i32;
}

fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Reads a property of fixed size. `qualifier` goes along when the property needs one.
unsafe fn get<T: Default>(object: AudioObjectID, selector: u32, qualifier: Option<&i32>) -> Result<T, i32> {
    let mut value = T::default();
    let mut size = size_of::<T>() as u32;
    let (qualifier_size, qualifier) = match qualifier {
        Some(q) => (size_of::<i32>() as u32, q as *const i32 as *const c_void),
        None => (0, std::ptr::null()),
    };
    let status = AudioObjectGetPropertyData(
        object,
        NonNull::from(&address(selector)),
        qualifier_size,
        qualifier,
        NonNull::from(&mut size),
        NonNull::from(&mut value).cast(),
    );
    if status == 0 { Ok(value) } else { Err(status) }
}

/// Reads a property that holds a CFString, as a Rust string
unsafe fn get_string(object: AudioObjectID, selector: u32) -> Result<String, i32> {
    let text: *const CFString = get::<usize>(object, selector, None)? as *const CFString;
    let text = NonNull::new(text.cast_mut()).ok_or(-1)?;
    let owned = objc2_core_foundation::CFRetained::from_raw(text);
    Ok(owned.to_string())
}

/// The process object of a pid. A process that never used Core Audio has none.
fn process_object(pid: i32) -> Option<AudioObjectID> {
    unsafe { get::<AudioObjectID>(kAudioObjectSystemObject as AudioObjectID, kAudioHardwarePropertyTranslatePIDToProcessObject, Some(&pid)) }
        .ok()
        .filter(|object| *object != 0)
}

/// The processes whose sound stays out of the tap: the host, and the cranvr app that started it.
/// Neither plays sound today. The headset's own sound must never come back to it.
fn own_processes() -> Vec<AudioObjectID> {
    let mut pids = vec![std::process::id() as i32];
    let exe = std::env::current_exe().unwrap_or_default();
    if exe.to_string_lossy().contains(".app/Contents/MacOS/") {
        pids.push(unsafe { libc::getppid() });
    }
    pids.into_iter().filter_map(process_object).collect()
}

fn cf(key: &CStr) -> Retained<NSString> {
    NSString::from_str(key.to_str().unwrap_or_default())
}

/// Any Objective-C object as a value of a dictionary
fn object<T: Message>(value: Retained<T>) -> Retained<AnyObject> {
    // Every object is an AnyObject
    unsafe { Retained::cast_unchecked(value) }
}

/// The description of a stereo tap of every process but `excluded`, visible to this process alone,
/// with its UUID. Making it needs no permission: the tap itself does.
fn tap_description(excluded: &[AudioObjectID]) -> Result<(Retained<AnyObject>, Retained<NSUUID>), String> {
    let class = AnyClass::get(c"CATapDescription").ok_or("macOS lacks Core Audio taps (macOS 14.2 or later has them)")?;
    let excluded: Vec<Retained<NSNumber>> = excluded.iter().copied().map(NSNumber::new_u32).collect();
    let excluded = NSArray::from_retained_slice(&excluded);
    unsafe {
        let description: Retained<AnyObject> = msg_send![msg_send![class, alloc], initStereoGlobalTapButExcludeProcesses: &*excluded];
        let _: () = msg_send![&*description, setName: &*NSString::from_str("cranvr game sound")];
        let _: () = msg_send![&*description, setPrivate: true];
        let uuid: Retained<NSUUID> = msg_send![&*description, UUID];
        Ok((description, uuid))
    }
}

type ListenerBlock = RcBlock<dyn Fn(u32, NonNull<AudioObjectPropertyAddress>)>;

/// Sends a message on `changed` each time the default output device changes, for the life of
/// the host. The capture then starts again on the new device.
pub fn watch_default_output(changed: Sender<()>) {
    let listener: ListenerBlock = RcBlock::new(move |_count: u32, _addresses: NonNull<AudioObjectPropertyAddress>| {
        changed.send(()).ok();
    });
    let queue = DispatchQueue::new("cranvr.game-audio.devices", None);
    unsafe {
        AudioObjectAddPropertyListenerBlock(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address(kAudioHardwarePropertyDefaultOutputDevice)),
            Some(&queue),
            RcBlock::as_ptr(&listener),
        );
    }
    // The listener stays as long as the host runs
    std::mem::forget(listener);
    std::mem::forget(queue);
}

type IoBlock = RcBlock<
    dyn Fn(NonNull<AudioTimeStamp>, NonNull<AudioBufferList>, NonNull<AudioTimeStamp>, NonNull<AudioBufferList>, NonNull<AudioTimeStamp>),
>;

/// A running capture of this Mac's sound.
pub struct Capture {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    output: AudioObjectID,
    io_proc: AudioDeviceIOProcID,
    rate_listener: ListenerBlock,
    _io_block: IoBlock,
    queue: DispatchRetained<DispatchQueue>,
    /// The format, for the log
    pub summary: String,
}

// The Core Audio objects are ids, and the blocks run on the capture's own queue
unsafe impl Send for Capture {}

impl Capture {
    /// Starts the capture. `changed` gets a message when the output device's rate changes: the
    /// capture must start again then.
    pub fn start(changed: Sender<()>) -> Result<Self, String> {
        unsafe { Self::start_unchecked(changed) }
    }

    unsafe fn start_unchecked(changed: Sender<()>) -> Result<Self, String> {
        let output: AudioObjectID = get(kAudioObjectSystemObject as AudioObjectID, kAudioHardwarePropertyDefaultOutputDevice, None)
            .map_err(|e| format!("no default output device ({e})"))?;
        if output == 0 {
            return Err("this Mac has no output device".into());
        }
        let output_uid = get_string(output, kAudioDevicePropertyDeviceUID).map_err(|e| format!("the output device has no UID ({e})"))?;

        let (description, uuid) = tap_description(&own_processes())?;
        let mut tap: AudioObjectID = 0;
        let status = AudioHardwareCreateProcessTap(Retained::as_ptr(&description).cast_mut(), &mut tap);
        if status != 0 || tap == 0 {
            return Err(format!("the tap failed ({status})"));
        }
        let mut capture = Self {
            tap,
            aggregate: 0,
            output,
            io_proc: None,
            rate_listener: RcBlock::new(|_count: u32, _addresses: NonNull<AudioObjectPropertyAddress>| {}),
            _io_block: RcBlock::new(|_, _, _, _, _| {}),
            queue: DispatchQueue::new(
                "cranvr.game-audio",
                Some(&DispatchQueueAttr::with_qos_class(None, DispatchQoS::UserInteractive, 0)),
            ),
            summary: String::new(),
        };
        // From here on, Drop takes down what the start built

        let tap_list = NSArray::from_retained_slice(&[NSDictionary::<NSString, AnyObject>::from_retained_objects(
            &[&*cf(kAudioSubTapUIDKey), &*cf(kAudioSubTapDriftCompensationKey)],
            &[object(uuid.UUIDString()), object(NSNumber::new_bool(true))],
        )]);
        let devices = NSArray::from_retained_slice(&[NSDictionary::<NSString, AnyObject>::from_retained_objects(
            &[&*cf(kAudioSubDeviceUIDKey)],
            &[object(NSString::from_str(&output_uid))],
        )]);
        let keys = [
            cf(kAudioAggregateDeviceNameKey),
            cf(kAudioAggregateDeviceUIDKey),
            cf(kAudioAggregateDeviceMainSubDeviceKey),
            cf(kAudioAggregateDeviceIsPrivateKey),
            cf(kAudioAggregateDeviceIsStackedKey),
            cf(kAudioAggregateDeviceTapAutoStartKey),
            cf(kAudioAggregateDeviceSubDeviceListKey),
            cf(kAudioAggregateDeviceTapListKey),
        ];
        let values: [Retained<AnyObject>; 8] = [
            object(NSString::from_str("cranvr game sound")),
            object(NSUUID::new().UUIDString()),
            object(NSString::from_str(&output_uid)),
            object(NSNumber::new_bool(true)),
            object(NSNumber::new_bool(false)),
            object(NSNumber::new_bool(true)),
            object(devices),
            object(tap_list),
        ];
        let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let dictionary = NSDictionary::<NSString, AnyObject>::from_retained_objects(&key_refs, &values);
        // NSDictionary and CFDictionary are the same object
        let dictionary = &*(Retained::as_ptr(&dictionary) as *const CFDictionary);
        let status = AudioHardwareCreateAggregateDevice(dictionary, NonNull::from(&mut capture.aggregate));
        if status != 0 || capture.aggregate == 0 {
            return Err(format!("the aggregate device failed ({status})"));
        }

        let format: StreamDescription = get(tap, kAudioTapPropertyFormat, None).map_err(|e| format!("the tap has no format ({e})"))?;
        let rate: f64 = get(capture.aggregate, kAudioDevicePropertyNominalSampleRate, None).unwrap_or(format.sample_rate);
        if rate <= 0.0 {
            return Err("the output device reports no sample rate".into());
        }
        let mut frames = (rate * BUFFER_SECONDS).round() as u32;
        AudioObjectSetPropertyData(
            capture.aggregate,
            NonNull::from(&address(kAudioDevicePropertyBufferFrameSize)),
            0,
            std::ptr::null(),
            size_of::<u32>() as u32,
            NonNull::from(&mut frames).cast(),
        );
        let frames: u32 = get(capture.aggregate, kAudioDevicePropertyBufferFrameSize, None).unwrap_or(frames);
        capture.summary = format!(
            "{} Hz, {} channels of {}-bit {}{}, {} frames a buffer ({:.1} ms)",
            rate,
            format.channels_per_frame,
            format.bits_per_channel,
            if format.format_flags & FORMAT_FLAG_FLOAT != 0 { "float" } else { "integer" },
            if rate == f64::from(SAMPLE_RATE) { "" } else { ", resampled to 48000 Hz" },
            frames,
            f64::from(frames) * 1000.0 / rate
        );

        *MEASURE.lock().unwrap() = Some(Measure { rate, ..Default::default() });
        let resampler = RefCell::new(Resampler::new(rate));
        capture._io_block = RcBlock::new(
            move |_now: NonNull<AudioTimeStamp>,
                  input: NonNull<AudioBufferList>,
                  input_time: NonNull<AudioTimeStamp>,
                  _output: NonNull<AudioBufferList>,
                  _output_time: NonNull<AudioTimeStamp>| {
                let handed = AudioGetCurrentHostTime();
                let list = input.as_ref();
                let count = (list.mNumberBuffers as usize).min(MAX_BUFFERS);
                let first = list.mBuffers.as_ptr() as *const AudioBuffer;
                let buffers = std::slice::from_raw_parts(first, count);
                let Some(samples) = interleave(&format, buffers) else {
                    UNREADABLE.fetch_add(1, Ordering::Relaxed);
                    return;
                };
                let device_frames = (samples.len() / CHANNELS / 2) as f64;
                let samples = resampler.borrow_mut().run(samples);
                FRAMES.fetch_add((samples.len() / CHANNELS / 2) as u64, Ordering::Relaxed);
                measure(input_time.as_ref(), device_frames, handed, &samples);
                crate::headset::native::audio::send(&samples);
            },
        );
        let status = AudioDeviceCreateIOProcIDWithBlock(
            NonNull::from(&mut capture.io_proc),
            capture.aggregate,
            Some(&capture.queue),
            RcBlock::as_ptr(&capture._io_block),
        );
        if status != 0 || capture.io_proc.is_none() {
            return Err(format!("the IO block failed ({status})"));
        }
        // A notice without a new rate changes nothing
        capture.rate_listener = RcBlock::new(move |_count: u32, _addresses: NonNull<AudioObjectPropertyAddress>| {
            if get::<f64>(output, kAudioDevicePropertyNominalSampleRate, None).is_ok_and(|now| now != rate) {
                changed.send(()).ok();
            }
        });
        AudioObjectAddPropertyListenerBlock(
            output,
            NonNull::from(&address(kAudioDevicePropertyNominalSampleRate)),
            Some(&capture.queue),
            RcBlock::as_ptr(&capture.rate_listener),
        );
        let status = AudioDeviceStart(capture.aggregate, capture.io_proc);
        if status != 0 {
            return Err(format!("the aggregate device did not start ({status})"));
        }
        Ok(capture)
    }
}

/// Counts the age of the buffer's newest sample, the gaps between buffers, the device's clock and
/// the peak level.
fn measure(time: &AudioTimeStamp, device_frames: f64, handed: u64, samples: &[u8]) {
    let mut guard = MEASURE.lock().unwrap();
    let Some(m) = guard.as_mut() else { return };
    let peak = samples.as_chunks::<2>().0.iter().map(|b| i16::from_ne_bytes(*b).unsigned_abs()).max().unwrap_or(0);
    m.peak = m.peak.max(peak);
    let flags = time.mFlags;
    let sample_time = flags.contains(AudioTimeStampFlags::SampleTimeValid);
    if sample_time {
        if m.next_sample_time.is_some_and(|next| (time.mSampleTime - next).abs() >= 1.0) {
            m.gaps += 1;
        }
        m.next_sample_time = Some(time.mSampleTime + device_frames);
    }
    if flags.contains(AudioTimeStampFlags::HostTimeValid) && m.rate > 0.0 {
        let duration = unsafe { AudioConvertNanosToHostTime((device_frames / m.rate * 1e9) as u64) };
        let newest = time.mHostTime + duration;
        let age = unsafe { AudioConvertHostTimeToNanos(handed.saturating_sub(newest)) };
        m.ages_ns += age;
        m.age_count += 1;
        m.age_max_ns = m.age_max_ns.max(age);
        // The device's own count of frames against the host time: a lost buffer does not change it
        if sample_time {
            m.first.get_or_insert((time.mHostTime, time.mSampleTime));
            m.newest = Some((time.mHostTime, time.mSampleTime));
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            AudioObjectRemovePropertyListenerBlock(
                self.output,
                NonNull::from(&address(kAudioDevicePropertyNominalSampleRate)),
                Some(&self.queue),
                RcBlock::as_ptr(&self.rate_listener),
            );
            if self.aggregate != 0 {
                if self.io_proc.is_some() {
                    AudioDeviceStop(self.aggregate, self.io_proc);
                    AudioDeviceDestroyIOProcID(self.aggregate, self.io_proc);
                }
                AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            AudioHardwareDestroyProcessTap(self.tap);
        }
        *MEASURE.lock().unwrap() = None;
    }
}

#[cfg(test)]
#[path = "tests/game_audio.rs"]
mod tests;
