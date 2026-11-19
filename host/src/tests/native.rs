use super::*;
use crate::headset::{FramePlan, FrameSlice};
use cranvr_proto::{
    link::ClientLink,
    media::FrameReceiver,
    message::{ClientCaps, FrameTiming, Haptics, Recenter, Views},
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

fn identity() -> cranvr_proto::identity::Identity {
    cranvr_proto::identity::Identity::from_seed([5; 32])
}

/// A folder of its own for the host's files, empty at the start of the test
fn config_dir(port: u16) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cranvr-native-test-{port}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::remove_file(dir.join("paired-headsets.txt")).ok();
    dir
}

fn config(port: u16) -> Config {
    Config {
        port,
        identity: cranvr_proto::identity::Identity::from_seed([7; 32]),
        pairings: Arc::new(crate::pairing::Pairings::load(&config_dir(port), None)),
        name: "Test Mac".into(),
        discovery_port: None,
        approval_wait: Duration::from_secs(30),
        approve_loopback: false,
        trust_all: false,
        display_lock: true,
        desktop: Default::default(),
        audio: false,
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
        display_time: sample + 18_000_000,
        head: Motion { pose: ProtoPose { position: [x, 1.6, 0.0], ..ProtoPose::IDENTITY }, linear_velocity: [1.0, 0.0, 0.0], ..Default::default() },
        left_grip: Some(Motion::default()),
        right_grip: None,
        // The aim pose lies below and ahead of the grip pose, turned down by 30 degrees, and moves
        // on its own
        left_aim: Some(Motion {
            pose: ProtoPose { orientation: Quat::from_rotation_x(-30f32.to_radians()).to_array(), position: [0.0, -0.05, -0.1] },
            linear_velocity: [0.0, 0.0, 2.0],
            angular_velocity: [0.5, 0.0, 0.0],
        }),
        right_aim: None,
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
    assert!(changes.iter().any(|b| b.path_id == RIGHT_THUMBSTICK_CLICK_ID && b.value == 1.0));
    assert!(changes.iter().any(|b| b.path_id == RIGHT_TRIGGER_VALUE_ID && b.scalar && b.value == 0.75));
    input.hands[1].buttons = 0;
    let changes = state.changes(&input);
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().all(|b| b.value == 0.0));
}

#[test]
fn a_hand_has_a_grip_pose_and_an_aim_pose_that_the_host_does_not_offset() {
    let motion = |x: f32, pitch: f32| Motion {
        pose: ProtoPose { orientation: Quat::from_rotation_x(pitch.to_radians()).to_array(), position: [x, 1.2, -0.3] },
        linear_velocity: [x, 0.0, -1.0],
        angular_velocity: [0.0, x, 0.0],
    };
    // Every pose of the sample differs, so a swap of two of them shows
    let sample = Tracking {
        left_grip: Some(motion(-0.1, 10.0)),
        right_grip: Some(motion(0.1, 20.0)),
        left_aim: Some(motion(-0.2, -40.0)),
        right_aim: Some(motion(0.2, -50.0)),
        ..tracking(1, 0.0)
    };
    for (device, pose, expected) in [
        (Device::LeftHand, HandPose::Grip, sample.left_grip),
        (Device::RightHand, HandPose::Grip, sample.right_grip),
        (Device::LeftHand, HandPose::Aim, sample.left_aim),
        (Device::RightHand, HandPose::Aim, sample.right_aim),
    ] {
        let given = device_of(&sample, device, pose);
        assert_eq!(given, expected, "{device:?} {pose:?}");
        // Only the floor moves a pose: the orientation and the velocities stay as the headset gave them
        let hand = device_motion(given.unwrap(), 0.0);
        let sent = expected.unwrap();
        assert_eq!(hand.pose.orientation, Quat::from_array(sent.pose.orientation));
        assert_eq!(hand.pose.position, Vec3::from_array(sent.pose.position));
        assert_eq!(hand.linear_velocity, Vec3::from_array(sent.linear_velocity));
        assert_eq!(hand.angular_velocity, Vec3::from_array(sent.angular_velocity));
    }
    // A hand without an aim pose has none: the runtime then gives a game the grip pose for both
    let grip_only = Tracking { left_aim: None, ..sample };
    assert!(device_of(&grip_only, Device::LeftHand, HandPose::Aim).is_none());
    assert!(device_of(&grip_only, Device::LeftHand, HandPose::Grip).is_some());
    // The head has one pose
    assert_eq!(device_of(&sample, Device::Head, HandPose::Aim), Some(sample.head));
    // The floor lowers every pose by its height
    let head = device_motion(sample.head, 0.3);
    assert!((head.pose.position - Vec3::new(0.0, 1.3, 0.0)).length() < 1e-6);
}

