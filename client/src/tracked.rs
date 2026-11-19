//! The user's hands, body, face and eyes, from the headset's frames to the host's games (see
//! `cranvr_proto::tracking` for the messages and `tracking.rs` for the sender).
//!
//! The build script asks Cranpose for the four kinds of tracking (`build.rs`). Cranpose then reads
//! them in each frame whatever the host wants, because an OpenXR instance fixes its extensions
//! before the app runs. This file keeps the cost of the host's wish down:
//! - [`offer`] turns the frame's capabilities into the features that the headset offers;
//! - [`TrackedReader`] copies only the wished features out of the frame into a buffer that it
//!   allocated once, and estimates the velocity of the wrists and of the body's root from successive
//!   frames, because Cranpose gives no velocities for joints;
//! - the sender takes the buffer from the [`crate::tracking::Feed`] and sends one datagram for each
//!   wished feature. Nothing allocates in a frame.

use cranpose_xr::{
    BodyJointType, BodyState, Eyes, FaceDataSource, HandJointType, HandState, MetaFaceState, Pose as XrPose, TrackingAvailability,
    TrackingCapabilities, TrackingFeature, TrackingState, XrFrame,
};
use cranvr_proto::{
    motion::velocities,
    tracking::{face, joint, BodySet, Features, Joint, FACE_CONFIDENCES, FACE_WEIGHTS, FULL_BODY_JOINTS, HAND_JOINTS},
    wire::{Motion, Nanos, Pose},
};
use std::time::Duration;

/// Two poses of a wrist further apart in time than this give no velocity.
const MAX_VELOCITY_GAP: Duration = Duration::from_millis(50);

/// What Cranpose reports about the headset's tracking, reduced to what the offer needs. The type
/// exists so that a test can state a headset: Cranpose builds its own `TrackingCapabilities` only
/// from a running runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// For each feature: a tracker runs and the frame's values are current
    pub active: [bool; 4],
    /// The runtime also tracks the legs and the feet
    pub full_body: bool,
    /// The face comes through `XR_FB_face_tracking2`, which has the 70 weights that the host takes
    pub meta_face: bool,
    /// The gaze is one pose, through `XR_EXT_eye_gaze_interaction`
    pub eye_gaze: bool,
}

impl Capabilities {
    pub fn of(tracking: &TrackingCapabilities) -> Self {
        Self {
            active: TrackingFeature::ALL.map(|feature| tracking.availability(feature) == TrackingAvailability::Active),
            full_body: tracking.is_full_body_available(),
            meta_face: tracking.face_tracking_provider() == Some(cranpose_xr::FaceTrackingProvider::Meta),
            eye_gaze: tracking.is_eye_gaze_interaction_available(),
        }
    }

    fn running(&self, feature: TrackingFeature) -> bool {
        self.active[TrackingFeature::ALL.iter().position(|f| *f == feature).unwrap_or(0)]
    }

    /// The features that the headset can send to the host now. The user's permissions decide: a feature
    /// that waits for a permission or lacks one is not offered. A face that comes from Android XR's own
    /// extension has the 68 weights of Jetpack XR, which the host's runtime has no use for, so it is not
    /// offered either.
    pub fn features(&self) -> Features {
        let mut offered = Features::NONE;
        if self.running(TrackingFeature::Hands) {
            offered = offered.union(Features::HANDS);
        }
        if self.running(TrackingFeature::Body) {
            offered = offered.union(Features::BODY);
            if self.full_body {
                offered = offered.union(Features::FULL_BODY);
            }
        }
        if self.running(TrackingFeature::Face) && self.meta_face {
            offered = offered.union(Features::FACE);
        }
        if self.running(TrackingFeature::Eyes) && self.eye_gaze {
            offered = offered.union(Features::EYES);
        }
        offered
    }
}

/// The features that the headset offers, from the capabilities of a frame.
pub fn offer(tracking: &TrackingCapabilities) -> Features {
    Capabilities::of(tracking).features()
}

/// What the headset knows of a joint: valid and tracked, in the bits of the wire.
fn flags(valid: bool, tracked: bool) -> u8 {
    let mut bits = 0;
    if valid {
        bits |= joint::POSITION_VALID | joint::ORIENTATION_VALID;
    }
    if tracked {
        bits |= joint::POSITION_TRACKED | joint::ORIENTATION_TRACKED;
    }
    bits
}

