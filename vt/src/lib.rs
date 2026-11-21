//! HEVC encode and decode with VideoToolbox, in the form of the stream: Annex-B frames, with the
//! VPS, SPS and PPS of each IDR frame returned apart from the frame data.

pub mod ffi;

use ffi::*;
use std::{
    collections::VecDeque,
    ffi::c_void,
    ptr,
    sync::{mpsc::Sender, Mutex},
    time::{Duration, Instant},
};

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// One NV12 frame from the encoder's pool. The Metal renderer writes into it.
pub struct PixelBuffer(CVPixelBufferRef);

// CVPixelBuffer is reference counted and safe to pass between threads.
unsafe impl Send for PixelBuffer {}

impl PixelBuffer {
    pub fn raw(&self) -> CVPixelBufferRef {
        self.0
    }

    /// Wraps a BGRA IOSurface that another process shares by its global ID (a game's swapchain
    /// image under Wine). IDs are global and can be reused, so the caller checks the size.
    pub fn from_iosurface_id(id: u32) -> Option<Self> {
        unsafe {
            let surface = IOSurfaceLookup(id);
            if surface.is_null() {
                return None;
            }
            let mut buffer = ptr::null_mut();
            let status = CVPixelBufferCreateWithIOSurface(kCFAllocatorDefault, surface, ptr::null(), &mut buffer);
            CFRelease(surface);
            if status != 0 || buffer.is_null() {
                return None;
            }
            Some(PixelBuffer(buffer))
        }
    }

    /// A new IOSurface-backed BGRA buffer that Metal can use.
    pub fn new_bgra(width: usize, height: usize) -> Option<Self> {
        Self::bgra(width, height, false)
    }

    /// Like `new_bgra`, but other processes can open the surface by `iosurface_id()`. Tests use
    /// it to stand in for a game's swapchain image.
    pub fn new_bgra_shared(width: usize, height: usize) -> Option<Self> {
        Self::bgra(width, height, true)
    }

    /// The global ID of the IOSurface behind this buffer.
    pub fn iosurface_id(&self) -> Option<u32> {
        let surface = unsafe { CVPixelBufferGetIOSurface(self.0) };
        (!surface.is_null()).then(|| unsafe { IOSurfaceGetID(surface) })
    }

    fn bgra(width: usize, height: usize, shared: bool) -> Option<Self> {
        unsafe {
            let empty = if shared { dictionary(&[(kIOSurfaceIsGlobal, kCFBooleanTrue)]) } else { dictionary(&[]) };
            let attrs = dictionary(&[
                (kCVPixelBufferIOSurfacePropertiesKey, empty),
                (kCVPixelBufferMetalCompatibilityKey, kCFBooleanTrue),
            ]);
            let mut buffer = ptr::null_mut();
            let status = CVPixelBufferCreate(kCFAllocatorDefault, width, height, kCVPixelFormatType_32BGRA, attrs, &mut buffer);
            CFRelease(attrs);
            CFRelease(empty);
            (status == 0 && !buffer.is_null()).then_some(PixelBuffer(buffer))
        }
    }

    /// Calls `f` with the single plane of a BGRA buffer and its bytes per row.
    pub fn with_bgra_mut<R>(&mut self, f: impl FnOnce(&mut [u8], usize) -> R) -> R {
        unsafe {
            CVPixelBufferLockBaseAddress(self.0, 0);
            let stride = CVPixelBufferGetBytesPerRowOfPlane(self.0, 0).max(cv_bytes_per_row(self.0));
            let data = std::slice::from_raw_parts_mut(cv_base_address(self.0) as *mut u8, stride * self.height());
            let r = f(data, stride);
            CVPixelBufferUnlockBaseAddress(self.0, 0);
            r
        }
    }