#[test]
fn the_display_grid_gives_the_next_refresh() {
    // Refreshes at 1000, 1100, 1200, ...
    assert_eq!(grid_at_or_after(1000, 100, 1000), 1000);
    assert_eq!(grid_at_or_after(1000, 100, 1001), 1100);
    assert_eq!(grid_at_or_after(1000, 100, 1100), 1100);
    assert_eq!(grid_at_or_after(1000, 100, 1950), 2000);
    // The grid reaches back from the newest refresh: the display shows frames at those points too
    assert_eq!(grid_at_or_after(1000, 100, 400), 400);
    assert_eq!(grid_at_or_after(1000, 100, 401), 500);
    assert_eq!(grid_at_or_after(1000, 100, 999), 1000);
    assert_eq!(grid_at_or_after(1000, 100, 10), 100);
    // A period that is not known leaves the later of the two
    assert_eq!(grid_at_or_after(1000, 0, 400), 1000);
}

#[test]
fn a_native_client_connects_and_the_host_serves_it() {
    let port = free_port();
    let (headset, mut events) = start(config(port)).unwrap();
    let client = ClientLink::connect(([127, 0, 0, 1], port).into(), &caps("Test headset"), &identity(), None, Duration::from_secs(10)).unwrap();
    let client = Arc::new(client);

    let Event::Connected { stream, ip } = wait_for(&mut *events, |e| matches!(e, Event::Connected { .. })) else { unreachable!() };
    assert_eq!((stream.eye_width, stream.eye_height, stream.fps), (2064, 2208, 120.0));
    assert!(stream.foveated && stream.codec == Codec::Hevc);
    assert_eq!(ip, std::net::IpAddr::from([127, 0, 0, 1]));
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
    // The aim pose is its own pose of the same hand
    let aim = headset.aim_motion(Device::LeftHand, sample).unwrap();
    assert_eq!(aim.pose.position, Vec3::new(0.0, -0.05, -0.1));
    assert_eq!(aim.linear_velocity, Vec3::new(0.0, 0.0, 2.0));
    assert!(headset.aim_motion(Device::RightHand, sample).is_none());
    // The pose moves by its velocity to a later display time: 30 ms at 1 m/s are 3 cm. The sample
    // is valid at its own display time, so a target at that time changes nothing. A target before
    // it does not move the pose back.
    let valid_at = sample + 18_000_000;
    let later = headset.motion_at(Device::Head, sample, valid_at + 30_000_000).unwrap();
    assert!((later.pose.position.x - 0.53).abs() < 1e-4, "{}", later.pose.position.x);
    assert_eq!(headset.motion_at(Device::Head, sample, valid_at).unwrap().pose.position.x, 0.5);
    assert_eq!(headset.motion_at(Device::Head, sample, valid_at - 5_000_000).unwrap().pose.position.x, 0.5);
    // A target far away is cut at the limit of 80 ms
    let far = headset.motion_at(Device::Head, sample, valid_at + 900_000_000).unwrap();
    assert!((far.pose.position.x - 0.58).abs() < 1e-4);

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
            plan: Some(FramePlan { start_in: Duration::ZERO, target: 7_000_000_000, start: 6_990_000_000 }),
            sample_ns: sample,
            idr: true,
            eye_poses: [
                RoomPose { orientation: Quat::from_rotation_y(0.5), position: Vec3::new(0.468, 1.6, -0.25) },
                RoomPose { orientation: Quat::from_rotation_y(0.5), position: Vec3::new(0.532, 1.6, -0.25) },
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
    // The frame carries the target of its plan
    assert_eq!(meta.target_display, 7_000_000_000);

    // The headset's floor lies 0.4 m below the room's origin. The host's scenes get the room moved
    // up by that much. The render poses that go back to the headset stay in its room.
    client.duplex.send_control(&ClientMsg::Floor(-0.4)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while (headset.motion(Device::Head, sample).unwrap().pose.position.y - 2.0).abs() > 1e-4 {
        assert!(Instant::now() < deadline, "the host did not take the floor");
        thread::sleep(Duration::from_millis(5));
    }
    // The hand lies at the room's origin, which the floor moves up. No offset lowers it. The aim
    // pose lies 5 cm below the grip pose.
    assert!((headset.motion_at(Device::LeftHand, sample, sample).unwrap().pose.position.y - 0.4).abs() < 1e-4);
    assert!((headset.aim_motion_at(Device::LeftHand, sample, sample).unwrap().pose.position.y - 0.35).abs() < 1e-4);
    // The headset reports the planned frame: it showed 20 ms after its arrival, and its compositor
    // took it 17 ms before. The host then knows the display's grid and the lead.
    let arrived = now_ns();
    let timing = FrameTiming {
        stream: 0,
        frame: meta.frame,
        tracking_sample: sample,
        received: arrived,
        decode_end: arrived + 2_000_000,
        picked: arrived + 3_000_000,
        display_time: arrived + 20_000_000,
        next_display_time: arrived + 28_333_333,
    };
    let mut seen = None;
    for _ in 0..20 {
        client.duplex.send_datagram(&ClientDatagram::FrameTiming(timing)).unwrap();
        headset.send_frame(OutFrame {
            plan: None,
            sample_ns: sample,
            idr: false,
            // The scene's eyes 2.0 m above the floor are 1.6 m above the room's origin
            eye_poses: [
                RoomPose { orientation: Quat::IDENTITY, position: Vec3::new(0.468, 2.0, -0.25) },
                RoomPose { orientation: Quat::IDENTITY, position: Vec3::new(0.532, 2.0, -0.25) },
            ],
            slices: vec![FrameSlice { rect: [0, 0, 2432, 1152], data: data.clone() }],
        });
        if let Ok(Heard::Frame(meta, _)) = from_client.recv_timeout(Duration::from_millis(500)) {
            seen = Some(meta);
            break;
        }
    }
    let meta = seen.expect("a frame arrived after the floor");
    assert!((meta.eye_poses[0].position[1] - 1.6).abs() < 1e-4 && (meta.eye_poses[1].position[1] - 1.6).abs() < 1e-4);
    // A frame that no plan started, such as a game's, aims for a display point at which the
    // headset can show it: after the frame's arrival and after the headset's take, which comes one
    // lead before the point. The report taught the host the lead of 17 ms. The first frames may
    // come before the report, so the test sends more.
    let mut target_ok = false;
    for _ in 0..40 {
        client.duplex.send_datagram(&ClientDatagram::FrameTiming(timing)).unwrap();
        headset.send_frame(OutFrame {
            plan: None,
            sample_ns: sample,
            idr: false,
            eye_poses: Default::default(),
            slices: vec![FrameSlice { rect: [0, 0, 2432, 1152], data: data.clone() }],
        });
        if let Ok(Heard::Frame(later, _)) = from_client.recv_timeout(Duration::from_millis(500)) {
            if later.target_display >= later.sent + 17_000_000 {
                target_ok = true;
                break;
            }
        }
    }
    assert!(target_ok, "the target of an unplanned frame lies a lead after its arrival");

    // Haptics
    headset.send_haptics(Haptic { hand: 1, duration: Duration::from_millis(20), frequency: 160.0, amplitude: 0.5 });
    loop {
        if let Heard::Datagram(HostDatagram::Haptics(Haptics { hand, amplitude, .. })) = from_client.recv_timeout(Duration::from_secs(10)).unwrap() {
            assert_eq!((hand, amplitude), (1, 0.5));
            break;
        }
    }

    // The console: the host's menu reaches the client and a button's request reaches the host
    headset.send_menu(r#"{"games":[],"fps":120}"#);
    loop {
        if let Heard::Control(HostMsg::Menu(menu)) = from_client.recv_timeout(Duration::from_secs(10)).unwrap() {
            assert_eq!(menu, r#"{"games":[],"fps":120}"#);
            break;
        }
    }
    client.duplex.send_control(&ClientMsg::Console(r#"{"action":"play","game":"Beat Saber"}"#.into())).unwrap();
    let Event::Console(request) = wait_for(&mut *events, |e| matches!(e, Event::Console(_))) else { unreachable!() };
    assert_eq!(request, r#"{"action":"play","game":"Beat Saber"}"#);

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
    let first = ClientLink::connect(addr, &caps("First"), &identity(), None, Duration::from_secs(10)).unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::Connected { .. }));
    let second = ClientLink::connect(addr, &caps("Second"), &identity(), None, Duration::from_secs(10));
    assert!(matches!(second, Err(cranvr_proto::Error::Rejected(RejectReason::Busy))), "{:?}", second.err());
    first.duplex.close();
    wait_for(&mut *events, |e| matches!(e, Event::Disconnected));
    // The host takes the next client
    let third = ClientLink::connect(addr, &caps("Third"), &identity(), None, Duration::from_secs(10)).unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::Connected { .. }));
    third.duplex.close();
}

#[test]
fn the_status_line_tells_the_app_who_is_paired_and_who_waits() {
    let dir = config_dir(1);
    // Nobody is paired and nobody waits
    let none = status_line(9950, &Pairings::load(&dir, None));
    assert_eq!(none, json!({"type": "native", "port": 9950, "paired": [], "pairing_s": null, "pending": []}));
    // A code opens the window of the older protocol for ten minutes
    let open = status_line(9951, &Pairings::load(&dir, Some(123_456)));
    assert_eq!(open["port"].as_u64(), Some(9951));
    assert!(open["pairing_s"].as_u64().is_some_and(|s| (590..=600).contains(&s)), "{open}");
    // A headset that waits has its key, name, address and the code that both screens show
    let pairings = Pairings::load(&dir, None);
    let key = identity().public_key();
    assert!(pairings.ask(key, "Meta Quest 3", [192, 168, 1, 30].into(), 4829));
    let waiting = status_line(9950, &pairings);
    assert_eq!(
        waiting["pending"],
        json!([{"key": cranvr_proto::identity::to_hex(&key), "name": "Meta Quest 3", "ip": "192.168.1.30", "code": "004 829"}])
    );
}

#[test]
fn the_user_answers_with_a_line_that_names_the_key() {
    let key = identity().public_key();
    let hex = cranvr_proto::identity::to_hex(&key);
    assert_eq!(parse_answer(&format!("allow {hex}")), Some((true, key)));
    assert_eq!(parse_answer(&format!("  deny   {hex}  ")), Some((false, key)));
    assert_eq!(parse_answer(&format!("allow {}", &hex[..60])), None, "a short key");
    assert_eq!(parse_answer(&format!("grant {hex}")), None);
    assert_eq!(parse_answer(&format!("allow {hex} now")), None);
    assert_eq!(parse_answer("allow"), None);
    assert_eq!(parse_answer(""), None);
}

#[test]
fn a_version_mismatch_tells_the_user_which_side_to_update() {
    use cranvr_proto::wire::Error;
    let old = Error::Version { ours: (4, 5), theirs: (4, 4) };
    assert_eq!(version_notice(&old).unwrap(), "A headset tried to connect with a cranvr app of an older version. Update the cranvr app on the headset.");
    let new = Error::Version { ours: (4, 5), theirs: (6, 7) };
    assert_eq!(version_notice(&new).unwrap(), "A headset tried to connect with a cranvr app of a newer version. Update cranvr on the Mac.");
    assert!(version_notice(&Error::Closed).is_none());
}

/// A host that asks the user about loopback clients too, with the files of its own folder
fn asking_host(port: u16, wait: Duration) -> (Arc<Pairings>, Box<dyn EventSource>, Arc<dyn Headset>) {
    let mut config = config(port);
    config.approve_loopback = true;
    config.approval_wait = wait;
    let pairings = Arc::clone(&config.pairings);
    let (headset, events) = start(config).unwrap();
    (pairings, events, headset)
}

/// What the headset's panel shows while it waits
struct Panel(mpsc::Sender<(u32, String)>, Arc<std::sync::atomic::AtomicBool>);

impl cranvr_proto::link::ConnectObserver for Panel {
    fn waiting(&mut self, code: u32, host_name: &str) {
        self.0.send((code, host_name.to_owned())).unwrap();
    }

    fn cancelled(&self) -> bool {
        self.1.load(Ordering::Relaxed)
    }
}

fn panel() -> (Panel, mpsc::Receiver<(u32, String)>, Arc<std::sync::atomic::AtomicBool>) {
    let (shown, seen) = mpsc::channel();
    let give_up = Arc::new(std::sync::atomic::AtomicBool::new(false));
    (Panel(shown, Arc::clone(&give_up)), seen, give_up)
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("{what} did not happen");
}

#[test]
fn an_unknown_headset_waits_for_the_user_and_connects_when_the_user_allows_it() {
    let port = free_port();
    let (pairings, mut events, _headset) = asking_host(port, Duration::from_secs(30));
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let (mut observer, seen, _) = panel();
    let client = thread::spawn(move || ClientLink::connect_with(addr, &caps("Meta Quest 3"), &identity(), None, Duration::from_secs(10), &mut observer));
    // The headset shows the code and the host's name
    let (code, host_name) = seen.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(host_name, "Test Mac");
    // The host lists the headset with the same code and has not paired it
    until("the request", || !pairings.waiting().is_empty());
    let request = pairings.waiting().remove(0);
    assert_eq!((request.name.as_str(), request.code), ("Meta Quest 3", code));
    assert_eq!(request.key, identity().public_key());
    assert!(!pairings.contains(&request.key));
    let status = status_line(port, &pairings);
    assert_eq!(status["pending"][0]["code"], cranvr_proto::identity::format_code(code));
    assert!(!client.is_finished(), "the client waits while the user has not answered");
    // The user presses Allow
    assert!(pairings.answer(&request.key, true));
    let client = client.join().unwrap().unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::Connected { .. }));
    // The headset is paired for good, and the request is gone
    assert!(pairings.contains(&identity().public_key()));
    until("the end of the request", || pairings.waiting().is_empty());
    assert_eq!(status_line(port, &pairings)["pending"], json!([]));
    assert_eq!(status_line(port, &pairings)["paired"], json!(["Meta Quest 3"]));
    let saved = std::fs::read_to_string(config_dir(port).join("paired-headsets.txt")).unwrap_or_default();
    assert!(saved.is_empty() || saved.contains("Meta Quest 3"), "the file belongs to this run's folder");
    client.duplex.close();
    wait_for(&mut *events, |e| matches!(e, Event::Disconnected));
    // The next connection needs no approval: the host knows the key
    let again = ClientLink::connect(addr, &caps("Meta Quest 3"), &identity(), None, Duration::from_secs(10)).unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::Connected { .. }));
    again.duplex.close();
}

