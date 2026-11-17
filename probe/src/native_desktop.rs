//! The probe as a client of the desktop stream and the sound (`--desktop`). A headset shows the
//! desktop as a panel and plays the Mac's sound. The probe does the same without a headset:
//! - it asks the host for the desktop and decodes the pictures of the stream `DESKTOP_STREAM` with
//!   VideoToolbox. It counts the pictures, key frames and bytes, times the decode and checks the
//!   picture's size against what the host announced;
//! - it asks for a key frame of that stream when a picture of it was lost;
//! - it plays a pointer script: a controller circles over the panel, pulls the trigger once a
//!   second and leans the thumbstick back and forth. The host's log shows the mouse events that it
//!   injected (start it with `CRANVR_POINTER_DRY_RUN=1` so that nothing moves the real mouse);
//! - it counts the datagrams of the Mac's cursor and the seconds of sound, and measures how old
//!   the sound is on arrival.

use crate::native::now_ns;
use cranvr_proto::{
    clock::ClockSync,
    link::ClientLink,
    media::Received,
    message::{Audio, ClientMsg, Cursor, DesktopParams, DesktopWish, HostMsg, Pointer, DESKTOP_STREAM},
};
use cranvr_vt::{DecodedFrame, Decoder};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const KEY_REQUEST_GAP: Duration = Duration::from_millis(250);
/// The wish that the probe sends: a Quest 3 takes pictures of this width
const MAX_WIDTH: u32 = 3840;
const MAX_FPS: u8 = 60;
/// The pointer script's rate, as a headset's frame rate
const POINTER_HZ: f32 = 120.0;

#[derive(Default)]
struct Totals {
    pictures: u64,
    key_pictures: u64,
    bytes: u64,
    /// Pictures that the decoder refused, or that waited for a key frame
    skipped: u64,
    decode_ms: Vec<f64>,
    /// The picture's size that the decoder gave, when it differs from the announced one
    wrong_size: u64,
    size: (usize, usize),
    first: Option<Instant>,
    last: Option<Instant>,
    /// Host send to the last packet, in ms
    network_ms: Vec<f64>,
    cursor_datagrams: u64,
    last_cursor: Option<Cursor>,
    audio_datagrams: u64,
    audio_bytes: u64,
    audio_gaps: u64,
    last_audio_sequence: Option<u32>,
    /// Host capture of the first sample to arrival, in ms
    audio_age_ms: Vec<f64>,
    audio_first: Option<Instant>,
    audio_last: Option<Instant>,
}

pub struct DesktopProbe {
    totals: Mutex<Totals>,
    params: Mutex<Option<DesktopParams>>,
    config: Mutex<Option<Vec<u8>>>,
    decoder: Mutex<Option<Decoder>>,
    decoded: Mutex<mpsc::Receiver<DecodedFrame>>,
    decoded_sender: mpsc::Sender<DecodedFrame>,
    waiting_key: AtomicBool,
    last_key_request: Mutex<Instant>,
    last_frame: Mutex<Option<u32>>,
}

impl DesktopProbe {
    pub fn new() -> Arc<Self> {
        let (decoded_sender, decoded) = mpsc::channel();
        Arc::new(Self {
            totals: Mutex::new(Totals::default()),
            params: Mutex::new(None),
            config: Mutex::new(None),
            decoder: Mutex::new(None),
            decoded: Mutex::new(decoded),
            decoded_sender,
            waiting_key: AtomicBool::new(true),
            last_key_request: Mutex::new(Instant::now() - KEY_REQUEST_GAP),
            last_frame: Mutex::new(None),
        })
    }

    /// Asks the host for the desktop.
    pub fn ask(&self, link: &ClientLink) {
        let wish = DesktopWish { enabled: true, max_width: MAX_WIDTH, max_fps: MAX_FPS };
        link.duplex.send_control(&ClientMsg::Desktop(wish)).ok();
        println!("probe: asked the host for the desktop at up to {MAX_WIDTH} pixels and {MAX_FPS} pictures a second");
    }