    /// Calls `f` with the single plane of a buffer of 4-byte pixels and its bytes per row, for CPU
    /// reads. The lock is read-only. `None` when the buffer cannot be locked.
    pub fn with_pixels<R>(&self, f: impl FnOnce(&[u8], usize) -> R) -> Option<R> {
        const LOCK_READ_ONLY: u64 = 1;
        unsafe {
            if CVPixelBufferLockBaseAddress(self.0, LOCK_READ_ONLY) != 0 {
                return None;
            }
            let base = cv_base_address(self.0) as *const u8;
            let stride = cv_bytes_per_row(self.0);
            let result = (!base.is_null()).then(|| f(std::slice::from_raw_parts(base, stride * self.height()), stride));
            CVPixelBufferUnlockBaseAddress(self.0, LOCK_READ_ONLY);
            result
        }
    }

    pub fn pixel_format(&self) -> u32 {
        unsafe { CVPixelBufferGetPixelFormatType(self.0) }
    }

    pub fn width(&self) -> usize {
        unsafe { CVPixelBufferGetWidth(self.0) }
    }

    pub fn height(&self) -> usize {
        unsafe { CVPixelBufferGetHeight(self.0) }
    }

    /// Calls `f` with (Y plane, Y bytes per row, CbCr plane, CbCr bytes per row), for CPU writes.
    pub fn with_planes_mut<R>(&mut self, f: impl FnOnce(&mut [u8], usize, &mut [u8], usize) -> R) -> R {
        unsafe {
            CVPixelBufferLockBaseAddress(self.0, 0);
            let h = self.height();
            let y_stride = CVPixelBufferGetBytesPerRowOfPlane(self.0, 0);
            let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(self.0, 1);
            let y = std::slice::from_raw_parts_mut(CVPixelBufferGetBaseAddressOfPlane(self.0, 0) as *mut u8, y_stride * h);
            let uv = std::slice::from_raw_parts_mut(
                CVPixelBufferGetBaseAddressOfPlane(self.0, 1) as *mut u8,
                uv_stride * h.div_ceil(2),
            );
            let r = f(y, y_stride, uv, uv_stride);
            CVPixelBufferUnlockBaseAddress(self.0, 0);
            r
        }
    }
}

unsafe fn cv_base_address(buffer: CVPixelBufferRef) -> *mut c_void {
    extern "C" {
        fn CVPixelBufferGetBaseAddress(buffer: CVPixelBufferRef) -> *mut c_void;
    }
    CVPixelBufferGetBaseAddress(buffer)
}

unsafe fn cv_bytes_per_row(buffer: CVPixelBufferRef) -> usize {
    extern "C" {
        fn CVPixelBufferGetBytesPerRow(buffer: CVPixelBufferRef) -> usize;
    }
    CVPixelBufferGetBytesPerRow(buffer)
}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        unsafe { CVBufferRelease(self.0) }
    }
}

impl Clone for PixelBuffer {
    fn clone(&self) -> Self {
        unsafe { PixelBuffer(CVBufferRetain(self.0)) }
    }
}

impl PixelBuffer {
    /// Wraps a buffer that someone else owns, and keeps it alive.
    ///
    /// # Safety
    /// `raw` must be a valid CVPixelBuffer.
    pub unsafe fn retain_raw(raw: CVPixelBufferRef) -> Self {
        PixelBuffer(CVBufferRetain(raw))
    }
}

pub struct EncodedFrame {
    pub timestamp_ns: u64,
    pub is_idr: bool,
    /// VPS, SPS and PPS in Annex-B form. Set on IDR frames only.
    pub config_nals: Option<Vec<u8>>,
    /// The frame NALs in Annex-B form, without the parameter sets.
    pub data: Vec<u8>,
    /// From the encode call to the encoded frame
    pub encode_time: Duration,
}

pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub bitrate_bps: u64,
    pub full_range: bool,
    /// VideoToolbox's low-latency rate control: each frame stays close to the average size, and
    /// the encoder takes one frame at a time. Without it the encoder overlaps frames: on an M5 a
    /// frame took 3.6 to 5.0 ms instead of 6.1 to 6.3 ms, and the encoder kept up with 232 frames a
    /// second instead of 170. A keyframe then may come out larger.
    pub strict_rate: bool,
    /// The longest run of frames without a key frame, as time at `fps`. `None`: a key frame comes
    /// only first and when the caller forces one.
    pub key_every: Option<Duration>,
    /// The highest quantizer that a frame may use, for a floor under the quality. `None` leaves it to
    /// the rate control.
    pub max_qp: Option<u8>,
}

