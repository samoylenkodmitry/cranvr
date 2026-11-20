//! The desktop stream and its messages: a second video stream beside the eye stream, the pointer
//! that goes to the host and the cursor that comes back.

mod common;

use common::{packets, Rng};
use cranvr_proto::{
    media::{sample_meta, FrameReceiver},
    message::*,
    wire::{decode, encode, Codec},
};

fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(value: T) {
    let mut bytes = Vec::new();
    encode(&value, &mut bytes).unwrap();
    assert_eq!(decode::<T>(&bytes).unwrap(), value);
}

fn params() -> DesktopParams {
    DesktopParams {
        stream: DESKTOP_STREAM,
        codec: Codec::Hevc,
        size: [3840, 2160],
        points: [1920.0, 1080.0],
        max_fps: 60.0,
        bitrate_bps: 40_000_000,
        full_range: true,
        flipped: false,
        visible: true,
        cursor_in_video: true,
    }
}

#[test]
fn the_desktop_messages_round_trip() {
    round_trip(ClientMsg::Desktop(DesktopWish { enabled: true, max_width: 3840, max_fps: 60 }));
    round_trip(HostMsg::Desktop(Some(params())));
    round_trip(HostMsg::Desktop(None));
    round_trip(ClientDatagram::Mouse(Mouse { seq: 4, sent: 99, hit: Some([0.1, 0.9]), buttons: 0b101, scroll: [-0.25, 1.0] }));
    round_trip(ClientDatagram::Mouse(Mouse { seq: 5, sent: 0, hit: None, buttons: 0, scroll: [0.0, 0.0] }));
    round_trip(HostDatagram::Cursor(Cursor { seq: 3, u: 0.5, v: 0.125, on: true }));
}

/// A desktop picture and an eye frame with the same frame number are two units. Neither hides the
/// other. Each comes back under its own stream.
#[test]
fn the_desktop_stream_has_its_own_units() {
    let mut rng = Rng(11);
    let eye_payload = rng.bytes(30_000);
    let desktop_payload = rng.bytes(45_000);
    let eye = sample_meta(0, 5);
    let desktop = sample_meta(DESKTOP_STREAM, 5);
    let mut receiver = FrameReceiver::new();
    let mut packets = [packets(&eye, &eye_payload, 0.1), packets(&desktop, &desktop_payload, 0.1)].concat();
    // The packets of the two units arrive mixed
    rng.shuffle(&mut packets);
    let mut got = Vec::new();
    for packet in &packets {
        if let Some(unit) = receiver.push(packet, 1).unwrap() {
            got.push((unit.meta.stream, unit.payload().to_vec()));
        }
    }
    got.sort_by_key(|(stream, _)| *stream);
    assert_eq!(got, vec![(0, eye_payload), (DESKTOP_STREAM, desktop_payload)]);
    assert!(receiver.take_lost().is_empty());
}

/// A desktop unit that never completes is a lost unit of the desktop stream, so the client asks
/// for a key frame of that stream and leaves the eye stream alone.
#[test]
fn a_lost_desktop_unit_names_its_stream() {
    let payload = Rng(12).bytes(60_000);
    let meta = sample_meta(DESKTOP_STREAM, 40);
    let all = packets(&meta, &payload, 0.0);
    let mut receiver = FrameReceiver::new();
    // The first half of the data packets arrives. Without parity the unit cannot complete.
    for packet in &all[..all.len() / 2] {
        assert!(receiver.push(packet, 1_000).unwrap().is_none());
    }
    receiver.expire(1_000 + 200_000_000, 100_000_000);
    let lost = receiver.take_lost();
    assert_eq!(lost.len(), 1);
    assert_eq!((lost[0].stream, lost[0].frame), (DESKTOP_STREAM, 40));
}
