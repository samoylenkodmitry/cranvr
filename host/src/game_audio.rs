//! The game sound in the headset. ScreenCaptureKit captures what this Mac plays, from every app
//! but the host. The host converts it to interleaved 16-bit stereo at 48 kHz and hands it to the
//! headset link (`headset/native/audio.rs`).
//!
//! The capture takes the sound before the Mac's volume, so the Mac can stay muted while the
//! headset plays it. It needs Screen Recording, as the desktop view does.

use crate::screen_capture::{self, SendBox};
use dispatch2::DispatchQueue;
use objc2::{define_class, msg_send, rc::Retained, runtime::ProtocolObject, AllocAnyThread};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{SCContentFilter, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicU64, Ordering},
};

const CHANNELS: usize = 2;
const SAMPLE_RATE: u32 = cranvr_proto::message::AudioFormat::HOST_CAPTURE.sample_rate;

// Stereo frames captured since the last `take_counts`. Buffers that the host could not read.
static FRAMES: AtomicU64 = AtomicU64::new(0);
static UNREADABLE: AtomicU64 = AtomicU64::new(0);

/// Seconds of sound captured and buffers in a format the host cannot read, since the last call.
pub fn take_counts() -> (f64, u64) {
    let frames = FRAMES.swap(0, Ordering::Relaxed);
    (frames as f64 / SAMPLE_RATE as f64, UNREADABLE.swap(0, Ordering::Relaxed))
}

#[repr(C)]
#[derive(Default)]
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

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    channels: u32,
    bytes: u32,
    data: *const c_void,
}

/// An AudioBufferList with room for 8 buffers: one per channel when the samples are not
/// interleaved.
#[repr(C)]
struct AudioBufferList {
    count: u32,
    buffers: [AudioBuffer; 8],
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetFormatDescription(sample: *const c_void) -> *const c_void;
    fn CMAudioFormatDescriptionGetStreamBasicDescription(format: *const c_void) -> *const StreamDescription;
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sample: *const c_void,
        size_needed: *mut usize,
        list: *mut AudioBufferList,
        list_size: usize,
        structure_allocator: *const c_void,
        block_allocator: *const c_void,
        flags: u32,
        block_buffer: *mut *const c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *mut c_void);
}

// kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, as in Apple's examples
const ASSURE_16_BYTE_ALIGNMENT: u32 = 1 << 0;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "CranvrGameAudioOutput"]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind == SCStreamOutputType::Audio {
                match unsafe { convert(sample as *const CMSampleBuffer as *const c_void) } {
                    Some(samples) => {
                        FRAMES.fetch_add((samples.len() / CHANNELS / 2) as u64, Ordering::Relaxed);
                        crate::headset::native::audio::send(&samples);
                    }
                    None => {
                        UNREADABLE.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
);

impl Output {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// The samples of one audio buffer from ScreenCaptureKit as interleaved 16-bit stereo.
unsafe fn convert(sample: *const c_void) -> Option<Vec<u8>> {
    let description = CMAudioFormatDescriptionGetStreamBasicDescription(CMSampleBufferGetFormatDescription(sample));
    let description = description.as_ref()?;
    let mut list = AudioBufferList { count: 0, buffers: [AudioBuffer { channels: 0, bytes: 0, data: std::ptr::null() }; 8] };
    let mut block: *const c_void = std::ptr::null();
    // CoreMedia wants the exact size of the list: a larger size fails with
    // kCMSampleBufferError_ArrayTooSmall
    let mut size = 0usize;
    let null = std::ptr::null();
    CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sample,
        &mut size,
        std::ptr::null_mut(),
        0,
        null,
        null,
        ASSURE_16_BYTE_ALIGNMENT,
        std::ptr::null_mut(),
    );
    if size == 0 || size > size_of::<AudioBufferList>() {
        return None;
    }
    let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sample,
        std::ptr::null_mut(),
        &mut list,
        size,
        null,
        null,
        ASSURE_16_BYTE_ALIGNMENT,
        &mut block,
    );
    if status != 0 {
        return None;
    }
    let buffers = &list.buffers[..(list.count as usize).min(8)];
    let samples = interleave(description, buffers);
    if !block.is_null() {
        CFRelease(block.cast_mut());
    }
    samples
}

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

/// A running capture of this Mac's sound.
pub struct Capture {
    _stream: SendBox<Retained<SCStream>>,
    _output: SendBox<Retained<Output>>,
    _queue: SendBox<dispatch2::DispatchRetained<DispatchQueue>>,
}

impl Capture {
    pub fn start() -> Result<Self, String> {
        let display = screen_capture::main_display()?;
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new())
        };
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setCapturesAudio(true);
            config.setSampleRate(SAMPLE_RATE as isize);
            config.setChannelCount(CHANNELS as isize);
            config.setExcludesCurrentProcessAudio(true);
            // The stream also captures the display. The smallest picture once per second costs
            // nearly nothing. No output takes it.
            config.setWidth(2);
            config.setHeight(2);
            config.setMinimumFrameInterval(CMTime { value: 1, timescale: 1, flags: CMTimeFlags::Valid, epoch: 0 });
            config.setShowsCursor(false);
        }
        let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None) };
        let output = Output::new();
        let queue = DispatchQueue::new("cranvr.game-audio", None);
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Audio,
                Some(&queue),
            )
        }
        .map_err(|e| format!("audio output: {}", e.localizedDescription()))?;
        screen_capture::start(&stream)?;
        Ok(Self { _stream: SendBox(stream), _output: SendBox(output), _queue: SendBox(queue) })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe { self._stream.0.stopCaptureWithCompletionHandler(None) };
    }
}

#[cfg(test)]
#[path = "tests/game_audio.rs"]
mod tests;
