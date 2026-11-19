use super::*;
use crate::{
    headset::Headset,
    runtime_link::{msg, read_message, GameEvent, Haptic, PROTOCOL_VERSION, TRACKING_FILE},
};
use cranvr_proto::tracking::{face, Body, BodySet, Eyes, Face, Hands, BODY_JOINTS, FACE_WEIGHTS, FULL_BODY_JOINTS};
use cranvr_proto::wire::Pose as ProtoPose;
use serde_json::Value;
use std::{
    fs,
    io::Write,
    net::TcpStream,
    path::PathBuf,
    sync::Mutex,
    time::Duration,
};

/// A headset that records what the hub asks of it and has a head that looks along -Z.
#[derive(Default)]
struct FakeHeadset {
    wishes: Mutex<Vec<Features>>,
}

impl FakeHeadset {
    fn last_wish(&self) -> Features {
        self.wishes.lock().unwrap().last().copied().unwrap_or(Features::NONE)
    }
}

impl Headset for FakeHeadset {
    fn until_next_vsync(&self) -> Option<Duration> {
        None
    }
    fn motion(&self, device: Device, _sample_ns: u64) -> Option<DeviceMotion> {
        (device == Device::Head).then(|| DeviceMotion { pose: Pose { orientation: Quat::IDENTITY, position: Vec3::new(0.0, 1.6, 0.0) }, ..Default::default() })
    }
    fn set_decoder_config(&self, _config: Vec<u8>) {}
    fn send_frame(&self, _frame: crate::headset::OutFrame) {}
    fn frame_submitted(&self, _sample_ns: u64) {}
    fn bitrate_bps(&self) -> Option<u64> {
        None
    }
    fn send_haptics(&self, _haptic: Haptic) {}
    fn take_stats(&self) -> Option<Value> {
        None
    }
    fn set_tracking_wish(&self, wish: Features) {
        self.wishes.lock().unwrap().push(wish);
    }
}

fn message(kind: u16, payload: &[u8]) -> Vec<u8> {
    let mut m = (2 + payload.len() as u32).to_le_bytes().to_vec();
    m.extend_from_slice(&kind.to_le_bytes());
    m.extend_from_slice(payload);
    m
}

struct Rig {
    dir: PathBuf,
    link: Arc<RuntimeLink>,
    headset: Arc<FakeHeadset>,
    hub: Hub,
    runtime: TcpStream,
    events: std::sync::mpsc::Receiver<GameEvent>,
}

