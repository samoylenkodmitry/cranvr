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
        cable: false,
        microphone: false,
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
        pairings: Arc::new(crate::pairing::Pairings::load(&config_dir(port))),
        name: "Test Mac".into(),
        discovery_port: None,
        approval_wait: Duration::from_secs(30),
        approve_loopback: false,
        trust_all: false,
        cable: Arc::new(AtomicBool::new(true)),
        display_lock: true,
        desktop: Default::default(),
        audio: false,
        microphone: None,
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
fn a_release_lets_go_of_every_button_that_the_last_input_held() {
    let mut state = InputState::default();
    // Before the first input nothing is held
    assert!(state.release().is_empty());
    let mut input = Input::default();
    state.changes(&input);
    input.hands[1].buttons = buttons::TRIGGER_CLICK;
    input.hands[1].trigger = 0.9;
    input.hands[0].stick = [0.0, -1.0];
    state.changes(&input);
    let released = state.release();
    assert_eq!(released.len(), 3);
    assert!(released.iter().all(|b| b.value == 0.0));
    // Everything is let go: the next release has nothing to do, and a new press is a change again
    assert!(state.release().is_empty());
    assert_eq!(state.changes(&input).len(), 3);
}

#[test]
fn an_app_without_the_focus_lets_go_of_its_buttons_and_the_host_hears_of_each_change_once() {
    let port = free_port();
    let (_headset, mut events) = start(config(port)).unwrap();
    let client = ClientLink::connect(([127, 0, 0, 1], port).into(), &caps("Test headset"), &identity(), None, Duration::from_secs(10)).unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::Connected { .. }));
    // The user holds the right trigger. The datagram may get lost, so the client repeats it.
    let mut held = Input::default();
    held.hands[1].trigger = 1.0;
    loop {
        client.duplex.send_datagram(&ClientDatagram::InputHold(InputHold { input: held, held: [false; 2] })).unwrap();
        if let Ok(Event::Buttons(buttons)) = events.next(Duration::from_millis(100)) {
            assert!(buttons.iter().any(|b| b.path_id == RIGHT_TRIGGER_VALUE_ID && b.value == 1.0));
            break;
        }
    }
    // The app loses the focus, as under the system's menu: the trigger goes up in the game
    client.duplex.send_control(&ClientMsg::Focus(Focus::Visible)).unwrap();
    let Event::Buttons(released) = wait_for(&mut *events, |e| matches!(e, Event::Buttons(b) if b.iter().any(|b| b.path_id == RIGHT_TRIGGER_VALUE_ID && b.value == 0.0))) else { unreachable!() };
    assert!(released.iter().all(|b| b.value == 0.0));
    let Event::Focus(focus) = wait_for(&mut *events, |e| matches!(e, Event::Focus(_))) else { unreachable!() };
    assert_eq!(focus, Focus::Visible);
    // The same state again is no change. The headset then hides the app, and later gives the focus back.
    client.duplex.send_control(&ClientMsg::Focus(Focus::Visible)).unwrap();
    client.duplex.send_control(&ClientMsg::Focus(Focus::Hidden)).unwrap();
    let Event::Focus(focus) = wait_for(&mut *events, |e| matches!(e, Event::Focus(_))) else { unreachable!() };
    assert_eq!(focus, Focus::Hidden);
    client.duplex.send_control(&ClientMsg::Focus(Focus::Focused)).unwrap();
    let Event::Focus(focus) = wait_for(&mut *events, |e| matches!(e, Event::Focus(_))) else { unreachable!() };
    assert_eq!(focus, Focus::Focused);
    // The input after the return reports what changed since the release: the trigger goes down again
    loop {
        client.duplex.send_datagram(&ClientDatagram::InputHold(InputHold { input: held, held: [false; 2] })).unwrap();
        if let Ok(Event::Buttons(buttons)) = events.next(Duration::from_millis(100)) {
            assert!(buttons.iter().any(|b| b.path_id == RIGHT_TRIGGER_VALUE_ID && b.value == 1.0));
            break;
        }
    }
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

    let Event::Connected { stream, ip, cable } = wait_for(&mut *events, |e| matches!(e, Event::Connected { .. })) else { unreachable!() };
    assert_eq!((stream.eye_width, stream.eye_height, stream.fps), (2064, 2208, 120.0));
    assert!(stream.foveated && stream.codec == Codec::Hevc);
    assert_eq!(ip, std::net::IpAddr::from([127, 0, 0, 1]));
    assert!(!cable, "the client said that it is not on a cable");
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

    // The console: the stream's state and the host's menu reach the client, and a button's request
    // reaches the host
    headset.send_menu_status(r#"{"refresh":120}"#);
    headset.send_menu(r#"{"pages":[],"controls":{}}"#);
    let mut heard = (false, false);
    while heard != (true, true) {
        match from_client.recv_timeout(Duration::from_secs(10)).unwrap() {
            Heard::Control(HostMsg::MenuStatus(status)) => {
                assert_eq!(status, r#"{"refresh":120}"#);
                heard.0 = true;
            }
            Heard::Control(HostMsg::Menu(menu)) => {
                assert!(heard.0, "the state comes before the menu");
                assert_eq!(menu, r#"{"pages":[],"controls":{}}"#);
                heard.1 = true;
            }
            _ => {}
        }
    }
    client.duplex.send_control(&ClientMsg::Console(r#"{"action":"run","id":"steam.stop"}"#.into())).unwrap();
    let Event::Console(request) = wait_for(&mut *events, |e| matches!(e, Event::Console(_))) else { unreachable!() };
    assert_eq!(request, r#"{"action":"run","id":"steam.stop"}"#);

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
    let none = status_line(9950, &Pairings::load(&dir));
    assert_eq!(none, json!({"type": "native", "port": 9950, "paired": [], "pending": []}));
    // A headset that waits has its key, name, address and the code that both screens show
    let pairings = Pairings::load(&dir);
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
    assert_eq!(parse_command(&format!("allow {hex}")), Some(Command::Answer { allow: true, key }));
    assert_eq!(parse_command(&format!("  deny   {hex}  ")), Some(Command::Answer { allow: false, key }));
    assert_eq!(parse_command(&format!("allow {}", &hex[..60])), None, "a short key");
    assert_eq!(parse_command(&format!("grant {hex}")), None);
    assert_eq!(parse_command(&format!("allow {hex} now")), None);
    assert_eq!(parse_command("allow"), None);
    assert_eq!(parse_command(""), None);
}

// The cranvr app writes the Connection choice to the host's input: off for Wi-Fi
#[test]
fn the_connection_choice_comes_as_a_line() {
    assert_eq!(parse_command("cable off"), Some(Command::Cable(false)));
    assert_eq!(parse_command(" cable  on "), Some(Command::Cable(true)));
    for wrong in ["cable", "cable maybe", "cable on now", "cables off"] {
        assert_eq!(parse_command(wrong), None, "{wrong}");
    }
}

// The cranvr app's button "Open the menu in the headset" writes a line to the host's input
#[test]
fn the_request_to_open_the_hand_menu_comes_as_a_line() {
    assert_eq!(parse_command("menu open"), Some(Command::OpenMenu));
    assert_eq!(parse_command("  menu   open "), Some(Command::OpenMenu));
    for wrong in ["menu", "menu close", "menu open now", "menus open"] {
        assert_eq!(parse_command(wrong), None, "{wrong}");
    }
}

#[test]
fn a_version_mismatch_tells_the_user_which_side_to_update() {
    use cranvr_proto::wire::Error;
    let old = Error::Version { ours: 8, theirs: 7 };
    assert_eq!(version_notice(&old).unwrap(), "A headset tried to connect with a cranvr app of an older version. Update the cranvr app on the headset.");
    let new = Error::Version { ours: 8, theirs: 9 };
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

// A headset's USB network to the Mac has IPv6 link-local addresses only. The host takes the
// connection on its IPv6 listener, sends the media over UDP and reports the cable that the client
// names. macOS's loopback has the link-local address fe80::1 on interface 1.
#[cfg(target_os = "macos")]
#[test]
fn a_headset_on_its_usb_network_reaches_the_host_by_a_link_local_address() {
    let port = free_port();
    let mut config = config(port);
    config.trust_all = true;
    let (_headset, mut events) = start(config).unwrap();
    let link_local = std::net::SocketAddrV6::new(std::net::Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1), port, 0, 1);
    let mut cabled = caps("Cabled headset");
    cabled.cable = true;
    let client = ClientLink::connect(link_local.into(), &cabled, &identity(), None, Duration::from_secs(10)).unwrap();
    let Event::Connected { ip, cable, .. } = wait_for(&mut *events, |e| matches!(e, Event::Connected { .. })) else { unreachable!() };
    assert!(matches!(ip, std::net::IpAddr::V6(ip) if ip.is_unicast_link_local()), "{ip}");
    assert!(cable);
    assert!(client.duplex.has_udp(), "the USB network carries the media over UDP");
    client.duplex.close();
}

// With the Connection choice Wi-Fi the host takes no link over the USB cable, on either way. A
// probe over the cable learns it with the host's key. A client that comes over the cable anyway
// hears `WiFiOnly`, and the same headset over Wi-Fi gets its session. The choice changes on the
// host's input and counts from the next connection.
#[cfg(target_os = "macos")]
#[test]
fn the_choice_of_wifi_stops_the_cable_on_both_ways() {
    let port = free_port();
    let mut config = config(port);
    config.trust_all = true;
    config.cable.store(false, Ordering::Relaxed);
    let cable = Arc::clone(&config.cable);
    let (_headset, mut events) = start(config).unwrap();
    // The headset's USB network (a link-local address) and `adb reverse` (loopback)
    let usb_network: SocketAddr = std::net::SocketAddrV6::new(std::net::Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1), port, 0, 1).into();
    let adb_reverse: SocketAddr = ([127, 0, 0, 1], port).into();
    let mut cabled = caps("Cabled headset");
    cabled.cable = true;
    for way in [usb_network, adb_reverse] {
        let probe = cranvr_proto::link::probe_host(way, Duration::from_secs(10)).unwrap();
        assert_eq!((probe.key, probe.cable), (Some(cranvr_proto::identity::Identity::from_seed([7; 32]).public_key()), false), "{way}");
        let refused = ClientLink::connect(way, &cabled, &identity(), None, Duration::from_secs(10));
        assert!(matches!(refused, Err(cranvr_proto::Error::Rejected(RejectReason::WiFiOnly))), "{way}: {:?}", refused.err());
    }
    // The same headset over Wi-Fi says no cable
    let wifi = ClientLink::connect(adb_reverse, &caps("Cabled headset"), &identity(), None, Duration::from_secs(10)).unwrap();
    let Event::Connected { cable: by_cable, .. } = wait_for(&mut *events, |e| matches!(e, Event::Connected { .. })) else { unreachable!() };
    assert!(!by_cable);
    wifi.duplex.close();
    wait_for(&mut *events, |e| matches!(e, Event::Disconnected));
    // The user chooses the cable again
    cable.store(true, Ordering::Relaxed);
    assert!(cranvr_proto::link::probe_host(usb_network, Duration::from_secs(10)).unwrap().cable);
    let client = ClientLink::connect(usb_network, &cabled, &identity(), None, Duration::from_secs(10)).unwrap();
    let Event::Connected { cable: by_cable, .. } = wait_for(&mut *events, |e| matches!(e, Event::Connected { .. })) else { unreachable!() };
    assert!(by_cable);
    client.duplex.close();
}

/// A client that says it speaks `version`, written by hand: it sends the preamble and reads the
/// host's answer, which holds the host's version.
fn client_of_version(addr: SocketAddr, version: u16) -> u16 {
    use cranvr_proto::message::{client_preamble, parse_host_preamble, HOST_PREAMBLE_LEN};
    use std::io::{Read, Write};
    let mut tcp = std::net::TcpStream::connect(addr).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tcp.write_all(&client_preamble(version)).unwrap();
    // The host answers a client of another version with its whole preamble
    let mut host_pre = [0u8; HOST_PREAMBLE_LEN];
    tcp.read_exact(&mut host_pre).unwrap();
    parse_host_preamble(&host_pre).unwrap()
}

#[test]
fn a_headset_of_another_version_is_refused_with_a_note_that_names_the_side_to_update() {
    let ours = cranvr_proto::PROTOCOL_VERSION;
    let port = free_port();
    let (pairings, _events, _headset) = asking_host(port, Duration::from_secs(30));
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    // An older headset app: the host answers with its own version and tells its user to update the app
    assert_eq!(client_of_version(addr, ours - 1), ours);
    let note = wait_for_note(|note| note["message"].as_str().is_some_and(|m| m.ends_with("Update the cranvr app on the headset.")));
    assert_eq!(note["problem"], "incompatible_client");
    assert!(note["message"].as_str().unwrap().contains("an older"), "{note}");
    // A newer headset app: the Mac is the side to update
    assert_eq!(client_of_version(addr, ours + 1), ours);
    let note = wait_for_note(|note| note["message"].as_str().is_some_and(|m| m.ends_with("Update cranvr on the Mac.")));
    assert!(note["message"].as_str().unwrap().contains("a newer"), "{note}");
    assert!(pairings.waiting().is_empty());
}

/// The newest `network` status line that `ok` takes, within a few seconds
fn wait_for_note(ok: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(note) = status::latest("network").filter(|note| ok(note)) {
            return note;
        }
        assert!(Instant::now() < deadline, "no note");
        std::thread::sleep(Duration::from_millis(20));
    }
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
    assert_eq!(host.answer.version, cranvr_proto::PROTOCOL_VERSION);
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

#[test]
fn the_wish_for_tracking_stays_within_the_offer() {
    let (hands, face, eyes) = (Features::HANDS, Features::FACE, Features::EYES);
    // Nothing to ask for at the start: no message
    assert_eq!(wish_to_send(Features::NONE, Features::ALL, None), None);
    // Only what the client offers
    assert_eq!(wish_to_send(hands.union(face), hands.union(eyes), None), Some(hands));
    // Before the offer came, a wish waits
    assert_eq!(wish_to_send(hands, Features::NONE, None), None);
    // The same answer again is not sent again. A change is, also the change to nothing.
    assert_eq!(wish_to_send(hands, Features::ALL, Some(hands)), None);
    assert_eq!(wish_to_send(face, Features::ALL, Some(hands)), Some(face));
    assert_eq!(wish_to_send(Features::NONE, Features::ALL, Some(hands)), Some(Features::NONE));
    // A headset that offers the full body serves a game that asks for the default body
    assert_eq!(wish_to_send(Features::BODY, Features::FULL_BODY, None), Some(Features::FULL_BODY));
    assert_eq!(wish_to_send(Features::BODY, Features::BODY.union(Features::FULL_BODY), None), Some(Features::BODY));
    // Bits that no feature has are dropped
    assert_eq!(wish_to_send(Features(0xe0 | 1), Features(0xff), None), Some(hands));
    // The face's source is a detail of the offer: the wish for the face has none, whatever the offer says
    let audio = face.union(Features::FACE_AUDIO);
    assert_eq!(wish_to_send(audio, audio, None), Some(face));
}

#[test]
fn tracking_goes_from_the_headset_to_the_events_only_when_the_host_asked() {
    use cranvr_proto::tracking::{Eyes, Hands};
    let port = free_port();
    let (headset, mut events) = start(config(port)).unwrap();
    let client = Arc::new(ClientLink::connect(([127, 0, 0, 1], port).into(), &caps("Test headset"), &identity(), None, Duration::from_secs(10)).unwrap());
    wait_for(&mut *events, |e| matches!(e, Event::Connected { .. }));
    let (to_test, from_client) = channel();
    for udp in [true, false] {
        let (client, to_test) = (Arc::clone(&client), to_test.clone());
        thread::spawn(move || {
            let handler = |incoming: Incoming<'_, cranvr_proto::link::ClientSide>| {
                if let Incoming::Control(HostMsg::TrackingWish(wish)) = incoming {
                    to_test.send(wish).ok();
                }
            };
            if udp {
                client.duplex.read_udp(handler)
            } else {
                client.duplex.read_tcp(handler)
            };
        });
    }
    // The floor lies 0.3 m below the room's origin
    client.duplex.send_control(&ClientMsg::Floor(-0.3)).unwrap();
    // A game asks before the headset offers: nothing goes out yet
    headset.set_tracking_wish(Features::HANDS.union(Features::EYES));
    assert!(from_client.recv_timeout(Duration::from_millis(300)).is_err());
    // The face comes from the microphone, as on a Quest 3. The offer keeps the source for the game's
    // runtime. A source of no face is dropped, and the wish never carries one.
    let offered = Features::HANDS.union(Features::FACE).union(Features::FACE_AUDIO);
    client.duplex.send_control(&ClientMsg::TrackingOffer(offered.union(Features(1 << 7)))).unwrap();
    let Event::TrackingOffer(offer) = wait_for(&mut *events, |e| matches!(e, Event::TrackingOffer(_))) else { unreachable!() };
    assert_eq!(offer, offered);
    assert_eq!(from_client.recv_timeout(Duration::from_secs(10)).unwrap(), Features::HANDS);

    // Eyes were not asked for and not offered: the datagram is dropped. Hands come through, with
    // the floor of the headset moved to height 0.
    let root = Motion { pose: ProtoPose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.1, 1.0, -0.4] }, ..Default::default() };
    let hand = cranvr_proto::tracking::Hand::new(root, &[cranvr_proto::tracking::Joint::tracked(root.pose); cranvr_proto::tracking::HAND_JOINTS]);
    let mut sample = 7_000_000_000u64;
    let tracked = loop {
        client.duplex.send_datagram(&ClientDatagram::Eyes(Eyes { sample_time: sample, display_time: sample, gaze: Some(ProtoPose::IDENTITY) })).unwrap();
        client.duplex.send_datagram(&ClientDatagram::Hands(Hands { sample_time: sample, display_time: sample, left: Some(hand.clone()), right: None })).unwrap();
        // UDP may lose a datagram: the client repeats it
        if let Ok(Event::Tracked(tracked)) = events.next(Duration::from_millis(100)) {
            break tracked;
        }
        sample += 1;
    };
    let cranvr_proto::tracking::Tracked::Hands(hands) = tracked else { panic!("the eyes came through, which nobody asked for") };
    let left = hands.left.expect("the left hand");
    assert!((left.root.pose.position[1] - 1.3).abs() < 1e-5, "{:?}", left.root.pose.position);
    assert_eq!(left.root.pose.position[0], 0.1);
    assert!(hands.right.is_none());

    // The game wants something else: the client hears the new wish, within what it offers
    headset.set_tracking_wish(Features::FACE.union(Features::EYES));
    assert_eq!(from_client.recv_timeout(Duration::from_secs(10)).unwrap(), Features::FACE);
    // The client leaves: the offer ends
    client.duplex.send_control(&ClientMsg::Bye).unwrap();
    wait_for(&mut *events, |e| matches!(e, Event::TrackingOffer(offer) if offer.is_empty()));
}

#[test]
fn a_value_that_the_runtime_would_refuse_never_reaches_it() {
    use cranvr_proto::tracking::{Body, BodySet, Eyes, Hand, Hands, Joint, BODY_JOINTS, HAND_JOINTS};
    let good = Motion { pose: ProtoPose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.1, 1.0, -0.4] }, ..Default::default() };
    let zero_turn = Motion { pose: ProtoPose { orientation: [0.0; 4], ..good.pose }, ..good };
    let not_a_number = Motion { linear_velocity: [f32::NAN, 0.0, 0.0], ..good };
    let far = Motion { pose: ProtoPose { position: [f32::INFINITY, 0.0, 0.0], ..good.pose }, ..good };
    let hand = |root: Motion| Hand::new(root, &[Joint::tracked(good.pose); HAND_JOINTS]);

    // A hand with a zero quaternion at its wrist, as a tracker reports one that it lost, is untracked
    let Some(Tracked::Hands(hands)) =
        sanitized(Tracked::Hands(Hands { sample_time: 1, display_time: 2, left: Some(hand(zero_turn)), right: Some(hand(good)) }))
    else {
        panic!("hands stay")
    };
    assert!(hands.left.is_none() && hands.right.is_some());
    for bad in [not_a_number, far] {
        let Some(Tracked::Hands(hands)) = sanitized(Tracked::Hands(Hands { sample_time: 1, display_time: 2, left: Some(hand(bad)), right: None })) else {
            panic!("hands stay")
        };
        assert!(hands.left.is_none());
    }
    // A body with an unsound root goes. A confidence that is not a number becomes 0 and a large one 1.
    let joints = vec![Joint::tracked(good.pose); BODY_JOINTS];
    assert!(sanitized(Tracked::Body(Body::new(1, 2, BodySet::Default, 1.0, zero_turn, &joints).unwrap())).is_none());
    for (given, kept) in [(f32::NAN, 0.0), (7.0, 1.0), (0.5, 0.5)] {
        let Some(Tracked::Body(body)) = sanitized(Tracked::Body(Body::new(1, 2, BodySet::Default, given, good, &joints).unwrap())) else { panic!("body stays") };
        assert_eq!(body.confidence, kept);
    }
    // A gaze that is not a pose is no gaze
    let nan_gaze = ProtoPose { position: [f32::NAN, 0.0, 0.0], ..good.pose };
    let Some(Tracked::Eyes(eyes)) = sanitized(Tracked::Eyes(Eyes { sample_time: 1, display_time: 2, gaze: Some(nan_gaze) })) else { panic!("eyes stay") };
    assert!(eyes.gaze.is_none());
    let Some(Tracked::Eyes(eyes)) = sanitized(Tracked::Eyes(Eyes { sample_time: 1, display_time: 2, gaze: Some(good.pose) })) else { panic!("eyes stay") };
    assert_eq!(eyes.gaze, Some(good.pose));
}