    pub fn on_message(&self, message: &HostMsg) {
        match message {
            HostMsg::Desktop(params) => {
                match params {
                    Some(p) => println!(
                        "probe: the host runs the desktop stream: {}x{} pixels, {}x{} points, up to {} a second, {} Mbps, visible {}, mirrored {}",
                        p.size[0], p.size[1], p.points[0], p.points[1], p.max_fps, p.bitrate_bps / 1_000_000, p.visible, p.flipped
                    ),
                    None => println!("probe: the host has no desktop for this client"),
                }
                *self.params.lock().unwrap() = *params;
                self.waiting_key.store(true, Ordering::Relaxed);
            }
            HostMsg::DecoderConfig { stream, config, .. } if *stream == DESKTOP_STREAM => {
                *self.config.lock().unwrap() = Some(config.clone());
                match Decoder::new(config, vec![(0.5, 0.5), (0.1, 0.1)], self.decoded_sender.clone()) {
                    Ok(decoder) => {
                        println!("probe: the desktop decoder is ready ({} bytes of parameter sets)", config.len());
                        *self.decoder.lock().unwrap() = Some(decoder);
                        self.waiting_key.store(true, Ordering::Relaxed);
                    }
                    Err(e) => eprintln!("probe: the desktop decoder: {e}"),
                }
            }
            _ => {}
        }
    }

    /// A picture of the desktop stream arrived whole.
    pub fn on_unit(&self, unit: &Received, link: &ClientLink, clock: &Mutex<ClockSync>) {
        let mut last_frame = self.last_frame.lock().unwrap();
        let in_order = last_frame.is_none_or(|last| unit.meta.frame == last.wrapping_add(1));
        *last_frame = Some(unit.meta.frame);
        drop(last_frame);
        if unit.meta.key {
            self.waiting_key.store(false, Ordering::Relaxed);
        } else if !in_order {
            self.waiting_key.store(true, Ordering::Relaxed);
        }
        let mut totals = self.totals.lock().unwrap();
        if !unit.meta.key && self.waiting_key.load(Ordering::Relaxed) {
            totals.skipped += 1;
            drop(totals);
            self.request_key_frame(link, unit.meta.frame);
            return;
        }
        let mut decoder = self.decoder.lock().unwrap();
        let Some(decoder) = decoder.as_mut() else {
            totals.skipped += 1;
            return;
        };
        let started = Instant::now();
        if let Err(e) = decoder.decode(unit.completed, unit.payload()) {
            eprintln!("probe: the desktop decode: {e}");
            totals.skipped += 1;
            return;
        }
        let decode_ms = started.elapsed().as_secs_f64() * 1e3;
        let Ok(frame) = self.decoded.lock().unwrap().try_recv() else { return };
        let announced = self.params.lock().unwrap().map(|p| (p.size[0] as usize, p.size[1] as usize));
        if announced.is_some_and(|a| a != (frame.width, frame.height)) {
            totals.wrong_size += 1;
        }
        totals.size = (frame.width, frame.height);
        totals.pictures += 1;
        totals.key_pictures += u64::from(unit.meta.key);
        totals.bytes += unit.payload().len() as u64;
        totals.decode_ms.push(decode_ms);
        let now = Instant::now();
        totals.first.get_or_insert(now);
        totals.last = Some(now);
        if let Some(sent) = clock.lock().unwrap().to_local(unit.meta.sent) {
            totals.network_ms.push(unit.completed.saturating_sub(sent) as f64 / 1e6);
        }
    }

    /// The receiver lost a unit of the desktop stream.
    pub fn on_lost(&self, link: &ClientLink, frame: u32) {
        self.waiting_key.store(true, Ordering::Relaxed);
        self.request_key_frame(link, frame);
    }

    fn request_key_frame(&self, link: &ClientLink, frame: u32) {
        let mut last = self.last_key_request.lock().unwrap();
        if last.elapsed() < KEY_REQUEST_GAP {
            return;
        }
        *last = Instant::now();
        link.duplex.send_control(&ClientMsg::RequestKeyFrame { stream: DESKTOP_STREAM, frame }).ok();
    }

    pub fn on_cursor(&self, cursor: Cursor) {
        let mut totals = self.totals.lock().unwrap();
        totals.cursor_datagrams += 1;
        totals.last_cursor = Some(cursor);
    }