fn proto_pose(pose: &XrPose) -> Pose {
    Pose { orientation: pose.orientation, position: pose.position }
}

const REST: Joint = Joint { pose: Pose::IDENTITY, flags: 0 };

/// One hand of a frame.
#[derive(Clone, Debug, PartialEq)]
pub struct HandSample {
    /// The cameras track the hand and its wrist has a pose
    pub tracked: bool,
    /// The wrist's pose with the velocities that the reader estimated
    pub root: Motion,
    pub joints: [Joint; HAND_JOINTS],
}

/// The body of a frame.
#[derive(Clone, Debug, PartialEq)]
pub struct BodySample {
    pub active: bool,
    /// The set that the sender sends: 84 joints only when the host wished the full body
    pub set: BodySet,
    pub confidence: f32,
    pub root: Motion,
    pub joints: [Joint; FULL_BODY_JOINTS],
}

/// The face of a frame, with the weights of `XR_FB_face_tracking2`.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceSample {
    pub valid: bool,
    /// Bits of `cranvr_proto::tracking::face`
    pub flags: u8,
    /// The lower face, then the upper face
    pub confidence: [f32; FACE_CONFIDENCES],
    pub weights: [f32; FACE_WEIGHTS],
}

/// What the sender takes from one frame. It has a fixed size, and the reader and the sender trade
/// two boxes of it, so no frame allocates.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackedFrame {
    /// The sample and display time of the frame's `Tracking` message
    pub sample_time: Nanos,
    pub display_time: Nanos,
    pub hands: [HandSample; 2],
    pub body: BodySample,
    pub face: FaceSample,
    pub gaze: Option<Pose>,
}

impl TrackedFrame {
    pub fn new() -> Self {
        let hand = || HandSample { tracked: false, root: Motion::default(), joints: [REST; HAND_JOINTS] };
        Self {
            sample_time: 0,
            display_time: 0,
            hands: [hand(), hand()],
            body: BodySample { active: false, set: BodySet::Default, confidence: 0.0, root: Motion::default(), joints: [REST; FULL_BODY_JOINTS] },
            face: FaceSample { valid: false, flags: 0, confidence: [0.0; FACE_CONFIDENCES], weights: [0.0; FACE_WEIGHTS] },
            gaze: None,
        }
    }
}

impl Default for TrackedFrame {
    fn default() -> Self {
        Self::new()
    }
}

/// Copies the wished features of the headset's frames and estimates the velocity of the wrists and of
/// the body's root. Each estimate needs the pose of the frame before, as the head's does.
pub struct TrackedReader {
    last_wrist: [Option<(Duration, XrPose)>; 2],
    last_root: Option<(Duration, XrPose)>,
    /// The buffer that the next frame fills. A push to the feed trades it for the sender's.
    scratch: Box<TrackedFrame>,
}

impl Default for TrackedReader {
    fn default() -> Self {
        Self { last_wrist: [None; 2], last_root: None, scratch: Box::new(TrackedFrame::new()) }
    }
}

/// The motion of a pose that came after `last`, when the two lie close enough in time.
fn motion_after(last: &mut Option<(Duration, XrPose)>, pose: &XrPose, time: Duration) -> Motion {
    let (linear_velocity, angular_velocity) = match last.replace((time, *pose)) {
        Some((before, earlier)) if time > before && time - before <= MAX_VELOCITY_GAP => {
            velocities(&proto_pose(&earlier), &proto_pose(pose), (time - before).as_secs_f32())
        }
        _ => ([0.0; 3], [0.0; 3]),
    };
    Motion { pose: proto_pose(pose), linear_velocity, angular_velocity }
}

fn finite(pose: &XrPose) -> bool {
    pose.position.iter().chain(&pose.orientation).all(|c| c.is_finite())
}