/// A host with the desktop on, for the tests of the displays. An added display shows the main display
/// again, so the test needs no Screen Recording and moves none of this Mac's displays.
fn display_host() -> (u16, Box<dyn EventSource>, Arc<ClientLink>, Receiver<HostMsg>) {
    std::env::set_var("CRANVR_DISPLAY_TEST", "main");
    let port = free_port();
    let mut config = config(port);
    config.desktop.enabled = true;
    let (headset, events) = start(config).unwrap();
    // The host serves while the handle lives, and the test ends with the process
    std::mem::forget(headset);
    let client = Arc::new(ClientLink::connect(([127, 0, 0, 1], port).into(), &caps("Test headset"), &identity(), None, Duration::from_secs(10)).unwrap());
    let (to_test, control) = channel();
    let reader = Arc::clone(&client);
    thread::spawn(move || {
        reader.duplex.read_tcp(|incoming| {
            if let Incoming::Control(message) = incoming {
                to_test.send(message).ok();
            }
        })
    });
    (port, events, client, control)
}

/// The next list of displays that the host sends, within a few seconds
fn next_list(control: &Receiver<HostMsg>) -> cranvr_proto::message::DisplayList {
    loop {
        if let HostMsg::Displays(list) = control.recv_timeout(Duration::from_secs(5)).expect("the host sent no list of displays") {
            return list;
        }
    }
}

