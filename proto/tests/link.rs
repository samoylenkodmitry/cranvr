//! A session over real sockets on loopback: the handshake, datagrams both ways, media over UDP
//! and over TCP. The tests bind port 0, so they run beside other work. They wait up to ten
//! seconds for each message and assert nothing about speed.

mod common;

use common::{caps, meta, Rng};
use cranvr_proto::{
    fec::ParityController,
    link::{ClientLink, HostLink, HostListener, Incoming, MediaMode},
    media::{FrameReceiver, DEFAULT_SHARD_LEN},
    message::*,
    wire::{Codec, Error},
    PROTOCOL_VERSION,
};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{mpsc, Arc},
    thread,
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(10);

fn wishes() -> HostWishes {
    HostWishes {
        eye_width: 2064,
        refresh_hz: 120.0,
        bitrate_bps: 100_000_000,
        codecs: vec![Codec::Av1, Codec::Hevc],
        foveation: Some(cranvr_common::foveation::FoveationConfig::default().into()),
        full_range: true,
        slices: 1,
    }
}

/// Connects a client to a host on loopback and returns both ends.
fn session(mode: MediaMode, client_caps: ClientCaps) -> (HostLink, ClientLink) {
    let listener = HostListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || {
        let pending = listener.accept().unwrap();
        let stream = negotiate(&wishes(), &pending.caps).unwrap();
        let shard = if mode == MediaMode::Tcp { 60_000 } else { DEFAULT_SHARD_LEN };
        pending.accept(stream, mode, shard).unwrap()
    });
    let client = ClientLink::connect(addr, &client_caps, WAIT).unwrap();
    (host.join().unwrap(), client)
}

#[test]
fn the_handshake_agrees_on_a_stream() {
    let (host, client) = session(MediaMode::Udp, caps());
    assert_eq!(host.version, PROTOCOL_VERSION);
    assert_eq!(client.welcome.version, PROTOCOL_VERSION);
    assert_eq!(client.welcome.session, host.session);
    let stream = &client.welcome.stream;
    assert_eq!(stream, &host.stream);
    // AV1 is first in the host's list, but the client does not decode it
    assert_eq!(stream.codec, Codec::Hevc);
    assert_eq!(stream.refresh_hz, 120.0);
    assert_eq!(stream.eye_size, [2064, 2208]);
    assert!(stream.foveation.is_some());
    assert!(matches!(client.welcome.media, MediaPlan::Udp { port } if port != 0));
    assert!(host.duplex.has_udp() && client.duplex.has_udp());
}

#[test]
fn control_messages_cross_in_both_directions() {
    let (host, client) = session(MediaMode::Udp, caps());
    let host = Arc::new(host);
    let client = Arc::new(client);
    let (to_host, host_inbox) = mpsc::channel();
    let (to_client, client_inbox) = mpsc::channel();
    {
        let host = Arc::clone(&host);
        thread::spawn(move || {
            host.duplex.read_tcp(|m| {
                if let Incoming::Control(m) = m {
                    to_host.send(m).unwrap();
                }
            })
        });
        let client = Arc::clone(&client);
        thread::spawn(move || {
            client.duplex.read_tcp(|m| {
                if let Incoming::Control(m) = m {
                    to_client.send(m).unwrap();
                }
            })
        });
    }
    client.duplex.send_control(&ClientMsg::RequestKeyFrame { stream: 0, frame: 17 }).unwrap();
    assert_eq!(host_inbox.recv_timeout(WAIT).unwrap(), ClientMsg::RequestKeyFrame { stream: 0, frame: 17 });
    let config = HostMsg::DecoderConfig { stream: 0, codec: Codec::Hevc, config: vec![0, 0, 0, 1, 0x40, 1, 2, 3] };
    host.duplex.send_control(&config).unwrap();
    assert_eq!(client_inbox.recv_timeout(WAIT).unwrap(), config);
    // Closing one side ends the read loop of the other
    host.duplex.close();
    client.duplex.send_control(&ClientMsg::Bye).ok();
    assert_eq!(client_inbox.recv_timeout(WAIT).err(), Some(mpsc::RecvTimeoutError::Disconnected));
}

