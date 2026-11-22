use super::*;
use crate::headset::native::{start, Config};
use cranvr_common::foveation::FoveationConfig;
use cranvr_proto::{
    link::{ClientLink, Incoming},
    message::{ClientCaps, HostWishes, Views},
    wire::{Codec, Fov, Pose},
};
use std::{net::TcpListener, sync::mpsc::channel};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn the_sound_reaches_the_client_in_datagrams_of_five_milliseconds() {
    let port = free_port();
    let fov = Fov { left: -0.9, right: 0.8, up: 0.9, down: -1.0 };
    let identity = cranvr_proto::identity::Identity::from_seed([6; 32]);
    let config = Config {
        port,
        identity: cranvr_proto::identity::Identity::from_seed([7; 32]),
        pairings: Arc::new(crate::pairing::Pairings::load(&std::env::temp_dir().join(format!("cranvr-native-audio-test-{port}")))),
        name: "Test Mac".into(),
        discovery_port: None,
        approval_wait: Duration::from_secs(30),
        approve_loopback: false,
        trust_all: false,
        display_lock: false,
        desktop: Default::default(),
        audio: true,
        tcp_media: false,
        wishes: HostWishes {
            eye_width: 2064,
            eye_width_unfoveated: 0,
            refresh_hz: 120.0,
            bitrate_bps: 100_000_000,
            codecs: vec![Codec::Hevc],
            foveation: Some(FoveationConfig::default().into()),
            full_range: true,
            slices: 1,
        },
    };
    let (_headset, _events) = start(config).unwrap();
    let caps = ClientCaps {
        name: "Test headset".into(),
        eye_size: [2064, 2208],
        views: Views { fov: [fov, fov], eye_pose: [Pose::IDENTITY; 2] },
        refresh_rates: vec![120.0],
        codecs: vec![Codec::Hevc],
        decoders: 1,
        foveation: true,
        tcp_only: false,
        cable: false,
    };
    let client = Arc::new(ClientLink::connect(([127, 0, 0, 1], port).into(), &caps, &identity, None, Duration::from_secs(10)).unwrap());
    // The welcome tells the client that sound comes
    assert_eq!(client.welcome.stream.audio, Some(cranvr_proto::message::AudioFormat::HOST_CAPTURE));
    let (to_test, heard) = channel();
    let reader = Arc::clone(&client);
    thread::spawn(move || {
        reader.duplex.read_udp(|incoming| {
            if let Incoming::Datagram(HostDatagram::Audio(audio)) = incoming {
                to_test.send(audio).ok();
            }
        })
    });

    // 1000 frames: four datagrams of 240 frames and one of 40. The frames count up, so the test
    // sees their order.
    let mut pcm = Vec::new();
    for i in 0..1000u16 {
        pcm.extend(i.to_le_bytes());
        pcm.extend((i + 1000).to_le_bytes());
    }
    // The session attaches after the handshake: wait until it takes the sound
    let mut got = Vec::new();
    for _ in 0..100 {
        send(&pcm);
        while let Ok(audio) = heard.recv_timeout(Duration::from_millis(100)) {
            got.push(audio);
            if got.len() == 5 {
                break;
            }
        }
        if !got.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    // UDP on loopback may lose a datagram on a busy machine; the rest still has to be in order
    while let Ok(audio) = heard.recv_timeout(Duration::from_millis(300)) {
        got.push(audio);
    }
    assert_eq!(got.len(), 5, "{} datagrams", got.len());
    let sizes: Vec<usize> = got.iter().map(|a| a.pcm.len()).collect();
    assert_eq!(sizes, vec![960, 960, 960, 960, 160]);
    assert!(got.windows(2).all(|w| w[1].sequence == w[0].sequence + 1));
    assert_eq!(got.iter().flat_map(|a| a.pcm.iter().copied()).collect::<Vec<u8>>(), pcm);
    // Each datagram names the capture time of its first sample: 5 ms after the one before
    for pair in got.windows(2).take(3) {
        let gap = pair[1].host_time.saturating_sub(pair[0].host_time);
        assert!((4_900_000..=5_100_000).contains(&gap), "{gap}");
    }
}

#[test]
fn sound_with_no_client_is_counted_and_dropped() {
    let before = discarded_seconds();
    // Another test may have a client attached: only a detached state counts
    if CURRENT.lock().unwrap().is_none() {
        send(&[0u8; 4800]);
        assert!((discarded_seconds() - before - 0.025).abs() < 1e-6);
    }
}
