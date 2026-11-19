//! Hands, body, face and eyes: what a headset tracks besides its head and controllers, and the
//! messages that carry it to the host. The host forwards the data to the game's OpenXR runtime,
//! which offers it through `XR_EXT_hand_tracking`, `XR_FB_body_tracking`,
//! `XR_META_body_tracking_full_body`, `XR_FB_face_tracking2` and `XR_EXT_eye_gaze_interaction`.
//!
//! Protocol version [`TRACKING_VERSION`] added these messages. The flow:
//! 1. After the welcome, a client of that version sends [`crate::message::ClientMsg::TrackingOffer`]
//!    with the features that its headset has and its user allowed.
//! 2. The host sends [`crate::message::HostMsg::TrackingWish`] with the features that a game uses now.
//!    It sends the wish again when the set changes and when the headset connects. A feature that no
//!    game uses costs no traffic.
//! 3. For every headset frame the client sends one datagram for each wished feature. The datagram
//!    carries the sample time and the display time of the frame's [`crate::message::Tracking`], so the
//!    host pairs them with the head and the controllers.
//!
//! A peer that does not know a message skips it (see `link.rs`), so an older host or client takes no
//! harm from them. The host sends a wish only to a client of version [`TRACKING_VERSION`] or newer.
//!
//! All poses are in the headset's room, like the head's and the controllers'. Joints travel as poses
//! relative to a root pose that has full precision and a velocity. The relative parts are quantized:
//! a position to 0.1 mm in an `i16` (a joint lies within 3.2 m of its root), an orientation to its
//! three smaller components in an `i16` each. The error of a position is below 0.1 mm and the error
//! of an angle below 0.005 degrees, far below what a headset's tracking resolves. A face weight is a
//! `u8`: steps of 0.4 percent.

use crate::wire::{Motion, Nanos, Pose};
use crate::Error;
use serde::{Deserialize, Serialize};

/// The first protocol version with the tracking messages.
pub const TRACKING_VERSION: u16 = 6;

/// The joints of one hand in `XR_EXT_hand_tracking`, in its order: palm, wrist, then the thumb with
/// four joints and each other finger with five.
pub const HAND_JOINTS: usize = 26;
/// The joints of `XR_FB_body_tracking`'s default set. The first 70 joints of
/// `XR_META_body_tracking_full_body` are the same.
pub const BODY_JOINTS: usize = 70;
/// The joints of the full body set of `XR_META_body_tracking_full_body`, with the legs.
pub const FULL_BODY_JOINTS: usize = 84;
/// The blend shapes of `XR_FB_face_tracking2`
pub const FACE_WEIGHTS: usize = 70;
/// The confidences of `XR_FB_face_tracking2`: the lower face, then the upper face
pub const FACE_CONFIDENCES: usize = 2;

/// A set of tracking features, as bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Features(pub u8);

impl Features {
    pub const NONE: Features = Features(0);
    /// Both hands with 26 joints each
    pub const HANDS: Features = Features(1);
    /// The body with the 70 joints of the default set
    pub const BODY: Features = Features(1 << 1);
    /// The body with 84 joints, the legs included. A client that sends it sends no separate default
    /// set: the first 70 joints are the default set.
    pub const FULL_BODY: Features = Features(1 << 2);
    /// 70 blend shapes
    pub const FACE: Features = Features(1 << 3);
    /// One gaze pose
    pub const EYES: Features = Features(1 << 4);
    /// Every feature that this version knows
    pub const ALL: Features = Features(0b1_1111);

    /// Whether every feature of `other` is in this set
    pub fn contains(self, other: Features) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn union(self, other: Features) -> Features {
        Features(self.0 | other.0)
    }

    /// The features in both sets
    pub fn intersect(self, other: Features) -> Features {
        Features(self.0 & other.0)
    }

    /// Drops the bits that this version does not know, so that a newer peer's bits do no harm.
    pub fn known(self) -> Features {
        self.intersect(Features::ALL)
    }

