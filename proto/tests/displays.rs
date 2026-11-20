//! Displays: the client adds and removes displays of the Mac, asks for the video of each one and
//! holds a hand back from the game while it points at a panel.

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

fn tag<T: serde::Serialize>(value: &T) -> u8 {
    let mut bytes = Vec::new();
    encode(value, &mut bytes).unwrap();
    bytes[0]
}

fn params(stream: u8) -> DesktopParams {
    DesktopParams {
        stream,
        codec: Codec::Hevc,
        size: [3840, 2160],
        points: [1920.0, 1080.0],
        max_fps: 30.0,
        bitrate_bps: 12_000_000,
        full_range: true,
        flipped: false,
        visible: true,
        cursor_in_video: true,
    }
}

#[test]
fn a_display_has_the_stream_after_the_first_ones() {
    assert_eq!(display_stream(0), DESKTOP_STREAM);
    assert_eq!(display_index(DESKTOP_STREAM), Some(0));
    assert_eq!(display_index(display_stream(MAX_DISPLAYS - 1)), Some(MAX_DISPLAYS - 1));
    // The eye stream and a stream past the last display are no display's
    assert_eq!(display_index(0), None);
    assert_eq!(display_index(display_stream(MAX_DISPLAYS)), None);
}

#[test]
fn the_display_messages_round_trip() {
    round_trip(ClientMsg::Display(DisplayRequest::Add));
    round_trip(ClientMsg::Display(DisplayRequest::Remove { stream: 3 }));
    round_trip(ClientMsg::DisplayWish { stream: 2, wish: DesktopWish { enabled: true, max_width: 1920, max_fps: 30 } });
    round_trip(ClientMsg::DisplayWish { stream: 2, wish: DesktopWish { enabled: false, max_width: 0, max_fps: 0 } });
    round_trip(HostMsg::Displays(DisplayList {
        displays: vec![
            DisplayInfo { stream: 1, name: "Mac display".into(), points: [3008, 1692], removable: false },
            DisplayInfo { stream: 2, name: "Added display 1".into(), points: [1920, 1080], removable: true },
        ],
        can_add: true,
        notice: Some("macOS made no display".into()),
    }));
    round_trip(HostMsg::Displays(DisplayList::default()));
    round_trip(HostMsg::DisplayStream { stream: 3, params: Some(params(3)) });
    round_trip(HostMsg::DisplayStream { stream: 3, params: None });
    round_trip(HostMsg::GameRuns(true));
    round_trip(ClientDatagram::DisplayPointer { stream: 2, pointer: Pointer { seq: 1, sent: 5, hit: Some([0.5, 0.5]), trigger: 1.0, scroll: 0.0 } });
    round_trip(HostDatagram::DisplayCursor { stream: 2, cursor: Cursor { seq: 8, u: 0.25, v: 0.75, on: true } });
    let input = Input { sample_time: 77, hands: [HandInput { trigger: 0.5, ..Default::default() }, HandInput { buttons: buttons::PRIMARY, ..Default::default() }] };
    round_trip(ClientDatagram::InputHold(InputHold { input, held: [false, true] }));
}

/// The new variants stand after the old ones, so a peer of version 7 reads every old message as
/// before and skips these.
#[test]
fn the_new_variants_stand_after_the_old_ones() {
    // The last variant of version 7 and the first of version 8 in each enum
    assert_eq!(tag(&ClientMsg::Focus(Focus::Focused)), 10);
    assert_eq!(tag(&ClientMsg::Display(DisplayRequest::Add)), 11);
    assert_eq!(tag(&ClientMsg::DisplayWish { stream: 1, wish: DesktopWish { enabled: false, max_width: 0, max_fps: 0 } }), 12);
    assert_eq!(tag(&HostMsg::TrackingWish(cranvr_proto::tracking::Features::NONE)), 10);
    assert_eq!(tag(&HostMsg::Displays(DisplayList::default())), 11);
    assert_eq!(tag(&HostMsg::DisplayStream { stream: 1, params: None }), 12);
    assert_eq!(tag(&HostMsg::GameRuns(false)), 13);
    // A datagram of version 6 and the version 8 ones that follow it
    let eyes = tag(&ClientDatagram::Eyes(cranvr_proto::tracking::Eyes::default()));
    let display_pointer = tag(&ClientDatagram::DisplayPointer { stream: 1, pointer: Pointer { seq: 0, sent: 0, hit: None, trigger: 0.0, scroll: 0.0 } });
    let held = tag(&ClientDatagram::InputHold(InputHold::default()));
    assert!(eyes < display_pointer && display_pointer < held);
    assert_eq!(tag(&HostDatagram::Cursor(Cursor { seq: 0, u: 0.0, v: 0.0, on: false })), 4);
    assert_eq!(tag(&HostDatagram::DisplayCursor { stream: 1, cursor: Cursor { seq: 0, u: 0.0, v: 0.0, on: false } }), 5);
}

/// A host of version 7 has no such variant: its read loop skips the message.
#[test]
fn a_peer_of_version_7_skips_the_new_host_messages() {
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    enum V7HostMsg {
        Welcome(Welcome),
        Reject(RejectReason),
        MediaReady,
        DecoderConfig { stream: u8, codec: Codec, config: Vec<u8> },
        StreamUpdate(StreamParams),
        Bye,
        Desktop(Option<DesktopParams>),
        Menu(String),
        AwaitApproval { host_name: String, commit: [u8; 32] },
        PairReveal([u8; 32]),
        TrackingWish(cranvr_proto::tracking::Features),
    }
    let mut bytes = Vec::new();
    encode(&HostMsg::GameRuns(true), &mut bytes).unwrap();
    assert!(decode::<V7HostMsg>(&bytes).is_err());
    bytes.clear();
    encode(&HostMsg::DisplayStream { stream: 2, params: None }, &mut bytes).unwrap();
    assert!(decode::<V7HostMsg>(&bytes).is_err());
    bytes.clear();
    encode(&HostMsg::Desktop(None), &mut bytes).unwrap();
    assert!(matches!(decode::<V7HostMsg>(&bytes).unwrap(), V7HostMsg::Desktop(None)));
}

/// Each display's pictures are units of their own stream, with their own frame numbers.
#[test]
fn two_displays_with_the_same_frame_number_are_two_units() {
    let mut rng = Rng(21);
    let first = rng.bytes(20_000);
    let second = rng.bytes(25_000);
    let mut receiver = FrameReceiver::new();
    let mut packets = [
        packets(&sample_meta(display_stream(0), 4), &first, 0.1),
        packets(&sample_meta(display_stream(1), 4), &second, 0.1),
    ]
    .concat();
    rng.shuffle(&mut packets);
    let mut got = Vec::new();
    for packet in &packets {
        if let Some(unit) = receiver.push(packet, 1).unwrap() {
            got.push((unit.meta.stream, unit.payload().to_vec()));
        }
    }
    got.sort_by_key(|(stream, _)| *stream);
    assert_eq!(got, vec![(display_stream(0), first), (display_stream(1), second)]);
}