#[test]
fn a_denied_headset_hears_no_and_stays_unpaired() {
    let port = free_port();
    let (pairings, _events, _headset) = asking_host(port, Duration::from_secs(30));
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let (mut observer, seen, _) = panel();
    let client = thread::spawn(move || ClientLink::connect_with(addr, &caps("Meta Quest 3"), &identity(), None, Duration::from_secs(10), &mut observer));
    seen.recv_timeout(Duration::from_secs(10)).unwrap();
    until("the request", || !pairings.waiting().is_empty());
    assert!(pairings.answer(&identity().public_key(), false));
    let result = client.join().unwrap();
    assert!(matches!(result, Err(cranvr_proto::Error::Rejected(RejectReason::Denied))), "{:?}", result.err());
    until("the end of the request", || pairings.waiting().is_empty());
    assert!(!pairings.contains(&identity().public_key()));
}

#[test]
fn a_headset_that_nobody_answers_is_told_so_after_the_wait() {
    let port = free_port();
    let (pairings, _events, _headset) = asking_host(port, Duration::from_millis(700));
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let (mut observer, _seen, _) = panel();
    let result = ClientLink::connect_with(addr, &caps("Meta Quest 3"), &identity(), None, Duration::from_secs(10), &mut observer);
    assert!(matches!(result, Err(cranvr_proto::Error::Rejected(RejectReason::NoAnswer))), "{:?}", result.err());
    until("the end of the request", || pairings.waiting().is_empty());
    assert!(!pairings.contains(&identity().public_key()));
}