    /// The names of the features in the set, in a fixed order. They are the words of the
    /// `tracking-features.txt` file that the host writes for the game's runtime.
    pub fn names(self) -> Vec<&'static str> {
        [(Features::HANDS, "hands"), (Features::BODY, "body"), (Features::FULL_BODY, "full_body"), (Features::FACE, "face"), (Features::EYES, "eyes")]
            .into_iter()
            .filter(|(feature, _)| self.contains(*feature))
            .map(|(_, name)| name)
            .collect()
    }

    /// The set that a list of names gives. A word that is no name is skipped.
    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Features {
        names.into_iter().fold(Features::NONE, |set, name| {
            set.union(match name {
                "hands" => Features::HANDS,
                "body" => Features::BODY,
                "full_body" => Features::FULL_BODY,
                "face" => Features::FACE,
                "eyes" => Features::EYES,
                _ => Features::NONE,
            })
        })
    }
}

/// Bits of a joint's flags. They say what the headset knows of the joint's pose.
pub mod joint {
    pub const POSITION_VALID: u8 = 1;
    pub const ORIENTATION_VALID: u8 = 1 << 1;
    /// The position comes from a measurement, not from a guess
    pub const POSITION_TRACKED: u8 = 1 << 2;
    pub const ORIENTATION_TRACKED: u8 = 1 << 3;
    pub const ALL: u8 = 0b1111;
}

/// One joint's pose in the headset's room with its flags (see [`joint`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Joint {
    pub pose: Pose,
    pub flags: u8,
}

impl Joint {
    /// A joint that the headset tracks fully
    pub fn tracked(pose: Pose) -> Joint {
        Joint { pose, flags: joint::ALL }
    }
}

/// The scale of a packed position in meters: 0.1 mm.
const POSITION_STEP: f32 = 1e-4;
/// The bytes of one packed joint: three positions, the three smaller components of the orientation
/// and a tag.
pub const PACKED_JOINT_LEN: usize = 13;
/// The largest component of a unit quaternion's three smaller ones is 1 / sqrt(2).
const SMALL_MAX: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn conjugate(q: [f32; 4]) -> [f32; 4] {
    [-q[0], -q[1], -q[2], q[3]]
}

fn multiply(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let u = [x, y, z];
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let t = cross(u, v).map(|c| 2.0 * c);
    let c = cross(u, t);
    [v[0] + w * t[0] + c[0], v[1] + w * t[1] + c[1], v[2] + w * t[2] + c[2]]
}

fn normalized(q: [f32; 4]) -> [f32; 4] {
    let length = q.iter().map(|c| c * c).sum::<f32>().sqrt();
    if length < 1e-6 || !length.is_finite() {
        return [0.0, 0.0, 0.0, 1.0];
    }
    q.map(|c| c / length)
}

/// The pose of `pose` in the frame of `root`
fn relative(root: &Pose, pose: &Pose) -> Pose {
    let inverse = conjugate(root.orientation);
    let offset = [0, 1, 2].map(|i| pose.position[i] - root.position[i]);
    Pose { orientation: normalized(multiply(inverse, pose.orientation)), position: rotate(inverse, offset) }
}

/// The pose of `relative` when its frame is `root`
fn absolute(root: &Pose, relative: &Pose) -> Pose {
    let moved = rotate(root.orientation, relative.position);
    Pose {
        orientation: normalized(multiply(root.orientation, relative.orientation)),
        position: [0, 1, 2].map(|i| root.position[i] + moved[i]),
    }
}

