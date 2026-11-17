use super::*;
use crate::headset::FrameSlice;
use cranvr_proto::{
    link::ClientLink,
    media::FrameReceiver,
    message::{ClientCaps, Haptics, Recenter, Views},
    wire::{Fov, Motion},
};
use std::{
    net::TcpListener,
    sync::mpsc::{channel, Receiver},
};

fn caps(name: &str) -> ClientCaps {
    let fov = Fov { left: -0.9, right: 0.8, up: 0.9, down: -1.0 };
    ClientCaps {
        name: name.into(),
        eye_size: [2064, 2208],
        views: Views { fov: [fov, fov], eye_pose: [ProtoPose { position: [-0.032, 0.0, 0.0], ..ProtoPose::IDENTITY }, ProtoPose { position: [0.032, 0.0, 0.0], ..ProtoPose::IDENTITY }] },
        refresh_rates: vec![72.0, 120.0],
        codecs: vec![Codec::Hevc],
        decoders: 1,
        foveation: true,
        tcp_only: false,
    }
}

fn config(port: u16) -> Config {
    Config {
        port,
        trust: vec![],
        trust_all: false,
        tcp_media: false,
        wishes: HostWishes {
            eye_width: 2064,
            eye_width_unfoveated: 0,
            refresh_hz: 120.0,
            bitrate_bps: 100_000_000,
            codecs: vec![Codec::Hevc],
            foveation: Some(FoveationConfig::default().into()),
            full_range: true,
            slices: 1,
        },
    }
}

/// A port that nothing listens on now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn tracking(sample: u64, x: f32) -> Tracking {
    Tracking {
        sample_time: sample,
        head: Motion { pose: ProtoPose { position: [x, 1.6, 0.0], ..ProtoPose::IDENTITY }, ..Default::default() },
        left: Some(Motion::default()),
        right: None,
    }
}

/// What the test's client threads hand to the test.
enum Heard {
    Datagram(HostDatagram),
    Control(HostMsg),
    Frame(FrameMeta, Vec<u8>),
}

fn wait_for(events: &mut dyn EventSource, mut wanted: impl FnMut(&Event) -> bool) -> Event {
    for _ in 0..200 {
        if let Ok(event) = events.next(Duration::from_millis(50)) {
            if wanted(&event) {
                return event;
            }
        }
    }
    panic!("the event did not come");
}

#[test]
fn history_returns_the_exact_sample_or_the_nearest() {
    let mut history = History::default();
    for i in 0..10u64 {
        history.push(tracking(1000 + i * 100, i as f32));
    }
    assert_eq!(history.find(1300).unwrap().head.pose.position[0], 3.0);
    assert_eq!(history.find(1340).unwrap().head.pose.position[0], 3.0);
    assert_eq!(history.find(1360).unwrap().head.pose.position[0], 4.0);
    assert_eq!(history.find(5).unwrap().head.pose.position[0], 0.0);
    // Old samples fall out
    for i in 10..(HISTORY as u64 + 20) {
        history.push(tracking(1000 + i * 100, i as f32));
    }
    assert_eq!(history.0.len(), HISTORY);
    assert!(history.find(1000).unwrap().sample_time > 1000);
    assert!(History::default().find(1).is_none());
}

#[test]
fn input_changes_name_the_buttons_that_moved() {
    let mut state = InputState::default();
    let mut input = Input::default();
    // The first input reports everything: 2 hands, 6 clicks, 5 touches and 4 analog values
    assert_eq!(state.changes(&input).len(), 2 * (6 + 5 + 4));
    assert!(state.changes(&input).is_empty());
    input.hands[1].buttons = buttons::PRIMARY | buttons::STICK_CLICK;
    input.hands[1].trigger = 0.75;
    let changes = state.changes(&input);
    assert_eq!(changes.len(), 3);
    let id = |hand: usize, i: usize| hand_ids(hand).clicks[i].1;
    assert!(changes.iter().any(|b| b.path_id == id(1, 0) && !b.scalar && b.value == 1.0));
    assert!(changes.iter().any(|b| b.path_id == *RIGHT_THUMBSTICK_CLICK_ID && b.value == 1.0));
    assert!(changes.iter().any(|b| b.path_id == *RIGHT_TRIGGER_VALUE_ID && b.scalar && b.value == 0.75));
    input.hands[1].buttons = 0;
    let changes = state.changes(&input);
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().all(|b| b.value == 0.0));
}