#[test]
fn a_request_ends_when_the_headset_leaves() {
    let port = free_port();
    let (pairings, _events, _headset) = asking_host(port, Duration::from_secs(30));
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let (mut observer, seen, give_up) = panel();
    let client = thread::spawn(move || ClientLink::connect_with(addr, &caps("Meta Quest 3"), &identity(), None, Duration::from_secs(10), &mut observer));
    seen.recv_timeout(Duration::from_secs(10)).unwrap();
    until("the request", || !pairings.waiting().is_empty());
    // The user of the headset presses Cancel
    give_up.store(true, Ordering::Relaxed);
    assert!(matches!(client.join().unwrap(), Err(cranvr_proto::Error::Closed)));
    until("the end of the request", || pairings.waiting().is_empty());
    assert!(!pairings.contains(&identity().public_key()));
}

#[test]
fn a_second_unknown_headset_gets_busy_while_the_first_waits() {
    let port = free_port();
    let (pairings, _events, _headset) = asking_host(port, Duration::from_secs(30));
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let (mut first, seen, give_up) = panel();
    let first_client = thread::spawn(move || ClientLink::connect_with(addr, &caps("First"), &identity(), None, Duration::from_secs(10), &mut first));
    seen.recv_timeout(Duration::from_secs(10)).unwrap();
    until("the request", || !pairings.waiting().is_empty());
    let other = cranvr_proto::identity::Identity::from_seed([9; 32]);
    let (mut second, _seen, _) = panel();
    let result = ClientLink::connect_with(addr, &caps("Second"), &other, None, Duration::from_secs(10), &mut second);
    assert!(matches!(result, Err(cranvr_proto::Error::Rejected(RejectReason::Busy))), "{:?}", result.err());
    // The first request stays
    assert_eq!(pairings.waiting()[0].name, "First");
    give_up.store(true, Ordering::Relaxed);
    first_client.join().unwrap().err();
}

