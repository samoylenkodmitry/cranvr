//! The small part of CoreFoundation, CoreVideo, CoreMedia and VideoToolbox that the encoder and
//! the decoder use. These are plain C APIs, so plain `extern "C"` declarations cover them.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::c_void;

pub type OSStatus = i32;
pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFBooleanRef = *const c_void;
pub type CFIndex = isize;
pub type CVPixelBufferRef = *mut c_void;
pub type CVPixelBufferPoolRef = *mut c_void;
pub type CVMetalTextureCacheRef = *mut c_void;
pub type CVMetalTextureRef = *mut c_void;
pub type CMSampleBufferRef = *mut c_void;
pub type CMBlockBufferRef = *mut c_void;
pub type CMFormatDescriptionRef = *mut c_void;
pub type VTCompressionSessionRef = *mut c_void;
pub type VTDecompressionSessionRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CMTime {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}

impl CMTime {
    pub const INVALID: Self = Self { value: 0, timescale: 0, flags: 0, epoch: 0 };

    pub fn new(value: i64, timescale: i32) -> Self {
        // flags 1 = kCMTimeFlags_Valid
        Self { value, timescale, flags: 1, epoch: 0 }
    }
}

/// Opaque: only their addresses are passed to CFDictionaryCreate.
#[repr(C)]
pub struct CFDictionaryKeyCallBacks {
    _private: [u8; 0],
}
#[repr(C)]
pub struct CFDictionaryValueCallBacks {
    _private: [u8; 0],
}
#[repr(C)]
pub struct CFArrayCallBacks {
    _private: [u8; 0],
}

pub const kCFNumberSInt32Type: CFIndex = 3;
pub const kCFNumberSInt64Type: CFIndex = 4;
pub const kCFNumberFloat64Type: CFIndex = 6;

pub const kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange: u32 = 0x3432_3076; // '420v'
pub const kCVPixelFormatType_420YpCbCr8BiPlanarFullRange: u32 = 0x3432_3066; // '420f'
pub const kCMVideoCodecType_HEVC: u32 = 0x6876_6331; // 'hvc1'
pub const kCMBlockBufferAssureMemoryNowFlag: u32 = 1;
pub const kVTEncodeInfo_FrameDropped: u32 = 1 << 1;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFAllocatorDefault: CFAllocatorRef;
    pub static kCFBooleanTrue: CFBooleanRef;
    pub static kCFBooleanFalse: CFBooleanRef;
    pub static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    pub static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    pub static kCFTypeArrayCallBacks: CFArrayCallBacks;
    pub fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const *const c_void,
        count: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;

    pub fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: CFIndex,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> CFDictionaryRef;
    pub fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    pub fn CFNumberCreate(allocator: CFAllocatorRef, kind: CFIndex, value: *const c_void) -> CFNumberRef;
    pub fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    pub fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;
    pub fn CFBooleanGetValue(boolean: CFBooleanRef) -> u8;
    pub fn CFRelease(cf: CFTypeRef);
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    pub fn CVPixelBufferGetIOSurface(buffer: CVPixelBufferRef) -> IOSurfaceRef;
    pub static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    pub static kCVPixelBufferWidthKey: CFStringRef;
    pub static kCVPixelBufferHeightKey: CFStringRef;
    pub static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
    pub static kCVPixelBufferMetalCompatibilityKey: CFStringRef;

    pub fn CVPixelBufferPoolCreatePixelBuffer(
        allocator: CFAllocatorRef,
        pool: CVPixelBufferPoolRef,
        out: *mut CVPixelBufferRef,
    ) -> i32;
    pub fn CVPixelBufferLockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    pub fn CVPixelBufferUnlockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    pub fn CVPixelBufferGetBaseAddressOfPlane(buffer: CVPixelBufferRef, plane: usize) -> *mut c_void;
    pub fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CVPixelBufferRef, plane: usize) -> usize;
    pub fn CVPixelBufferGetWidth(buffer: CVPixelBufferRef) -> usize;
    pub fn CVPixelBufferGetHeight(buffer: CVPixelBufferRef) -> usize;
    pub fn CVPixelBufferGetPixelFormatType(buffer: CVPixelBufferRef) -> u32;
    pub fn CVBufferRetain(buffer: *mut c_void) -> *mut c_void;
    pub fn CVBufferRelease(buffer: *mut c_void);

    pub fn CVMetalTextureCacheCreate(
        allocator: CFAllocatorRef,
        cache_attributes: CFDictionaryRef,
        metal_device: *mut c_void,
        texture_attributes: CFDictionaryRef,
        out: *mut CVMetalTextureCacheRef,
    ) -> i32;
    pub fn CVMetalTextureCacheCreateTextureFromImage(
        allocator: CFAllocatorRef,
        cache: CVMetalTextureCacheRef,
        image: CVPixelBufferRef,
        texture_attributes: CFDictionaryRef,
        pixel_format: usize,
        width: usize,
        height: usize,
        plane_index: usize,
        out: *mut CVMetalTextureRef,
    ) -> i32;
    pub fn CVMetalTextureGetTexture(texture: CVMetalTextureRef) -> *mut c_void;
    pub fn CVMetalTextureCacheFlush(cache: CVMetalTextureCacheRef, options: u64);
}

