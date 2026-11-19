//! The user's hands, body, face and eyes, from the headset's frames to the host's games (see
//! `cranvr_proto::tracking` for the messages and `tracking.rs` for the sender).
//!
//! The build script asks Cranpose for the four kinds of tracking (`build.rs`). Cranpose then reads
//! them in each frame whatever the host wants, because an OpenXR instance fixes its extensions
//! before the app runs. This file keeps the cost of the host's wish down:
//! - [`TrackedReader::offer`] turns the frame's capabilities into the features that the headset offers,
//!   with the source of the face (cameras or microphone), which a game of the host's runtime reads;
//! - [`TrackedReader`] copies only the wished features out of the frame into a buffer that it
//!   allocated once, and estimates the velocity of the wrists and of the body's root from successive
//!   frames, because Cranpose gives no velocities for joints;
//! - the sender takes the buffer from the [`crate::tracking::Feed`] and sends one datagram for each
//!   wished feature. Nothing allocates in a frame.

use cranpose_xr::{
    BodyJointType, BodyState, Eyes, FaceBlendShapeType, FaceConfidenceRegion, FaceDataSource, FaceExpression, FaceState, FaceTrackingProvider,
    HandJointType, HandState, MetaFaceState, Pose as XrPose, TrackingAvailability, TrackingCapabilities, TrackingFeature, TrackingState, XrFrame,
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
    /// The extension that gives the face: Meta's `XR_FB_face_tracking2` with the 70 weights that the
    /// host takes, or Android XR's own with the 68 weights of Jetpack XR, which the reader maps to
    /// Meta's
    pub face: Option<FaceTrackingProvider>,
    /// The gaze is one pose, through `XR_EXT_eye_gaze_interaction`
    pub eye_gaze: bool,
}

impl Capabilities {
    pub fn of(tracking: &TrackingCapabilities) -> Self {
        Self {
            active: TrackingFeature::ALL.map(|feature| tracking.availability(feature) == TrackingAvailability::Active),
            full_body: tracking.is_full_body_available(),
            face: tracking.face_tracking_provider(),
            eye_gaze: tracking.is_eye_gaze_interaction_available(),
        }
    }

    fn running(&self, feature: TrackingFeature) -> bool {
        self.active[TrackingFeature::ALL.iter().position(|f| *f == feature).unwrap_or(0)]
    }

    /// The features that the headset can send to the host now. The user's permissions decide: a feature
    /// that waits for a permission or lacks one is not offered. The face comes with its source:
    /// - Android XR's extension reads the face with the headset's cameras: visual.
    /// - Meta's extension picks the source itself, and a Quest 3 has no camera for the mouth, so it
    ///   gives the microphone's. `face_seen` holds the sources that its samples came from (see
    ///   [`TrackedReader::offer`]). The face of an unknown source is offered with no source, until a
    ///   sample names one.
    pub fn features(&self, face_seen: Features) -> Features {
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
        if self.running(TrackingFeature::Face) {
            match self.face {
                Some(FaceTrackingProvider::Android) => offered = offered.union(Features::FACE).union(Features::FACE_VISUAL),
                Some(_) => offered = offered.union(Features::FACE).union(face_seen.intersect(Features::FACE_SOURCES)),
                None => {}
            }
        }
        if self.running(TrackingFeature::Eyes) && self.eye_gaze {
            offered = offered.union(Features::EYES);
        }
        offered
    }
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
    /// The sources (`Features::FACE_VISUAL` and `FACE_AUDIO`) that a valid face came from since the
    /// face tracker started
    face_seen: Features,
}

