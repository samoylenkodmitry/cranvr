//! The ring of the headset's microphone: the host asks the client to record while a reader stamps the
//! ring, writes the client's datagrams in order with silence for a lost one, drops a late one and
//! stops the recording when the stamps stop. One test, because the ring is one for the process.

use super::*;
use crate::headset::native::{start, Config};
use cranvr_common::foveation::FoveationConfig;
use cranvr_proto::{
    link::{ClientLink, Incoming},
    message::{ClientCaps, ClientDatagram, HostWishes, Views},
    wire::{Codec, Fov, Pose},
};
use std::{net::TcpListener, sync::mpsc::channel};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A datagram whose frames all hold `value`
fn datagram(sequence: u32, value: i16) -> ClientDatagram {
    let pcm = (0..MICROPHONE_FRAMES).flat_map(|_| value.to_le_bytes()).collect();
    ClientDatagram::Microphone(Microphone { sequence, captured: now_ns(), pcm })
}

#[test]
fn the_host_records_the_headset_while_a_game_reads_the_ring() {
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("cranvr-native-microphone-test-{port}"));
    std::fs::create_dir_all(&dir).unwrap();
    let fov = Fov { left: -0.9, right: 0.8, up: 0.9, down: -1.0 };
    let config = Config {
        port,
        identity: cranvr_proto::identity::Identity::from_seed([8; 32]),
        pairings: Arc::new(crate::pairing::Pairings::load(&dir)),
        name: "Test Mac".into(),
        discovery_port: None,
        approval_wait: Duration::from_secs(30),
        approve_loopback: false,
        trust_all: false,
        loopback: false,
        cable: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        display_lock: false,
        desktop: Default::default(),
        audio: false,
        microphone: Some(dir.join("microphone.pcm")),
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
    let ring = RING.get().expect("the host made the ring");
    let header = std::fs::read(dir.join("microphone.pcm")).unwrap();
    assert_eq!(&header[..4], b"CRMC");
    assert_eq!(header.len(), HEADER + CAPACITY * 2);

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
        microphone: true,
    };
    let identity = cranvr_proto::identity::Identity::from_seed([9; 32]);
    let client = Arc::new(ClientLink::connect(([127, 0, 0, 1], port).into(), &caps, &identity, None, Duration::from_secs(10)).unwrap());
    let (to_test, wishes) = channel();
    let reader = Arc::clone(&client);
    thread::spawn(move || {
        reader.duplex.read_tcp(|incoming| {
            if let Incoming::Control(HostMsg::Microphone(record)) = incoming {
                to_test.send(record).ok();
            }
        })
    });
    // No game records: the client hears so when it connects
    assert_eq!(wishes.recv_timeout(Duration::from_secs(5)), Ok(false));

    // A game's stream stamps the ring every 10 ms, as Wine's driver does
    let stamping = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let stamper = {
        let stamping = Arc::clone(&stamping);
        thread::spawn(move || {
            while stamping.load(Ordering::Relaxed) {
                RING.get().unwrap().word(READER_BEAT).store(continuous_ns(), Ordering::Relaxed);
                thread::sleep(Duration::from_millis(10));
            }
        })
    };
    assert_eq!(wishes.recv_timeout(Duration::from_secs(2)), Ok(true), "the host asks the client to record");

    // Datagrams 0, 1, then 3 (2 is lost), then 2 (late), then 4. The session attaches after the
    // handshake, so the first datagrams go out until the ring moves.
    let start_pos = ring.word(WRITE_POS).load(Ordering::Relaxed);
    let mut sent = 0;
    while ring.word(WRITE_POS).load(Ordering::Relaxed) == start_pos && sent < 200 {
        client.duplex.send_datagram(&datagram(0, 100)).ok();
        sent += 1;
        thread::sleep(Duration::from_millis(10));
    }
    let first = ring.word(WRITE_POS).load(Ordering::Relaxed);
    assert_eq!(first - start_pos, MICROPHONE_FRAMES as u64, "the first datagram went into the ring");
    for (sequence, value) in [(1, 200), (3, 400), (2, 300), (4, 500)] {
        client.duplex.send_datagram(&datagram(sequence, value)).unwrap();
        thread::sleep(Duration::from_millis(20));
    }
    let end = ring.word(WRITE_POS).load(Ordering::Relaxed);
    assert_eq!(end - first, 4 * MICROPHONE_FRAMES as u64, "datagrams 1, a silence for 2, 3 and 4; the late 2 is dropped");
    let samples = std::fs::read(dir.join("microphone.pcm")).unwrap();
    let at = |frame: u64| {
        let i = HEADER + (frame as usize & (CAPACITY - 1)) * 2;
        i16::from_le_bytes([samples[i], samples[i + 1]])
    };
    let blocks: Vec<i16> = (0..4).map(|n| at(first + n * MICROPHONE_FRAMES as u64 + 7)).collect();
    assert_eq!(blocks, [200, 0, 400, 500]);

    // The game stops: the host ends the recording
    stamping.store(false, Ordering::Relaxed);
    stamper.join().unwrap();
    assert_eq!(wishes.recv_timeout(Duration::from_secs(2)), Ok(false), "the host stops the recording");
    std::fs::remove_dir_all(&dir).ok();
}
