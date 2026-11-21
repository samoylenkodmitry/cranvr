//! A stand-in for a game under Wine. It speaks the BasaltVR runtime protocol to the host, shares
//! one swapchain of IOSurfaces per eye and sends frames for the newest head pose, the way the
//! runtime does. Its eye badges have swapped colors (left blue, right red), so
//! `cranvr-probe --game` can tell its frames from the host's test scene.
//!
//! It also reads the controller messages that the host sends and writes what arrived when it ends.
//! Each hand arrives with a grip pose and an aim pose. A pull of the left grip
//! makes it answer with a haptic pulse on the left hand, as a game does. `--expect-input` makes the
//! exit code depend on the input: the probe's scripted controllers (`cranvr-probe`) must have
//! arrived on both hands with their own grip and aim poses, as `hands.rs` scripts them, and with
//! the Touch buttons.
//!
//! With `--use-tracking hands,body,full_body,face,eyes` it plays a game that uses these features, as
//! the runtime of patch 0005 does: it tells the host a second after the start which features it uses
//! (`TRACKING_REQUEST`). With `--release-tracking`
//! it takes them away again six seconds before the end, while the probe still runs. The host then sends their data. With `cranvr-probe --tracking` the data
//! is the script of `tracked.rs`, and `--expect-tracking` makes the exit code depend on it.
//! `--expect-hand-controllers` plays a game without hand code: with `cranvr-probe --tracking
//! --no-controllers` the host must make the controllers of the probe's hands.
//!
//! `--expect-held` checks the host's mask: with the probe's `--hold-script`, the right hand's presses
//! that belong to a panel must never reach the game, and the ones that belong to the game must.
//!
//! `--stall AT:FOR` plays a game that loads a level or compiles shaders: it sends no frame for FOR
//! seconds from AT seconds after its start, and logs the stall's wall-clock times, so that a test can
//! look at what the headset showed in the meantime (the host must keep the game's last picture).
//!
//! Usage: cranvr-fake-game --control FILE [--fps N] [--seconds N] [--expect-input] [--expect-held]
//!        [--use-tracking FEATURES] [--release-tracking] [--expect-tracking] [--expect-hand-controllers]
//!        [--expect-no-controllers] [--stall AT:FOR]

use cranvr_common::inputs::{LEFT_SQUEEZE_VALUE_ID, LEFT_TRIGGER_VALUE_ID, RIGHT_A_CLICK_ID, RIGHT_B_CLICK_ID, RIGHT_SQUEEZE_VALUE_ID, RIGHT_THUMBSTICK_X_ID, RIGHT_TRIGGER_VALUE_ID};
use cranvr_proto::wire::Motion;
use cranvr_vt::PixelBuffer;
use std::{
    collections::BTreeMap,
    env, fs,
    io::{Read, Write},
    net::TcpStream,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const PROTOCOL_VERSION: u16 = 9;
const HELLO: u16 = 1;
const HELLO_ACK: u16 = 2;
const CREATE_SWAPCHAIN: u16 = 3;
const SWAPCHAIN_ACK: u16 = 4;
const FRAME_READY: u16 = 7;
const TEARDOWN: u16 = 8;
const HEAD_POSE: u16 = 9;
const CONTROLLER_MOTION: u16 = 10;
const BUTTON_STATE: u16 = 11;
const HAPTIC: u16 = 12;
const INTERACTION_PROFILE: u16 = 13;
const CONTROLLER_MASK: u16 = 16;
const VIEW_CONFIG: u16 = 15;
const FRAME_CONSUMED: u16 = 17;
/// cranvr's runtime patch 0004: the grip and the aim pose of a hand in one message
const CONTROLLER_MOTION_AIM: u16 = 18;
/// cranvr's runtime patch 0005: tracking of hands, body, face and eyes
const TRACKING_REQUEST: u16 = 19;
const HAND_TRACKING: u16 = 20;
const BODY_TRACKING: u16 = 21;
const FACE_TRACKING: u16 = 22;
const EYE_GAZE: u16 = 23;
/// The probe puts its floor 0.3 m below its room's origin (`PROBE_FLOOR` in probe/src/native.rs).
/// The host lifts every pose by that much.
const FLOOR_LIFT: f32 = 0.3;

// The probe plays the script of hands.rs and uses all of it. The game only checks it.
#[allow(dead_code)]
#[path = "../hands.rs"]
mod hands;
#[allow(dead_code)]
#[path = "../tracked.rs"]
mod tracked;
const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;
const IMAGES: usize = 3;

/// The badge rect in each eye's view, the same as the host's test scene: x0, x1, y0, y1.
const BADGE: [f32; 4] = [0.08, 0.14, 0.46, 0.54];

fn message(kind: u16, payload: &[u8]) -> Vec<u8> {
    let mut m = (2 + payload.len() as u32).to_le_bytes().to_vec();
    m.extend_from_slice(&kind.to_le_bytes());
    m.extend_from_slice(payload);
    m
}

fn read_message(stream: &mut TcpStream) -> std::io::Result<(u16, Vec<u8>)> {
    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    let mut body = vec![0u8; u32::from_le_bytes(head) as usize];
    stream.read_exact(&mut body)?;
    Ok((u16::from_le_bytes([body[0], body[1]]), body.split_off(2)))
}

/// What the host sent about the controllers.
#[derive(Default)]
struct Input {
    /// Pose messages per hand, and how many of them carried an aim pose
    poses: [u32; 2],
    aim_poses: [u32; 2],
    /// The last grip motion of each hand and its last aim motion
    grip: [Motion; 2],
    aim: [Option<Motion>; 2],
    /// The smallest and the largest grip position that each hand reached, per axis
    reach: [[[f32; 2]; 3]; 2],
    /// The last mask: bit 0 left, bit 1 right
    mask: Option<u32>,
    profiles: [bool; 2],
    /// The last value of each button path and how often it changed
    buttons: BTreeMap<u64, (f32, u32)>,
    /// Every value that each button path took, as f32 bits
    values: BTreeMap<u64, Vec<u32>>,
    resets: u32,
}

/// A motion from the 13 floats of a pose message: orientation x, y, z, w, position, then the
/// linear and the angular velocity
fn motion(values: &[f32]) -> Motion {
    Motion {
        pose: cranvr_proto::wire::Pose { orientation: values[..4].try_into().unwrap(), position: values[4..7].try_into().unwrap() },
        linear_velocity: values[7..10].try_into().unwrap(),
        angular_velocity: values[10..13].try_into().unwrap(),
    }
}

/// The same turn, up to the sign of the quaternion
fn same_turn(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>().abs() > 0.999_99
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3)
}