/// The key frame interval of the eye stream
pub const KEY_EVERY: Duration = Duration::from_secs(10);

/// Hard limits on the data that the encoder sends without strict rate control: for each window,
/// the share of the average bitrate and the window's length in seconds.
const RATE_WINDOWS: [(f64, f64); 1] = [(1.0, 1.0)];

/// What the output callback needs: where frames go, and when each frame went in.
struct Output {
    sender: Sender<EncodedFrame>,
    submitted: Mutex<VecDeque<(u64, Instant)>>,
}

impl Output {
    // The frame's submit time, which also leaves the count of frames in flight
    fn take_submitted(&self, timestamp_ns: u64) -> Option<Instant> {
        let mut submitted = self.submitted.lock().unwrap();
        let index = submitted.iter().position(|(t, _)| *t == timestamp_ns)?;
        submitted.remove(index).map(|(_, at)| at)
    }
}

pub struct Encoder {
    session: VTCompressionSessionRef,
    output: *mut Output,
    full_range: bool,
    strict_rate: bool,
}

unsafe impl Send for Encoder {}
// The host takes buffers from the pool on one thread and encodes on another. CVPixelBufferPool
// is thread safe, and only one thread calls encode and set_bitrate.
unsafe impl Sync for Encoder {}

impl Encoder {
    /// Encoded frames arrive on `sender`, from a VideoToolbox thread.
    pub fn new(config: &EncoderConfig, sender: Sender<EncodedFrame>) -> Result<Self, String> {
        unsafe {
            let output = Box::into_raw(Box::new(Output { sender, submitted: Mutex::new(VecDeque::new()) }));
            let format = if config.full_range {
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
            } else {
                kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            };
            let format_num = number_i64(format as i64);
            let width_num = number_i64(config.width as i64);
            let height_num = number_i64(config.height as i64);
            let empty = dictionary(&[]);
            let source_attrs = dictionary(&[
                (kCVPixelBufferPixelFormatTypeKey, format_num),
                (kCVPixelBufferWidthKey, width_num),
                (kCVPixelBufferHeightKey, height_num),
                (kCVPixelBufferIOSurfacePropertiesKey, empty),
                (kCVPixelBufferMetalCompatibilityKey, kCFBooleanTrue),
            ]);
            let spec = if config.strict_rate {
                dictionary(&[(kVTVideoEncoderSpecification_EnableLowLatencyRateControl, kCFBooleanTrue)])
            } else {
                dictionary(&[])
            };

            let mut session = ptr::null_mut();
            let status = VTCompressionSessionCreate(
                kCFAllocatorDefault,
                config.width as i32,
                config.height as i32,
                kCMVideoCodecType_HEVC,
                spec,
                source_attrs,
                kCFAllocatorDefault,
                Some(output_callback),
                output as *mut c_void,
                &mut session,
            );
            for cf in [format_num, width_num, height_num, empty, source_attrs, spec] {
                CFRelease(cf);
            }
            if status != 0 || session.is_null() {
                drop(Box::from_raw(output));
                return Err(format!("VTCompressionSessionCreate failed: {status}"));
            }

            let encoder = Self { session, output, full_range: config.full_range, strict_rate: config.strict_rate };
            encoder.set(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue)?;
            encoder.set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_HEVC_Main_AutoLevel)?;
            encoder.set(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse)?;
            encoder.set(kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, kCFBooleanTrue).ok();
            encoder.set(kVTCompressionPropertyKey_MaximizePowerEfficiency, kCFBooleanFalse).ok();
            encoder.set_number(kVTCompressionPropertyKey_ExpectedFrameRate, config.fps as f64)?;
            // The host asks for an IDR frame when a client needs one; a long interval avoids bitrate spikes.
            // 0 means no interval: key frames come only when the host asks.
            let key_interval = config.key_every.map_or(f64::from(i32::MAX), |every| (config.fps * every.as_secs_f32()).round() as f64);
            encoder.set_number(kVTCompressionPropertyKey_MaxKeyFrameInterval, key_interval)?;
            // A cap on the quantizer is a floor under the quality. The encoder spends more bits on a hard
            // picture, or drops it, instead of blurring it. macOS 12 and later.
            if let Some(qp) = config.max_qp {
                encoder.set_number(kVTCompressionPropertyKey_MaxAllowedFrameQP, f64::from(qp)).ok();
            }
            encoder.set_bitrate(config.bitrate_bps);
            VTCompressionSessionPrepareToEncodeFrames(session);
            Ok(encoder)
        }
    }

    pub fn full_range(&self) -> bool {
        self.full_range
    }

    unsafe fn set(&self, key: CFStringRef, value: CFTypeRef) -> Result<(), String> {
        match VTSessionSetProperty(self.session, key, value) {
            0 => Ok(()),
            status => Err(format!("VTSessionSetProperty failed: {status}")),
        }
    }

    unsafe fn set_number(&self, key: CFStringRef, value: f64) -> Result<(), String> {
        let n = number_f64(value);
        let r = self.set(key, n);
        CFRelease(n);
        r
    }

    pub fn set_bitrate(&self, bitrate_bps: u64) {
        unsafe {
            let n = number_i64(bitrate_bps as i64);
            VTSessionSetProperty(self.session, kVTCompressionPropertyKey_AverageBitRate, n);
            CFRelease(n);
            // Without strict rate control the average alone did not hold: at an 8 Mbps target the
            // encoder sent 12 to 16 Mbps. A hard limit per window makes it follow the target,
            // which the host can lower when the network falls behind.
            if !self.strict_rate {
                let limits: Vec<CFNumberRef> = RATE_WINDOWS
                    .iter()
                    .flat_map(|&(share, seconds)| {
                        let bytes = bitrate_bps as f64 / 8.0 * seconds * share;
                        [number_i64(bytes as i64), number_f64(seconds)]
                    })
                    .collect();
                let array = CFArrayCreate(
                    kCFAllocatorDefault,
                    limits.as_ptr(),
                    limits.len() as CFIndex,
                    &kCFTypeArrayCallBacks,
                );
                VTSessionSetProperty(self.session, kVTCompressionPropertyKey_DataRateLimits, array);
                CFRelease(array);
                for number in limits {
                    CFRelease(number);
                }
            }
        }
    }

    /// A free buffer from the encoder's pool (IOSurface backed, usable by Metal).
    pub fn pixel_buffer(&self) -> Option<PixelBuffer> {
        unsafe {
            let pool = VTCompressionSessionGetPixelBufferPool(self.session);
            if pool.is_null() {
                return None;
            }
            let mut buffer = ptr::null_mut();
            if CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &mut buffer) != 0 || buffer.is_null() {
                return None;
            }
            Some(PixelBuffer(buffer))
        }
    }

    /// Frames handed to the encoder whose output has not come yet.
    pub fn in_flight(&self) -> usize {
        unsafe { (*self.output).submitted.lock().unwrap().len() }
    }

    /// Queues one frame. `presentation` places the frame on the encoder's time line and must grow from
    /// frame to frame. The rate control spreads the average bitrate over that time line: frames that
    /// stand 1 ms apart on it get the bits of 1 ms each, whatever time passes between the calls.
    /// `timestamp_ns` comes back with the output.
    pub fn encode(&self, buffer: &PixelBuffer, presentation: Duration, timestamp_ns: u64, force_idr: bool) -> Result<(), String> {
        unsafe {
            (*self.output).submitted.lock().unwrap().push_back((timestamp_ns, Instant::now()));
            let props = force_idr.then(|| dictionary(&[(kVTEncodeFrameOptionKey_ForceKeyFrame, kCFBooleanTrue)]));
            let status = VTCompressionSessionEncodeFrame(
                self.session,
                buffer.0,
                CMTime::new(presentation.as_micros() as i64, 1_000_000),
                CMTime::INVALID,
                props.unwrap_or(ptr::null()),
                timestamp_ns as *mut c_void,
                ptr::null_mut(),
            );
            if let Some(p) = props {
                CFRelease(p);
            }
            match status {
                0 => Ok(()),
                _ => {
                    (*self.output).take_submitted(timestamp_ns);
                    Err(format!("VTCompressionSessionEncodeFrame failed: {status}"))
                }
            }
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            VTCompressionSessionCompleteFrames(self.session, CMTime::INVALID);
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session);
            drop(Box::from_raw(self.output));
        }
    }
}