fn pack_joint(out: &mut Vec<u8>, relative: &Pose, flags: u8) {
    for c in relative.position {
        let steps = (c / POSITION_STEP).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        out.extend_from_slice(&steps.to_le_bytes());
    }
    let q = normalized(relative.orientation);
    // The largest component is rebuilt from the others, so it is not sent. Its sign is made positive
    // by negating the whole quaternion, which is the same rotation.
    let largest = (0..4).max_by(|&a, &b| q[a].abs().total_cmp(&q[b].abs())).unwrap_or(3);
    let sign = if q[largest] < 0.0 { -1.0 } else { 1.0 };
    for i in (0..4).filter(|&i| i != largest) {
        let steps = (q[i] * sign / SMALL_MAX * i16::MAX as f32).round().clamp(-(i16::MAX as f32), i16::MAX as f32) as i16;
        out.extend_from_slice(&steps.to_le_bytes());
    }
    out.push(largest as u8 | (flags & joint::ALL) << 2);
}

fn unpack_joint(bytes: &[u8]) -> Option<(Pose, u8)> {
    let value = |at: usize| i16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let tag = bytes[12];
    let largest = (tag & 3) as usize;
    let flags = tag >> 2 & joint::ALL;
    let position = [0, 1, 2].map(|i| value(i * 2) as f32 * POSITION_STEP);
    let mut q = [0.0f32; 4];
    let mut sum = 0.0;
    let mut small = (0..4).filter(|&i| i != largest);
    for k in 0..3 {
        let i = small.next()?;
        q[i] = value(6 + k * 2) as f32 / i16::MAX as f32 * SMALL_MAX;
        sum += q[i] * q[i];
    }
    q[largest] = (1.0 - sum).max(0.0).sqrt();
    Some((Pose { orientation: normalized(q), position }, flags))
}

/// Packs joints that lie in the room as poses in the frame of `root`.
fn pack(root: &Pose, joints: &[Joint]) -> Vec<u8> {
    let mut out = Vec::with_capacity(joints.len() * PACKED_JOINT_LEN);
    for joint in joints {
        pack_joint(&mut out, &relative(root, &joint.pose), joint.flags);
    }
    out
}

fn unpack(root: &Pose, bytes: &[u8], count: usize) -> Result<Vec<Joint>, Error> {
    if bytes.len() != count * PACKED_JOINT_LEN {
        return Err(Error::Malformed("a joint block of the wrong length"));
    }
    let (chunks, _) = bytes.as_chunks::<PACKED_JOINT_LEN>();
    chunks
        .iter()
        .map(|chunk| {
            let (pose, flags) = unpack_joint(&chunk[..]).ok_or(Error::Malformed("a joint that does not unpack"))?;
            Ok(Joint { pose: absolute(root, &pose), flags })
        })
        .collect()
}

/// One hand: the pose of its root with the velocities, and the 26 joints in the frame of the root.
/// The root is the wrist joint of `XR_EXT_hand_tracking`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hand {
    pub root: Motion,
    packed: Vec<u8>,
}

impl Hand {
    /// A hand from the poses of its joints in the room. `root` leads the packing, so its velocities
    /// carry the motion of the whole hand: the host moves all joints with them.
    pub fn new(root: Motion, joints: &[Joint; HAND_JOINTS]) -> Hand {
        Hand { root, packed: pack(&root.pose, joints) }
    }

    /// The joints as they lay in the room, within the quantization step.
    pub fn joints(&self) -> Result<[Joint; HAND_JOINTS], Error> {
        let joints = unpack(&self.root.pose, &self.packed, HAND_JOINTS)?;
        joints.try_into().map_err(|_| Error::Malformed("a hand with the wrong joint count"))
    }
}

/// Both hands at one sample. A hand that the headset does not track is `None`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hands {
    /// The `sample_time` of the [`crate::message::Tracking`] sample of the same headset frame
    pub sample_time: Nanos,
    /// The `display_time` of that sample. The poses are predictions for it.
    pub display_time: Nanos,
    pub left: Option<Hand>,
    pub right: Option<Hand>,
}

/// Which joints a body message holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodySet {
    /// 70 joints
    Default,
    /// 84 joints
    Full,
}

impl BodySet {
    pub fn joints(self) -> usize {
        match self {
            BodySet::Default => BODY_JOINTS,
            BodySet::Full => FULL_BODY_JOINTS,
        }
    }
}

