//! The microphone that games under Wine hear (issue #94).
//!
//! cranvr's Wine has a capture device "cranvr microphone", the default one of every game (patch 0008
//! of `third_party/wine/patches`). It records the source that the Settings tab chose. The app writes
//! the choice into `microphone.txt` in the config dir, and a game's stream reads the file when it
//! opens:
//! - `headset`: the headset's microphone. The host keeps the ring that the stream reads
//!   (`host/src/headset/native/microphone.rs`), and the headset records only while a game does.
//! - `mac <UID>`: the Mac's input device with this UID, through Core Audio. macOS asks once whether
//!   cranvr may use the microphone.
//! - `off`: silence.

use serde::{Deserialize, Serialize};
use std::{ffi::c_void, fs, path::Path, ptr};

/// What games hear from the device "cranvr microphone"
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MicrophoneSource {
    /// The headset's microphone
    #[default]
    Headset,
    /// A Mac input device: its Core Audio UID, and the name that the Settings tab shows
    Mac { uid: String, name: String },
    /// Silence
    Off,
}

impl MicrophoneSource {
    /// The line of `microphone.txt` that Wine's driver reads
    fn line(&self) -> String {
        match self {
            Self::Headset => "headset".into(),
            Self::Mac { uid, .. } => format!("mac {uid}"),
            Self::Off => "off".into(),
        }
    }
}

/// Writes the choice for Wine's driver into `dir/microphone.txt`, through a new file, so a game
/// that opens its microphone meanwhile reads the old choice or the new one.
pub fn write_choice(dir: &Path, source: &MicrophoneSource) -> Result<(), String> {
    let path = dir.join("microphone.txt");
    let new = dir.join("microphone.txt.new");
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    fs::write(&new, format!("{}\n", source.line())).map_err(|e| e.to_string())?;
    fs::rename(&new, &path).map_err(|e| e.to_string())
}

/// An input device of this Mac
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDevice {
    pub uid: String,
    pub name: String,
}

#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[repr(C)]
struct Buffer {
    channels: u32,
    size: u32,
    data: *mut c_void,
}

/// Core Audio's four-letter codes
const fn code(text: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*text)
}

const SYSTEM_OBJECT: u32 = 1;
const DEVICES: u32 = code(b"dev#");
const STREAM_CONFIGURATION: u32 = code(b"slay");
const NAME: u32 = code(b"lnam");
const DEVICE_UID: u32 = code(b"uid ");
const SCOPE_GLOBAL: u32 = code(b"glob");
const SCOPE_INPUT: u32 = code(b"inpt");
const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(object: u32, address: *const PropertyAddress, qualifier_size: u32, qualifier: *const c_void, size: *mut u32) -> i32;
    fn AudioObjectGetPropertyData(object: u32, address: *const PropertyAddress, qualifier_size: u32, qualifier: *const c_void, size: *mut u32, data: *mut c_void) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringGetCString(string: *const c_void, buffer: *mut u8, size: isize, encoding: u32) -> u8;
    fn CFRelease(object: *const c_void);
}

/// The bytes of a property of `object`
fn property(object: u32, selector: u32, scope: u32) -> Option<Vec<u8>> {
    let address = PropertyAddress { selector, scope, element: 0 };
    let mut size = 0u32;
    // SAFETY: `address` and `size` are valid for the call.
    if unsafe { AudioObjectGetPropertyDataSize(object, &address, 0, ptr::null(), &mut size) } != 0 || size == 0 {
        return None;
    }
    // Room in units of 8 bytes, so a list of buffers sits at its alignment
    let mut data = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: `data` has room for `size` bytes.
    if unsafe { AudioObjectGetPropertyData(object, &address, 0, ptr::null(), &mut size, data.as_mut_ptr().cast()) } != 0 {
        return None;
    }
    let bytes: Vec<u8> = data.iter().flat_map(|word| word.to_ne_bytes()).take(size as usize).collect();
    Some(bytes)
}

/// A property that holds a CFString
fn text_property(object: u32, selector: u32) -> Option<String> {
    let bytes = property(object, selector, SCOPE_GLOBAL)?;
    let string = usize::from_ne_bytes(bytes.get(..size_of::<usize>())?.try_into().ok()?) as *const c_void;
    if string.is_null() {
        return None;
    }
    let mut buffer = [0u8; 512];
    // SAFETY: the property gave a CFString that this call owns. It goes back with CFRelease.
    let ok = unsafe { CFStringGetCString(string, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) } != 0;
    unsafe { CFRelease(string) };
    let end = buffer.iter().position(|&b| b == 0)?;
    ok.then(|| String::from_utf8_lossy(&buffer[..end]).into_owned())
}

/// Whether the device has a channel to record from
fn records(device: u32) -> bool {
    let Some(bytes) = property(device, STREAM_CONFIGURATION, SCOPE_INPUT) else { return false };
    let count = u32::from_ne_bytes(bytes[..4].try_into().unwrap_or_default()) as usize;
    // An AudioBufferList: the count, then buffers at the alignment of a pointer
    let buffers = bytes.get(8..).unwrap_or_default();
    (0..count).any(|i| {
        let at = i * size_of::<Buffer>();
        buffers.get(at..at + 4).is_some_and(|channels| u32::from_ne_bytes(channels.try_into().unwrap_or_default()) > 0)
    })
}

/// The input devices of this Mac, as Core Audio lists them. Listing them asks the person nothing.
pub fn input_devices() -> Vec<InputDevice> {
    let Some(bytes) = property(SYSTEM_OBJECT, DEVICES, SCOPE_GLOBAL) else { return Vec::new() };
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|id| u32::from_ne_bytes(*id))
        .filter(|&device| records(device))
        .filter_map(|device| Some(InputDevice { uid: text_property(device, DEVICE_UID)?, name: text_property(device, NAME)? }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choice_reaches_the_file_that_wine_reads() {
        let dir = std::env::temp_dir().join(format!("cranvr-microphone-choice-{}", std::process::id()));
        for (source, line) in [
            (MicrophoneSource::Headset, "headset\n"),
            (MicrophoneSource::Mac { uid: "BuiltInMicrophoneDevice".into(), name: "MacBook Pro Microphone".into() }, "mac BuiltInMicrophoneDevice\n"),
            (MicrophoneSource::Off, "off\n"),
        ] {
            write_choice(&dir, &source).unwrap();
            assert_eq!(fs::read_to_string(dir.join("microphone.txt")).unwrap(), line);
        }
        assert!(!dir.join("microphone.txt.new").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_list_of_input_devices_names_each_one() {
        // A Mac without an input device lists none; each listed one has a UID and a name
        for device in input_devices() {
            assert!(!device.uid.is_empty() && !device.name.is_empty(), "{device:?}");
        }
    }
}