extern "C" fn output_callback(
    output_refcon: *mut c_void,
    source_frame_refcon: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    sample: CMSampleBufferRef,
) {
    let output = unsafe { &*(output_refcon as *const Output) };
    let timestamp_ns = source_frame_refcon as u64;
    // A dropped frame also leaves the frames in flight
    let submitted = output.take_submitted(timestamp_ns);
    if status != 0 || sample.is_null() || info_flags & kVTEncodeInfo_FrameDropped != 0 {
        return;
    }
    if let Some(mut frame) = unsafe { read_sample(sample, timestamp_ns) } {
        frame.encode_time = submitted.map_or(Duration::ZERO, |at| at.elapsed());
        output.sender.send(frame).ok();
    }
}

unsafe fn read_sample(sample: CMSampleBufferRef, timestamp_ns: u64) -> Option<EncodedFrame> {
    let mut is_idr = true;
    let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, 0);
    if !attachments.is_null() && CFArrayGetCount(attachments) > 0 {
        let first = CFArrayGetValueAtIndex(attachments, 0);
        let not_sync = CFDictionaryGetValue(first, kCMSampleAttachmentKey_NotSync);
        if !not_sync.is_null() && CFBooleanGetValue(not_sync) != 0 {
            is_idr = false;
        }
    }

    let config_nals = if is_idr { parameter_sets(CMSampleBufferGetFormatDescription(sample)) } else { None };

    let block = CMSampleBufferGetDataBuffer(sample);
    if block.is_null() {
        return None;
    }
    let len = CMBlockBufferGetDataLength(block);
    let mut avcc = vec![0u8; len];
    if CMBlockBufferCopyDataBytes(block, 0, len, avcc.as_mut_ptr() as *mut c_void) != 0 {
        return None;
    }
    Some(EncodedFrame { timestamp_ns, is_idr, config_nals, data: avcc_to_annex_b(&avcc), encode_time: Duration::ZERO })
}

