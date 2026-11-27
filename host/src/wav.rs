//! WAV files of 16-bit samples at 48 kHz: a recording's sound on its way to the final file.

use std::{
    fs::File,
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};

pub const SAMPLE_RATE: u32 = 48_000;
const HEADER: usize = 44;

/// A WAV file that grows as sound comes. `finish` writes the sizes into its header.
pub struct WavWriter {
    file: BufWriter<File>,
    channels: u16,
    bytes: u64,
}

impl WavWriter {
    pub fn create(path: &Path, channels: u16) -> io::Result<Self> {
        let mut writer = Self { file: BufWriter::new(File::create(path)?), channels, bytes: 0 };
        writer.file.write_all(&header(channels, 0))?;
        Ok(writer)
    }

    /// Appends interleaved little-endian 16-bit samples.
    pub fn write_bytes(&mut self, pcm: &[u8]) -> io::Result<()> {
        self.bytes += pcm.len() as u64;
        self.file.write_all(pcm)
    }

    pub fn write_samples(&mut self, samples: &[i16]) -> io::Result<()> {
        for sample in samples {
            self.file.write_all(&sample.to_le_bytes())?;
        }
        self.bytes += samples.len() as u64 * 2;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        let bytes = u32::try_from(self.bytes).unwrap_or(u32::MAX);
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header(self.channels, bytes))?;
        self.file.flush()
    }
}

fn header(channels: u16, data_bytes: u32) -> [u8; HEADER] {
    let block = channels * 2;
    let mut out = [0u8; HEADER];
    let mut at = 0;
    let mut put = |bytes: &[u8]| {
        out[at..at + bytes.len()].copy_from_slice(bytes);
        at += bytes.len();
    };
    put(b"RIFF");
    put(&(36 + data_bytes).to_le_bytes());
    put(b"WAVEfmt ");
    put(&16u32.to_le_bytes());
    put(&1u16.to_le_bytes());
    put(&channels.to_le_bytes());
    put(&SAMPLE_RATE.to_le_bytes());
    put(&(SAMPLE_RATE * u32::from(block)).to_le_bytes());
    put(&block.to_le_bytes());
    put(&16u16.to_le_bytes());
    put(b"data");
    put(&data_bytes.to_le_bytes());
    out
}

/// Reads a WAV file of 16-bit samples: its channels and its interleaved samples.
pub fn read(path: &Path) -> io::Result<(u16, Vec<i16>)> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    let refused = || io::Error::other(format!("{} is no WAV file of 16-bit samples", path.display()));
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(refused());
    }
    let (mut at, mut channels) = (12, None);
    while at + 8 <= data.len() {
        let size = u32::from_le_bytes([data[at + 4], data[at + 5], data[at + 6], data[at + 7]]) as usize;
        let body = &data[at + 8..(at + 8 + size).min(data.len())];
        match &data[at..at + 4] {
            b"fmt " if body.len() >= 16 && u16::from_le_bytes([body[14], body[15]]) == 16 => channels = Some(u16::from_le_bytes([body[2], body[3]])),
            b"data" => {
                let samples = body.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect();
                return channels.map(|channels| (channels, samples)).ok_or_else(refused);
            }
            _ => {}
        }
        at += 8 + size + size % 2;
    }
    Err(refused())
}

#[cfg(test)]
#[path = "tests/wav.rs"]
mod tests;
