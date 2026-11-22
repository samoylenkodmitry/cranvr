use super::{clock_ppm, interleave, own_processes, tap_description, AudioBuffer, Resampler, StreamDescription, FORMAT_FLAG_FLOAT, FORMAT_FLAG_NON_INTERLEAVED};
use objc2::{msg_send, rc::Retained, runtime::Bool};
use objc2_foundation::{NSArray, NSNumber, NSString};
use objc2_core_audio::AudioConvertNanosToHostTime;

fn samples(bytes: &[u8]) -> Vec<i16> {
    bytes.as_chunks::<2>().0.iter().map(|b| i16::from_ne_bytes(*b)).collect()
}

fn buffer<T>(channels: u32, data: &[T]) -> AudioBuffer {
    AudioBuffer { channels, bytes: size_of_val(data) as u32, data: data.as_ptr().cast() }
}

// A Core Audio tap may deliver 32-bit float samples with one buffer per channel
#[test]
fn planar_float_becomes_interleaved_16_bit() {
    let description = StreamDescription {
        format_flags: FORMAT_FLAG_FLOAT | FORMAT_FLAG_NON_INTERLEAVED,
        bits_per_channel: 32,
        ..Default::default()
    };
    let left = [0.0f32, 1.0, -2.0];
    let right = [0.5f32, -1.0, 0.25];
    let out = unsafe { interleave(&description, &[buffer(1, &left), buffer(1, &right)]) }.unwrap();
    assert_eq!(samples(&out), [0, 16383, 32767, -32767, -32767, 8191]);
}

#[test]
fn interleaved_mono_16_bit_plays_on_both_sides() {
    let description = StreamDescription { bits_per_channel: 16, ..Default::default() };
    let mono = [7i16, -9];
    let out = unsafe { interleave(&description, &[buffer(1, &mono)]) }.unwrap();
    assert_eq!(samples(&out), [7, 7, -9, -9]);
}

fn stereo(frames: &[[i16; 2]]) -> Vec<u8> {
    frames.iter().flat_map(|[l, r]| [l.to_ne_bytes(), r.to_ne_bytes()]).flatten().collect()
}

#[test]
fn sound_at_48_khz_passes_as_it_is() {
    let input = stereo(&[[1, 2], [3, 4], [5, 6]]);
    assert_eq!(Resampler::new(48_000.0).run(input.clone()), input);
}

// A device at 44.1 kHz hands over buffers of 441 frames. One second of them must become one
// second at 48 kHz. A ramp must stay a ramp across the seams of the buffers.
#[test]
fn sound_at_44_1_khz_becomes_48_khz_without_seams() {
    let mut resampler = Resampler::new(44_100.0);
    let mut out = Vec::new();
    for chunk in 0..100 {
        let frames: Vec<[i16; 2]> = (0..441).map(|i| ((chunk * 441 + i) / 2) as i16).map(|v| [v, -v]).collect();
        out.extend(samples(&resampler.run(stereo(&frames))));
    }
    let left: Vec<i16> = out.iter().step_by(2).copied().collect();
    assert!((left.len() as i64 - 48_000).abs() <= 2, "{} frames", left.len());
    // The ramp rises by half a step per input frame: 0.459 per output frame
    for pair in left.windows(2) {
        assert!((0..=1).contains(&(pair[1] - pair[0])), "{pair:?}");
    }
    assert!(out.chunks(2).all(|f| f[0] == -f[1]));
}

#[test]
fn sound_at_96_khz_keeps_every_other_frame() {
    let frames: Vec<[i16; 2]> = (0..8).map(|i| [i * 10, i]).collect();
    let out = samples(&Resampler::new(96_000.0).run(stereo(&frames)));
    assert_eq!(out, [0, 0, 20, 2, 40, 4, 60, 6]);
}

#[test]
fn the_clock_reads_its_ppm_after_ten_seconds() {
    let ticks = |seconds: f64| unsafe { AudioConvertNanosToHostTime((seconds * 1e9) as u64) };
    let first = Some((ticks(5.0), 1_000.0));
    // 100 ppm fast: 480 048 frames in 10 s
    let ppm = clock_ppm(first, Some((ticks(15.0), 481_048.0)), 48_000.0).unwrap();
    assert!((ppm - 100.0).abs() < 1.0, "{ppm}");
    assert_eq!(clock_ppm(first, Some((ticks(9.0), 200_000.0)), 48_000.0), None);
}

// The description takes no permission, so a test can check the calls that the tap's start makes:
// in a debug build objc2 checks each selector and its argument types.
#[test]
fn the_tap_description_leaves_out_the_host() {
    let own = own_processes();
    // A test uses Core Audio here first, which gives it a process object
    assert_eq!(own.len(), 1, "{own:?}");
    let (description, uuid) = tap_description(&own).unwrap();
    let processes: Retained<NSArray<NSNumber>> = unsafe { msg_send![&*description, processes] };
    assert_eq!(processes.iter().map(|n| n.as_u32()).collect::<Vec<_>>(), own);
    let exclusive: Bool = unsafe { msg_send![&*description, isExclusive] };
    let private: Bool = unsafe { msg_send![&*description, isPrivate] };
    let mono: Bool = unsafe { msg_send![&*description, isMono] };
    assert!(exclusive.as_bool() && private.as_bool() && !mono.as_bool());
    let name: Retained<NSString> = unsafe { msg_send![&*description, name] };
    assert_eq!(name.to_string(), "cranvr game sound");
    assert_eq!(uuid.UUIDString().len(), 36);
}