unsafe fn parameter_sets(format: CMFormatDescriptionRef) -> Option<Vec<u8>> {
    if format.is_null() {
        return None;
    }
    let mut count = 0;
    if CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(format, 0, ptr::null_mut(), ptr::null_mut(), &mut count, ptr::null_mut()) != 0 {
        return None;
    }
    let mut out = Vec::new();
    for i in 0..count {
        let mut p = ptr::null();
        let mut size = 0;
        if CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(format, i, &mut p, &mut size, ptr::null_mut(), ptr::null_mut()) == 0 {
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(std::slice::from_raw_parts(p, size));
        }
    }
    Some(out)
}

/// Mirrors pictures top to bottom into another buffer, on the Mac's media hardware
/// (VTPixelRotationSession). The desktop layer needs it: the Quest's compositor reads a decoder's
/// surface bottom row first.
pub struct Flipper(*mut c_void);

unsafe impl Send for Flipper {}

impl Flipper {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let mut session = ptr::null_mut();
            let status = VTPixelRotationSessionCreate(kCFAllocatorDefault, &mut session);
            if status != 0 || session.is_null() {
                return Err(format!("VTPixelRotationSessionCreate failed: {status}"));
            }
            let flipper = Self(session);
            match VTSessionSetProperty(session, kVTPixelRotationPropertyKey_FlipVerticalOrientation, kCFBooleanTrue) {
                0 => Ok(flipper),
                status => Err(format!("the rotation session takes no vertical flip: {status}")),
            }
        }
    }

    /// Writes `source` mirrored top to bottom into `destination`, which has the same size.
    pub fn flip(&self, source: &PixelBuffer, destination: &PixelBuffer) -> Result<(), String> {
        match unsafe { VTPixelRotationSessionRotateImage(self.0, source.raw(), destination.raw()) } {
            0 => Ok(()),
            status => Err(format!("VTPixelRotationSessionRotateImage failed: {status}")),
        }
    }
}

