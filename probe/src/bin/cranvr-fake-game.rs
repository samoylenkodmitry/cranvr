//! A stand-in for a game under Wine. It speaks the BasaltVR runtime protocol to the host, shares
//! one swapchain of IOSurfaces per eye and sends frames for the newest head pose, the way the
//! runtime does. Its eye badges have swapped colors (left blue, right red), so
//! `cranvr-probe --game` can tell its frames from the host's test scene.
//!
//! Usage: cranvr-fake-game --control FILE [--fps N] [--seconds N]

use cranvr_vt::PixelBuffer;
use std::{
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
const VIEW_CONFIG: u16 = 15;
const FRAME_CONSUMED: u16 = 17;
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
    let (mut control, mut fps, mut seconds) = (None, 72.0f64, 10.0f64);
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--control" => control = args.next(),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).expect("--fps N"),
            "--seconds" => seconds = args.next().and_then(|v| v.parse().ok()).expect("--seconds N"),
            _ => {
                eprintln!("usage: cranvr-fake-game --control FILE [--fps N] [--seconds N]");
                std::process::exit(2);
            }
        }
    }
    let control = fs::read_to_string(control.expect("--control FILE")).expect("read the control file");
    let fields: Vec<&str> = control.split_whitespace().collect();
    let port: u16 = fields[0].parse().expect("port");
    let token = fields[1];
    let (w, h): (u32, u32) = (fields[2].parse().unwrap(), fields[3].parse().unwrap());

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
    let (acks, ack_rx) = mpsc::channel::<u16>();
    let (consumed, consumed_rx) = mpsc::channel::<u64>();
    {
        let (head_ns, fov) = (Arc::clone(&head_ns), Arc::clone(&fov));
        let mut reader = stream.try_clone().unwrap();
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
    std::process::exit(if sent > 0 && done * 10 >= sent * 9 { 0 } else { 1 });
}
