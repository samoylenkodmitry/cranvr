//! The tracking messages: quantization, packet sizes, and a session over loopback.

mod common;

use common::{caps, host_identity, identity, Rng};
use cranvr_proto::{
    link::{ClientLink, HostListener, Incoming, MediaMode},
    media::DEFAULT_SHARD_LEN,
    message::*,
    secure::OVERHEAD,
    tracking::*,
    wire::{decode, encode, Error, Motion, Pose},
    MTU,
};
use std::{
    sync::{mpsc, Arc},
    thread,
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(10);

fn quaternion(rng: &mut Rng) -> [f32; 4] {
    let q: [f64; 4] = std::array::from_fn(|_| rng.unit() * 2.0 - 1.0);
    let length = q.iter().map(|c| c * c).sum::<f64>().sqrt();
    q.map(|c| (c / length) as f32)
}

fn random_pose(rng: &mut Rng, reach: f64) -> Pose {
    Pose { orientation: quaternion(rng), position: std::array::from_fn(|_| ((rng.unit() * 2.0 - 1.0) * reach) as f32) }
}

/// The angle in radians between two orientations. The turn from one to the other is computed
/// in double precision and measured with atan2: `acos` of a dot product near 1 loses half its digits.
fn angle(a: [f32; 4], b: [f32; 4]) -> f64 {
    let (a, b) = (a.map(f64::from), b.map(f64::from));
    let (ax, ay, az, aw) = (-a[0], -a[1], -a[2], a[3]);
    let [bx, by, bz, bw] = b;
    let x = aw * bx + ax * bw + ay * bz - az * by;
    let y = aw * by - ax * bz + ay * bw + az * bx;
    let z = aw * bz + ax * by - ay * bx + az * bw;
    let w = aw * bw - ax * bx - ay * by - az * bz;
    2.0 * (x * x + y * y + z * z).sqrt().atan2(w.abs())
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f64 {
    (0..3).map(|i| (a[i] as f64 - b[i] as f64).powi(2)).sum::<f64>().sqrt()
}

fn joints(rng: &mut Rng, count: usize, around: &Pose, reach: f64) -> Vec<Joint> {
    (0..count)
        .map(|i| {
            let local = random_pose(rng, reach);
            // The joints lie in the room: around the root, turned by the root's orientation
            let turned = {
                let [x, y, z, w] = around.orientation;
                let v = local.position;
                let u = [x, y, z];
                let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
                let t = cross(u, v).map(|c| 2.0 * c);
                let c = cross(u, t);
                [v[0] + w * t[0] + c[0], v[1] + w * t[1] + c[1], v[2] + w * t[2] + c[2]]
            };
            Joint {
                pose: Pose { orientation: local.orientation, position: std::array::from_fn(|k| around.position[k] + turned[k]) },
                flags: (i % 16) as u8,
            }
        })
        .collect()
}

fn root_motion(rng: &mut Rng) -> Motion {
    Motion {
        pose: Pose { orientation: quaternion(rng), position: [-0.3, 1.2, -0.5] },
        linear_velocity: [0.5, -0.25, 0.125],
        angular_velocity: [0.0, 1.0, -2.0],
    }
}

/// The size of a datagram message as UDP carries it, in bytes
fn sealed_size(datagram: &ClientDatagram) -> usize {
    let mut out = vec![cranvr_proto::wire::KIND_DATAGRAM];
    encode(datagram, &mut out).unwrap();
    out.len() + OVERHEAD
}

#[test]
fn a_hand_comes_back_within_the_quantization_step() {
    let mut rng = Rng(7);
    for _ in 0..200 {
        let root = root_motion(&mut rng);
        let sent = joints(&mut rng, HAND_JOINTS, &root.pose, 0.25);
        let hand = Hand::new(root, sent.as_slice().try_into().unwrap());
        // Through the wire, as the host receives it
        let mut bytes = Vec::new();
        encode(&hand, &mut bytes).unwrap();
        let back: Hand = decode(&bytes).unwrap();
        assert_eq!(back.root, root);
        for (a, b) in sent.iter().zip(back.joints().unwrap()) {
            assert!(distance(a.pose.position, b.pose.position) < 1e-4, "position {:?} {:?}", a.pose.position, b.pose.position);
            assert!(angle(a.pose.orientation, b.pose.orientation) < 1e-4, "angle {}", angle(a.pose.orientation, b.pose.orientation));
            assert_eq!(a.flags, b.flags);
        }
    }
}

#[test]
fn orientations_of_every_kind_survive_the_packing() {
    // Each component in turn is the largest, with both signs, and the turns that make the largest
    // component small are there too
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let cases = [
        [0.0, 0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0, -1.0],
        [1.0, 0.0, 0.0, 0.0],
        [0.0, -1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [s, s, 0.0, 0.0],
        [-s, 0.0, s, 0.0],
        [0.5, 0.5, 0.5, 0.5],
        [-0.5, 0.5, -0.5, 0.5],
        [0.1, -0.2, 0.3, -0.927_361_8],
    ];
    for q in cases {
        let root = Pose::IDENTITY;
        let mut sent = vec![Joint::tracked(Pose { orientation: q, position: [0.0; 3] }); BODY_JOINTS];
        sent[1].pose.position = [1.0, -2.0, 3.0];
        let body = Body::new(1, 2, BodySet::Default, 1.0, Motion { pose: root, ..Default::default() }, &sent).unwrap();
        let back = body.joints().unwrap();
        assert!(angle(q, back[0].pose.orientation) < 1e-4, "{q:?} became {:?}", back[0].pose.orientation);
        assert!(distance([1.0, -2.0, 3.0], back[1].pose.position) < 1e-4);
    }
}

#[test]
fn a_joint_beyond_the_range_stops_at_the_edge() {
    let root = Motion::default();
    let mut sent = vec![Joint::tracked(Pose::IDENTITY); HAND_JOINTS];
    sent[3].pose.position = [100.0, -100.0, 0.5];
    let back = Hand::new(root, sent.as_slice().try_into().unwrap()).joints().unwrap();
    assert!((back[3].pose.position[0] - 3.2767).abs() < 1e-3);
    assert!((back[3].pose.position[1] + 3.2768).abs() < 1e-3);
    assert!((back[3].pose.position[2] - 0.5).abs() < 1e-4);
}

#[test]
fn the_body_keeps_its_joints_in_order_for_both_sets() {
    let mut rng = Rng(11);
    for (set, count) in [(BodySet::Default, BODY_JOINTS), (BodySet::Full, FULL_BODY_JOINTS)] {
        let root = root_motion(&mut rng);
        let sent = joints(&mut rng, count, &root.pose, 1.2);
        let body = Body::new(10, 20, set, 0.75, root, &sent).unwrap();
        let mut bytes = Vec::new();
        encode(&body, &mut bytes).unwrap();
        let back: Body = decode(&bytes).unwrap();
        assert_eq!((back.sample_time, back.display_time, back.set, back.confidence), (10, 20, set, 0.75));
        let joints = back.joints().unwrap();
        assert_eq!(joints.len(), count);
        for (a, b) in sent.iter().zip(&joints) {
            assert!(distance(a.pose.position, b.pose.position) < 1e-4);
            assert!(angle(a.pose.orientation, b.pose.orientation) < 1e-4);
        }
        // Too few joints are refused at the sender
        assert!(Body::new(1, 2, set, 1.0, root, &sent[1..]).is_err());
    }
}

#[test]
fn a_block_of_the_wrong_length_is_refused_not_trusted() {
    let mut rng = Rng(5);
    let root = root_motion(&mut rng);
    let sent = joints(&mut rng, HAND_JOINTS, &root.pose, 0.2);
    let hand = Hand::new(root, sent.as_slice().try_into().unwrap());
    let mut bytes = Vec::new();
    encode(&hand, &mut bytes).unwrap();
    // The block is the last bytes: take one of them away and the length prefix no longer fits
    bytes.pop();
    assert!(decode::<Hand>(&bytes).is_err());
    // A body that says 84 joints and carries 70
    let body = Body::new(1, 2, BodySet::Default, 1.0, root, &joints(&mut rng, BODY_JOINTS, &root.pose, 1.0)).unwrap();
    let mut bytes = Vec::new();
    encode(&body, &mut bytes).unwrap();
    // The two times take a byte each. The set follows them: 0 is the default set, 1 the full one.
    assert_eq!(bytes[2], 0);
    bytes[2] = 1;
    let back: Body = decode(&bytes).unwrap();
    assert!(matches!(back.joints(), Err(Error::Malformed(_))));
}

#[test]
fn the_face_keeps_its_weights_within_half_a_step() {
    let weights: [f32; FACE_WEIGHTS] = std::array::from_fn(|i| i as f32 / (FACE_WEIGHTS - 1) as f32);
    let face = Face::new(5, 6, face::VALID | face::EYES_FOLLOWING_VALID, [0.25, 1.0], &weights);
    let mut bytes = Vec::new();
    encode(&face, &mut bytes).unwrap();
    let back: Face = decode(&bytes).unwrap();
    assert_eq!(back.flags, face::VALID | face::EYES_FOLLOWING_VALID);
    for (a, b) in weights.iter().zip(back.weights().unwrap()) {
        assert!((a - b).abs() <= 0.5 / 255.0 + 1e-6, "{a} {b}");
    }
    assert!((back.confidences()[0] - 0.25).abs() < 0.003 && back.confidences()[1] == 1.0);
    // Values outside the range stop at its ends
    let wild = Face::new(0, 0, 0, [2.0, -1.0], &[7.0; FACE_WEIGHTS]);
    assert_eq!(wild.weights().unwrap(), [1.0; FACE_WEIGHTS]);
    assert_eq!(wild.confidence, [255, 0]);
}

#[test]
fn every_datagram_fits_a_packet_with_room_to_spare() {
    let mut rng = Rng(3);
    let root = root_motion(&mut rng);
    let hand = |rng: &mut Rng| Hand::new(root, joints(rng, HAND_JOINTS, &root.pose, 0.2).as_slice().try_into().unwrap());
    let hands = Hands { sample_time: u64::MAX, display_time: u64::MAX, left: Some(hand(&mut rng)), right: Some(hand(&mut rng)) };
    let body = Body::new(u64::MAX, u64::MAX, BodySet::Full, 1.0, root, &joints(&mut rng, FULL_BODY_JOINTS, &root.pose, 1.5)).unwrap();
    let face = Face::new(u64::MAX, u64::MAX, 7, [1.0; 2], &[0.5; FACE_WEIGHTS]);
    let eyes = Eyes { sample_time: u64::MAX, display_time: u64::MAX, gaze: Some(random_pose(&mut rng, 1.0)) };
    let sizes = [
        ("hands", sealed_size(&ClientDatagram::Hands(hands))),
        ("body", sealed_size(&ClientDatagram::Body(body))),
        ("face", sealed_size(&ClientDatagram::Face(face))),
        ("eyes", sealed_size(&ClientDatagram::Eyes(eyes))),
    ];
    println!("sealed sizes in bytes: {sizes:?}, the limit is {MTU}");
    for (name, size) in sizes {
        assert!(size <= MTU, "{name} needs {size} bytes");
    }
    // The 84 joint body is the largest. It stays below the limit by a margin.
    assert!(sizes[1].1 < MTU - 100, "{sizes:?}");
}

#[test]
fn feature_sets_keep_their_names_and_drop_unknown_bits() {
    assert_eq!(Features::ALL.names(), ["hands", "body", "full_body", "face", "eyes"]);
    assert_eq!(Features::from_names(["eyes", "hands", "toes"]), Features::EYES.union(Features::HANDS));
    assert_eq!(Features(0xff).known(), Features::ALL);
    assert!(Features::ALL.contains(Features::FACE) && !Features::HANDS.contains(Features::FACE));
    assert!(Features::NONE.is_empty() && Features::NONE.names().is_empty());
    for set in 0..=0x7f {
        assert_eq!(Features::from_names(Features(set).names()), Features(set));
    }
}

#[test]
fn the_face_sources_belong_to_the_offer_and_a_wish_never_keeps_them() {
    let quest_3 = Features::HANDS.union(Features::FACE).union(Features::FACE_AUDIO);
    assert_eq!(quest_3.names(), ["hands", "face", "face_audio"]);
    assert!(quest_3.offers_face_audio() && !quest_3.offers_face_visual());
    assert!(Features::FACE.union(Features::FACE_VISUAL).offers_face_visual());
    // A face that names no source is of an unknown one
    assert!(!Features::FACE.offers_face_audio() && !Features::FACE.offers_face_visual());
    // A source without a face is no source, and a source is no feature: a wish never keeps it
    assert!(!Features::FACE_AUDIO.offers_face_audio());
    assert_eq!(Features::FACE_AUDIO.union(Features::HANDS).offered(), Features::HANDS);
    assert_eq!(quest_3.offered(), quest_3);
    assert_eq!(Features(0xff).offered(), Features::ALL.union(Features::FACE_SOURCES));
    assert_eq!(quest_3.known(), Features::HANDS.union(Features::FACE), "a wish drops the sources and keeps the face");
    assert!(Features::FACE_SOURCES.known().is_empty());
    // The sources travel as bits of the same byte
    let mut bytes = Vec::new();
    encode(&ClientMsg::TrackingOffer(quest_3), &mut bytes).unwrap();
    assert!(matches!(decode::<ClientMsg>(&bytes).unwrap(), ClientMsg::TrackingOffer(f) if f == quest_3));
    assert_eq!(bytes.len(), 2, "the offer is still a variant tag and one byte");
}

#[test]
fn tracking_travels_through_a_session() {
    let listener = HostListener::bind("127.0.0.1:0", host_identity()).unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || {
        let pending = listener.accept().unwrap();
        let stream = negotiate(
            &HostWishes {
                eye_width: 2064,
                eye_width_unfoveated: 1248,
                refresh_hz: 120.0,
                bitrate_bps: 100_000_000,
                codecs: vec![cranvr_proto::wire::Codec::Hevc],
                foveation: None,
                full_range: true,
                slices: 1,
            },
            &pending.caps,
        )
        .unwrap();
        pending.accept(stream, MediaMode::Udp, DEFAULT_SHARD_LEN).unwrap()
    });
    let client = ClientLink::connect(addr, &caps(), &identity(), None, WAIT).unwrap();
    let host = Arc::new(host.join().unwrap());
    let client = Arc::new(client);

    let (to_host, host_inbox) = mpsc::channel();
    let (to_client, client_inbox) = mpsc::channel();
    {
        let (h, to_host_control) = (Arc::clone(&host), to_host.clone());
        thread::spawn(move || {
            h.duplex.read_tcp(|m| {
                if let Incoming::Control(ClientMsg::TrackingOffer(f)) = m {
                    to_host_control.send(Err(f)).unwrap();
                }
            })
        });
        let h = Arc::clone(&host);
        thread::spawn(move || {
            h.duplex.read_udp(|m| {
                if let Incoming::Datagram(d) = m {
                    to_host.send(Ok(d)).unwrap();
                }
            })
        });
        let c = Arc::clone(&client);
        thread::spawn(move || {
            c.duplex.read_tcp(|m| {
                if let Incoming::Control(HostMsg::TrackingWish(f)) = m {
                    to_client.send(f).unwrap();
                }
            })
        });
    }
    client.duplex.send_control(&ClientMsg::TrackingOffer(Features::HANDS.union(Features::EYES))).unwrap();
    assert_eq!(host_inbox.recv_timeout(WAIT).unwrap().err(), Some(Features::HANDS.union(Features::EYES)));
    host.duplex.send_control(&HostMsg::TrackingWish(Features::HANDS)).unwrap();
    assert_eq!(client_inbox.recv_timeout(WAIT).unwrap(), Features::HANDS);

    let mut rng = Rng(2);
    let root = root_motion(&mut rng);
    let hand = Hand::new(root, joints(&mut rng, HAND_JOINTS, &root.pose, 0.2).as_slice().try_into().unwrap());
    let sent = Hands { sample_time: 99, display_time: 120, left: Some(hand.clone()), right: None };
    client.duplex.send_datagram(&ClientDatagram::Hands(sent.clone())).unwrap();
    client.duplex.send_datagram(&ClientDatagram::Eyes(Eyes { sample_time: 99, display_time: 120, gaze: Some(Pose::IDENTITY) })).unwrap();
    let mut seen = Vec::new();
    while seen.len() < 2 {
        if let Ok(d) = host_inbox.recv_timeout(WAIT).unwrap() {
            seen.push(d);
        }
    }
    assert!(seen.contains(&ClientDatagram::Hands(sent)));
    assert!(seen.iter().any(|d| matches!(d, ClientDatagram::Eyes(e) if e.gaze == Some(Pose::IDENTITY))));
}

#[test]
fn a_sample_that_replaces_another_is_the_same_as_a_new_one() {
    let mut rng = Rng(21);
    let root = root_motion(&mut rng);
    let first = joints(&mut rng, HAND_JOINTS, &root.pose, 0.2);
    let second = joints(&mut rng, HAND_JOINTS, &root.pose, 0.2);
    let mut hand = Hand::new(root, first.as_slice().try_into().unwrap());
    let other_root = Motion { linear_velocity: [1.0, 2.0, 3.0], ..root };
    hand.set(other_root, second.as_slice().try_into().unwrap());
    assert_eq!(hand, Hand::new(other_root, second.as_slice().try_into().unwrap()));

    let body_joints = joints(&mut rng, FULL_BODY_JOINTS, &root.pose, 1.0);
    let mut body = Body::new(1, 2, BodySet::Full, 0.5, root, &body_joints).unwrap();
    // A smaller set in the same value: the block shrinks to its 70 joints
    body.set(3, 4, BodySet::Default, 0.75, root, &body_joints[..BODY_JOINTS]).unwrap();
    assert_eq!(body, Body::new(3, 4, BodySet::Default, 0.75, root, &body_joints[..BODY_JOINTS]).unwrap());
    assert!(body.set(5, 6, BodySet::Full, 1.0, root, &body_joints[..BODY_JOINTS]).is_err());

    let mut face = Face::new(1, 2, face::VALID, [1.0; 2], &[0.5; FACE_WEIGHTS]);
    face.set(3, 4, 0, [0.0; 2], &[0.25; FACE_WEIGHTS]);
    assert_eq!(face, Face::new(3, 4, 0, [0.0; 2], &[0.25; FACE_WEIGHTS]));
    // A default value is what `mem::take` leaves in a sender's slot
    assert_eq!(Hand::default().root, Motion::default());
    assert_eq!(Body::default().set, BodySet::Default);
}