#[test]
fn datagrams_cross_by_udp_in_both_directions() {
    let (host, client) = session(MediaMode::Udp, caps());
    let host = Arc::new(host);
    let client = Arc::new(client);
    let (to_host, host_inbox) = mpsc::channel();
    let (to_client, client_inbox) = mpsc::channel();
    let h = Arc::clone(&host);
    thread::spawn(move || {
        h.duplex.read_udp(|m| {
            if let Incoming::Datagram(d) = m {
                to_host.send(d).unwrap();
            }
        })
    });
    let c = Arc::clone(&client);
    thread::spawn(move || {
        c.duplex.read_udp(|m| {
            if let Incoming::Datagram(d) = m {
                to_client.send(d).unwrap();
            }
        })
    });
    let tracking = Tracking { sample_time: 123_456_789, left: Some(Default::default()), ..Default::default() };
    // UDP may lose a datagram even on loopback when the machine is busy, so send until one comes
    let mut seen = None;
    for _ in 0..50 {
        client.duplex.send_datagram(&ClientDatagram::Tracking(tracking)).unwrap();
        if let Ok(d) = host_inbox.recv_timeout(Duration::from_millis(200)) {
            seen = Some(d);
            break;
        }
    }
    assert_eq!(seen, Some(ClientDatagram::Tracking(tracking)));
    let haptics = Haptics { hand: 1, duration_ns: 20_000_000, frequency: 160.0, amplitude: 0.5 };
    let mut seen = None;
    for _ in 0..50 {
        host.duplex.send_datagram(&HostDatagram::Haptics(haptics)).unwrap();
        if let Ok(d) = client_inbox.recv_timeout(Duration::from_millis(200)) {
            seen = Some(d);
            break;
        }
    }
    assert_eq!(seen, Some(HostDatagram::Haptics(haptics)));
    host.duplex.close();
    client.duplex.close();
}

#[test]
fn a_unit_crosses_by_udp_with_parity() {
    let (host, client) = session(MediaMode::Udp, caps());
    let client = Arc::new(client);
    let (to_test, inbox) = mpsc::channel();
    let c = Arc::clone(&client);
    thread::spawn(move || {
        let mut receiver = FrameReceiver::new();
        c.duplex.read_udp(|m| {
            if let Incoming::Media(packet) = m {
                if let Some(r) = receiver.push(packet, 1).unwrap() {
                    to_test.send((r.meta.clone(), r.payload().to_vec())).unwrap();
                }
            }
        })
    });
    let mut rng = Rng(4);
    let payload = rng.bytes(150_000);
    let mut m = meta(12);
    m.key = false;
    // Loopback UDP can drop packets under load. Parity covers a few. Send the frame again
    // when it did not arrive.
    let mut got = None;
    for _ in 0..20 {
        let stats = host.send_unit(&m, &payload, &ParityController::new(0.2, 0.5)).unwrap();
        assert!(stats.parity_packets > 0 && stats.data_packets > 100);
        if let Ok(r) = inbox.recv_timeout(Duration::from_millis(500)) {
            got = Some(r);
            break;
        }
    }
    let (meta_back, bytes) = got.expect("the unit arrived");
    assert_eq!(bytes, payload);
    assert_eq!(meta_back.frame, 12);
    assert_eq!(meta_back, m);
    host.duplex.close();
    client.duplex.close();
}