#[test]
fn the_client_adds_a_display_and_removes_it_again() {
    use cranvr_proto::message::{display_stream, DisplayRequest};
    let (_port, _events, client, control) = display_host();
    // The host lists the Mac's own display first, and says that no game runs
    let first = next_list(&control);
    assert_eq!(first.displays.len(), 1);
    assert_eq!((first.displays[0].stream, first.displays[0].removable), (display_stream(0), false));
    assert!(first.can_add);

    client.duplex.send_control(&ClientMsg::Display(DisplayRequest::Add)).unwrap();
    let two = loop {
        let list = next_list(&control);
        if list.displays.len() == 2 {
            break list;
        }
    };
    assert_eq!(two.displays[1].stream, display_stream(1));
    assert!(two.displays[1].removable, "the client may remove a display that it added");

    // A request to remove the first display does nothing: the host keeps it
    client.duplex.send_control(&ClientMsg::Display(DisplayRequest::Remove { stream: display_stream(0) })).unwrap();
    client.duplex.send_control(&ClientMsg::Display(DisplayRequest::Remove { stream: display_stream(1) })).unwrap();
    let back = loop {
        let list = next_list(&control);
        if list.displays.len() == 1 {
            break list;
        }
    };
    assert_eq!(back.displays[0].stream, display_stream(0));
    // The host never sent a list that dropped the first display
}