impl Drop for Flipper {
    fn drop(&mut self) {
        unsafe {
            VTPixelRotationSessionInvalidate(self.0);
            CFRelease(self.0 as CFTypeRef);
        }
    }
}

/// 4-byte length prefixes (VideoToolbox) to start codes (Annex-B).
pub fn avcc_to_annex_b(avcc: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(avcc.len() + 16);
    let mut off = 0;
    while off + 4 <= avcc.len() {
        let n = u32::from_be_bytes([avcc[off], avcc[off + 1], avcc[off + 2], avcc[off + 3]]) as usize;
        off += 4;
        if n == 0 || off + n > avcc.len() {
            break;
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&avcc[off..off + n]);
        off += n;
    }
    out
}

/// Splits an Annex-B stream (3- or 4-byte start codes) into NAL units without start codes.
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map_or(data.len(), |&next| next - 3);
        // a 4-byte start code leaves one more zero before the next NAL
        while end > s && data[end - 1] == 0 && k + 1 < starts.len() {
            end -= 1;
        }
        if end > s {
            nals.push(&data[s..end]);
        }
    }
    nals
}

/// Y, Cb and Cr read at one point of a decoded frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub y: u8,
    pub cb: u8,
    pub cr: u8,
}

pub struct DecodedFrame {
    pub timestamp_ns: u64,
    pub width: usize,
    pub height: usize,
    pub samples: Vec<Sample>,
    /// The whole Y plane, `width` bytes a row, when the decoder was made with [`Decoder::with_luma`].
    /// It is empty otherwise.
    pub luma: Vec<u8>,
}

struct DecoderOutput {
    sender: Sender<DecodedFrame>,
    /// Points to read, as fractions of the frame width and height
    points: Vec<(f32, f32)>,
    /// Copy the whole Y plane of each frame
    luma: bool,
}

/// An HEVC decoder for tests: it reads a few points from each frame instead of copying it.
pub struct Decoder {
    session: VTDecompressionSessionRef,
    format: CMFormatDescriptionRef,
    output: *mut DecoderOutput,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(config_nals: &[u8], points: Vec<(f32, f32)>, sender: Sender<DecodedFrame>) -> Result<Self, String> {
        Self::open(config_nals, points, false, sender)
    }

    /// A decoder that also copies the whole Y plane of each frame, so that a test can compare the
    /// decoded picture with the one that went into the encoder.
    pub fn with_luma(config_nals: &[u8], sender: Sender<DecodedFrame>) -> Result<Self, String> {
        Self::open(config_nals, Vec::new(), true, sender)
    }