    pub fn on_audio(&self, audio: &Audio, clock: &Mutex<ClockSync>) {
        let mut totals = self.totals.lock().unwrap();
        totals.audio_datagrams += 1;
        totals.audio_bytes += audio.pcm.len() as u64;
        if totals.last_audio_sequence.is_some_and(|last| audio.sequence != last.wrapping_add(1)) {
            totals.audio_gaps += 1;
        }
        totals.last_audio_sequence = Some(audio.sequence);
        let now = Instant::now();
        totals.audio_first.get_or_insert(now);
        totals.audio_last = Some(now);
        if let Some(captured) = clock.lock().unwrap().to_local(audio.host_time) {
            totals.audio_age_ms.push(now_ns().saturating_sub(captured) as f64 / 1e6);
        }
    }

    /// Plays the pointer script until `stop`: the controller circles over the panel at 120 Hz. Once
    /// a second the trigger goes down for 200 ms. The thumbstick leans forward for a second, then
    /// back for a second.
    pub fn pointer_script(send: impl Fn(Pointer) + Send + 'static, stopped: impl Fn() -> bool + Send + 'static) {
        thread::Builder::new()
            .name("pointer-script".into())
            .spawn(move || {
                let start = Instant::now();
                let (mut seq, mut next) = (0u32, start);
                while !stopped() {
                    let t = start.elapsed().as_secs_f32();
                    let angle = t * std::f32::consts::TAU / 4.0;
                    let hit = Some([0.5 + 0.3 * angle.cos(), 0.5 + 0.3 * angle.sin()]);
                    let trigger = if t % 1.0 > 0.8 { 1.0 } else { 0.0 };
                    let scroll = if t % 2.0 < 1.0 { 0.5 } else { -0.5 };
                    send(Pointer { seq, sent: now_ns(), hit, trigger, scroll });
                    seq = seq.wrapping_add(1);
                    next += Duration::from_secs_f32(1.0 / POINTER_HZ);
                    thread::sleep(next.saturating_duration_since(Instant::now()));
                }
                // A pointer that leaves the panel lets the button go
                send(Pointer { seq, sent: now_ns(), hit: None, trigger: 0.0, scroll: 0.0 });
            })
            .ok();
    }

    /// The numbers, for the end of the run. `Some(ok)` when the desktop was asked for.
    pub fn summary(&self) -> bool {
        let totals = self.totals.lock().unwrap();
        let seconds = match (totals.first, totals.last) {
            (Some(first), Some(last)) => last.duration_since(first).as_secs_f64().max(1e-3),
            _ => 1.0,
        };
        let describe = |values: &[f64]| {
            if values.is_empty() {
                return "no data".to_string();
            }
            let mut sorted = values.to_vec();
            sorted.sort_by(f64::total_cmp);
            let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
            format!("median {:.2}, p95 {:.2}, max {:.2}", at(0.5), at(0.95), sorted[sorted.len() - 1])
        };
        println!(
            "probe: desktop: {} pictures ({} key) of {}x{} in {:.1} s: {:.1} a second, {:.1} Mbps, {} skipped, {} of the wrong size; decode (ms) {}; network (ms) {}",
            totals.pictures,
            totals.key_pictures,
            totals.size.0,
            totals.size.1,
            seconds,
            totals.pictures as f64 / seconds,
            totals.bytes as f64 * 8.0 / seconds / 1e6,
            totals.skipped,
            totals.wrong_size,
            describe(&totals.decode_ms),
            describe(&totals.network_ms),
        );
        println!(
            "probe: desktop: {} cursor datagrams, the last at {:?}",
            totals.cursor_datagrams,
            totals.last_cursor.map(|c| (c.u, c.v, c.on))
        );
        let audio_seconds = totals.audio_bytes as f64 / 4.0 / 48_000.0;
        let audio_span = match (totals.audio_first, totals.audio_last) {
            (Some(first), Some(last)) => last.duration_since(first).as_secs_f64(),
            _ => 0.0,
        };
        println!(
            "probe: audio: {} datagrams, {:.2} s of sound in {:.2} s, {} sequence gaps, {:.2} Mbps; age on arrival (ms) {}",
            totals.audio_datagrams,
            audio_seconds,
            audio_span,
            totals.audio_gaps,
            totals.audio_bytes as f64 * 8.0 / audio_span.max(1e-3) / 1e6,
            describe(&totals.audio_age_ms),
        );
        totals.pictures > 0
    }
}