#[test]
fn a_headset_on_this_mac_needs_no_approval() {
    let port = free_port();
    let (headset, mut events) = start(config(port)).unwrap();
    let _ = headset;
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    // `config` does not ask about loopback clients: a USB link and a flat window need no approval
    let client = ClientLink::connect(addr, &caps("USB headset"), &identity(), None, Duration::from_secs(10)).unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::Connected { .. }));
    client.duplex.close();
}

/// A client of protocol version 4, written by hand: the real client speaks 5. It sends its Hello and
/// returns what the host answers.
fn old_client(addr: SocketAddr, identity: &cranvr_proto::identity::Identity) -> HostMsg {
    use cranvr_proto::{
        message::{client_preamble, VersionRange, HOST_PREAMBLE_LEN},
        secure::{derive, sign_as_client, transcript_hash, Ephemeral, HostHello, Role, HOST_HELLO_LEN},
        wire::{decode, encode, read_tcp_packet, PacketBuf, KIND_CONTROL, KIND_HANDSHAKE},
    };
    use std::io::{Read, Write};
    let mut tcp = std::net::TcpStream::connect(addr).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let client_pre = client_preamble(VersionRange { min: 4, max: 4 });
    tcp.write_all(&client_pre).unwrap();
    let mut host_pre = [0u8; HOST_PREAMBLE_LEN];
    tcp.read_exact(&mut host_pre).unwrap();
    let fresh = Ephemeral::new().unwrap();
    tcp.write_all(&[&33u32.to_le_bytes()[..], &[KIND_HANDSHAKE], &fresh.public()].concat()).unwrap();
    let mut buf = Vec::new();
    read_tcp_packet(&mut tcp, &mut buf).unwrap();
    assert_eq!(buf.len() - 1, HOST_HELLO_LEN);
    let hello = HostHello::parse(&buf[1..]).unwrap();
    let transcript = transcript_hash(&client_pre, &host_pre, &fresh.public(), &hello.ephemeral, &hello.key);
    let mut channels = derive(Role::Client, &fresh.agree(&hello.ephemeral).unwrap(), &transcript);
    let mut auth = PacketBuf::new();
    let body = auth.begin(KIND_HANDSHAKE);
    body.extend_from_slice(&identity.public_key());
    body.extend_from_slice(&sign_as_client(identity, &transcript));
    tcp.write_all(channels.tcp_tx.seal(&mut auth).unwrap()).unwrap();
    let mut hello_packet = PacketBuf::new();
    // Over TCP the host needs no UDP hello from this hand-written client
    encode(&ClientMsg::Hello(ClientCaps { tcp_only: true, ..caps("Old headset") }), hello_packet.begin(KIND_CONTROL)).unwrap();
    tcp.write_all(channels.tcp_tx.seal(&mut hello_packet).unwrap()).unwrap();
    read_tcp_packet(&mut tcp, &mut buf).unwrap();
    let content = channels.tcp_rx.open(&mut buf).unwrap();
    decode(&content[1..]).unwrap()
}