#[test]
fn a_display_that_the_client_does_not_wish_sends_no_picture_and_the_host_says_that_no_game_runs() {
    let (_port, _events, client, control) = display_host();
    let mut game = None;
    let mut seen_units = 0;
    // The client reads the media too: no unit of a display's stream may come while no wish was made
    let (units, from_media) = channel();
    let media = Arc::clone(&client);
    thread::spawn(move || {
        let mut receiver = FrameReceiver::new();
        media.duplex.read_udp(|incoming| {
            if let Incoming::Media(packet) = incoming {
                if let Ok(Some(unit)) = receiver.push(packet, 1) {
                    units.send(unit.meta.stream).ok();
                }
            }
        })
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Ok(HostMsg::GameRuns(runs)) = control.recv_timeout(Duration::from_millis(50)) {
            game = Some(runs);
        }
        while let Ok(stream) = from_media.try_recv() {
            if stream != 0 {
                seen_units += 1;
            }
        }
    }
    assert_eq!(game, Some(false), "the host says whether a game runs");
    assert_eq!(seen_units, 0, "no display was wished, so none sent a picture");
}

#[test]
fn the_hands_that_the_app_holds_back_reach_the_frame_loop_and_the_game_sees_rest_values() {
    use cranvr_proto::message::{buttons, HandInput, Input, InputHold};
    let (_port, mut events, client, _control) = display_host();
    let pressed = HandInput { trigger: 0.9, buttons: buttons::TRIGGER_CLICK, ..Default::default() };
    // The right hand presses a panel: the frame loop hears of the hold, and the game hears of no button
    let hold = InputHold { input: Input { sample_time: 1, hands: [HandInput::default(), pressed] }, held: [false, true] };
    client.duplex.send_datagram(&ClientDatagram::InputHold(hold)).unwrap();
    let mut held = None;
    let mut right_trigger = None;
    for _ in 0..40 {
        match events.next(Duration::from_millis(100)) {
            Ok(Event::Held(h)) => held = Some(h),
            Ok(Event::Buttons(buttons)) => {
                right_trigger = buttons.iter().find(|b| b.path_id == cranvr_common::inputs::RIGHT_TRIGGER_VALUE_ID).map(|b| b.value);
            }
            _ => {}
        }
        if held.is_some() && right_trigger.is_some() {
            break;
        }
        // UDP may lose the datagram on a busy machine: the client sends it again
        client.duplex.send_datagram(&ClientDatagram::InputHold(hold)).unwrap();
    }
    assert_eq!(held, Some([false, true]));
    assert_eq!(right_trigger, Some(0.0), "the game sees a trigger at rest while a panel holds the hand");
}
