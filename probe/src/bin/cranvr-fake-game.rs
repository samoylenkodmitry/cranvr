//! A stand-in for a game under Wine. It speaks the BasaltVR runtime protocol to the host, shares
//! one swapchain of IOSurfaces per eye and sends frames for the newest head pose, the way the
//! runtime does. Its eye badges have swapped colors (left blue, right red), so
//! `cranvr-probe --game` can tell its frames from the host's test scene.
//!
//! It also reads the controller messages that the host sends and writes what arrived when it ends.
//! Like the runtime with cranvr's patch 0004, it tells the host in `runtime-features.txt` that it
//! takes aim poses, so each hand arrives with a grip pose and an aim pose. A pull of the left grip
//! makes it answer with a haptic pulse on the left hand, as a game does. `--expect-input` makes the
//! exit code depend on the input: the probe's scripted controllers (`cranvr-probe --protocol
//! native`) must have arrived on both hands with their own grip and aim poses, as `hands.rs`
//! scripts them, and with the Touch buttons.
//!
//! Usage: cranvr-fake-game --control FILE [--fps N] [--seconds N] [--expect-input]

use alvr_common::{LEFT_SQUEEZE_VALUE_ID, RIGHT_A_CLICK_ID, RIGHT_THUMBSTICK_X_ID, RIGHT_TRIGGER_VALUE_ID};
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
/// The probe puts its floor 0.3 m below its room's origin (`PROBE_FLOOR` in probe/src/native.rs).
/// The host lifts every pose by that much.
const FLOOR_LIFT: f32 = 0.3;

// The probe plays the script of hands.rs and uses all of it. The game only checks it.
#[allow(dead_code)]
#[path = "../hands.rs"]
mod hands;
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
        let entry = self.buttons.entry(path).or_insert((value, 0));
        if entry.0 != value {
            entry.1 += 1;
        }
        entry.0 = value;
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
        for (name, id, wanted) in [("right trigger", *RIGHT_TRIGGER_VALUE_ID, 0.75), ("right stick x", *RIGHT_THUMBSTICK_X_ID, 0.5)] {
            match self.buttons.get(&id) {
                Some((_, changes)) if *changes >= 2 => {}
                other => out.push(format!("{name} {other:?}: expected changes up to {wanted}")),
            }
        }
        for (name, id) in [("right A", *RIGHT_A_CLICK_ID), ("left squeeze", *LEFT_SQUEEZE_VALUE_ID)] {
            if self.buttons.get(&id).is_none_or(|b| b.1 < 2) {
                out.push(format!("{name} did not go down and up"));
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
    let (mut control, mut fps, mut seconds, mut expect_input) = (None, 72.0f64, 10.0f64, false);
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--control" => control = args.next(),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).expect("--fps N"),
            "--seconds" => seconds = args.next().and_then(|v| v.parse().ok()).expect("--seconds N"),
            "--expect-input" => expect_input = true,
            _ => {
                eprintln!("usage: cranvr-fake-game --control FILE [--fps N] [--seconds N] [--expect-input]");
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

    // The runtime of cranvr's patch 0004 writes this file before it connects. Without it the host
    // sends no aim poses, because a runtime without the patch drops the connection at the message.
    let features = std::path::Path::new(&control_path).with_file_name("runtime-features.txt");
    fs::write(&features, "aim\n").expect("write runtime-features.txt");
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
    let (acks, ack_rx) = mpsc::channel::<u16>();
    let (consumed, consumed_rx) = mpsc::channel::<u64>();
    {
        let (head_ns, fov, input) = (Arc::clone(&head_ns), Arc::clone(&fov), Arc::clone(&input));
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
                        if path == *LEFT_SQUEEZE_VALUE_ID && was < 0.5 && value >= 0.5 {
                            let mut pulse = 0u32.to_le_bytes().to_vec();
                            pulse.extend_from_slice(&20_000_000u64.to_le_bytes());
                            pulse.extend_from_slice(&160.0f32.to_le_bytes());
                            pulse.extend_from_slice(&0.8f32.to_le_bytes());
                            haptic_stream.write_all(&message(HAPTIC, &pulse)).ok();
                        }
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
    while start.elapsed().as_secs_f64() < seconds {
        next += period;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
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
    let problems = input.problems();
    for problem in &problems {
        println!("fake-game: input problem: {problem}");
    }
    let input_ok = !expect_input || problems.is_empty();
    std::process::exit(if sent > 0 && done * 10 >= sent * 9 && input_ok { 0 } else { 1 });
}