#[test]
fn a_headset_with_the_older_app_is_refused_with_a_note_to_update_it() {
    let port = free_port();
    let (pairings, _events, _headset) = asking_host(port, Duration::from_secs(30));
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    // The older app cannot wait: the host refuses it at once, as before
    assert_eq!(old_client(addr, &identity()), HostMsg::Reject(RejectReason::NotPaired));
    assert!(pairings.waiting().is_empty());
    let note = status::latest("network").expect("a note for the user");
    assert_eq!(note["problem"], "incompatible_client");
    assert!(note["message"].as_str().unwrap().ends_with("Update the cranvr app on the headset."), "{note}");
}

#[test]
fn a_headset_with_the_older_app_that_the_host_paired_still_connects() {
    let port = free_port();
    let (pairings, mut events, _headset) = asking_host(port, Duration::from_secs(30));
    pairings.add(&identity().public_key(), "Old headset");
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    // The host speaks version 4 to it and starts the session: the first answer is the welcome
    assert!(matches!(old_client(addr, &identity()), HostMsg::Welcome(w) if w.version == 4));
    wait_for(&mut *events, |e| matches!(e, Event::Connected { .. }));
}

#[test]
fn the_host_answers_a_search_with_its_name_its_port_and_its_key() {
    let port = free_port();
    let discovery = {
        // A UDP port that nothing uses
        std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    };
    let mut config = config(port);
    config.discovery_port = Some(discovery);
    let (_headset, _events) = start(config).unwrap();
    let found = cranvr_proto::discovery::find(vec![([127, 0, 0, 1], discovery).into()], Duration::from_millis(800)).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let host = &found[0];
    assert_eq!(host.answer.name, "Test Mac");
    assert_eq!(host.answer.port, port);
    assert_eq!(host.answer.fingerprint, cranvr_proto::identity::fingerprint(&cranvr_proto::identity::Identity::from_seed([7; 32]).public_key()));
    assert_eq!(host.answer.versions, cranvr_proto::message::VersionRange::OURS);
    // The address in the answer is one that a client can connect to
    let client = ClientLink::connect(host.socket_addr(), &caps("Found it"), &identity(), None, Duration::from_secs(10));
    assert!(client.is_ok(), "{:?}", client.err());
}

#[test]
fn only_a_few_threads_hold_clients_for_the_user_at_once() {
    let asking = Asking::default();
    let places: Vec<_> = (0..MAX_ASKING).map(|_| asking.enter().expect("a free place")).collect();
    assert!(asking.enter().is_none(), "the next one waits for no one");
    assert!(asking.enter().is_none());
    // A place that ends frees itself, whatever ended the thread
    drop(places);
    assert!(asking.enter().is_some());
}