/// The body at one sample.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Body {
    pub sample_time: Nanos,
    pub display_time: Nanos,
    pub set: BodySet,
    /// How far the headset trusts the whole body, from 0 to 1
    pub confidence: f32,
    /// The pose of the root joint, the first joint, with the velocities
    pub root: Motion,
    packed: Vec<u8>,
}

impl Body {
    /// A body of the joint set `set` from the room poses of its joints. `joints` holds as many as the
    /// set has, the root joint first.
    pub fn new(sample_time: Nanos, display_time: Nanos, set: BodySet, confidence: f32, root: Motion, joints: &[Joint]) -> Result<Body, Error> {
        if joints.len() != set.joints() {
            return Err(Error::Malformed("a body with the wrong joint count"));
        }
        Ok(Body { sample_time, display_time, set, confidence, root, packed: pack(&root.pose, joints) })
    }

    pub fn joints(&self) -> Result<Vec<Joint>, Error> {
        unpack(&self.root.pose, &self.packed, self.set.joints())
    }
}

/// Bits of [`Face::flags`].
pub mod face {
    /// The weights are valid
    pub const VALID: u8 = 1;
    /// The weights of the eyes follow the eyes' real movement
    pub const EYES_FOLLOWING_VALID: u8 = 1 << 1;
    /// The weights come from the user's voice, not from cameras
    pub const FROM_AUDIO: u8 = 1 << 2;
}

/// The face at one sample: blend shape weights in the order of `XrFaceExpression2FB`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Face {
    pub sample_time: Nanos,
    pub display_time: Nanos,
    /// Bits of [`face`]
    pub flags: u8,
    /// The lower face, then the upper face, each from 0 to 255
    pub confidence: [u8; FACE_CONFIDENCES],
    weights: Vec<u8>,
}

fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

impl Face {
    pub fn new(sample_time: Nanos, display_time: Nanos, flags: u8, confidence: [f32; FACE_CONFIDENCES], weights: &[f32; FACE_WEIGHTS]) -> Face {
        Face { sample_time, display_time, flags, confidence: confidence.map(to_byte), weights: weights.iter().map(|w| to_byte(*w)).collect() }
    }

    /// The weights from 0 to 1, within half a step of 1/255
    pub fn weights(&self) -> Result<[f32; FACE_WEIGHTS], Error> {
        let weights: Vec<f32> = self.weights.iter().map(|w| *w as f32 / 255.0).collect();
        weights.try_into().map_err(|_| Error::Malformed("a face with the wrong weight count"))
    }

    pub fn confidences(&self) -> [f32; FACE_CONFIDENCES] {
        self.confidence.map(|c| c as f32 / 255.0)
    }
}

/// The user's gaze at one sample: the middle between the eyes and the direction of both eyes' view,
/// which is the pose's -Z axis. `None` while the headset has no gaze.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Eyes {
    pub sample_time: Nanos,
    pub display_time: Nanos,
    pub gaze: Option<Pose>,
}

/// What a tracking datagram carries, for a host that handles them alike.
#[derive(Clone, Debug, PartialEq)]
pub enum Tracked {
    Hands(Hands),
    Body(Body),
    Face(Face),
    Eyes(Eyes),
}

impl Tracked {
    /// The feature that the data belongs to. The body is the full body set when it has 84 joints.
    pub fn feature(&self) -> Features {
        match self {
            Tracked::Hands(_) => Features::HANDS,
            Tracked::Body(body) if body.set == BodySet::Full => Features::FULL_BODY,
            Tracked::Body(_) => Features::BODY,
            Tracked::Face(_) => Features::FACE,
            Tracked::Eyes(_) => Features::EYES,
        }
    }

    pub fn sample_time(&self) -> Nanos {
        match self {
            Tracked::Hands(h) => h.sample_time,
            Tracked::Body(b) => b.sample_time,
            Tracked::Face(f) => f.sample_time,
            Tracked::Eyes(e) => e.sample_time,
        }
    }
}