pub type IOSurfaceRef = *mut c_void;

#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    pub fn IOSurfaceLookup(id: u32) -> IOSurfaceRef;
    pub fn IOSurfaceGetWidth(surface: IOSurfaceRef) -> usize;
    pub fn IOSurfaceGetHeight(surface: IOSurfaceRef) -> usize;
    pub fn IOSurfaceGetPixelFormat(surface: IOSurfaceRef) -> u32;
    pub fn IOSurfaceIncrementUseCount(surface: IOSurfaceRef);
    pub fn IOSurfaceDecrementUseCount(surface: IOSurfaceRef);
    pub fn IOSurfaceGetID(surface: IOSurfaceRef) -> u32;
    /// Deprecated, but the only way to share a surface by ID; DXMT under Wine sets it too.
    pub static kIOSurfaceIsGlobal: CFStringRef;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    pub fn CVPixelBufferCreate(
        allocator: CFAllocatorRef,
        width: usize,
        height: usize,
        pixel_format: u32,
        attributes: CFDictionaryRef,
        out: *mut CVPixelBufferRef,
    ) -> i32;
    pub fn CVPixelBufferCreateWithIOSurface(
        allocator: CFAllocatorRef,
        surface: IOSurfaceRef,
        pixel_buffer_attributes: CFDictionaryRef,
        out: *mut CVPixelBufferRef,
    ) -> i32;
}

pub const kCVPixelFormatType_32BGRA: u32 = 0x4247_5241; // 'BGRA'

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    pub static kCMSampleAttachmentKey_NotSync: CFStringRef;

    pub fn CMSampleBufferGetDataBuffer(sample: CMSampleBufferRef) -> CMBlockBufferRef;
    pub fn CMSampleBufferGetFormatDescription(sample: CMSampleBufferRef) -> CMFormatDescriptionRef;
    pub fn CMSampleBufferGetSampleAttachmentsArray(sample: CMSampleBufferRef, create: u8) -> CFArrayRef;
    pub fn CMBlockBufferGetDataLength(block: CMBlockBufferRef) -> usize;
    pub fn CMBlockBufferCopyDataBytes(
        block: CMBlockBufferRef,
        offset: usize,
        length: usize,
        destination: *mut c_void,
    ) -> OSStatus;
    pub fn CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        format: CMFormatDescriptionRef,
        index: usize,
        pointer_out: *mut *const u8,
        size_out: *mut usize,
        count_out: *mut usize,
        nal_header_length_out: *mut i32,
    ) -> OSStatus;
    pub fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: CFAllocatorRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_length: i32,
        extensions: CFDictionaryRef,
        out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;
    pub fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CFAllocatorRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        out: *mut CMBlockBufferRef,
    ) -> OSStatus;
    pub fn CMBlockBufferReplaceDataBytes(
        source: *const c_void,
        destination: CMBlockBufferRef,
        offset_into_destination: usize,
        data_length: usize,
    ) -> OSStatus;
    pub fn CMSampleBufferCreateReady(
        allocator: CFAllocatorRef,
        data_buffer: CMBlockBufferRef,
        format: CMFormatDescriptionRef,
        num_samples: isize,
        num_timing_entries: isize,
        timing: *const c_void,
        num_size_entries: isize,
        sizes: *const usize,
        out: *mut CMSampleBufferRef,
    ) -> OSStatus;
}

pub type VTCompressionOutputCallback = extern "C" fn(
    output_refcon: *mut c_void,
    source_frame_refcon: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    sample: CMSampleBufferRef,
);

pub type VTDecompressionOutputCallback = extern "C" fn(
    output_refcon: *mut c_void,
    source_frame_refcon: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    image: CVPixelBufferRef,
    presentation_time: CMTime,
    duration: CMTime,
);

#[repr(C)]
pub struct VTDecompressionOutputCallbackRecord {
    pub callback: VTDecompressionOutputCallback,
    pub refcon: *mut c_void,
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    pub static kVTVideoEncoderSpecification_EnableLowLatencyRateControl: CFStringRef;
    pub static kVTCompressionPropertyKey_RealTime: CFStringRef;
    pub static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    pub static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    pub static kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality: CFStringRef;
    pub static kVTCompressionPropertyKey_MaximizePowerEfficiency: CFStringRef;
    pub static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    pub static kVTCompressionPropertyKey_DataRateLimits: CFStringRef;
    pub static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxAllowedFrameQP: CFStringRef;
    pub static kVTProfileLevel_HEVC_Main_AutoLevel: CFStringRef;
    pub static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;

