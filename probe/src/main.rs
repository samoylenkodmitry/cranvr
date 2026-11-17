//! cranvr-probe: a headless ALVR client for tests. It uses ALVR's own client core (the code
//! inside the Quest app), sends a slowly turning head, decodes the HEVC stream and checks the red
//! and blue eye badges that the host draws.
//!
//! On a Mac it decodes with VideoToolbox and runs beside the host. On Android it runs as a plain
//! program started by adb and decodes with ALVR's MediaCodec path, as the Quest app does; a
//! display loop takes one image per refresh (see android.rs). The host then reaches it through
//! `adb forward` of TCP ports 9943 and 9944.
//!
//! With `--protocol native` it is a client of cranvr's own protocol instead (see native.rs): it
//! connects to the host's TCP port, sends synthetic tracking once per refresh and reports its frame
//! timing and its loss, as a headset app does.
//!
//! Exit code 0 means at least half the expected frames arrived and decoded with the right badges.

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "macos")]
mod native;
#[cfg(target_os = "macos")]
mod native_desktop;

use alvr_client_core::{ClientCapabilities, ClientCoreContext, ClientCoreEvent};
use alvr_common::{
    glam::{Quat, UVec2, Vec3},
    DeviceMotion, Fov, Pose, RelaxedAtomic, ViewParams, HAND_LEFT_ID, HEAD_ID, LEFT_SQUEEZE_VALUE_ID,
};
use alvr_packets::{ButtonEntry, ButtonValue, FaceData};
use alvr_session::CodecType;
#[cfg(target_os = "android")]
use android::{DecodedFrame, Decoder, Stream};
use cranvr_common::foveation::{view_to_frame, Foveation, FoveationConfig};
#[cfg(target_os = "macos")]
use cranvr_vt::{DecodedFrame, Decoder};
use std::{
    env,
    f32::consts::FRAC_PI_4,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// The host draws the eye badges at this point of each eye's view.
const BADGE_VIEW: [f32; 2] = [0.11, 0.5];

/// Where the badges sit in the encoded frame. With foveated encoding this uses the stock ALVR
/// client's unwarp formula, so a match proves the host's warp agrees with the client.
fn badge_points(foveation: Option<&Foveation>) -> Vec<(f32, f32)> {
    (0..2)
        .map(|eye| {
            let p = view_to_frame(foveation, eye, BADGE_VIEW);
            (p[0], p[1])
        })
        .collect()
}

fn usage() -> ! {
    eprintln!(
        "usage: cranvr-probe [--seconds N] [--game | --any-picture] [--protocol native|alvr]\n\
         \n  --protocol native  connect to cranvr-host --protocol native, not to an ALVR host\
         \n  --connect HOST:PORT  with native: the host's address (default 127.0.0.1:9950)\
         \n  --tcp              with native: ask for the media over TCP, as a USB link needs it\
         \n  --drop-percent P   with native: drop this share of the media packets, to test the parity\
         \n  --desktop          with native: ask for the Mac's desktop as a second stream, play a pointer script\
         \n                     and count the sound (start the host with --desktop and CRANVR_POINTER_DRY_RUN=1)"
    );
    std::process::exit(2);
}

fn main() {
    let mut seconds = 10.0f32;
    // --game: expect the fake game's badges (left blue, right red) instead of the test scene's.
    // --any-picture: count every frame of the right size, for a real game that draws no badges.
    let mut game = false;
    let mut any_picture = false;
    // Native mode: the host's address, media over TCP, a share of media packets to drop
    let mut native = false;
    let mut connect = "127.0.0.1:9950".to_string();
    let mut tcp = false;
    let mut drop_percent = 0.0f32;
    let mut desktop = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seconds" => seconds = args.next().and_then(|v| v.parse().ok()).expect("--seconds N"),
            "--game" => game = true,
            "--any-picture" => any_picture = true,
            "--protocol" => match args.next().as_deref() {
                Some("native") => native = true,
                Some("alvr") => native = false,
                _ => usage(),
            },
            "--connect" => connect = args.next().unwrap_or_else(|| usage()),
            "--tcp" => tcp = true,
            "--desktop" => desktop = true,
            "--drop-percent" => drop_percent = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    if native {
        #[cfg(target_os = "macos")]
        {
            use std::net::ToSocketAddrs;
            let Some(address) = connect.to_socket_addrs().ok().and_then(|mut a| a.next()) else { usage() };
            std::process::exit(native::run(native::Options { seconds, game, any_picture, connect: address, tcp, drop_percent, desktop }));
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (&connect, tcp, drop_percent, desktop);
            eprintln!("probe: --protocol native runs on macOS only for now");
            std::process::exit(2);
        }
    }

    let capabilities = ClientCapabilities {
        default_view_resolution: UVec2::new(2064, 2208), // Quest 3
        // A Quest 3's rates, and higher ones that measure how much room the stream leaves
        refresh_rates: vec![72.0, 80.0, 90.0, 120.0, 144.0, 165.0, 180.0, 240.0],
        foveated_encoding: true,
        encoder_high_profile: true,
        encoder_10_bits: false,
        encoder_av1: false,
        prefer_10bit: false,
        prefer_full_range: true,
        preferred_encoding_gamma: 1.0,
        prefer_hdr: false,
    };
    #[cfg(target_os = "android")]
    if let Err(e) = android::start_binder_threads() {
        eprintln!("probe: {e}; the decoder may stall");
    }
    let context = Arc::new(ClientCoreContext::new(capabilities));

    let decoder: Arc<Mutex<Option<Decoder>>> = Arc::new(Mutex::new(None));
    let received = Arc::new(AtomicU64::new(0));
    let (decoded_sender, decoded_receiver) = mpsc::channel::<DecodedFrame>();
    context.set_decoder_input_callback(Box::new({
        let decoder = Arc::clone(&decoder);
        let received = Arc::clone(&received);
        move |timestamp, nal| {
            // Frames that come before the decoder exists do not count: the share of frames shown
            // then measures the stream, not the start
            match decoder.lock().unwrap().as_mut() {
                Some(d) => {
                    received.fetch_add(1, Ordering::Relaxed);
                    d.decode(timestamp.as_nanos() as u64, nal).map_err(|e| eprintln!("decode: {e}")).is_ok()
                }
                None => false,
            }
        }
    }));
    context.resume();
    println!("probe: waiting for the host (run cranvr-host --trust-all)");

    let streaming = Arc::new(RelaxedAtomic::new(false));
    let start = Instant::now();
    let mut fps = 90.0f32;
    let mut streaming_since: Option<Instant> = None;
    let (mut decoded, mut good, mut bad) = (0u64, 0u64, 0u64);
    let mut last_hud = String::new();
    let mut last_report = Duration::ZERO;
    let mut size = (0, 0);
    // set when streaming starts: where the badges are, and the frame size the client expects
    let mut points = badge_points(None);
    let mut haptics = 0u32;
    let mut expected_size = (0usize, 0usize);
    #[cfg(target_os = "android")]
    let mut stream: Option<Stream> = None;

    while streaming_since.map_or(start.elapsed() < Duration::from_secs(60), |s| s.elapsed().as_secs_f32() < seconds) {
        while let Some(event) = context.poll_event() {
            match event {
                ClientCoreEvent::UpdateHudMessage(message) => {
                    if message != last_hud {
                        println!("probe: hud: {}", message.replace('\n', " | "));
                        last_hud = message;
                    }
                }
                ClientCoreEvent::StreamingStarted(config) => {
                    fps = config.negotiated_config.refresh_rate_hint;
                    let res = config.negotiated_config.view_resolution;
                    let foveation = config.negotiated_config.enable_foveated_encoding.then(|| {
                        let c = config.settings.video.foveated_encoding.as_option().expect("foveation settings");
                        let fc = FoveationConfig {
                            center_size: [c.center_size_x, c.center_size_y],
                            center_shift: [c.center_shift_x, c.center_shift_y],
                            edge_ratio: [c.edge_ratio_x, c.edge_ratio_y],
                        };
                        Foveation::new([res.x, res.y], &fc)
                    });
                    points = badge_points(foveation.as_ref());
                    expected_size = foveation.map_or((res.x as usize * 2, res.y as usize), |f| {
                        (f.optimized[0] as usize * 2, f.optimized[1] as usize)
                    });
                    println!(
                        "probe: streaming started: {}x{} per eye at {fps} Hz, foveated: {}, expecting {}x{} frames",
                        res.x,
                        res.y,
                        foveation.is_some(),
                        expected_size.0,
                        expected_size.1
                    );
                    #[cfg(target_os = "android")]
                    {
                        stream = Some(Stream { fps, video: config.settings.video.clone() });
                    }
                    streaming.set(true);
                    // A restarted stream counts from its new start
                    streaming_since = Some(Instant::now());
                    last_report = Duration::ZERO;
                    let context = Arc::clone(&context);
                    let streaming = Arc::clone(&streaming);
                    thread::spawn(move || tracking_loop(context, streaming, fps));
                }
                ClientCoreEvent::StreamingStopped => {
                    println!("probe: streaming stopped");
                    streaming.set(false);
                    *decoder.lock().unwrap() = None;
                }
                ClientCoreEvent::DecoderConfig { codec, config_nal } => {
                    // ALVR: ignore later configs until the next connection
                    if decoder.lock().unwrap().is_some() {
                        continue;
                    }
                    if codec != CodecType::Hevc {
                        eprintln!("probe: codec {codec:?} is not HEVC");
                        continue;
                    }
                    #[cfg(target_os = "macos")]
                    let made = Decoder::new(&config_nal, points.clone(), decoded_sender.clone());
                    #[cfg(target_os = "android")]
                    let made = match &stream {
                        Some(stream) => {
                            Decoder::new(&config_nal, stream, Arc::clone(&context), points.clone(), decoded_sender.clone())
                        }
                        None => Err("the decoder config came before the stream started".into()),
                    };
                    match made {
                        Ok(d) => {
                            println!("probe: decoder ready ({} bytes of parameter sets)", config_nal.len());
                            *decoder.lock().unwrap() = Some(d);
                        }
                        Err(e) => eprintln!("probe: decoder: {e}"),
                    }
                }
                ClientCoreEvent::Haptics { device_id, .. } => {
                    if haptics == 0 {
                        println!("probe: first haptic pulse (device {device_id:#x})");
                    }
                    haptics += 1;
                }
                _ => (),
            }
        }

        for frame in decoded_receiver.try_iter() {
            // On Android the decoder and the display report the frame themselves
            #[cfg(target_os = "macos")]
            {
                let timestamp = Duration::from_nanos(frame.timestamp_ns);
                context.report_frame_decoded(timestamp);
                context.report_compositor_start(timestamp);
                context.report_submit(timestamp, Duration::ZERO);
            }
            decoded += 1;
            size = (frame.width, frame.height);
            let (left, right) = (frame.samples[0], frame.samples[1]);
            let size_ok = size == expected_size;
            if !size_ok && bad == 0 {
                eprintln!("probe: frame is {}x{}, the client expects {}x{}", size.0, size.1, expected_size.0, expected_size.1);
            }
            let (red, blue) = if game { (right, left) } else { (left, right) };
            let badges_ok = red.cr > 180 && red.cb < 128 && blue.cb > 180 && blue.cr < 128;
            if size_ok && (any_picture || badges_ok) {
                good += 1;
            } else {
                bad += 1;
                if bad <= 3 {
                    eprintln!("probe: wrong badge colors: left {left:?} right {right:?}");
                }
            }
        }

        if let Some(since) = streaming_since {
            if since.elapsed().saturating_sub(last_report) >= Duration::from_secs(2) {
                last_report = since.elapsed();
                println!(
                    "probe: {:.1}s: {} frames received, {} decoded ({} good), {:.1} fps",
                    last_report.as_secs_f32(),
                    received.load(Ordering::Relaxed),
                    decoded,
                    good,
                    decoded as f32 / last_report.as_secs_f32()
                );
            }
        }
        thread::sleep(Duration::from_millis(2));
    }

    streaming.set(false);
    drop(decoder.lock().unwrap().take());
    context.pause();
    let expected = (seconds * fps) as u64;
    println!(
        "probe: done: {} received, {decoded} decoded ({good} good, {bad} wrong) at {}x{}, expected about {expected}; {haptics} haptic pulses",
        received.load(Ordering::Relaxed),
        size.0,
        size.1
    );
    let audio = *alvr_client_core::audio_meter::GAME_AUDIO.lock();
    println!(
        "probe: game audio: {} packets, {:.2} s of stereo, rms {:.4}, peak {}",
        audio.packets,
        audio.samples as f64 / 2.0 / 48_000.0,
        audio.rms(),
        audio.peak
    );
    std::process::exit(if streaming_since.is_some() && good * 2 >= expected { 0 } else { 1 });
}

/// Sends a head that turns 20 degrees per second and a still left controller, 3 times per frame.
fn tracking_loop(context: Arc<ClientCoreContext>, streaming: Arc<RelaxedAtomic>, fps: f32) {
    let fov = Fov { left: -FRAC_PI_4, right: FRAC_PI_4, up: FRAC_PI_4, down: -FRAC_PI_4 };
    let eye = |x: f32| ViewParams { pose: Pose { orientation: Quat::IDENTITY, position: Vec3::new(x, 0.0, 0.0) }, fov };
    context.send_view_params([eye(-0.032), eye(0.032)]);

    let origin = Instant::now();
    let mut deadline = Instant::now();
    let turn_rate = 20f32.to_radians();
    let mut squeezed = false;
    while streaming.value() {
        let t = origin.elapsed();
        // Squeeze the left grip for half a second every 3 seconds, as a game test may ask
        let squeeze = t.as_secs_f32() % 3.0 > 2.5;
        if squeeze != squeezed {
            squeezed = squeeze;
            let value = ButtonValue::Scalar(if squeeze { 1.0 } else { 0.0 });
            context.send_buttons(vec![ButtonEntry { path_id: *LEFT_SQUEEZE_VALUE_ID, value }]);
        }
        let head = DeviceMotion {
            pose: Pose { orientation: Quat::from_rotation_y(turn_rate * t.as_secs_f32()), position: Vec3::new(0.0, 1.6, 0.0) },
            linear_velocity: Vec3::ZERO,
            angular_velocity: Vec3::new(0.0, turn_rate, 0.0),
        };
        let left_hand = DeviceMotion {
            pose: Pose { orientation: Quat::IDENTITY, position: Vec3::new(-0.2, 1.2, -0.4) },
            linear_velocity: Vec3::ZERO,
            angular_velocity: Vec3::ZERO,
        };
        context.send_tracking(
            t,
            vec![(*HEAD_ID, head), (*HAND_LEFT_ID, left_hand)],
            [None, None],
            FaceData { eye_gazes: [None, None], fb_face_expression: None, htc_eye_expression: None, htc_lip_expression: None },
        );
        deadline += Duration::from_secs_f32(1.0 / fps / 3.0);
        thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }
}