    fn open(config_nals: &[u8], points: Vec<(f32, f32)>, luma: bool, sender: Sender<DecodedFrame>) -> Result<Self, String> {
        unsafe {
            let nals = split_annex_b(config_nals);
            if nals.len() < 3 {
                return Err(format!("expected VPS, SPS and PPS, got {} NALs", nals.len()));
            }
            let pointers: Vec<*const u8> = nals.iter().map(|n| n.as_ptr()).collect();
            let sizes: Vec<usize> = nals.iter().map(|n| n.len()).collect();
            let mut format = ptr::null_mut();
            let status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                kCFAllocatorDefault,
                nals.len(),
                pointers.as_ptr(),
                sizes.as_ptr(),
                4,
                ptr::null(),
                &mut format,
            );
            if status != 0 {
                return Err(format!("CMVideoFormatDescriptionCreateFromHEVCParameterSets failed: {status}"));
            }

            let pixel_format = number_i64(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange as i64);
            let attrs = dictionary(&[(kCVPixelBufferPixelFormatTypeKey, pixel_format)]);
            let output = Box::into_raw(Box::new(DecoderOutput { sender, points, luma }));
            let record = VTDecompressionOutputCallbackRecord { callback: decode_callback, refcon: output as *mut c_void };
            let mut session = ptr::null_mut();
            let status = VTDecompressionSessionCreate(kCFAllocatorDefault, format, ptr::null(), attrs, &record, &mut session);
            CFRelease(pixel_format);
            CFRelease(attrs);
            if status != 0 {
                CFRelease(format);
                drop(Box::from_raw(output));
                return Err(format!("VTDecompressionSessionCreate failed: {status}"));
            }
            Ok(Self { session, format, output })
        }
    }

    /// Decodes one Annex-B frame and waits for it.
    pub fn decode(&mut self, timestamp_ns: u64, annex_b: &[u8]) -> Result<(), String> {
        let mut avcc = Vec::with_capacity(annex_b.len() + 16);
        for nal in split_annex_b(annex_b) {
            avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            avcc.extend_from_slice(nal);
        }
        unsafe {
            let mut block = ptr::null_mut();
            let status = CMBlockBufferCreateWithMemoryBlock(
                kCFAllocatorDefault,
                ptr::null_mut(),
                avcc.len(),
                kCFAllocatorDefault,
                ptr::null(),
                0,
                avcc.len(),
                kCMBlockBufferAssureMemoryNowFlag,
                &mut block,
            );
            if status != 0 {
                return Err(format!("CMBlockBufferCreateWithMemoryBlock failed: {status}"));
            }
            CMBlockBufferReplaceDataBytes(avcc.as_ptr() as *const c_void, block, 0, avcc.len());
            let mut sample = ptr::null_mut();
            let size = avcc.len();
            let status = CMSampleBufferCreateReady(kCFAllocatorDefault, block, self.format, 1, 0, ptr::null(), 1, &size, &mut sample);
            CFRelease(block);
            if status != 0 {
                return Err(format!("CMSampleBufferCreateReady failed: {status}"));
            }
            let status = VTDecompressionSessionDecodeFrame(self.session, sample, 0, timestamp_ns as *mut c_void, ptr::null_mut());
            VTDecompressionSessionWaitForAsynchronousFrames(self.session);
            CFRelease(sample);
            match status {
                0 => Ok(()),
                _ => Err(format!("VTDecompressionSessionDecodeFrame failed: {status}")),
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            VTDecompressionSessionWaitForAsynchronousFrames(self.session);
            VTDecompressionSessionInvalidate(self.session);
            CFRelease(self.session);
            CFRelease(self.format);
            drop(Box::from_raw(self.output));
        }
    }
}

