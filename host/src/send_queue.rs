//! The eye stream's frames between the encoder and the send (#157).
//!
//! The encoder's output thread pushes each encoded frame, and the thread `video-send` takes them
//! and sends them to the headset. A send can block: a TCP write over `adb reverse` waits while the
//! link carries less than the stream, and so does a link that gets slower while it streams. The
//! channel between the two had no bound before. Behind a test cap that held each frame for its time
//! at 60 Mbps, 8 s of frames piled up in it, the time from the pose to the display grew to about
//! 1.8 s, and 592 frames went out in one second when the cap ended.
//!
//! The queue holds at most its depth of frames that wait for the send. A frame that comes to a full
//! queue makes the oldest frame go. The frames after it up to the next key frame go with it: each
//! of them refers to the one before it, and the headset numbers the frames that it gets one by one,
//! so it would decode them without their reference. The queue then takes key frames only, and the
//! push asks the encoder for one. A slow send thus costs frames and a key frame, not latency: a
//! frame waits at most the depth of frames before its send.

use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex},
};

/// What became of a pushed frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push {
    /// The frame waits for the send.
    Queued,
    /// The frame went, or older ones went for it. `wants_key`: the queue takes key frames only from
    /// now on, so the encoder should make the next frame a key frame. It is true at the first drop
    /// of a run of drops, and again after each `ASK_AGAIN` frames without a key frame.
    Dropped { wants_key: bool },
}

/// The queue's counts since the last look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Frames that the queue dropped
    pub dropped: u32,
    /// The most frames that waited at once
    pub most: usize,
}

/// The frames that may come without a key frame after the queue asked for one, before it asks again.
/// The encoder holds up to two frames (`MAX_IN_FLIGHT` in main.rs) and the job queue one more when the
/// request comes, so three frames without a key frame are normal. A request can also get lost: an
/// encode that fails, or a frame that VideoToolbox drops, takes it along. Without a new request the
/// stream would then send nothing until the encoder's next key frame on its timer, 10 s later.
pub const ASK_AGAIN: u32 = 6;

pub struct SendQueue<T> {
    depth: usize,
    state: Mutex<State<T>>,
    ready: Condvar,
}

struct State<T> {
    /// The frames that wait, oldest first, each with whether it is a key frame
    frames: VecDeque<(T, bool)>,
    /// A dropped frame left the frames after it without their reference: only a key frame may come
    wants_key: bool,
    /// The frames dropped since the last request of a key frame
    since_ask: u32,
    counts: Counts,
    closed: bool,
}

impl<T> SendQueue<T> {
    /// A queue that holds at most `depth` frames, 1 or more.
    pub fn new(depth: usize) -> Self {
        Self {
            depth: depth.max(1),
            state: Mutex::new(State { frames: VecDeque::with_capacity(depth.max(1)), wants_key: false, since_ask: 0, counts: Counts::default(), closed: false }),
            ready: Condvar::new(),
        }
    }

    /// Adds an encoded frame. It never waits for the send.
    pub fn push(&self, frame: T, key: bool) -> Push {
        let mut state = self.state.lock().unwrap();
        if state.wants_key && !key {
            state.counts.dropped += 1;
            state.since_ask += 1;
            let again = state.since_ask >= ASK_AGAIN;
            if again {
                state.since_ask = 0;
            }
            return Push::Dropped { wants_key: again };
        }
        // A key frame refers to no frame before it: the frames after it can be decoded again
        state.wants_key = false;
        let mut dropped = false;
        if state.frames.len() >= self.depth {
            // The oldest frame goes, and the frames that refer to it up to the next key frame
            state.frames.pop_front();
            state.counts.dropped += 1;
            while state.frames.front().is_some_and(|(_, key)| !key) {
                state.frames.pop_front();
                state.counts.dropped += 1;
            }
            dropped = true;
            if state.frames.is_empty() && !key {
                // This frame refers to a dropped one too
                state.counts.dropped += 1;
                state.wants_key = true;
                state.since_ask = 0;
                return Push::Dropped { wants_key: true };
            }
        }
        state.frames.push_back((frame, key));
        state.counts.most = state.counts.most.max(state.frames.len());
        drop(state);
        self.ready.notify_one();
        if dropped {
            Push::Dropped { wants_key: false }
        } else {
            Push::Queued
        }
    }

    /// The oldest frame. Waits until one comes. `None` once the queue is closed and empty.
    pub fn pop(&self) -> Option<T> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some((frame, _)) = state.frames.pop_front() {
                return Some(frame);
            }
            if state.closed {
                return None;
            }
            state = self.ready.wait(state).unwrap();
        }
    }

    /// Ends the wait of `pop` once the waiting frames are gone. The host's queue lives as long as the
    /// host; a test ends its send thread with it.
    #[cfg(test)]
    pub fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.ready.notify_all();
    }

    /// The frames that wait now
    #[cfg(test)]
    pub fn waiting(&self) -> usize {
        self.state.lock().unwrap().frames.len()
    }

    /// The counts since the last call.
    pub fn take_counts(&self) -> Counts {
        let mut state = self.state.lock().unwrap();
        let waiting = state.frames.len();
        std::mem::replace(&mut state.counts, Counts { dropped: 0, most: waiting })
    }
}

#[cfg(test)]
#[path = "tests/send_queue.rs"]
mod tests;
