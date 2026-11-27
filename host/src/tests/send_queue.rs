use super::{Push, SendQueue};

const DEPTH: usize = 3;

/// A frame of the fake encoder: its number in the encoder's order and whether it is a key frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Frame {
    number: u32,
    key: bool,
}

/// An encoder that makes a key frame first and when the queue asks for one, as the host's does
/// through `force_idr`.
struct Encoder {
    next: u32,
    key_wanted: bool,
    pushes: Vec<Push>,
}

impl Encoder {
    fn new() -> Self {
        Self { next: 0, key_wanted: true, pushes: Vec::new() }
    }

    fn push(&mut self, queue: &SendQueue<Frame>) {
        let frame = Frame { number: self.next, key: std::mem::take(&mut self.key_wanted) };
        self.next += 1;
        let push = queue.push(frame, frame.key);
        if push == (Push::Dropped { wants_key: true }) {
            self.key_wanted = true;
        }
        self.pushes.push(push);
    }
}

/// A headset's decoder: it numbers the frames that it gets one by one, so it decodes a frame that
/// is not a key frame only when the frame before it in the encoder's order came just before.
#[derive(Default)]
struct Decoder {
    last: Option<u32>,
    frames: Vec<Frame>,
}

impl Decoder {
    fn take(&mut self, frame: Frame) {
        assert!(frame.key || self.last.is_some_and(|last| last + 1 == frame.number), "frame {frame:?} came after {:?}: its reference is gone", self.last);
        self.last = Some(frame.number);
        self.frames.push(frame);
    }

    fn take_all(&mut self, queue: &SendQueue<Frame>) {
        queue.close();
        while let Some(frame) = queue.pop() {
            self.take(frame);
        }
    }
}

/// The oldest frame, when one waits
fn try_pop(queue: &SendQueue<Frame>) -> Option<Frame> {
    queue.state.lock().unwrap().frames.pop_front().map(|(frame, _)| frame)
}

#[test]
fn a_blocked_send_never_holds_more_frames_than_the_depth() {
    let queue = SendQueue::new(DEPTH);
    let (mut encoder, mut decoder) = (Encoder::new(), Decoder::default());
    // A steady second
    for _ in 0..120 {
        encoder.push(&queue);
        decoder.take(queue.pop().unwrap());
    }
    assert_eq!(queue.take_counts().dropped, 0);
    // The send blocks for half a second, as a TCP write behind a slow link does
    for _ in 0..60 {
        encoder.push(&queue);
        assert!(queue.state.lock().unwrap().frames.len() <= DEPTH, "more frames than the depth waited");
    }
    let waiting = queue.state.lock().unwrap().frames.len();
    let counts = queue.take_counts();
    assert!(counts.most <= DEPTH);
    assert_eq!(counts.dropped as usize + waiting, 60, "the frames that do not wait went");
    // Each run of drops asked for a key frame once: the key frame waits, and a full queue drops it with the
    // frames after it again while the send stays blocked
    let asked = encoder.pushes.iter().filter(|p| **p == (Push::Dropped { wants_key: true })).count();
    assert_eq!(asked, 60 / (DEPTH + 1), "{asked} key frame requests");
    // The send runs again
    encoder.push(&queue);
    decoder.take_all(&queue);
}

#[test]
fn the_stream_recovers_with_a_key_frame() {
    let queue = SendQueue::new(DEPTH);
    let (mut encoder, mut decoder) = (Encoder::new(), Decoder::default());
    encoder.push(&queue);
    decoder.take(queue.pop().unwrap());
    // Blocked: the newest frames wait, and the frames after the dropped ones start with a key frame
    for _ in 0..10 {
        encoder.push(&queue);
    }
    let first = queue.pop().unwrap();
    assert!(first.key, "the first frame after the drops is {first:?}");
    decoder.take(first);
    // The send runs again: the frames after the key frame refer to it
    for _ in 0..20 {
        encoder.push(&queue);
        decoder.take(queue.pop().unwrap());
    }
    decoder.take_all(&queue);
    assert!(decoder.frames.iter().filter(|f| f.key).count() == 2, "one key frame at the start and one after the drops");
}

#[test]
fn a_key_frame_in_the_queue_keeps_the_frames_after_it() {
    let queue = SendQueue::new(DEPTH);
    let mut decoder = Decoder::default();
    for (number, key) in [(0, true), (1, false), (2, true)] {
        assert_eq!(queue.push(Frame { number, key }, key), Push::Queued);
    }
    // The oldest frame and the one that refers to it go. The key frame and the new frame stay.
    assert_eq!(queue.push(Frame { number: 3, key: false }, false), Push::Dropped { wants_key: false });
    assert_eq!(queue.take_counts().dropped, 2);
    decoder.take_all(&queue);
    assert_eq!(decoder.frames.iter().map(|f| f.number).collect::<Vec<_>>(), [2, 3]);
}

#[test]
fn a_steady_link_drops_nothing() {
    let queue = SendQueue::new(DEPTH);
    let (mut encoder, mut decoder) = (Encoder::new(), Decoder::default());
    // The send of a frame takes up to two frame periods now and then, as on the USB network at 200 Mbps:
    // the frames wait at most two at a time
    for second in 0..10 {
        for frame in 0..120 {
            encoder.push(&queue);
            if (frame + second) % 7 != 0 {
                while let Some(frame) = try_pop(&queue) {
                    decoder.take(frame);
                }
            }
        }
    }
    assert_eq!(queue.take_counts().dropped, 0);
    assert!(encoder.pushes.iter().all(|p| *p == Push::Queued));
    decoder.take_all(&queue);
    assert_eq!(decoder.frames.len(), 1200);
}

#[test]
fn a_waiting_send_gets_the_next_frame() {
    let queue = std::sync::Arc::new(SendQueue::new(DEPTH));
    let sender = {
        let queue = std::sync::Arc::clone(&queue);
        std::thread::spawn(move || queue.pop())
    };
    std::thread::sleep(std::time::Duration::from_millis(20));
    queue.push(Frame { number: 0, key: true }, true);
    assert_eq!(sender.join().unwrap(), Some(Frame { number: 0, key: true }));
}