impl Default for TrackedReader {
    fn default() -> Self {
        Self { last_wrist: [None; 2], last_root: None, scratch: Box::new(TrackedFrame::new()), face_seen: Features::NONE }
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
    /// The features that the headset offers, from the capabilities of a frame. The face's source comes
    /// from the samples, because Cranpose names it only per frame. A sample that is not valid says
    /// nothing: the runtime fills in the source of a face that it does not have. A source that a valid
    /// sample came from stays in the offer until the face tracker stops, so that a headset which
    /// switches between its sources does not send a new offer at each switch.
    pub fn offer(&mut self, xr: &XrFrame) -> Features {
        self.offer_with(&Capabilities::of(&xr.tracking), xr)
    }

    fn offer_with(&mut self, capabilities: &Capabilities, xr: &XrFrame) -> Features {
        if !capabilities.running(TrackingFeature::Face) {
            self.face_seen = Features::NONE;
        } else if let Some(source) = face_source(capabilities.face, xr) {
            self.face_seen = self.face_seen.union(source);
        }
        capabilities.features(self.face_seen)
    }

    /// Fills the buffer with the wished features of `xr` and returns it. `sample_time` and
    /// `display_time` are those of the frame's `Tracking` message. A feature that the host did not
    /// wish stays untracked, so the sender sends nothing for it and the reader reads nothing.
    pub fn read(&mut self, xr: &XrFrame, sample_time: Nanos, display_time: Nanos, wish: Features) -> &mut Box<TrackedFrame> {
        self.read_face_of(xr, sample_time, display_time, wish, xr.tracking.face_tracking_provider())
    }

    /// `read` for a face of the extension `face`. A test names it, because Cranpose builds its own
    /// capabilities only from a running runtime.
    pub(crate) fn read_face_of(&mut self, xr: &XrFrame, sample_time: Nanos, display_time: Nanos, wish: Features, face: Option<FaceTrackingProvider>) -> &mut Box<TrackedFrame> {
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
        frame.face.valid = wish.contains(Features::FACE)
            && match face {
                Some(FaceTrackingProvider::Android) => fill_android_face(&mut frame.face, &xr.face),
                _ => fill_meta_face(&mut frame.face, &xr.meta_face),
            };
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

/// The source that the frame's valid face came from, as a bit of the offer. `None` while the face is
/// not valid.
fn face_source(provider: Option<FaceTrackingProvider>, xr: &XrFrame) -> Option<Features> {
    match provider {
        // Android XR reads the face with the headset's cameras
        Some(FaceTrackingProvider::Android) => xr.face.is_valid.then_some(Features::FACE_VISUAL),
        Some(_) if xr.meta_face.is_valid => match xr.meta_face.data_source {
            Some(FaceDataSource::Audio) => Some(Features::FACE_AUDIO),
            Some(FaceDataSource::Visual) => Some(Features::FACE_VISUAL),
            _ => None,
        },
        _ => None,
    }
}

fn fill_meta_face(out: &mut FaceSample, state: &MetaFaceState) -> bool {
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

/// For each blend shape of Jetpack XR (by its index), the slot of Meta's weight with the same meaning.
/// It is `None` for the tongue shapes that only Android XR has.
const META_SLOT: [Option<usize>; FaceBlendShapeType::COUNT] = {
    let mut table = [None; FaceBlendShapeType::COUNT];
    let mut i = 0;
    while i < FaceBlendShapeType::COUNT {
        table[i] = match FaceBlendShapeType::ALL[i].expression() {
            Some(expression) => Some(expression.index()),
            None => None,
        };
        i += 1;
    }
    table
};

// The wire's weights are Meta's, in the order of `FaceExpression`
const _: () = assert!(FaceExpression::COUNT == FACE_WEIGHTS);

/// The face that Android XR gave, in the weights of Meta that the host takes. A weight that Android
/// XR does not give stays 0. The Jetpack XR names map to Meta's by `FaceBlendShapeType::expression`.
/// Android XR reports no separate validity for the eyes' look weights: they come with the rest, so
/// they follow `is_valid`. Meta has one confidence for the upper face: the mean of the two upper
/// regions.
fn fill_android_face(out: &mut FaceSample, state: &FaceState) -> bool {
    if !state.is_valid {
        return false;
    }
    out.flags = face::VALID | face::EYES_FOLLOWING_VALID;
    out.weights = [0.0; FACE_WEIGHTS];
    for (slot, weight) in META_SLOT.iter().zip(&state.blend_shapes) {
        if let Some(slot) = slot {
            out.weights[*slot] = *weight;
        }
    }
    let region = |r: FaceConfidenceRegion| state.confidence_regions[r.index()];
    out.confidence = [region(FaceConfidenceRegion::Lower), (region(FaceConfidenceRegion::LeftUpper) + region(FaceConfidenceRegion::RightUpper)) / 2.0];
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
        let meta = Some(FaceTrackingProvider::Meta);
        let headset = Capabilities { active: [true, true, true, false], full_body: true, face: meta, eye_gaze: false };
        // A Quest 3 as the coordinator's desk check found it: no eyes. Its face has no source until
        // a sample names one.
        let no_source = Features::NONE;
        assert_eq!(headset.features(no_source), Features::HANDS.union(Features::BODY).union(Features::FULL_BODY).union(Features::FACE));
        // Nothing runs yet: the user has not answered
        assert_eq!(Capabilities { full_body: true, face: meta, eye_gaze: true, ..Capabilities::default() }.features(no_source), Features::NONE);
        // The legs only count with a running body tracker
        assert_eq!(Capabilities { active: [false, false, false, false], full_body: true, ..Capabilities::default() }.features(no_source), Features::NONE);
        let upper = Capabilities { active: [false, true, false, false], full_body: false, ..Capabilities::default() };
        assert_eq!(upper.features(no_source), Features::BODY);
        // The gaze needs a running tracker and the combined gaze pose
        let eyes = Capabilities { active: [false, false, false, true], eye_gaze: true, ..Capabilities::default() };
        assert_eq!(eyes.features(no_source), Features::EYES);
        let social = Capabilities { active: [false, false, false, true], eye_gaze: false, ..Capabilities::default() };
        assert_eq!(social.features(no_source), Features::NONE);
    }

    #[test]
    fn the_face_is_offered_with_the_source_that_it_comes_from() {
        let face = |provider| Capabilities { active: [false, false, true, false], face: provider, ..Capabilities::default() };
        let meta = face(Some(FaceTrackingProvider::Meta));
        // Meta's extension picks the source: the offer names what valid samples came from
        let audio = Features::FACE.union(Features::FACE_AUDIO);
        assert_eq!(meta.features(Features::FACE_AUDIO), audio);
        assert_eq!(meta.features(Features::FACE_VISUAL), Features::FACE.union(Features::FACE_VISUAL));
        assert_eq!(meta.features(Features::FACE_SOURCES), Features::FACE.union(Features::FACE_SOURCES));
        // Bits that are no source stay out of the offer
        assert_eq!(meta.features(Features::HANDS.union(Features::FACE_AUDIO)), audio);
        // Android XR's own extension reads the face with cameras. Its 68 weights go to the host as
        // Meta's 70, so the face is offered.
        let android = face(Some(FaceTrackingProvider::Android));
        assert_eq!(android.features(Features::NONE), Features::FACE.union(Features::FACE_VISUAL));
        // No provider, no face, and a face that does not run is not offered
        assert_eq!(face(None).features(Features::FACE_AUDIO), Features::NONE);
        assert_eq!(Capabilities { face: Some(FaceTrackingProvider::Android), ..Capabilities::default() }.features(Features::NONE), Features::NONE);
    }

    fn meta_face_frame(source: Option<FaceDataSource>, valid: bool) -> XrFrame {
        let mut xr = at(100);
        xr.meta_face = MetaFaceState { is_valid: valid, data_source: source, ..MetaFaceState::default() };
        xr
    }

    #[test]
    fn the_offer_learns_the_face_source_from_valid_samples_and_keeps_it_while_the_tracker_runs() {
        let running = Capabilities { active: [false, false, true, false], face: Some(FaceTrackingProvider::Meta), ..Capabilities::default() };
        let mut reader = TrackedReader::default();
        // A sample that is not valid says nothing, whatever source it names: the runtime fills it in
        let invalid = meta_face_frame(Some(FaceDataSource::Visual), false);
        assert_eq!(reader.offer_with(&running, &invalid), Features::FACE);
        // A Quest 3: the first valid sample names the microphone
        let audio = meta_face_frame(Some(FaceDataSource::Audio), true);
        let quest_3 = Features::FACE.union(Features::FACE_AUDIO);
        assert_eq!(reader.offer_with(&running, &audio), quest_3);
        // The offer stays while the samples are not valid
        assert_eq!(reader.offer_with(&running, &invalid), quest_3);
        // A headset that switches sources offers both from then on and does not flap
        let both = Features::FACE.union(Features::FACE_SOURCES);
        assert_eq!(reader.offer_with(&running, &meta_face_frame(Some(FaceDataSource::Visual), true)), both);
        assert_eq!(reader.offer_with(&running, &audio), both);
        // A valid sample with no source says nothing
        let mut fresh = TrackedReader::default();
        assert_eq!(fresh.offer_with(&running, &meta_face_frame(None, true)), Features::FACE);
        // The tracker stops, as when the user takes the permission away: the sources go. It starts again with none.
        let stopped = Capabilities { active: [false; 4], ..running };
        assert_eq!(reader.offer_with(&stopped, &audio), Features::NONE);
        assert_eq!(reader.offer_with(&running, &invalid), Features::FACE);
        // Android XR: cameras, with no wait for a sample
        let android = Capabilities { face: Some(FaceTrackingProvider::Android), ..running };
        assert_eq!(TrackedReader::default().offer_with(&android, &at(100)), Features::FACE.union(Features::FACE_VISUAL));
    }

    #[test]
    fn a_frame_with_no_tracking_capabilities_has_no_face_source() {
        let mut reader = TrackedReader::default();
        assert_eq!(reader.offer(&meta_face_frame(Some(FaceDataSource::Audio), true)), Features::NONE);
    }

    #[test]
    fn a_frame_with_no_tracking_offers_nothing() {
        assert_eq!(TrackedReader::default().offer(&at(0)), Features::NONE);
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

    /// A face of Android XR in which each shape has its own weight, so a swap of two shows
    fn android_face() -> FaceState {
        let mut state = FaceState { tracking_state: TrackingState::Tracking, is_valid: true, ..FaceState::default() };
        for shape in FaceBlendShapeType::ALL {
            state.blend_shapes[shape.index()] = (shape.index() + 1) as f32 / 100.0;
        }
        state.confidence_regions[FaceConfidenceRegion::Lower.index()] = 0.5;
        state.confidence_regions[FaceConfidenceRegion::LeftUpper.index()] = 0.25;
        state.confidence_regions[FaceConfidenceRegion::RightUpper.index()] = 0.75;
        state
    }

    #[test]
    fn android_xrs_68_weights_land_on_the_70_of_meta_and_the_unmapped_stay_zero() {
        let mut xr = at(100);
        xr.face = android_face();
        let android = Some(FaceTrackingProvider::Android);
        let mut reader = TrackedReader::default();
        let frame = reader.read_face_of(&xr, 1, 2, Features::FACE, android);
        assert!(frame.face.valid);
        assert_eq!(frame.face.flags, face::VALID | face::EYES_FOLLOWING_VALID, "the cameras give it: no audio flag");
        // Every shape that has a Meta name lands on that name's slot
        let mut covered = [false; FACE_WEIGHTS];
        let mut mapped = 0;
        for shape in FaceBlendShapeType::ALL {
            match shape.expression() {
                Some(expression) => {
                    mapped += 1;
                    covered[expression.index()] = true;
                    assert_eq!(frame.face.weights[expression.index()], (shape.index() + 1) as f32 / 100.0, "{shape:?}");
                }
                None => assert!(matches!(shape, FaceBlendShapeType::TongueLeft | FaceBlendShapeType::TongueRight | FaceBlendShapeType::TongueUp | FaceBlendShapeType::TongueDown), "{shape:?}"),
            }
        }
        assert_eq!(mapped, 64, "63 shapes by index and the tongue's out");
        // The slots that no shape fills are the six tongue shapes of Meta. They stay 0, so the 70 weights are filled by 64.
        let unmapped: Vec<usize> = (0..FACE_WEIGHTS).filter(|i| !covered[*i]).collect();
        assert_eq!(unmapped.len(), 6);
        assert!(unmapped.iter().all(|i| frame.face.weights[*i] == 0.0), "{unmapped:?}");
        // The tongue's out has different places on the two sides
        assert_eq!(frame.face.weights[FaceExpression::TongueOut.index()], (FaceBlendShapeType::TongueOut.index() + 1) as f32 / 100.0);
        assert_eq!(frame.face.weights[FaceExpression::JawDrop.index()], (FaceBlendShapeType::JawDrop.index() + 1) as f32 / 100.0);
        // The lower region goes as it is, the two upper ones as their mean
        assert_eq!(frame.face.confidence, [0.5, 0.5]);
    }

    #[test]
    fn a_face_of_android_xr_that_is_not_valid_sends_nothing_and_a_refill_leaves_no_old_weight() {
        let mut xr = at(100);
        xr.face = android_face();
        let android = Some(FaceTrackingProvider::Android);
        let mut reader = TrackedReader::default();
        assert!(reader.read_face_of(&xr, 1, 2, Features::FACE, android).face.valid);
        // The next sample has a weight at 0 where the one before had one: the buffer is cleared first
        xr.face.blend_shapes[FaceBlendShapeType::JawDrop.index()] = 0.0;
        xr.face.blend_shapes[FaceBlendShapeType::TongueLeft.index()] = 1.0;
        let frame = reader.read_face_of(&xr, 3, 4, Features::FACE, android);
        assert_eq!(frame.face.weights[FaceExpression::JawDrop.index()], 0.0);
        assert!(frame.face.weights.iter().all(|w| *w <= 0.69), "a shape that Meta lacks reaches no slot");
        xr.face.is_valid = false;
        assert!(!reader.read_face_of(&xr, 5, 6, Features::FACE, android).face.valid);
        // The host did not wish the face: nothing is read
        xr.face.is_valid = true;
        assert!(!reader.read_face_of(&xr, 7, 8, Features::HANDS, android).face.valid);
        // The provider decides which state the reader takes: Meta's tracker leaves `face` alone, and
        // an Android face leaves `meta_face` alone
        xr.meta_face = MetaFaceState { is_valid: true, data_source: Some(FaceDataSource::Audio), ..MetaFaceState::default() };
        let from_meta = reader.read_face_of(&xr, 9, 10, Features::FACE, Some(FaceTrackingProvider::Meta));
        assert_eq!(from_meta.face.flags, face::VALID | face::FROM_AUDIO);
        assert_eq!(from_meta.face.weights, [0.0; FACE_WEIGHTS]);
        xr.meta_face.is_valid = false;
        assert!(!reader.read_face_of(&xr, 11, 12, Features::FACE, android.filter(|_| false)).face.valid, "no provider reads Meta's state");
    }

    #[test]
    fn every_shape_that_the_two_runtimes_share_keeps_its_slot() {
        // The names of the Meta shapes and the Jetpack XR shapes differ by their side letters only, which
        // Cranpose's own tests check. Here the index map holds the property that the client relies on:
        // no two shapes share a slot, and the map is the same as the one that Cranpose makes.
        let mut seen = [false; FACE_WEIGHTS];
        for (shape, slot) in FaceBlendShapeType::ALL.iter().zip(META_SLOT) {
            assert_eq!(slot, shape.expression().map(|e| e.index()), "{shape:?}");
            if let Some(slot) = slot {
                assert!(!seen[slot], "{shape:?} shares slot {slot}");
                seen[slot] = true;
            }
        }
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