#[test]
fn the_display_grid_gives_the_next_refresh() {
    // Refreshes at 1000, 1100, 1200, ...
    assert_eq!(grid_at_or_after(1000, 100, 1000), 1000);
    assert_eq!(grid_at_or_after(1000, 100, 1001), 1100);
    assert_eq!(grid_at_or_after(1000, 100, 1100), 1100);
    assert_eq!(grid_at_or_after(1000, 100, 1950), 2000);
    // A time before the first known refresh waits for it
    assert_eq!(grid_at_or_after(1000, 100, 400), 1000);
}

#[test]
fn a_native_client_connects_and_the_host_serves_it() {
    let port = free_port();
    let (headset, mut events) = start(config(port)).unwrap();
    let client = ClientLink::connect(([127, 0, 0, 1], port).into(), &caps("Test headset"), Duration::from_secs(10)).unwrap();
    let client = Arc::new(client);

    let Event::Connected { stream: Some(stream), ip } = wait_for(&mut *events, |e| matches!(e, Event::Connected { .. })) else { unreachable!() };
    assert_eq!((stream.eye_width, stream.eye_height, stream.fps), (2064, 2208, 120.0));
    assert!(stream.foveated && stream.codec == Codec::Hevc);
    assert_eq!(ip, Some([127, 0, 0, 1].into()));
    let Event::Views(views) = wait_for(&mut *events, |e| matches!(e, Event::Views(_))) else { unreachable!() };
    assert!((views.fov[0].left + 0.9).abs() < 1e-6);

    // The client's read loops: datagrams to a channel, media into a receiver
    let (to_test, from_client): (_, Receiver<Heard>) = channel();
    {
        let (udp_client, udp_tx) = (Arc::clone(&client), to_test.clone());
        thread::spawn(move || {
            let mut receiver = FrameReceiver::new();
            udp_client.duplex.read_udp(|incoming| match incoming {
                Incoming::Media(packet) => {
                    if let Some(unit) = receiver.push(packet, 1).unwrap() {
                        udp_tx.send(Heard::Frame(unit.meta.clone(), unit.payload().to_vec())).ok();
                    }
                }
                Incoming::Datagram(d) => {
                    udp_tx.send(Heard::Datagram(d)).ok();
                }
                Incoming::Control(_) => {}
            })
        });
        let tcp_client = Arc::clone(&client);
        thread::spawn(move || {
            tcp_client.duplex.read_tcp(|incoming| {
                if let Incoming::Control(m) = incoming {
                    to_test.send(Heard::Control(m)).ok();
                }
            })
        });
    }

    // Tracking reaches the frame loop's interface. UDP may lose a datagram on a busy machine, so
    // the client repeats it.
    let mut sample = 5_000_000_000u64;
    loop {
        client.duplex.send_datagram(&ClientDatagram::Tracking(tracking(sample, 0.5))).unwrap();
        if let Ok(Event::Tracking { sample_ns }) = events.next(Duration::from_millis(100)) {
            assert_eq!(sample_ns, sample);
            break;
        }
        sample += 1;
    }
    let head = headset.motion(Device::Head, sample).unwrap();
    assert_eq!(head.pose.position.x, 0.5);
    assert!(headset.motion(Device::LeftHand, sample).is_some());
    assert!(headset.motion(Device::RightHand, sample).is_none());

    // The host answers a ping and sends its own, so both sides can estimate the clock offset
    client.duplex.send_datagram(&ClientDatagram::Ping(Ping { id: 9, sent: 1234 })).unwrap();
    let mut got_pong = false;
    let mut got_ping = false;
    while !(got_pong && got_ping) {
        match from_client.recv_timeout(Duration::from_secs(10)).unwrap() {
            Heard::Datagram(HostDatagram::Pong(p)) if p.id == 9 => got_pong = true,
            Heard::Datagram(HostDatagram::Ping(p)) => {
                got_ping = true;
                let pong = Pong { id: p.id, ping_sent: p.sent, received: now_ns(), sent: now_ns() };
                client.duplex.send_datagram(&ClientDatagram::Pong(pong)).unwrap();
            }
            _ => {}
        }
    }

    // A decoder config, then a frame with parity
    headset.set_decoder_config(vec![0, 0, 0, 1, 0x40, 1]);
    loop {
        if let Heard::Control(HostMsg::DecoderConfig { config, .. }) = from_client.recv_timeout(Duration::from_secs(10)).unwrap() {
            assert_eq!(config, vec![0, 0, 0, 1, 0x40, 1]);
            break;
        }
    }
    let data: Vec<u8> = (0..40_000u32).map(|i| (i * 7) as u8).collect();
    let mut seen = None;
    for _ in 0..20 {
        headset.send_frame(OutFrame {
            sample_ns: sample,
            idr: true,
            eye_poses: [
                AlvrPose { orientation: Quat::from_rotation_y(0.5), position: Vec3::new(0.468, 1.6, -0.25) },
                AlvrPose { orientation: Quat::from_rotation_y(0.5), position: Vec3::new(0.532, 1.6, -0.25) },
            ],
            slices: vec![FrameSlice { rect: [0, 0, 2432, 1152], data: data.clone() }],
        });
        if let Ok(Heard::Frame(meta, bytes)) = from_client.recv_timeout(Duration::from_millis(500)) {
            seen = Some((meta, bytes));
            break;
        }
    }
    let (meta, bytes) = seen.expect("a frame arrived");
    assert!(meta.frame < 20);
    assert_eq!(bytes, data);
    // The metadata that the client's compositor needs
    assert!(meta.key);
    assert_eq!(meta.tracking_sample, sample);
    assert_eq!(meta.slices, vec![Rect { x: 0, y: 0, w: 2432, h: 1152 }]);
    assert_eq!(meta.eye_poses[0].position, [0.468, 1.6, -0.25]);
    assert_eq!(meta.eye_poses[1].position, [0.532, 1.6, -0.25]);
    assert_eq!(meta.eye_poses[0].orientation, Quat::from_rotation_y(0.5).to_array());
    assert!(meta.sent > 0);

    // Haptics
    headset.send_haptics(Haptic { hand: 1, duration: Duration::from_millis(20), frequency: 160.0, amplitude: 0.5 });
    loop {
        if let Heard::Datagram(HostDatagram::Haptics(Haptics { hand, amplitude, .. })) = from_client.recv_timeout(Duration::from_secs(10)).unwrap() {
            assert_eq!((hand, amplitude), (1, 0.5));
            break;
        }
    }

    // A recenter of the headset reaches the frame loop as an event
    client.duplex.send_control(&ClientMsg::Recenter(Recenter { change_time: now_ns(), origin: None })).unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::Recenter));

    // A client that leaves ends the session
    client.duplex.send_control(&ClientMsg::Bye).unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::Disconnected));
    assert!(headset.motion(Device::Head, sample).is_none());
}

#[test]
fn a_second_headset_gets_busy_and_the_host_takes_the_next_one() {
    let port = free_port();
    let (_headset, mut events) = start(config(port)).unwrap();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let first = ClientLink::connect(addr, &caps("First"), Duration::from_secs(10)).unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::Connected { .. }));
    let second = ClientLink::connect(addr, &caps("Second"), Duration::from_secs(10));
    assert!(matches!(second, Err(cranvr_proto::Error::Rejected(RejectReason::Busy))), "{:?}", second.err());
    first.duplex.close();
    wait_for(&mut *events, |e| matches!(e, Event::Disconnected));
    // The host takes the next client
    let third = ClientLink::connect(addr, &caps("Third"), Duration::from_secs(10)).unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::Connected { .. }));
    third.duplex.close();
}
