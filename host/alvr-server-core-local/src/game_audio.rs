//! cranvr: the game sound on macOS. ALVR records game audio through a loopback of the output
//! device, which only Windows and Linux offer. On macOS the host captures the sound itself and
//! hands it to this module. The connection then sends it to the headset as ALVR's game audio.

use alvr_common::parking_lot::Mutex;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};

/// The sample rate that the host captures at and that the headset is told to play at.
pub const GAME_AUDIO_SAMPLE_RATE: u32 = 48_000;

/// Chunks of about 10 ms each; a full queue means the connection does not take them.
const QUEUE_CHUNKS: usize = 64;

static SINK: Mutex<Option<SyncSender<Vec<u8>>>> = Mutex::new(None);

/// Sends one chunk of interleaved 16-bit stereo samples at `GAME_AUDIO_SAMPLE_RATE` to the
/// headset, when one streams. Without a stream, or when the stream falls behind, the chunk is
/// dropped.
pub fn send_game_audio(samples: Vec<u8>) {
    let mut sink = SINK.lock();
    if let Some(sender) = sink.as_ref() {
        if let Err(TrySendError::Disconnected(_)) = sender.try_send(samples) {
            *sink = None;
        }
    }
}

/// The chunks for one connection. A new connection takes the place of the previous one.
pub(crate) fn subscribe() -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CHUNKS);
    *SINK.lock() = Some(sender);
    receiver
}