impl TrackedReader {
    /// Fills the buffer with the wished features of `xr` and returns it. `sample_time` and
    /// `display_time` are those of the frame's `Tracking` message. A feature that the host did not
    /// wish stays untracked, so the sender sends nothing for it and the reader reads nothing.
    pub fn read(&mut self, xr: &XrFrame, sample_time: Nanos, display_time: Nanos, wish: Features) -> &mut Box<TrackedFrame> {
        let time = xr.predicted_display_time;
        let frame = &mut *self.scratch;
        frame.sample_time = sample_time;
        frame.display_time = display_time;
        for (side, hand) in [cranpose_xr::Hand::Left, cranpose_xr::Hand::Right].into_iter().enumerate() {
            let wished = wish.contains(Features::HANDS);
            let tracked = wished && fill_hand(&mut frame.hands[side], xr.hand(hand), &mut self.last_wrist[side], time);
            frame.hands[side].tracked = tracked;
            if !tracked {
                self.last_wrist[side] = None;
            }
        }
        let body_wished = wish.contains(Features::BODY) || wish.contains(Features::FULL_BODY);
        frame.body.active = body_wished && fill_body(&mut frame.body, &xr.body, wish.contains(Features::FULL_BODY), &mut self.last_root, time);
        if !frame.body.active {
            self.last_root = None;
        }
        frame.face.valid = wish.contains(Features::FACE) && fill_face(&mut frame.face, &xr.meta_face);
        frame.gaze = if wish.contains(Features::EYES) { gaze_of(&xr.eyes) } else { None };
        &mut self.scratch
    }
}

fn fill_hand(out: &mut HandSample, state: &HandState, last: &mut Option<(Duration, XrPose)>, time: Duration) -> bool {
    let wrist = state.joint(HandJointType::Wrist);
    if state.tracking_state != TrackingState::Tracking || !wrist.valid || !finite(&wrist.pose) {
        return false;
    }
    out.root = motion_after(last, &wrist.pose, time);
    for (target, source) in out.joints.iter_mut().zip(&state.joints) {
        *target = Joint { pose: proto_pose(&source.pose), flags: flags(source.valid, source.tracked) };
    }
    true
}

fn fill_body(out: &mut BodySample, state: &BodyState, full: bool, last: &mut Option<(Duration, XrPose)>, time: Duration) -> bool {
    let (default, full_body) = (BodySet::Default.joints(), BodySet::Full.joints());
    if !state.active || ![default, full_body].contains(&state.joint_count) {
        return false;
    }
    let root = &state.joints[BodyJointType::Root.index()];
    if !root.valid || !finite(&root.pose) {
        return false;
    }
    // The first 70 joints of the full body are the default set. The 84 joints go only to a host that
    // wished them.
    out.set = if full && state.joint_count == full_body { BodySet::Full } else { BodySet::Default };
    out.confidence = state.confidence;
    out.root = motion_after(last, &root.pose, time);
    for (target, source) in out.joints.iter_mut().zip(&state.joints).take(out.set.joints()) {
        *target = Joint { pose: proto_pose(&source.pose), flags: flags(source.valid, source.tracked) };
    }
    true
}

fn fill_face(out: &mut FaceSample, state: &MetaFaceState) -> bool {
    if !state.is_valid {
        return false;
    }
    out.flags = face::VALID
        | if state.is_eye_following_blendshapes_valid { face::EYES_FOLLOWING_VALID } else { 0 }
        | if state.data_source == Some(FaceDataSource::Audio) { face::FROM_AUDIO } else { 0 };
    out.confidence = [state.lower_face_confidence, state.upper_face_confidence];
    out.weights = state.weights;
    true
}