/// A hub with a game's runtime attached. `features` is what the runtime wrote to
/// runtime-features.txt before it connected.
fn rig(name: &str, features: &str, hand_controllers: bool) -> Rig {
    let dir = std::env::temp_dir().join(format!("cranvr-tracking-test-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let (link, events) = RuntimeLink::start(&dir.join("control.txt"), (1600, 1700), 120, |_| ()).unwrap();
    let text = fs::read_to_string(link.control_file()).unwrap();
    let fields: Vec<&str> = text.split_whitespace().collect();
    fs::write(dir.join("runtime-features.txt"), features).unwrap();
    let mut runtime = TcpStream::connect(("127.0.0.1", fields[0].parse::<u16>().unwrap())).unwrap();
    let mut hello = PROTOCOL_VERSION.to_le_bytes().to_vec();
    hello.extend_from_slice(fields[1].as_bytes());
    hello.extend_from_slice(&7u64.to_le_bytes());
    hello.extend_from_slice(&0u16.to_le_bytes());
    runtime.write_all(&message(msg::HELLO, &hello)).unwrap();
    assert_eq!(read_message(&mut runtime).unwrap().0, msg::HELLO_ACK);
    for _ in 0..2 {
        assert_eq!(read_message(&mut runtime).unwrap().0, msg::INTERACTION_PROFILE);
    }
    let headset = Arc::new(FakeHeadset::default());
    let hub = Hub::new(Arc::clone(&link), headset.clone(), hand_controllers);
    // The host knows the game once its runtime connected
    assert!(matches!(events.recv_timeout(Duration::from_secs(2)).unwrap(), GameEvent::Connected { .. }));
    Rig { dir, link, headset, hub, runtime, events }
}

impl Rig {
    /// The runtime tells the host which features its game uses, and waits until the host has it.
    fn game_uses(&mut self, features: Features) {
        self.runtime.write_all(&message(msg::TRACKING_REQUEST, &u32::from(features.0).to_le_bytes())).unwrap();
        for _ in 0..200 {
            if self.link.requested_tracking() == features {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the host did not take the request");
    }

    /// The next message from the host that is not the keepalive's reset
    fn next(&mut self) -> (u16, Vec<u8>) {
        self.runtime.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        loop {
            let (kind, payload) = read_message(&mut self.runtime).unwrap();
            if kind != msg::INPUT_RESET && kind != msg::INTERACTION_PROFILE {
                return (kind, payload);
            }
        }
    }

    /// No further message arrives for a short while
    fn quiet(&mut self) -> bool {
        self.runtime.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        loop {
            match read_message(&mut self.runtime) {
                Ok((kind, _)) if kind == msg::INPUT_RESET || kind == msg::INTERACTION_PROFILE => {}
                Ok(_) => return false,
                Err(_) => return true,
            }
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = &self.events;
        fs::remove_dir_all(&self.dir).ok();
    }
}

fn floats(payload: &[u8]) -> Vec<f32> {
    payload.chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn joint_at(position: [f32; 3]) -> Joint {
    Joint::tracked(ProtoPose { orientation: [0.0, 0.0, 0.0, 1.0], position })
}

/// A hand with its wrist at `wrist` and the tips of thumb and index finger `gap` apart
fn tracked_hand(wrist: [f32; 3], gap: f32) -> Hand {
    let mut joints = [joint_at(wrist); HAND_JOINTS];
    for (i, joint) in joints.iter_mut().enumerate() {
        // Every joint lies at its own place, so a swap of two joints shows
        *joint = joint_at([wrist[0] + 0.001 * i as f32, wrist[1] - 0.0005 * i as f32, wrist[2] - 0.002 * i as f32]);
    }
    joints[hand_controller::PALM] = joint_at([wrist[0], wrist[1], wrist[2] - 0.04]);
    joints[hand_controller::INDEX_TIP] = joint_at([wrist[0], wrist[1], wrist[2] - 0.14]);
    joints[hand_controller::THUMB_TIP] = joint_at([wrist[0], wrist[1] - gap, wrist[2] - 0.14]);
    for tip in [hand_controller::MIDDLE_TIP, hand_controller::RING_TIP, hand_controller::LITTLE_TIP] {
        joints[tip] = joint_at([wrist[0] + 0.01, wrist[1], wrist[2] - 0.14]);
    }
    let root = Motion { pose: ProtoPose { orientation: [0.0, 0.0, 0.0, 1.0], position: wrist }, linear_velocity: [0.5, 0.0, 0.0], angular_velocity: [0.0; 3] };
    Hand::new(root, &joints)
}

fn hands(sample: Nanos, left: Option<Hand>, right: Option<Hand>) -> Tracked {
    Tracked::Hands(Hands { sample_time: sample, display_time: sample + 10_000_000, left, right })
}

#[test]
fn the_need_for_hands_comes_after_a_pause_and_stays_after_a_short_return() {
    let mut need = HandNeed::default();
    let ms = |n: u64| n * 1_000_000;
    // A controller missing for less than the pause asks for nothing
    assert!(!need.update(ms(0), false));
    assert!(!need.update(ms(200), false));
    assert!(need.update(ms(300), false));
    assert!(need.update(ms(400), false));
    // Both controllers back for a moment: the question stays
    assert!(need.update(ms(500), true));
    assert!(need.update(ms(2000), true));
    // A controller missing again resets the wait for the return
    assert!(need.update(ms(2100), false));
    assert!(need.update(ms(4000), true));
    assert!(need.update(ms(6900), true));
    assert!(!need.update(ms(7000), true));
}

#[test]
fn features_that_no_game_uses_are_not_asked_for() {
    let mut rig = rig("none", "aim tracking\n", true);
    rig.hub.on_offer(Features::ALL);
    // Both controllers tracked: no hand is needed, and no game uses any feature
    for i in 0..50u64 {
        assert_eq!(rig.hub.on_sample(i * 11_000_000, 3), 3);
    }
    assert!(rig.headset.wishes.lock().unwrap().is_empty());
    rig.hub.on_tracked(Tracked::Face(Face::new(1, 2, face::VALID, [1.0; 2], &[0.5; FACE_WEIGHTS])));
    assert!(rig.quiet(), "a face that nobody uses is not sent");
}

#[test]
fn a_game_with_hand_tracking_is_given_joints_and_no_controller() {
    let mut rig = rig("joints", "aim tracking\n", true);
    rig.hub.on_offer(Features::HANDS.union(Features::EYES));
    rig.game_uses(Features::HANDS);
    // The game's request is the wish, with no controller in sight
    assert_eq!(rig.hub.on_sample(1_000_000, 0), 0);
    assert_eq!(rig.headset.last_wish(), Features::HANDS);

    let left = tracked_hand([-0.2, 1.2, -0.4], 0.08);
    rig.hub.on_tracked(hands(1_000_000, Some(left), None));
    let (kind, payload) = rig.next();
    assert_eq!(kind, msg::HAND_TRACKING);
    // A timestamp, the hand, its flags, the wrist's motion, 26 joints with 7 floats and a flag byte each
    assert_eq!(payload.len(), 8 + 4 + 4 + 13 * 4 + HAND_JOINTS * 7 * 4 + HAND_JOINTS);
    assert_eq!(&payload[..8], &1_000_000u64.to_le_bytes());
    assert_eq!(&payload[8..12], &0u32.to_le_bytes());
    assert_eq!(&payload[12..16], &1u32.to_le_bytes());
    let values = floats(&payload[16..16 + 13 * 4]);
    // The wrist's orientation, position and velocities
    assert_eq!(&values[..7], &[0.0, 0.0, 0.0, 1.0, -0.2, 1.2, -0.4]);
    assert_eq!(&values[7..10], &[0.5, 0.0, 0.0]);
    // The joints are in the room, in order: the third one lies 3 mm along -z of the wrist
    let joints = floats(&payload[16 + 13 * 4..16 + 13 * 4 + HAND_JOINTS * 28]);
    let joint = |i: usize| &joints[i * 7..i * 7 + 7];
    assert!((joint(3)[4] - (-0.2 + 0.003)).abs() < 2e-4 && (joint(3)[6] - (-0.4 - 0.006)).abs() < 2e-4, "{:?}", joint(3));
    assert!((joint(hand_controller::INDEX_TIP)[6] - (-0.4 - 0.14)).abs() < 2e-4);
    assert!(payload[payload.len() - HAND_JOINTS..].iter().all(|f| *f == 15));
    // The right hand is not tracked: the runtime hears that it is inactive, with no joints
    let (kind, payload) = rig.next();
    assert_eq!((kind, payload.len()), (msg::HAND_TRACKING, 16));
    assert_eq!((&payload[8..12], &payload[12..16]), (&1u32.to_le_bytes()[..], &0u32.to_le_bytes()[..]));
    // Both hands are lost
    rig.hub.on_tracked(hands(2_000_000, None, None));
    for hand in 0..2u32 {
        let (kind, payload) = rig.next();
        assert_eq!((kind, payload.len()), (msg::HAND_TRACKING, 16));
        assert_eq!(&payload[8..12], &hand.to_le_bytes());
    }
    // No controller is made of the hand: the game has hand code of its own
    assert!(rig.quiet());
    assert_eq!(rig.hub.on_sample(3_000_000, 0), 0);
}

#[test]
fn a_game_without_hand_code_gets_the_controller_that_a_hand_makes() {
    let mut rig = rig("controller", "aim tracking\n", true);
    rig.hub.on_offer(Features::HANDS);
    let ms = |n: u64| n * 1_000_000;
    // The headset tracks no controller. A game that has hand code creates its tracker in the
    // first seconds, so the host waits for 3 seconds and then for the pause of a missing controller
    assert_eq!(rig.hub.on_sample(ms(0), 0), 0);
    assert_eq!(rig.hub.on_sample(ms(100), 0), 0);
    assert_eq!(rig.hub.on_sample(ms(3000), 0), 0);
    assert_eq!(rig.hub.on_sample(ms(3200), 0), 0);
    assert_eq!(rig.headset.last_wish(), Features::NONE);
    assert_eq!(rig.hub.on_sample(ms(3400), 0), 0);
    assert_eq!(rig.headset.last_wish(), Features::HANDS, "hands are asked for after the pause");
    let ms = |n: u64| (3000 + n) * 1_000_000 + 400_000_000;

    // A hand arrives with its fingertips pinched
    rig.hub.on_tracked(hands(ms(10), None, Some(tracked_hand([0.25, 1.25, -0.35], 0.018))));
    let (kind, payload) = rig.next();
    assert_eq!(kind, msg::CONTROLLER_MOTION_AIM);
    assert_eq!(&payload[8..12], &1u32.to_le_bytes(), "the right hand");
    let motion = floats(&payload[16..]);
    // The grip pose is the palm, 4 cm in front of the wrist
    assert_eq!(&motion[4..7], &[0.25, 1.25, -0.39]);
    // The aim pose lies between the pinching fingertips
    assert!((motion[17] - 0.25).abs() < 1e-5 && (motion[18] - (1.25 - 0.009)).abs() < 1e-5, "{:?}", &motion[13..20]);
    // The trigger value is 1, the squeeze is open, the finger touches the trigger
    let mut seen = Vec::new();
    for _ in 0..3 {
        let (kind, payload) = rig.next();
        assert_eq!(kind, msg::BUTTON_STATE);
        let id = u64::from_le_bytes(payload[..8].try_into().unwrap());
        seen.push((id, u32::from_le_bytes(payload[8..12].try_into().unwrap()), f32::from_le_bytes(payload[12..16].try_into().unwrap())));
    }
    let ids: Vec<(u64, u32)> = seen.iter().map(|(id, kind, _)| (*id, *kind)).collect();
    assert_eq!(ids, [(RIGHT_TRIGGER_VALUE_ID, 1), (RIGHT_SQUEEZE_VALUE_ID, 1), (RIGHT_TRIGGER_TOUCH_ID, 0)]);
    assert!((seen[0].2 - 1.0).abs() < 1e-5 && seen[1].2 == 0.0 && seen[2].2 == 1.0, "{seen:?}");
    // The next sample counts the hand as a tracked controller
    assert_eq!(rig.hub.on_sample(ms(20), 0), 2);
    // The same values do not go out again
    rig.hub.on_tracked(hands(ms(21), None, Some(tracked_hand([0.25, 1.25, -0.35], 0.018))));
    assert_eq!(rig.next().0, msg::CONTROLLER_MOTION_AIM);
    assert!(rig.quiet());

    // The user opens the fingers: the trigger value changes by itself
    rig.hub.on_tracked(hands(ms(30), None, Some(tracked_hand([0.25, 1.25, -0.35], 0.08))));
    assert_eq!(rig.next().0, msg::CONTROLLER_MOTION_AIM);
    let (kind, payload) = rig.next();
    assert_eq!((kind, f32::from_le_bytes(payload[12..16].try_into().unwrap())), (msg::BUTTON_STATE, 0.0));
    let (kind, _) = rig.next();
    assert_eq!(kind, msg::BUTTON_STATE, "the touch ends");

    // A real controller of that hand returns: the hand makes none, and the values go back to zero
    assert_eq!(rig.hub.on_sample(ms(40), 2), 2);
    let ids: Vec<u64> = (0..3).map(|_| u64::from_le_bytes(rig.next().1[..8].try_into().unwrap())).collect();
    assert_eq!(ids, [RIGHT_TRIGGER_VALUE_ID, RIGHT_SQUEEZE_VALUE_ID, RIGHT_TRIGGER_TOUCH_ID]);
    rig.hub.on_tracked(hands(ms(50), None, Some(tracked_hand([0.25, 1.25, -0.35], 0.018))));
    assert!(rig.quiet(), "a tracked controller beats the hand");
}

#[test]
fn the_user_can_turn_off_controllers_made_of_hands() {
    let mut rig = rig("off", "aim tracking\n", false);
    rig.hub.on_offer(Features::HANDS);
    for i in 0..600u64 {
        rig.hub.on_sample(i * 11_000_000, 0);
    }
    assert!(rig.headset.wishes.lock().unwrap().is_empty(), "no hands are asked for");
    rig.hub.on_tracked(hands(6_600_000_000, None, Some(tracked_hand([0.25, 1.25, -0.35], 0.018))));
    assert!(rig.quiet());
    assert_eq!(rig.hub.on_sample(6_700_000_000, 0), 0);
}

#[test]
fn a_runtime_without_tracking_still_gets_a_controller_from_a_hand_in_the_old_messages() {
    // The older runtime wrote only "aim"
    let mut rig = rig("old", "aim\n", true);
    rig.hub.on_offer(Features::HANDS);
    for i in 0..600u64 {
        rig.hub.on_sample(i * 11_000_000, 0);
    }
    assert_eq!(rig.headset.last_wish(), Features::HANDS);
    rig.hub.on_tracked(hands(6_600_000_000, Some(tracked_hand([-0.2, 1.2, -0.4], 0.08)), None));
    assert_eq!(rig.next().0, msg::CONTROLLER_MOTION_AIM);
    // It never gets a message that it does not know
    rig.hub.on_tracked(Tracked::Eyes(Eyes { sample_time: 1, display_time: 2, gaze: Some(ProtoPose::IDENTITY) }));
    let mut kinds = Vec::new();
    while let Ok((kind, _)) = {
        rig.runtime.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        read_message(&mut rig.runtime)
    } {
        kinds.push(kind);
    }
    assert!(kinds.iter().all(|k| [msg::BUTTON_STATE, msg::INPUT_RESET, msg::INTERACTION_PROFILE].contains(k)), "{kinds:?}");
}

#[test]
fn body_face_and_eyes_reach_the_runtime_when_the_game_uses_them() {
    let mut rig = rig("others", "aim tracking\n", true);
    rig.hub.on_offer(Features::ALL);
    rig.game_uses(Features::FULL_BODY.union(Features::FACE).union(Features::EYES));
    rig.hub.on_sample(1_000_000, 3);
    assert_eq!(rig.headset.last_wish(), Features::FULL_BODY.union(Features::FACE).union(Features::EYES));

    let root = Motion { pose: ProtoPose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.5, 0.0, -1.0] }, linear_velocity: [0.0, 0.0, 0.25], angular_velocity: [0.0; 3] };
    let joints: Vec<Joint> = (0..FULL_BODY_JOINTS).map(|i| joint_at([0.5 + 0.01 * i as f32, 0.02 * i as f32, -1.0])).collect();
    let body = Body::new(10_000_000, 20_000_000, BodySet::Full, 0.75, root, &joints).unwrap();
    rig.hub.on_tracked(Tracked::Body(body));
    let (kind, payload) = rig.next();
    assert_eq!(kind, msg::BODY_TRACKING);
    assert_eq!(payload.len(), 8 + 4 + 4 + 4 + 13 * 4 + FULL_BODY_JOINTS * 28 + FULL_BODY_JOINTS);
    assert_eq!(&payload[8..12], &(FULL_BODY_JOINTS as u32).to_le_bytes());
    assert_eq!(&payload[12..16], &1u32.to_le_bytes());
    assert_eq!(f32::from_le_bytes(payload[16..20].try_into().unwrap()), 0.75);
    let values = floats(&payload[20..20 + 13 * 4 + FULL_BODY_JOINTS * 28]);
    assert_eq!(&values[4..7], &[0.5, 0.0, -1.0]);
    // Joint 83 (the last) at x = 0.5 + 0.83
    let last = &values[13 + 83 * 7..13 + 84 * 7];
    assert!((last[4] - 1.33).abs() < 2e-4 && (last[5] - 1.66).abs() < 2e-4, "{last:?}");

    let weights: [f32; FACE_WEIGHTS] = std::array::from_fn(|i| i as f32 / 69.0);
    rig.hub.on_tracked(Tracked::Face(Face::new(30_000_000, 40_000_000, face::VALID, [0.5, 1.0], &weights)));
    let (kind, payload) = rig.next();
    assert_eq!((kind, payload.len()), (msg::FACE_TRACKING, 8 + 4 + 8 + FACE_WEIGHTS * 4));
    assert_eq!(&payload[8..12], &u32::from(face::VALID).to_le_bytes());
    let values = floats(&payload[12..]);
    assert!((values[0] - 0.5).abs() < 0.003 && values[1] == 1.0);
    assert!((values[2 + 69] - 1.0).abs() < 1e-6 && (values[2 + 35] - 35.0 / 69.0).abs() < 0.003);

    let turn = std::f32::consts::FRAC_1_SQRT_2;
    let gaze = ProtoPose { orientation: [0.0, turn, 0.0, turn], position: [0.0, 1.6, 0.0] };
    rig.hub.on_tracked(Tracked::Eyes(Eyes { sample_time: 50_000_000, display_time: 60_000_000, gaze: Some(gaze) }));
    let (kind, payload) = rig.next();
    assert_eq!((kind, payload.len()), (msg::EYE_GAZE, 8 + 4 + 28));
    assert_eq!(floats(&payload[12..]), [0.0, turn, 0.0, turn, 0.0, 1.6, 0.0]);
    assert_eq!(&payload[8..12], &1u32.to_le_bytes());
    // A default body that nobody asked for is dropped: the game asked for the full body, which
    // contains it
    let joints: Vec<Joint> = (0..BODY_JOINTS).map(|i| joint_at([0.0, i as f32 * 0.01, 0.0])).collect();
    rig.hub.on_tracked(Tracked::Body(Body::new(1, 2, BodySet::Default, 1.0, root, &joints).unwrap()));
    assert_eq!(rig.next().0, msg::BODY_TRACKING);
    // When the game stops using the face, no face goes out
    rig.game_uses(Features::FULL_BODY.union(Features::EYES));
    rig.hub.on_tracked(Tracked::Face(Face::new(1, 2, face::VALID, [1.0; 2], &weights)));
    assert!(rig.quiet());
}

#[test]
fn the_runtime_learns_which_features_the_headset_offers_through_a_file() {
    let mut rig = rig("file", "aim tracking\n", true);
    let file = rig.dir.join(TRACKING_FILE);
    assert!(!file.exists(), "no headset offered anything yet");
    rig.hub.on_offer(Features::HANDS.union(Features::EYES));
    assert_eq!(fs::read_to_string(&file).unwrap(), "hands eyes\n");
    rig.hub.on_offer(Features::ALL);
    assert_eq!(fs::read_to_string(&file).unwrap(), "hands body full_body face eyes\n");
    // The headset leaves: the file goes with it, and the wish ends
    rig.hub.on_offer(Features::NONE);
    assert!(!file.exists());
}

#[test]
fn the_runtimes_request_ends_with_its_connection() {
    let mut rig = rig("gone", "aim tracking\n", true);
    rig.hub.on_offer(Features::ALL);
    rig.game_uses(Features::FACE);
    assert_eq!(rig.hub.on_sample(1, 3), 3);
    assert_eq!(rig.headset.last_wish(), Features::FACE);
    rig.runtime.shutdown(std::net::Shutdown::Both).unwrap();
    for _ in 0..200 {
        if rig.link.requested_tracking().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(rig.link.requested_tracking().is_empty());
    rig.hub.on_sample(2, 3);
    assert_eq!(rig.headset.last_wish(), Features::NONE);
}

#[test]
fn a_game_that_creates_its_hand_tracker_in_the_first_seconds_sees_no_controller_come_and_go() {
    let mut rig = rig("grace", "aim tracking\n", true);
    rig.hub.on_offer(Features::HANDS);
    let ms = |n: u64| n * 1_000_000;
    // The headset tracks no controller and a hand. The game starts its tracker after 1.5 seconds.
    for t in (0..1500).step_by(11) {
        assert_eq!(rig.hub.on_sample(ms(t), 0), 0);
    }
    rig.game_uses(Features::HANDS);
    for t in (1500..9000).step_by(150) {
        assert_eq!(rig.hub.on_sample(ms(t), 0), 0);
        rig.hub.on_tracked(hands(ms(t), None, Some(tracked_hand([0.25, 1.25, -0.35], 0.018))));
    }
    // The runtime got the hand and no controller
    let mut kinds = std::collections::BTreeSet::new();
    rig.runtime.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    while let Ok((kind, _)) = read_message(&mut rig.runtime) {
        kinds.insert(kind);
    }
    assert!(kinds.contains(&msg::HAND_TRACKING));
    assert!(!kinds.contains(&msg::CONTROLLER_MOTION_AIM) && !kinds.contains(&msg::CONTROLLER_MOTION) && !kinds.contains(&msg::BUTTON_STATE), "{kinds:?}");
}