extern "C" fn decode_callback(
    refcon: *mut c_void,
    source_frame_refcon: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    image: CVPixelBufferRef,
    _pts: CMTime,
    _duration: CMTime,
) {
    if status != 0 || image.is_null() {
        return;
    }
    let output = unsafe { &*(refcon as *const DecoderOutput) };
    unsafe {
        CVPixelBufferLockBaseAddress(image, 1);
        let width = CVPixelBufferGetWidth(image);
        let height = CVPixelBufferGetHeight(image);
        let y_plane = CVPixelBufferGetBaseAddressOfPlane(image, 0) as *const u8;
        let uv_plane = CVPixelBufferGetBaseAddressOfPlane(image, 1) as *const u8;
        let y_stride = CVPixelBufferGetBytesPerRowOfPlane(image, 0);
        let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(image, 1);
        let samples = output
            .points
            .iter()
            .map(|&(fx, fy)| {
                let x = ((fx * width as f32) as usize).min(width - 1);
                let y = ((fy * height as f32) as usize).min(height - 1);
                let uv = uv_plane.add((y / 2) * uv_stride + (x / 2) * 2);
                Sample { y: *y_plane.add(y * y_stride + x), cb: *uv, cr: *uv.add(1) }
            })
            .collect();
        let luma = if output.luma {
            (0..height).flat_map(|row| std::slice::from_raw_parts(y_plane.add(row * y_stride), width)).copied().collect()
        } else {
            Vec::new()
        };
        CVPixelBufferUnlockBaseAddress(image, 1);
        output
            .sender
            .send(DecodedFrame { timestamp_ns: source_frame_refcon as u64, width, height, samples, luma })
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn split_handles_three_and_four_byte_start_codes() {
        let data = [0, 0, 0, 1, 0x40, 1, 0, 0, 1, 0x42, 2, 3, 0, 0, 0, 1, 0x44, 4];
        let nals = split_annex_b(&data);
        assert_eq!(nals, vec![&[0x40, 1][..], &[0x42, 2, 3][..], &[0x44, 4][..]]);
    }

    #[test]
    fn avcc_converts_to_annex_b() {
        let avcc = [0, 0, 0, 2, 0x40, 1, 0, 0, 0, 1, 0x42];
        assert_eq!(avcc_to_annex_b(&avcc), vec![0, 0, 0, 1, 0x40, 1, 0, 0, 0, 1, 0x42]);
    }

    /// Encodes a frame that is red on the left half and blue on the right half, decodes it, and
    /// checks the colors. Needs the Mac's hardware encoder.
    #[test]
    fn encode_then_decode_keeps_left_and_right_colors() {
        let (tx, rx) = mpsc::channel();
        let config =
            EncoderConfig { width: 1024, height: 512, fps: 90.0, bitrate_bps: 30_000_000, full_range: true, strict_rate: false, key_every: Some(KEY_EVERY), max_qp: None };
        let encoder = Encoder::new(&config, tx).expect("encoder");
        for i in 0..5 {
            let mut buffer = encoder.pixel_buffer().expect("pixel buffer");
            let w = buffer.width();
            buffer.with_planes_mut(|y, ys, uv, uvs| {
                for row in 0..512 {
                    y[row * ys..row * ys + w].fill(80);
                }
                for row in 0..256 {
                    for x in 0..w / 2 {
                        // left: red (Cr high); right: blue (Cb high)
                        let (cb, cr) = if x < w / 4 { (90, 240) } else { (240, 110) };
                        uv[row * uvs + x * 2] = cb;
                        uv[row * uvs + x * 2 + 1] = cr;
                    }
                }
            });
            encoder.encode(&buffer, Duration::from_millis(i as u64 * 11), 1000 + i as u64, i == 0).expect("encode");
        }
        drop(encoder);
        let frames: Vec<EncodedFrame> = rx.try_iter().collect();
        assert_eq!(frames.len(), 5);
        assert!(frames[0].is_idr && frames[0].config_nals.is_some());
        assert!(frames[1..].iter().all(|f| !f.is_idr));
        assert_eq!(frames[2].timestamp_ns, 1002);

        let (dtx, drx) = mpsc::channel();
        let mut decoder = Decoder::new(frames[0].config_nals.as_ref().unwrap(), vec![(0.25, 0.5), (0.75, 0.5)], dtx).expect("decoder");
        for f in &frames {
            decoder.decode(f.timestamp_ns, &f.data).expect("decode");
        }
        let decoded = drx.recv_timeout(Duration::from_secs(2)).expect("decoded frame");
        assert_eq!((decoded.width, decoded.height), (1024, 512));
        let (left, right) = (decoded.samples[0], decoded.samples[1]);
        assert!(left.cr > 200 && left.cb < 130, "left {left:?}");
        assert!(right.cb > 200 && right.cr < 150, "right {right:?}");
    }
}