#[test]
fn the_tcp_fallback_carries_everything_over_one_connection() {
    let mut tcp_caps = caps();
    tcp_caps.tcp_only = true;
    let (host, client) = session(MediaMode::Tcp, tcp_caps);
    assert_eq!(client.welcome.media, MediaPlan::Tcp);
    assert!(!host.duplex.has_udp() && !client.duplex.has_udp());
    let host = Arc::new(host);
    let client = Arc::new(client);
    let (to_test, inbox) = mpsc::channel();
    let c = Arc::clone(&client);
    thread::spawn(move || {
        let mut receiver = FrameReceiver::new();
        c.duplex.read_tcp(|m| match m {
            Incoming::Media(packet) => {
                if let Some(r) = receiver.push(packet, 1).unwrap() {
                    to_test.send(Ok((r.meta.frame, r.payload().to_vec()))).unwrap();
                }
            }
            Incoming::Datagram(d) => to_test.send(Err(d)).unwrap(),
            Incoming::Control(_) => {}
        })
    });
    let (to_host, host_inbox) = mpsc::channel();
    let h = Arc::clone(&host);
    thread::spawn(move || {
        h.duplex.read_tcp(|m| {
            if let Incoming::Datagram(d) = m {
                to_host.send(d).unwrap();
            }
        })
    });
    // Parity is useless on TCP, so the host sends none even when asked
    let payload = Rng(8).bytes(500_000);
    let stats = host.send_unit(&meta(3), &payload, &ParityController::new(0.5, 0.5)).unwrap();
    assert_eq!(stats.parity_packets, 0);
    assert!(stats.data_packets < 20, "large packets over TCP: {}", stats.data_packets);
    assert_eq!(inbox.recv_timeout(WAIT).unwrap().unwrap(), (3, payload));
    // Datagram messages travel through the same connection, in order
    let ping = Ping { id: 5, sent: 99 };
    client.duplex.send_datagram(&ClientDatagram::Ping(ping)).unwrap();
    assert_eq!(host_inbox.recv_timeout(WAIT).unwrap(), ClientDatagram::Ping(ping));
    host.duplex.send_datagram(&HostDatagram::Pong(Pong { id: 5, ping_sent: 99, received: 100, sent: 101 })).unwrap();
    assert!(matches!(inbox.recv_timeout(WAIT).unwrap(), Err(HostDatagram::Pong(p)) if p.id == 5));
}

#[test]
fn a_host_that_finds_no_common_codec_rejects() {
    let listener = HostListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || {
        let pending = listener.accept().unwrap();
        let reason = negotiate(&wishes(), &pending.caps).unwrap_err();
        pending.reject(reason);
    });
    let mut only_h264 = caps();
    only_h264.codecs = vec![Codec::H264];
    let result = ClientLink::connect(addr, &only_h264, WAIT);
    assert!(matches!(result, Err(Error::Rejected(RejectReason::NoCommonCodec))), "{:?}", result.err());
    host.join().unwrap();
}

#[test]
fn a_peer_of_another_version_is_told_so() {
    let listener = HostListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || listener.accept().err());
    let mut raw = TcpStream::connect(addr).unwrap();
    raw.write_all(&client_preamble(VersionRange { min: 7, max: 9 })).unwrap();
    let mut answer = [0u8; HOST_PREAMBLE_LEN];
    raw.read_exact(&mut answer).unwrap();
    let (chosen, range) = parse_host_preamble(&answer).unwrap();
    assert_eq!(chosen, None);
    assert_eq!(range, VersionRange::OURS);
    assert!(matches!(host.join().unwrap(), Some(Error::Version { .. })));
}

#[test]
fn a_stranger_on_the_port_is_refused() {
    let listener = HostListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || listener.accept().err());
    let mut raw = TcpStream::connect(addr).unwrap();
    raw.write_all(b"GET / HT").unwrap();
    assert!(matches!(host.join().unwrap(), Some(Error::Malformed(_))));
}

#[test]
fn the_version_ranges_pick_the_highest_common_version() {
    let v = |min, max| VersionRange { min, max };
    assert_eq!(v(1, 3).common(v(2, 5)), Some(3));
    assert_eq!(v(1, 1).common(v(1, 4)), Some(1));
    assert_eq!(v(1, 2).common(v(3, 4)), None);
    assert_eq!(v(3, 4).common(v(1, 2)), None);
}

#[test]
fn the_host_picks_the_nearest_rate_and_keeps_the_aspect() {
    let mut c = caps();
    c.refresh_rates = vec![72.0, 90.0];
    let stream = negotiate(&wishes(), &c).unwrap();
    assert_eq!(stream.refresh_hz, 90.0);
    c.refresh_rates = vec![144.0];
    assert_eq!(negotiate(&wishes(), &c).unwrap().refresh_hz, 144.0);
    c.refresh_rates = vec![];
    assert_eq!(negotiate(&wishes(), &c).unwrap_err(), RejectReason::NoCommonRefreshRate);
    let mut w = wishes();
    w.eye_width = 1000;
    assert_eq!(negotiate(&w, &caps()).unwrap().eye_size, [1000, 1070]);
    c = caps();
    c.foveation = false;
    assert!(negotiate(&wishes(), &c).unwrap().foveation.is_none());
}