impl Input {
    fn pose(&mut self, hand: usize, grip: Motion, aim: Option<Motion>) {
        self.poses[hand] += 1;
        self.aim_poses[hand] += u32::from(aim.is_some());
        self.grip[hand] = grip;
        self.aim[hand] = aim;
        for axis in 0..3 {
            let v = grip.pose.position[axis];
            let r = &mut self.reach[hand][axis];
            *r = if self.poses[hand] == 1 { [v, v] } else { [r[0].min(v), r[1].max(v)] };
        }
    }

    fn button(&mut self, path: u64, value: f32) {
        let seen = self.values.entry(path).or_default();
        if !seen.contains(&value.to_bits()) {
            seen.push(value.to_bits());
        }
        let entry = self.buttons.entry(path).or_insert((value, 0));
        if entry.0 != value {
            entry.1 += 1;
        }
        entry.0 = value;
    }

    /// Whether the path ever took `value`
    fn took(&self, id: u64, value: f32) -> bool {
        self.values.get(&id).is_some_and(|seen| seen.contains(&value.to_bits()))
    }

    /// What the host's masking must have done to the probe's `--hold-script`: the right hand's
    /// panel presses (trigger 0.35 and B) never reach the game, and its game presses (trigger 0.75
    /// and A) do. The left hand is never held.
    fn held_problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.took(RIGHT_TRIGGER_VALUE_ID, 0.35) {
            out.push("the right trigger reached the game at 0.35, a press that belongs to a panel".into());
        }
        if self.buttons.get(&RIGHT_B_CLICK_ID).is_some_and(|b| b.0 != 0.0 || self.took(RIGHT_B_CLICK_ID, 1.0)) {
            out.push("the right B button went down in the game, a press that belongs to a panel".into());
        }
        if !self.took(RIGHT_TRIGGER_VALUE_ID, 0.75) || !self.took(RIGHT_A_CLICK_ID, 1.0) {
            out.push("the right hand's presses that belong to the game did not arrive".into());
        }
        out
    }

    fn last(&self, id: u64) -> Option<f32> {
        self.buttons.get(&id).map(|b| b.0)
    }

    /// What the probe's script must have brought: both hands tracked, the left one still, the
    /// right one moving, and each hand with the grip pose and the aim pose that the probe scripted
    /// (`hands.rs`), with the floor's lift and with no offset of the host. A game that locates both
    /// spaces of a hand finds these two poses. The buttons must have come too.
    fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.poses[0] < 50 || self.poses[1] < 50 {
            out.push(format!("poses: {} left, {} right", self.poses[0], self.poses[1]));
        }
        if self.aim_poses != self.poses {
            out.push(format!("aim poses: {:?} of {:?} messages carried one", self.aim_poses, self.poses));
        }
        if self.mask != Some(3) {
            out.push(format!("controller mask {:?}, expected both hands", self.mask));
        }
        if !self.profiles[0] || !self.profiles[1] {
            out.push("no interaction profile for a hand".into());
        }
        for hand in 0..2 {
            let name = ["left", "right"][hand];
            let grip = &self.grip[hand];
            if !same_turn(grip.pose.orientation, hands::grip_orientation(hand)) {
                out.push(format!("{name} grip turned {:?}, expected {:?}", grip.pose.orientation, hands::grip_orientation(hand)));
            }
            if grip.angular_velocity != [0.0; 3] {
                out.push(format!("{name} grip angular velocity {:?}, expected none", grip.angular_velocity));
            }
            let Some(aim) = self.aim[hand] else {
                out.push(format!("{name} hand has no aim pose"));
                continue;
            };
            if !same_turn(aim.pose.orientation, hands::aim_orientation(hand)) {
                out.push(format!("{name} aim turned {:?}, expected {:?}", aim.pose.orientation, hands::aim_orientation(hand)));
            }
            // The aim pose lies where the probe put it relative to the grip pose, and has its own velocities
            let expected = hands::aim_with(hand, grip);
            if !near(aim.pose.position, expected.pose.position) {
                out.push(format!("{name} aim at {:?}, expected {:?}", aim.pose.position, expected.pose.position));
            }
            if !near(aim.linear_velocity, expected.linear_velocity) || !near(aim.angular_velocity, expected.angular_velocity) {
                out.push(format!(
                    "{name} aim moves {:?} and turns {:?}, expected {:?} and {:?}",
                    aim.linear_velocity, aim.angular_velocity, expected.linear_velocity, expected.angular_velocity
                ));
            }
        }
        // The grip pose of the left hand is where the probe holds it. The host lifted it by the floor.
        let lifted = |at: [f32; 3]| [at[0], at[1] + FLOOR_LIFT, at[2]];
        if !near(self.grip[0].pose.position, lifted(hands::LEFT_AT)) {
            out.push(format!("left grip at {:?}, expected {:?}", self.grip[0].pose.position, lifted(hands::LEFT_AT)));
        }
        let right_at = self.grip[1].pose.position;
        if (right_at[1] - lifted(hands::RIGHT_AT)[1]).abs() > 1e-3 || (right_at[2] - hands::RIGHT_AT[2]).abs() > 1e-3 {
            out.push(format!("right grip at {right_at:?}, expected y {} and z {}", lifted(hands::RIGHT_AT)[1], hands::RIGHT_AT[2]));
        }
        let x = self.reach[1][0];
        if x[1] - x[0] < 0.1 {
            out.push(format!("the right hand moved {:.3} m across, expected 0.2 m", x[1] - x[0]));
        }
        let x = self.reach[0][0];
        if x[1] - x[0] > 0.001 {
            out.push("the left hand moved, expected it to stay still".into());
        }
        // The probe pulls the right trigger to 0.75, presses A and tilts the right stick to 0.5, as it squeezes the left grip
        for (name, id, wanted) in [("right trigger", RIGHT_TRIGGER_VALUE_ID, 0.75), ("right stick x", RIGHT_THUMBSTICK_X_ID, 0.5)] {
            match self.buttons.get(&id) {
                Some((_, changes)) if *changes >= 2 => {}
                other => out.push(format!("{name} {other:?}: expected changes up to {wanted}")),
            }
        }
        for (name, id) in [("right A", RIGHT_A_CLICK_ID), ("left squeeze", LEFT_SQUEEZE_VALUE_ID)] {
            if self.buttons.get(&id).is_none_or(|b| b.1 < 2) {
                out.push(format!("{name} did not go down and up"));
            }
        }
        out
    }
}

