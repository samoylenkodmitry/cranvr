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
    round_trip(HostMsg::GameScreen(Some(GameScreen {
        loading: Some("LOADING".into()),
        lines: vec!["A tip".into()],
        message: Some("Press Trigger To Start".into()),
        facing_deg: Some(90.0),
    })));
    round_trip(HostMsg::GameScreen(None));
    round_trip(ClientDatagram::DisplayMouse { stream: 2, mouse: Mouse { seq: 1, sent: 5, hit: Some([0.5, 0.5]), buttons: 3, scroll: [0.0, 0.5] } });
    round_trip(HostDatagram::DisplayCursor { stream: 2, cursor: Cursor { seq: 8, u: 0.25, v: 0.75, on: true } });
    let input = Input { sample_time: 77, hands: [HandInput { trigger: 0.5, ..Default::default() }, HandInput { buttons: buttons::PRIMARY, ..Default::default() }] };
    round_trip(ClientDatagram::InputHold(InputHold { input, held: [false, true] }));
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
