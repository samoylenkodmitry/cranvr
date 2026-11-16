use super::{interleave, AudioBuffer, StreamDescription, FORMAT_FLAG_FLOAT, FORMAT_FLAG_NON_INTERLEAVED};

fn samples(bytes: &[u8]) -> Vec<i16> {
    bytes.as_chunks::<2>().0.iter().map(|b| i16::from_ne_bytes(*b)).collect()
}

fn buffer<T>(channels: u32, data: &[T]) -> AudioBuffer {
    AudioBuffer { channels, bytes: size_of_val(data) as u32, data: data.as_ptr().cast() }
}

// ScreenCaptureKit delivers 32-bit float samples with one buffer per channel
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
