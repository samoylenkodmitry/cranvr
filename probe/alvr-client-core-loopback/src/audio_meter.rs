//! cranvr: the probe plays no game sound. Tests must stay silent. On Android cpal also needs a
//! Java VM, which the probe does not have. This module measures the sound instead, so a test can
//! tell that the host's game audio arrived and that it is not silence.

use alvr_common::{parking_lot::Mutex, ConnectionError};
use alvr_sockets::StreamReceiver;
use std::time::Duration;

/// The game sound received so far: interleaved 16-bit stereo, as ALVR sends it.
#[derive(Clone, Copy, Default, Debug)]
pub struct GameAudio {
    pub packets: u64,
    pub samples: u64,
    pub sum_squares: f64,
    pub peak: u16,
}

impl GameAudio {
    /// The root mean square of all samples, from 0 to 1.
    pub fn rms(&self) -> f64 {
        if self.samples == 0 {
            return 0.0;
        }
        (self.sum_squares / self.samples as f64).sqrt() / i16::MAX as f64
    }
}

pub static GAME_AUDIO: Mutex<GameAudio> =
    Mutex::new(GameAudio { packets: 0, samples: 0, sum_squares: 0.0, peak: 0 });

/// Takes the game audio packets while `is_running` holds.
pub fn measure_loop(is_running: impl Fn() -> bool, receiver: &mut StreamReceiver<()>) {
    while is_running() {
        let data = match receiver.recv(Duration::from_millis(500)) {
            Ok(data) => data,
            Err(ConnectionError::TryAgain(_)) => continue,
            Err(ConnectionError::Other(_)) => return,
        };
        let Ok((_, packet)) = data.get() else { continue };
        let mut audio = GAME_AUDIO.lock();
        audio.packets += 1;
        for pair in packet.chunks_exact(2) {
            let sample = i16::from_ne_bytes([pair[0], pair[1]]);
            audio.samples += 1;
            audio.sum_squares += (sample as f64) * (sample as f64);
            audio.peak = audio.peak.max(sample.unsigned_abs());
        }
    }
}