fn gaze_of(eyes: &Eyes) -> Option<Pose> {
    eyes.gaze.filter(finite).as_ref().map(proto_pose)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranpose_xr::{BodyJoint, HandJoint};
    use std::f32::consts::FRAC_1_SQRT_2;

    fn tracked_hand(wrist_at: [f32; 3]) -> HandState {
        let mut state = HandState { tracking_state: TrackingState::Tracking, ..HandState::default() };
        for (i, joint) in state.joints.iter_mut().enumerate() {
            // Every joint at its own place, so a swap of two joints shows
            *joint = HandJoint {
                pose: XrPose { position: [wrist_at[0] + 0.01 * i as f32, wrist_at[1], wrist_at[2] - 0.002 * i as f32], orientation: [0.0, 0.0, 0.0, 1.0] },
                radius: 0.01,
                valid: i != 3,
                tracked: i % 2 == 0,
            };
        }
        state.joints[HandJointType::Wrist.index()].pose.position = wrist_at;
        state
    }

    fn at(ms: u64) -> XrFrame {
        let mut xr = XrFrame::default();
        xr.predicted_display_time = Duration::from_millis(ms);
        xr
    }

    fn all() -> Features {
        Features::ALL
    }

    #[test]
    fn the_offer_follows_what_runs_and_what_the_host_can_use() {
        let headset = Capabilities { active: [true, true, true, false], full_body: true, meta_face: true, eye_gaze: false };
        // A Quest 3 as the coordinator's desk check found it: no eyes
        assert_eq!(headset.features(), Features::HANDS.union(Features::BODY).union(Features::FULL_BODY).union(Features::FACE));
        // Nothing runs yet: the user has not answered
        assert_eq!(Capabilities { full_body: true, meta_face: true, eye_gaze: true, ..Capabilities::default() }.features(), Features::NONE);
        // The legs only count with a running body tracker
        assert_eq!(Capabilities { active: [false, false, false, false], full_body: true, ..Capabilities::default() }.features(), Features::NONE);
        let upper = Capabilities { active: [false, true, false, false], full_body: false, ..Capabilities::default() };
        assert_eq!(upper.features(), Features::BODY);
        // A face from Android XR's extension has the 68 weights of Jetpack XR: no offer
        let android = Capabilities { active: [false, false, true, false], meta_face: false, ..Capabilities::default() };
        assert_eq!(android.features(), Features::NONE);
        // The gaze needs a running tracker and the combined gaze pose
        let eyes = Capabilities { active: [false, false, false, true], eye_gaze: true, ..Capabilities::default() };
        assert_eq!(eyes.features(), Features::EYES);
        let social = Capabilities { active: [false, false, false, true], eye_gaze: false, ..Capabilities::default() };
        assert_eq!(social.features(), Features::NONE);
    }

    #[test]
    fn a_frame_with_no_tracking_offers_nothing() {
        assert_eq!(offer(&TrackingCapabilities::default()), Features::NONE);
        assert_eq!(Capabilities::of(&TrackingCapabilities::default()), Capabilities::default());
    }

    #[test]
    fn a_wished_hand_gives_its_wrist_its_joints_in_order_and_their_flags() {
        let mut xr = at(100);
        xr.left_hand = tracked_hand([-0.2, 1.2, -0.4]);
        let mut reader = TrackedReader::default();
        let frame = reader.read(&xr, 5, 9, Features::HANDS);
        assert_eq!((frame.sample_time, frame.display_time), (5, 9));
        let left = &frame.hands[0];
        assert!(left.tracked && !frame.hands[1].tracked);
        assert_eq!(left.root.pose.position, [-0.2, 1.2, -0.4]);
        for (i, joint) in left.joints.iter().enumerate() {
            let want = if i == HandJointType::Wrist.index() { -0.2 } else { -0.2 + 0.01 * i as f32 };
            assert!((joint.pose.position[0] - want).abs() < 1e-6, "joint {i}");
            let valid = if i != 3 { joint::POSITION_VALID | joint::ORIENTATION_VALID } else { 0 };
            let tracked = if i % 2 == 0 { joint::POSITION_TRACKED | joint::ORIENTATION_TRACKED } else { 0 };
            assert_eq!(joint.flags, valid | tracked, "joint {i}");
        }
    }

    #[test]
    fn a_feature_that_the_host_did_not_wish_is_not_read() {
        let mut xr = at(100);
        xr.left_hand = tracked_hand([0.0, 1.0, 0.0]);
        xr.meta_face = MetaFaceState { is_valid: true, ..MetaFaceState::default() };
        xr.eyes.gaze = Some(XrPose::at([0.0, 1.6, 0.0]));
        let mut reader = TrackedReader::default();
        let frame = reader.read(&xr, 1, 2, Features::NONE);
        assert!(!frame.hands[0].tracked && !frame.body.active && !frame.face.valid && frame.gaze.is_none());
        // Each feature on its own
        assert!(reader.read(&xr, 1, 2, Features::HANDS).hands[0].tracked);
        assert!(reader.read(&xr, 1, 2, Features::FACE).face.valid);
        assert_eq!(reader.read(&xr, 1, 2, Features::EYES).gaze, Some(Pose { position: [0.0, 1.6, 0.0], ..Pose::IDENTITY }));
        assert!(!reader.read(&xr, 1, 2, Features::FACE).hands[0].tracked);
    }

    #[test]
    fn a_hand_that_is_not_tracked_or_has_no_wrist_pose_sends_nothing() {
        let mut xr = at(100);
        xr.left_hand = HandState { tracking_state: TrackingState::Paused, ..tracked_hand([0.0, 1.0, 0.0]) };
        let mut invalid_wrist = tracked_hand([0.0, 1.0, 0.0]);
        invalid_wrist.joints[HandJointType::Wrist.index()].valid = false;
        xr.right_hand = invalid_wrist;
        let frame = TrackedReader::default().read(&xr, 1, 2, all()).clone();
        assert!(!frame.hands[0].tracked && !frame.hands[1].tracked);
        // A wrist that is not a number is no pose
        let mut nan = tracked_hand([0.0, 1.0, 0.0]);
        nan.joints[HandJointType::Wrist.index()].pose.position[0] = f32::NAN;
        xr.right_hand = nan;
        assert!(!TrackedReader::default().read(&xr, 1, 2, all()).hands[1].tracked);
    }

    #[test]
    fn the_wrists_velocity_follows_two_frames_and_restarts_after_a_gap_or_a_lost_hand() {
        let mut reader = TrackedReader::default();
        let mut xr = at(100);
        xr.left_hand = tracked_hand([0.0, 1.0, -0.4]);
        assert_eq!(reader.read(&xr, 1, 2, Features::HANDS).hands[0].root.linear_velocity, [0.0; 3]);
        // 1 cm in 10 ms: 1 m/s along x
        xr = at(110);
        xr.left_hand = tracked_hand([0.01, 1.0, -0.4]);
        let root = reader.read(&xr, 3, 4, Features::HANDS).hands[0].root;
        assert!((root.linear_velocity[0] - 1.0).abs() < 1e-3 && root.linear_velocity[1].abs() < 1e-3 && root.angular_velocity == [0.0; 3], "{root:?}");
        // The right hand is not tracked: no velocity either
        assert_eq!(reader.read(&xr, 3, 4, Features::HANDS).hands[1].root.linear_velocity, [0.0; 3]);
        // A gap of 200 ms gives none
        xr = at(310);
        xr.left_hand = tracked_hand([0.5, 1.0, -0.4]);
        assert_eq!(reader.read(&xr, 5, 6, Features::HANDS).hands[0].root.linear_velocity, [0.0; 3]);
        // A hand that went out of view and came back starts over, though the frames lie close
        xr = at(320);
        xr.left_hand = HandState::default();
        reader.read(&xr, 7, 8, Features::HANDS);
        xr = at(330);
        xr.left_hand = tracked_hand([0.6, 1.0, -0.4]);
        assert_eq!(reader.read(&xr, 9, 10, Features::HANDS).hands[0].root.linear_velocity, [0.0; 3]);
        // A turn of the wrist gives an angular velocity: 0.1 rad about y in 10 ms is 10 rad/s
        let turned = |angle: f32| {
            let mut hand = tracked_hand([0.6, 1.0, -0.4]);
            hand.joints[HandJointType::Wrist.index()].pose.orientation = [0.0, (angle / 2.0).sin(), 0.0, (angle / 2.0).cos()];
            hand
        };
        xr = at(340);
        xr.left_hand = turned(0.0);
        reader.read(&xr, 11, 12, Features::HANDS);
        xr = at(350);
        xr.left_hand = turned(0.1);
        let root = reader.read(&xr, 13, 14, Features::HANDS).hands[0].root;
        assert!((root.angular_velocity[1] - 10.0).abs() < 0.05, "{:?}", root.angular_velocity);
    }

    fn body_of(count: usize) -> BodyState {
        let mut state = BodyState { active: true, confidence: 0.8, joint_count: count, ..BodyState::default() };
        for (i, joint) in state.joints.iter_mut().enumerate().take(count) {
            *joint = BodyJoint { pose: XrPose::at([0.0, 0.02 * i as f32, 0.0]), valid: true, tracked: true };
        }
        state
    }

    #[test]
    fn the_body_goes_as_the_set_that_the_host_wished() {
        let mut xr = at(100);
        xr.body = body_of(84);
        let mut reader = TrackedReader::default();
        // The default body is the first 70 joints of the full one
        let frame = reader.read(&xr, 1, 2, Features::BODY);
        assert!(frame.body.active);
        assert_eq!((frame.body.set, frame.body.confidence), (BodySet::Default, 0.8));
        assert_eq!(frame.body.joints[69].pose.position, [0.0, 0.02 * 69.0, 0.0]);
        // The full set goes to a host that wished it, and to one that wished both
        assert_eq!(reader.read(&xr, 1, 2, Features::FULL_BODY).body.set, BodySet::Full);
        assert_eq!(reader.read(&xr, 1, 2, Features::BODY.union(Features::FULL_BODY)).body.set, BodySet::Full);
        assert_eq!(reader.read(&xr, 1, 2, Features::FULL_BODY).body.joints[83].pose.position, [0.0, 0.02 * 83.0, 0.0]);
        // A headset with 70 joints cannot serve the full set: the default one goes
        xr.body = body_of(70);
        assert_eq!(reader.read(&xr, 1, 2, Features::FULL_BODY).body.set, BodySet::Default);
        // An inactive body, a joint count that no set has and a root without a pose send nothing
        xr.body.active = false;
        assert!(!reader.read(&xr, 1, 2, all()).body.active);
        xr.body = body_of(70);
        xr.body.joint_count = 50;
        assert!(!reader.read(&xr, 1, 2, all()).body.active);
        xr.body = body_of(70);
        xr.body.joints[0].valid = false;
        assert!(!reader.read(&xr, 1, 2, all()).body.active);
    }

    #[test]
    fn the_body_roots_velocity_follows_two_frames() {
        let mut reader = TrackedReader::default();
        let mut xr = at(100);
        xr.body = body_of(70);
        reader.read(&xr, 1, 2, Features::BODY);
        xr = at(120);
        xr.body = body_of(70);
        xr.body.joints[0].pose.position = [0.0, 0.0, -0.04];
        let root = reader.read(&xr, 3, 4, Features::BODY).body.root;
        assert!((root.linear_velocity[2] + 2.0).abs() < 1e-3, "{root:?}");
    }

    #[test]
    fn the_face_keeps_its_weights_confidences_and_source() {
        let mut xr = at(100);
        let mut weights = [0.0; FACE_WEIGHTS];
        weights[12] = 0.75;
        xr.meta_face = MetaFaceState {
            is_valid: true,
            is_eye_following_blendshapes_valid: true,
            data_source: Some(FaceDataSource::Audio),
            weights,
            lower_face_confidence: 0.25,
            upper_face_confidence: 0.5,
        };
        let mut reader = TrackedReader::default();
        let frame = reader.read(&xr, 1, 2, Features::FACE);
        assert!(frame.face.valid);
        // A Quest 3 gives the audio source without a face in view
        assert_eq!(frame.face.flags, face::VALID | face::EYES_FOLLOWING_VALID | face::FROM_AUDIO);
        assert_eq!((frame.face.confidence, frame.face.weights[12]), ([0.25, 0.5], 0.75));
        xr.meta_face.data_source = Some(FaceDataSource::Visual);
        xr.meta_face.is_eye_following_blendshapes_valid = false;
        assert_eq!(reader.read(&xr, 1, 2, Features::FACE).face.flags, face::VALID);
        xr.meta_face.is_valid = false;
        assert!(!reader.read(&xr, 1, 2, Features::FACE).face.valid);
    }

    #[test]
    fn the_gaze_is_the_combined_pose_when_the_runtime_has_one() {
        let mut xr = at(100);
        assert_eq!(TrackedReader::default().read(&xr, 1, 2, Features::EYES).gaze, None);
        xr.eyes.gaze = Some(XrPose { position: [0.0, 1.6, 0.0], orientation: [0.0, FRAC_1_SQRT_2, 0.0, FRAC_1_SQRT_2] });
        assert_eq!(TrackedReader::default().read(&xr, 1, 2, Features::EYES).gaze.unwrap().orientation, [0.0, FRAC_1_SQRT_2, 0.0, FRAC_1_SQRT_2]);
        xr.eyes.gaze = Some(XrPose::at([f32::NAN, 0.0, 0.0]));
        assert_eq!(TrackedReader::default().read(&xr, 1, 2, Features::EYES).gaze, None);
    }
}