    pub fn VTCompressionSessionCreate(
        allocator: CFAllocatorRef,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: CFDictionaryRef,
        source_image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: CFAllocatorRef,
        output_callback: Option<VTCompressionOutputCallback>,
        output_refcon: *mut c_void,
        out: *mut VTCompressionSessionRef,
    ) -> OSStatus;
    pub fn VTSessionSetProperty(session: *mut c_void, key: CFStringRef, value: CFTypeRef) -> OSStatus;

    // VTPixelRotationSession, macOS 13 and later: turns or mirrors a picture into another buffer
    pub static kVTPixelRotationPropertyKey_FlipVerticalOrientation: CFStringRef;
    pub fn VTPixelRotationSessionCreate(allocator: CFAllocatorRef, out: *mut *mut c_void) -> OSStatus;
    pub fn VTPixelRotationSessionRotateImage(
        session: *mut c_void,
        source: CVPixelBufferRef,
        destination: CVPixelBufferRef,
    ) -> OSStatus;
    pub fn VTPixelRotationSessionInvalidate(session: *mut c_void);

    // VTPixelTransferSession: scales a picture into another buffer
    pub static kVTPixelTransferPropertyKey_ScalingMode: CFStringRef;
    pub static kVTScalingMode_Normal: CFStringRef;
    pub static kVTPixelTransferPropertyKey_DownsamplingMode: CFStringRef;
    pub static kVTDownsamplingMode_Average: CFStringRef;
    pub fn VTPixelTransferSessionCreate(allocator: CFAllocatorRef, out: *mut *mut c_void) -> OSStatus;
    pub fn VTPixelTransferSessionTransferImage(
        session: *mut c_void,
        source: CVPixelBufferRef,
        destination: CVPixelBufferRef,
    ) -> OSStatus;
    pub fn VTPixelTransferSessionInvalidate(session: *mut c_void);
    pub fn VTCompressionSessionPrepareToEncodeFrames(session: VTCompressionSessionRef) -> OSStatus;
    pub fn VTCompressionSessionGetPixelBufferPool(session: VTCompressionSessionRef) -> CVPixelBufferPoolRef;
    pub fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image: CVPixelBufferRef,
        presentation_time: CMTime,
        duration: CMTime,
        frame_properties: CFDictionaryRef,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;
    pub fn VTCompressionSessionCompleteFrames(session: VTCompressionSessionRef, until: CMTime) -> OSStatus;
    pub fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);

    pub fn VTDecompressionSessionCreate(
        allocator: CFAllocatorRef,
        format: CMFormatDescriptionRef,
        decoder_specification: CFDictionaryRef,
        destination_image_buffer_attributes: CFDictionaryRef,
        output_callback: *const VTDecompressionOutputCallbackRecord,
        out: *mut VTDecompressionSessionRef,
    ) -> OSStatus;
    pub fn VTDecompressionSessionDecodeFrame(
        session: VTDecompressionSessionRef,
        sample: CMSampleBufferRef,
        flags: u32,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;
    pub fn VTDecompressionSessionWaitForAsynchronousFrames(session: VTDecompressionSessionRef) -> OSStatus;
    pub fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);
}

/// Builds a CFDictionary from (key, value) pairs. The caller owns the result.
///
/// # Safety
/// Every key and value must be a valid CoreFoundation object.
pub unsafe fn dictionary(pairs: &[(CFStringRef, CFTypeRef)]) -> CFDictionaryRef {
    let keys: Vec<*const c_void> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<*const c_void> = pairs.iter().map(|p| p.1).collect();
    CFDictionaryCreate(
        kCFAllocatorDefault,
        keys.as_ptr(),
        values.as_ptr(),
        pairs.len() as CFIndex,
        &kCFTypeDictionaryKeyCallBacks,
        &kCFTypeDictionaryValueCallBacks,
    )
}

/// A CFNumber holding an i64. The caller owns the result.
///
/// # Safety
/// The caller must release the result with `CFRelease`.
pub unsafe fn number_i64(value: i64) -> CFNumberRef {
    CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt64Type, &value as *const i64 as *const c_void)
}

/// A CFNumber holding an f64. The caller owns the result.
///
/// # Safety
/// The caller must release the result with `CFRelease`.
pub unsafe fn number_f64(value: f64) -> CFNumberRef {
    CFNumberCreate(kCFAllocatorDefault, kCFNumberFloat64Type, &value as *const f64 as *const c_void)
}
