use super::{read, WavWriter, SAMPLE_RATE};

// A file of two channels written in pieces reads back with the same samples, and its header names
// the rate and the size that a player needs
#[test]
fn a_written_file_reads_back_and_its_header_holds_the_sizes() {
    let dir = std::env::temp_dir().join(format!("cranvr-wav-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sound.wav");
    let mut writer = WavWriter::create(&path, 2).unwrap();
    writer.write_samples(&[1, -1, 300, -300]).unwrap();
    writer.write_bytes(&[0x10, 0x00, 0xf0, 0xff]).unwrap();
    writer.finish().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), SAMPLE_RATE);
    assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 12, "the data chunk's size");
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 48, "the RIFF chunk's size");
    let (channels, samples) = read(&path).unwrap();
    assert_eq!((channels, samples), (2, vec![1, -1, 300, -300, 16, -16]));
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
}

// A kill of the sound's process leaves a header that says 0 bytes: the samples still read up to the
// file's end, so the recording keeps its sound
#[test]
fn a_file_that_a_kill_cut_short_reads_up_to_its_end() {
    let dir = std::env::temp_dir().join(format!("cranvr-wav-cut-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sound.wav");
    let mut writer = WavWriter::create(&path, 2).unwrap();
    writer.write_samples(&[5, -5, 7, -7]).unwrap();
    // The writer goes away without `finish`, as in a kill: its buffer reaches the file, its header
    // keeps the size 0
    drop(writer);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 0);
    assert_eq!(read(&path).unwrap(), (2, vec![5, -5, 7, -7]));
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
}