/// What the host sent of hands, body, face and eyes: the newest message of each kind and how many came.
#[derive(Default)]
struct Tracked {
    hands: [Option<Vec<u8>>; 2],
    body: Option<Vec<u8>>,
    face: Option<Vec<u8>>,
    eyes: Option<Vec<u8>>,
    counts: [u32; 4],
    /// The words of the runtime's tracking-features.txt when the first face came: the host writes them
    /// for the headset's offer, and removes the file when the headset leaves
    offered_words: Option<String>,
}

fn floats_of(bytes: &[u8]) -> Vec<f32> {
    bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

/// Whether the joints in `bytes` (a pose of 7 floats each, then a flags byte each) are the expected
/// ones, with the floor's lift
fn joints_match(bytes: &[u8], expected: &[cranvr_proto::tracking::Joint], what: &str, out: &mut Vec<String>) {
    let count = expected.len();
    if bytes.len() != count * 29 {
        out.push(format!("{what}: {} bytes for {count} joints", bytes.len()));
        return;
    }
    let poses = floats_of(&bytes[..count * 28]);
    for (i, joint) in expected.iter().enumerate() {
        let got = &poses[i * 7..i * 7 + 7];
        let (q, p) = (<[f32; 4]>::try_from(&got[..4]).unwrap(), <[f32; 3]>::try_from(&got[4..7]).unwrap());
        let mut want = joint.pose.position;
        want[1] += FLOOR_LIFT;
        if !same_turn_within(q, joint.pose.orientation, 1e-4) || p.iter().zip(want).any(|(a, b)| (a - b).abs() > 3e-4) {
            out.push(format!("{what}: joint {i} at {p:?} turned {q:?}, expected {want:?} turned {:?}", joint.pose.orientation));
            return;
        }
        if bytes[count * 28 + i] != joint.flags {
            out.push(format!("{what}: joint {i} has flags {}, expected {}", bytes[count * 28 + i], joint.flags));
            return;
        }
    }
}

/// The same turn within `tolerance` in the angle's sine, up to the quaternion's sign
fn same_turn_within(a: [f32; 4], b: [f32; 4], tolerance: f32) -> bool {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>().abs();
    (1.0 - dot) < tolerance * tolerance + 1e-6
}

impl Tracked {
    /// What the script of tracked.rs must have brought for the features that the game used
    fn problems(&self, used: &str, face_source: tracked::FaceSource) -> Vec<String> {
        let mut out = Vec::new();
        let uses = |name: &str| used.split(',').any(|f| f == name);
        if uses("hands") {
            for (side, hand) in self.hands.iter().enumerate() {
                let Some(p) = hand else {
                    out.push(format!("no message for hand {side}"));
                    continue;
                };
                if p.len() != 16 + 13 * 4 + 26 * 29 || u32_at(p, 8) != side as u32 || u32_at(p, 12) != 1 {
                    out.push(format!("hand {side}: {} bytes, hand {}, flags {}", p.len(), u32_at(p, 8), u32_at(p, 12)));
                    continue;
                }
                let (root, joints) = tracked::hand_joints(side);
                let got = floats_of(&p[16..16 + 52]);
                let mut want = root.pose.position;
                want[1] += FLOOR_LIFT;
                if !same_turn_within(got[..4].try_into().unwrap(), root.pose.orientation, 1e-6) || !near(got[4..7].try_into().unwrap(), want) {
                    out.push(format!("hand {side}: wrist at {:?} turned {:?}, expected {want:?}", &got[4..7], &got[..4]));
                }
                if got[7..10] != tracked::HAND_VELOCITY[side] || got[10..13].iter().any(|v| *v != 0.0) {
                    out.push(format!("hand {side}: velocities {:?}, expected {:?} and no turn", &got[7..13], tracked::HAND_VELOCITY[side]));
                }
                joints_match(&p[16 + 52..], &joints, &format!("hand {side}"), &mut out);
            }
        }
        if uses("body") || uses("full_body") {
            let set = if uses("full_body") { cranvr_proto::tracking::BodySet::Full } else { cranvr_proto::tracking::BodySet::Default };
            match &self.body {
                None => out.push("no body message".into()),
                Some(p) => {
                    let count = set.joints();
                    // The probe sends the set that the host asked for: the full one when it was asked
                    if p.len() != 8 + 4 + 4 + 4 + 52 + count * 29 || u32_at(p, 8) != count as u32 || u32_at(p, 12) != 1 {
                        out.push(format!("body: {} bytes, {} joints, flags {}", p.len(), u32_at(p, 8), u32_at(p, 12)));
                    } else {
                        if (f32::from_le_bytes(p[16..20].try_into().unwrap()) - 0.9).abs() > 1e-6 {
                            out.push("body: confidence is not 0.9".into());
                        }
                        let (root, joints) = tracked::body_joints(set);
                        let got = floats_of(&p[20..20 + 52]);
                        let mut want = root.pose.position;
                        want[1] += FLOOR_LIFT;
                        if !near(got[4..7].try_into().unwrap(), want) {
                            out.push(format!("body: root at {:?}, expected {want:?}", &got[4..7]));
                        }
                        joints_match(&p[20 + 52..], &joints, "body", &mut out);
                    }
                }
            }
        }
        if uses("face") {
            match &self.face {
                None => out.push("no face message".into()),
                Some(p) if p.len() == 8 + 4 + 8 + 280 && u32_at(p, 8) == u32::from(face_source.flags()) => {
                    let values = floats_of(&p[12..]);
                    for (k, want) in tracked::FACE_CONFIDENCES.iter().enumerate() {
                        if (values[k] - want).abs() > 0.003 {
                            out.push(format!("face: confidence {k} is {}, expected {want}", values[k]));
                        }
                    }
                    for (i, want) in tracked::face_weights().iter().enumerate() {
                        if (values[2 + i] - want).abs() > 0.003 {
                            out.push(format!("face: weight {i} is {}, expected {want}", values[2 + i]));
                            break;
                        }
                    }
                }
                Some(p) => out.push(format!(
                    "face: {} bytes, flags {}, expected flags {} for the {} source",
                    p.len(),
                    u32_at(p, 8),
                    face_source.flags(),
                    face_source.word()
                )),
            }
            // The headset's offer names the face's source for the game's runtime
            let words: Vec<&str> = self.offered_words.as_deref().unwrap_or("").split_whitespace().collect();
            let want = face_source.words();
            let all = ["face", "face_visual", "face_audio"];
            if all.iter().any(|word| words.contains(word) != want.contains(word)) {
                out.push(format!("face: tracking-features.txt held {words:?}, expected the words {want:?} for the {} source", face_source.word()));
            }
        }
        if uses("eyes") {
            match &self.eyes {
                None => out.push("no eye message".into()),
                Some(p) if p.len() == 8 + 4 + 28 && u32_at(p, 8) == 1 => {
                    let values = floats_of(&p[12..]);
                    let gaze = tracked::gaze();
                    let mut want = gaze.position;
                    want[1] += FLOOR_LIFT;
                    if !same_turn_within(values[..4].try_into().unwrap(), gaze.orientation, 1e-5) || !near(values[4..7].try_into().unwrap(), want) {
                        out.push(format!("eyes: gaze at {:?} turned {:?}, expected {want:?} turned {:?}", &values[4..7], &values[..4], gaze.orientation));
                    }
                }
                Some(p) => out.push(format!("eyes: {} bytes, flags {}", p.len(), u32_at(p, 8))),
            }
        }
        out
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// The +X and the -Z axis that the OpenXR grip pose of a tracked hand must have, in the room, for a
/// palm joint turned by `palm`. The words of the specification decide them:
/// - XR_EXT_hand_tracking: the palm joint's +Y is "perpendicular to palm surface and pointing
///   towards the back of the hand", its +Z points along the middle finger's metacarpal "away from
///   the finger tips", and its X follows the right hand rule. For a right hand with the palm down
///   and the fingers forward, +Y is up, +Z is back and +X is to the right, where the little finger
///   lies. For a left hand +X points to the thumb.
/// - The grip pose (the standard pose identifiers, and XR_EXT_hand_interaction): its +X is the
///   palm's normal "away from the palm of the left hand and into the palm of the right hand", and
///   its -Z "goes from the little finger to the index finger".
///
/// So the right hand's grip has the palm's +Y as its +X and the palm's -X as its -Z. The left hand's
/// grip has the palm's -Y as its +X and the palm's +X as its -Z.
fn grip_axes(left: bool, palm: [f32; 4]) -> (glam::Vec3, glam::Vec3) {
    let palm = glam::Quat::from_array(palm).normalize();
    let side = if left { -1.0 } else { 1.0 };
    (palm * glam::Vec3::Y * side, palm * glam::Vec3::NEG_X * side)
}

impl Input {
    /// What a game without hand code must have got from the probe's hands: a controller for each
    /// hand, with the grip pose of the OpenXR specification at the palm joint, the trigger of a
    /// half-open pinch and the aim pose through the fingers
    fn hand_controller_problems(&self, held: bool) -> Vec<String> {
        let mut out = Vec::new();
        if self.mask != Some(3) {
            out.push(format!("controller mask {:?}, expected both hands", self.mask));
        }
        for (side, (trigger_id, squeeze_id)) in [(LEFT_TRIGGER_VALUE_ID, LEFT_SQUEEZE_VALUE_ID), (RIGHT_TRIGGER_VALUE_ID, RIGHT_SQUEEZE_VALUE_ID)].into_iter().enumerate() {
            if self.poses[side] == 0 || self.aim_poses[side] == 0 {
                out.push(format!("hand {side} made no controller with an aim pose"));
                continue;
            }
            let (_, joints) = tracked::hand_joints(side);
            let mut palm = joints[0].pose.position;
            palm[1] += FLOOR_LIFT;
            if !near(self.grip[side].pose.position, palm) {
                out.push(format!("hand {side}: grip at {:?}, expected the palm joint at {palm:?}", self.grip[side].pose.position));
            }
            let (x, minus_z) = grip_axes(side == 0, joints[0].pose.orientation);
            let grip = glam::Quat::from_array(self.grip[side].pose.orientation).normalize();
            let (got_x, got_minus_z) = (grip * glam::Vec3::X, grip * glam::Vec3::NEG_Z);
            if got_x.dot(x) < 0.9999 || got_minus_z.dot(minus_z) < 0.9999 {
                out.push(format!("hand {side}: grip +X {got_x:?} and -Z {got_minus_z:?}, expected +X {x:?} and -Z {minus_z:?} of the OpenXR grip pose"));
            }
            // The thumb tip and the index tip: 0.0418 m apart in the script. The trigger runs from 1
            // at 18 mm to 0 at 60 mm.
            let distance = |a: usize, b: usize| (0..3).map(|k| (joints[a].pose.position[k] - joints[b].pose.position[k]).powi(2)).sum::<f32>().sqrt();
            // A hand that a panel holds back from the game makes a controller with rest values
            let wanted = if held { 0.0 } else { ((0.060 - distance(5, 10)) / (0.060 - 0.018)).clamp(0.0, 1.0) };
            match self.last(trigger_id) {
                Some(v) if (v - wanted).abs() < 0.01 => {}
                other => out.push(format!("hand {side}: trigger {other:?}, expected {wanted:.3}")),
            }
            if self.last(squeeze_id).is_none_or(|v| v != 0.0) {
                out.push(format!("hand {side}: squeeze {:?}, expected an open hand", self.last(squeeze_id)));
            }
            // The aim pose lies between the thumb tip and the index tip and points away from the
            // shoulder, which sits below the head at the hand's side
            let aim = self.aim[side].expect("checked above");
            let mut target = [0, 1, 2].map(|k| (joints[5].pose.position[k] + joints[10].pose.position[k]) / 2.0);
            target[1] += FLOOR_LIFT;
            if !near(aim.pose.position, target) {
                out.push(format!("hand {side}: aim at {:?}, expected {target:?}", aim.pose.position));
            }
            let head = [0.0, 1.6 + FLOOR_LIFT, 0.0];
            let side_sign = if side == 0 { -1.0 } else { 1.0 };
            let shoulder = [head[0] + 0.17 * side_sign, head[1] - 0.25, head[2] + 0.05];
            let ray = [0, 1, 2].map(|k| target[k] - shoulder[k]);
            let length = ray.iter().map(|v| v * v).sum::<f32>().sqrt();
            let q = aim.pose.orientation;
            // The -Z axis of the aim pose
            let forward = {
                let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
                [-(2.0 * (x * z + w * y)), -(2.0 * (y * z - w * x)), -(1.0 - 2.0 * (x * x + y * y))]
            };
            let along: f32 = (0..3).map(|k| forward[k] * ray[k] / length).sum();
            if along < 0.9999 {
                out.push(format!("hand {side}: aim points {forward:?}, expected along {:?}", ray.map(|v| v / length)));
            }
        }
        out
    }
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Gray, with a badge: blue in the left eye, red in the right eye.
fn paint(image: &mut PixelBuffer, eye: usize) {
    let (w, h) = (image.width(), image.height());
    let badge = if eye == 0 { [255u8, 25, 25, 255] } else { [25u8, 25, 255, 255] }; // BGRA
    image.with_bgra_mut(|data, stride| {
        for y in 0..h {
            let v = (y as f32 + 0.5) / h as f32;
            for x in 0..w {
                let u = (x as f32 + 0.5) / w as f32;
                let inside = u > BADGE[0] && u < BADGE[1] && v > BADGE[2] && v < BADGE[3];
                let px = if inside { badge } else { [90, 90, 90, 255] };
                data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&px);
            }
        }
    });
}

fn main() {
    let (mut control, mut fps, mut seconds, mut expect_input, mut expect_held) = (None, 72.0f64, 10.0f64, false, false);
    let (mut use_tracking, mut expect_tracking, mut expect_hand_controllers, mut expect_no_controllers) = (String::new(), false, false, false);
    let mut expect_hand_held = false;
    let mut release_tracking = false;
    let mut stall: Option<(f64, f64)> = None;
    let mut face_source = tracked::FaceSource::default();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--control" => control = args.next(),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).expect("--fps N"),
            "--seconds" => seconds = args.next().and_then(|v| v.parse().ok()).expect("--seconds N"),
            "--expect-input" => expect_input = true,
            "--expect-held" => expect_held = true,
            "--use-tracking" => use_tracking = args.next().expect("--use-tracking FEATURES"),
            "--expect-tracking" => expect_tracking = true,
            "--expect-hand-controllers" => expect_hand_controllers = true,
            "--expect-hand-held" => expect_hand_held = true,
            "--expect-no-controllers" => expect_no_controllers = true,
            "--release-tracking" => release_tracking = true,
            "--stall" => {
                let value = args.next().expect("--stall AT:FOR");
                stall = value.split_once(':').and_then(|(at, length)| Some((at.parse().ok()?, length.parse().ok()?)));
                assert!(stall.is_some(), "--stall AT:FOR takes two numbers of seconds");
            }
            "--face-source" => face_source = args.next().as_deref().and_then(tracked::FaceSource::parse).expect("--face-source visual|audio|unknown"),
            _ => {
                eprintln!("usage: cranvr-fake-game --control FILE [--fps N] [--seconds N] [--expect-input] [--expect-held] [--use-tracking FEATURES] [--expect-tracking] [--face-source visual|audio|unknown] [--expect-hand-controllers] [--expect-hand-held] [--expect-no-controllers] [--release-tracking] [--stall AT:FOR]");
                std::process::exit(2);
            }
        }
    }
    let control_path = control.expect("--control FILE");
    let control = fs::read_to_string(&control_path).expect("read the control file");
    let fields: Vec<&str> = control.split_whitespace().collect();
    let port: u16 = fields[0].parse().expect("port");
    let token = fields[1];
    let (w, h): (u32, u32) = (fields[2].parse().unwrap(), fields[3].parse().unwrap());

    // The host writes the headset's features here for the runtime (see runtime_link.rs)
    let tracking_file = std::path::Path::new(&control_path).with_file_name("tracking-features.txt");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the host");
    stream.set_nodelay(true).ok();
    let name = b"cranvr-fake-game";
    let mut hello = PROTOCOL_VERSION.to_le_bytes().to_vec();
    hello.extend_from_slice(token.as_bytes());
    hello.extend_from_slice(&1u64.to_le_bytes());
    hello.extend_from_slice(&(name.len() as u16).to_le_bytes());
    hello.extend_from_slice(name);
    stream.write_all(&message(HELLO, &hello)).unwrap();

    // The reader thread keeps the newest pose and view config, and reports acks and consumed frames.
    let head_ns = Arc::new(AtomicU64::new(0));
    let fov = Arc::new(Mutex::new([[-0.8f32, 0.8, 0.8, -0.8]; 2]));
    let input = Arc::new(Mutex::new(Input::default()));
    let tracked_data = Arc::new(Mutex::new(Tracked::default()));
    let (acks, ack_rx) = mpsc::channel::<u16>();
    let (consumed, consumed_rx) = mpsc::channel::<u64>();
    {
        let (head_ns, fov, input, tracked_data) = (Arc::clone(&head_ns), Arc::clone(&fov), Arc::clone(&input), Arc::clone(&tracked_data));
        let tracking_file = tracking_file.clone();
        let mut reader = stream.try_clone().unwrap();
        let mut haptic_stream = stream.try_clone().unwrap();
        thread::spawn(move || {
            while let Ok((kind, p)) = read_message(&mut reader) {
                match kind {
                    HELLO_ACK | SWAPCHAIN_ACK => {
                        acks.send(kind).ok();
                    }
                    HEAD_POSE => head_ns.store(u64_at(&p, 0), Ordering::Relaxed),
                    VIEW_CONFIG => {
                        let mut f = fov.lock().unwrap();
                        for (e, eye) in f.iter_mut().enumerate() {
                            for (i, v) in eye.iter_mut().enumerate() {
                                *v = f32_at(&p, 4 + (e * 4 + i) * 4);
                            }
                        }
                    }
                    FRAME_CONSUMED => {
                        consumed.send(u64_at(&p, 0)).ok();
                    }
                    CONTROLLER_MOTION | CONTROLLER_MOTION_AIM if p.len() == 16 + 13 * 4 * (1 + usize::from(kind == CONTROLLER_MOTION_AIM)) => {
                        let hand = u32::from_le_bytes(p[8..12].try_into().unwrap()) as usize;
                        let values: Vec<f32> = (0..(p.len() - 16) / 4).map(|i| f32_at(&p, 16 + i * 4)).collect();
                        let aim = (kind == CONTROLLER_MOTION_AIM).then(|| motion(&values[13..]));
                        input.lock().unwrap().pose(hand.min(1), motion(&values[..13]), aim);
                    }
                    CONTROLLER_MASK if p.len() >= 12 => {
                        input.lock().unwrap().mask = Some(u32::from_le_bytes(p[8..12].try_into().unwrap()));
                    }
                    INTERACTION_PROFILE if p.len() >= 4 => {
                        let hand = u32::from_le_bytes(p[0..4].try_into().unwrap()) as usize;
                        input.lock().unwrap().profiles[hand.min(1)] = true;
                    }
                    BUTTON_STATE if p.len() >= 16 => {
                        let (path, value) = (u64_at(&p, 0), f32_at(&p, 12));
                        let mut input = input.lock().unwrap();
                        let was = input.last(path).unwrap_or(0.0);
                        input.button(path, value);
                        // A game answers a pull of the left grip with a pulse
                        if path == LEFT_SQUEEZE_VALUE_ID && was < 0.5 && value >= 0.5 {
                            let mut pulse = 0u32.to_le_bytes().to_vec();
                            pulse.extend_from_slice(&20_000_000u64.to_le_bytes());
                            pulse.extend_from_slice(&160.0f32.to_le_bytes());
                            pulse.extend_from_slice(&0.8f32.to_le_bytes());
                            haptic_stream.write_all(&message(HAPTIC, &pulse)).ok();
                        }
                    }
                    HAND_TRACKING if p.len() >= 16 => {
                        let mut t = tracked_data.lock().unwrap();
                        t.counts[0] += 1;
                        // An inactive hand has no joints: only a tracked one is kept
                        if u32_at(&p, 12) == 1 {
                            let side = (u32_at(&p, 8) as usize).min(1);
                            t.hands[side] = Some(p);
                        }
                    }
                    BODY_TRACKING => {
                        let mut t = tracked_data.lock().unwrap();
                        t.counts[1] += 1;
                        t.body = Some(p);
                    }
                    FACE_TRACKING => {
                        let mut t = tracked_data.lock().unwrap();
                        t.counts[2] += 1;
                        t.face = Some(p);
                        if t.offered_words.is_none() {
                            t.offered_words = fs::read_to_string(&tracking_file).ok();
                        }
                    }
                    EYE_GAZE => {
                        let mut t = tracked_data.lock().unwrap();
                        t.counts[3] += 1;
                        t.eyes = Some(p);
                    }
                    14 => input.lock().unwrap().resets += 1,
                    TEARDOWN => break,
                    _ => {}
                }
            }
        });
    }
    let wait_ack = |kind| {
        let got = ack_rx.recv_timeout(Duration::from_secs(5)).expect("the host did not answer");
        assert_eq!(got, kind);
    };
    wait_ack(HELLO_ACK);

    // One swapchain per eye: IDs 1 (left) and 2 (right)
    let mut swapchains: Vec<Vec<PixelBuffer>> = Vec::new();
    for eye in 0..2 {
        let mut images = Vec::new();
        let mut create = (eye as u64 + 1).to_le_bytes().to_vec();
        for v in [w, h, DXGI_FORMAT_B8G8R8A8_UNORM, IMAGES as u32] {
            create.extend_from_slice(&v.to_le_bytes());
        }
        for _ in 0..IMAGES {
            let mut image = PixelBuffer::new_bgra_shared(w as usize, h as usize).expect("IOSurface");
            paint(&mut image, eye);
            create.extend_from_slice(&image.iosurface_id().expect("IOSurface ID").to_le_bytes());
            images.push(image);
        }
        stream.write_all(&message(CREATE_SWAPCHAIN, &create)).unwrap();
        wait_ack(SWAPCHAIN_ACK);
        swapchains.push(images);
    }
    println!("fake-game: {IMAGES} images of {w}x{h} per eye, {fps} fps");

    let period = Duration::from_secs_f64(1.0 / fps);
    let start = Instant::now();
    let mut next = start;
    let mut in_flight: [Option<u64>; IMAGES] = [None; IMAGES];
    let (mut sent, mut done, mut no_pose, mut stalled) = (0u64, 0u64, 0u64, 0u64);
    let mut number = 0u64;
    let features_used = |names: &str| {
        [("hands", 1u32), ("body", 2), ("full_body", 4), ("face", 8), ("eyes", 16)]
            .into_iter()
            .filter(|(name, _)| names.split(',').any(|f| f == *name))
            .fold(0, |bits, (_, bit)| bits | bit)
    };
    // The game creates its trackers a second after the start. It may destroy them six seconds before the end.
    let (mut requested, mut released) = (false, false);
    let unix_ms = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let mut stall_begun = false;
    while start.elapsed().as_secs_f64() < seconds {
        if !use_tracking.is_empty() {
            let at = start.elapsed().as_secs_f64();
            if !requested && at >= 1.0 {
                requested = true;
                println!("fake-game: tracking requested: {use_tracking} at unix {} ms", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis()));
                stream.write_all(&message(TRACKING_REQUEST, &features_used(&use_tracking).to_le_bytes())).unwrap();
            } else if release_tracking && requested && !released && at >= seconds - 6.0 {
                released = true;
                stream.write_all(&message(TRACKING_REQUEST, &0u32.to_le_bytes())).unwrap();
            }
        }
        next += period;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        // A stall: the game sends no frame, as while it loads a level
        if let Some((at, length)) = stall {
            let now = start.elapsed().as_secs_f64();
            if (at..at + length).contains(&now) {
                if !stall_begun {
                    stall_begun = true;
                    println!("fake-game: no frames from unix {} ms for {length} s", unix_ms());
                }
                continue;
            }
            if stall_begun && now >= at + length {
                stall_begun = false;
                stall = None;
                println!("fake-game: frames again at unix {} ms", unix_ms());
            }
        }
        for n in consumed_rx.try_iter() {
            done += 1;
            in_flight.iter_mut().filter(|f| **f == Some(n)).for_each(|f| *f = None);
        }
        let tracking = head_ns.load(Ordering::Relaxed);
        if tracking == 0 {
            no_pose += 1;
            continue;
        }
        let image = (number as usize) % IMAGES;
        if in_flight[image].is_some() {
            stalled += 1;
            continue;
        }
        number += 1;
        in_flight[image] = Some(number);
        let f = *fov.lock().unwrap();
        let mut frame = number.to_le_bytes().to_vec();
        frame.extend_from_slice(&0i64.to_le_bytes());
        frame.extend_from_slice(&tracking.to_le_bytes());
        frame.extend_from_slice(&2u32.to_le_bytes());
        for (eye, eye_fov) in f.iter().enumerate() {
            frame.extend_from_slice(&(eye as u64 + 1).to_le_bytes());
            frame.extend_from_slice(&(image as u32).to_le_bytes());
            frame.extend_from_slice(&0u32.to_le_bytes());
            for v in [0, 0, w as i32, h as i32] {
                frame.extend_from_slice(&v.to_le_bytes());
            }
            for v in [0.0f32, 0.0, 0.0, 1.0, 0.0, 1.6, 0.0] {
                frame.extend_from_slice(&v.to_le_bytes());
            }
            for v in eye_fov {
                frame.extend_from_slice(&v.to_le_bytes());
            }
        }
        if stream.write_all(&message(FRAME_READY, &frame)).is_err() {
            eprintln!("fake-game: the host closed the connection");
            std::process::exit(1);
        }
        sent += 1;
    }
    stream.write_all(&message(TEARDOWN, &1u32.to_le_bytes())).ok();
    thread::sleep(Duration::from_millis(100));
    done += consumed_rx.try_iter().count() as u64;
    println!("fake-game: {sent} frames sent, {done} consumed, {no_pose} waits for a first pose, {stalled} waits for a free image");
    let input = input.lock().unwrap();
    println!(
        "fake-game: input: {} left and {} right poses, {} and {} of them with an aim pose, mask {:?}, profiles {:?}, {} input resets",
        input.poses[0], input.poses[1], input.aim_poses[0], input.aim_poses[1], input.mask, input.profiles, input.resets
    );
    for (hand, name) in ["left", "right"].into_iter().enumerate() {
        let (grip, aim) = (&input.grip[hand], &input.aim[hand]);
        println!(
            "fake-game: input: last {name} grip at {:.3?} turned {:.3?}; aim at {:.3?} turned {:.3?}, moving {:.3?}, turning {:.3?}",
            grip.pose.position,
            grip.pose.orientation,
            aim.map(|a| a.pose.position),
            aim.map(|a| a.pose.orientation),
            aim.map(|a| a.linear_velocity),
            aim.map(|a| a.angular_velocity),
        );
    }
    println!("fake-game: input: {} button paths, buttons that changed: {:?}", input.buttons.len(), input.buttons.iter().filter(|(_, b)| b.1 > 0).map(|(id, b)| (id, b.0, b.1)).collect::<Vec<_>>());
    let mut problems = if expect_input { input.problems() } else { Vec::new() };
    if expect_held {
        problems.extend(input.held_problems());
        println!("fake-game: input: the right trigger took the values {:?}", input.values.get(&RIGHT_TRIGGER_VALUE_ID).map(|v| v.iter().map(|b| f32::from_bits(*b)).collect::<Vec<_>>()));
    }
    for problem in &problems {
        println!("fake-game: input problem: {problem}");
    }
    let mut ok = sent > 0 && done * 10 >= sent * 9 && problems.is_empty();
    let tracked_data = tracked_data.lock().unwrap();
    println!(
        "fake-game: tracking: {} hand, {} body, {} face and {} eye messages",
        tracked_data.counts[0], tracked_data.counts[1], tracked_data.counts[2], tracked_data.counts[3]
    );
    if expect_tracking {
        let problems = tracked_data.problems(&use_tracking, face_source);
        for problem in &problems {
            println!("fake-game: tracking problem: {problem}");
        }
        ok &= problems.is_empty();
        println!("fake-game: tracking {}", if ok { "matches the script" } else { "does not match the script" });
    }
    if expect_no_controllers {
        // A game that uses hands has hand code of its own: no controller is made of a hand
        if input.poses != [0, 0] || input.mask.is_some_and(|m| m != 0) {
            println!("fake-game: controllers {:?} with mask {:?} came, expected none", input.poses, input.mask);
            ok = false;
        }
    }
    if expect_hand_controllers {
        let problems = input.hand_controller_problems(expect_hand_held);
        for problem in &problems {
            println!("fake-game: hand controller problem: {problem}");
        }
        ok &= problems.is_empty();
        println!("fake-game: controllers made of hands {}", if problems.is_empty() { "match the script" } else { "do not match the script" });
    }
    std::process::exit(if ok { 0 } else { 1 });
}
