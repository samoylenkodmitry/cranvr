//! The sealed session: what an attacker on the network can and cannot do. The tests drive a real
//! host with a hand-written client, so they also pin the wire format of the handshake and of the
//! sealed packets. They bind port 0 and wait up to ten seconds for each event.

mod common;

use common::{caps, host_identity, identity, meta, Rng};
use cranvr_proto::{
    fec::ParityController,
    identity::{Identity, KEY_LEN},
    link::{ClientLink, ConnectObserver, HostLink, HostListener, Incoming, MediaMode},
    media::{FrameReceiver, DEFAULT_SHARD_LEN},
    message::*,
    secure::{
        approval_commit, derive, sign_as_client, sign_as_host, transcript_hash, Channels, Ephemeral, HostHello, Role, HOST_HELLO_LEN, OVERHEAD,
    },
    wire::{encode, read_tcp_packet, Error, PacketBuf, KIND_CONTROL, KIND_DATAGRAM, KIND_HANDSHAKE},
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, Arc},
    thread,
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(10);

fn wishes() -> HostWishes {
    HostWishes {
        eye_width: 2064,
        eye_width_unfoveated: 1248,
        refresh_hz: 120.0,
        bitrate_bps: 100_000_000,
        codecs: vec![Codec::Hevc],
        foveation: None,
        full_range: true,
        slices: 1,
    }
}

use cranvr_proto::wire::Codec;

/// A client written by hand against the wire format. It can send what the real client never sends.
struct Raw {
    tcp: TcpStream,
    channels: Channels,
    transcript: [u8; 32],
}

impl Raw {
    /// Runs the version exchange and the three flights. `claimed` is the key that flight 3 names.
    /// `signer` signs it. They differ in the tests of a forged identity.
    fn handshake(addr: std::net::SocketAddr, claimed: &Identity, signer: &Identity) -> std::io::Result<Raw> {
        Self::handshake_with_versions(addr, claimed, signer, VersionRange::OURS)
    }

    /// The same, for a client that speaks only the versions in `range`.
    fn handshake_with_versions(addr: std::net::SocketAddr, claimed: &Identity, signer: &Identity, range: VersionRange) -> std::io::Result<Raw> {
        let mut tcp = TcpStream::connect(addr)?;
        tcp.set_read_timeout(Some(WAIT))?;
        let client_pre = client_preamble(range);
        tcp.write_all(&client_pre)?;
        let mut host_pre = [0u8; HOST_PREAMBLE_LEN];
        tcp.read_exact(&mut host_pre)?;
        let fresh = Ephemeral::new().unwrap();
        // Flight 1: a TCP packet of the handshake kind with the fresh key
        let mut flight = vec![];
        flight.extend_from_slice(&(33u32).to_le_bytes());
        flight.push(KIND_HANDSHAKE);
        flight.extend_from_slice(&fresh.public());
        tcp.write_all(&flight)?;
        let mut buf = Vec::new();
        read_tcp_packet(&mut tcp, &mut buf).map_err(std::io::Error::other)?;
        assert_eq!(buf[0], KIND_HANDSHAKE);
        assert_eq!(buf.len() - 1, HOST_HELLO_LEN);
        let hello = HostHello::parse(&buf[1..]).unwrap();
        let transcript = transcript_hash(&client_pre, &host_pre, &fresh.public(), &hello.ephemeral, &hello.key);
        let mut channels = derive(Role::Client, &fresh.agree(&hello.ephemeral).unwrap(), &transcript);
        // Flight 3, sealed
        let mut auth = PacketBuf::new();
        let body = auth.begin(KIND_HANDSHAKE);
        body.extend_from_slice(&claimed.public_key());
        body.extend_from_slice(&sign_as_client(signer, &transcript));
        tcp.write_all(channels.tcp_tx.seal(&mut auth).unwrap())?;
        Ok(Raw { tcp, channels, transcript })
    }

    fn send_control(&mut self, msg: &ClientMsg) {
        let mut buf = PacketBuf::new();
        encode(msg, buf.begin(KIND_CONTROL)).unwrap();
        self.tcp.write_all(self.channels.tcp_tx.seal(&mut buf).unwrap()).unwrap();
    }

    fn read_control(&mut self) -> Result<HostMsg, Error> {
        let mut buf = Vec::new();
        read_tcp_packet(&mut self.tcp, &mut buf)?;
        let content = self.channels.tcp_rx.open(&mut buf)?;
        assert_eq!(content[0], KIND_CONTROL);
        cranvr_proto::wire::decode(&content[1..])
    }
}

/// A host that serves one client. It gives the `Incoming` datagrams of the client's UDP path and
/// the host link so that a test can read the counters.
struct Served {
    link: Arc<HostLink>,
    datagrams: mpsc::Receiver<ClientDatagram>,
    ended: mpsc::Receiver<Error>,
    /// The sealed channel of the hand-written client and its sockets
    raw: Raw,
    udp: UdpSocket,
}

fn serve() -> Served {
    let listener = HostListener::bind("127.0.0.1:0", host_identity()).unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || {
        let pending = listener.accept().unwrap();
        assert_eq!(pending.client_key, identity().public_key());
        let stream = negotiate(&wishes(), &pending.caps).unwrap();
        pending.accept(stream, MediaMode::Udp, DEFAULT_SHARD_LEN).unwrap()
    });
    let mut raw = Raw::handshake(addr, &identity(), &identity()).unwrap();
    raw.send_control(&ClientMsg::Hello(caps()));
    let welcome = match raw.read_control().unwrap() {
        HostMsg::Welcome(w) => w,
        other => panic!("{other:?}"),
    };
    let MediaPlan::Udp { port } = welcome.media else { panic!("UDP media expected") };
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.connect(("127.0.0.1", port)).unwrap();
    udp.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    raw.tcp.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    // The hello datagram, sealed, until MediaReady comes
    loop {
        let mut out = PacketBuf::new();
        encode(&ClientDatagram::Hello { session: welcome.session }, out.begin(KIND_DATAGRAM)).unwrap();
        raw.channels.udp_tx.seal(&mut out).unwrap();
        udp.send(out.datagram()).unwrap();
        match raw.read_control() {
            Ok(HostMsg::MediaReady) => break,
            Err(Error::Timeout) => {}
            other => panic!("{other:?}"),
        }
    }
    raw.tcp.set_read_timeout(None).unwrap();
    let link = Arc::new(host.join().unwrap());
    let (to_test, datagrams) = mpsc::channel();
    let (to_ended, ended) = mpsc::channel();
    let reader = Arc::clone(&link);
    thread::spawn(move || {
        reader.duplex.read_udp(|incoming| {
            if let Incoming::Datagram(d) = incoming {
                to_test.send(d).ok();
            }
        })
    });
    let reader = Arc::clone(&link);
    thread::spawn(move || {
        let reason = reader.duplex.read_tcp(|_| {});
        to_ended.send(reason).ok();
    });
    Served { link, datagrams, ended, raw, udp }
}

fn ping(id: u32) -> ClientDatagram {
    ClientDatagram::Ping(Ping { id, sent: 1_000 + id as u64 })
}

impl Served {
    /// Seals a ping on the client's UDP path and returns the datagram without sending it
    fn seal(&self, id: u32) -> Vec<u8> {
        let mut out = PacketBuf::new();
        encode(&ping(id), out.begin(KIND_DATAGRAM)).unwrap();
        self.raw.channels.udp_tx.seal(&mut out).unwrap();
        out.datagram().to_vec()
    }

    fn send(&self, datagram: &[u8]) {
        self.udp.send(datagram).unwrap();
    }

    /// The pings that arrive within a short time, in the order of arrival
    fn heard(&self) -> Vec<u32> {
        let mut ids = vec![];
        while let Ok(d) = self.datagrams.recv_timeout(Duration::from_millis(300)) {
            if let ClientDatagram::Ping(p) = d {
                ids.push(p.id);
            }
        }
        ids
    }

    fn rejected(&self) -> u64 {
        self.link.duplex.crypto_stats().rejected
    }
}

#[test]
fn a_datagram_that_was_changed_on_the_way_is_dropped() {
    let host = serve();
    let good = host.seal(1);
    // The counter, the encrypted content and the tag: one flipped bit in each
    for at in [0, 8 + 1, good.len() - 1] {
        let mut bad = good.clone();
        bad[at] ^= 0x10;
        host.send(&bad);
    }
    assert_eq!(host.heard(), Vec::<u32>::new());
    assert_eq!(host.rejected(), 3);
    // A truncated datagram and a longer one fail too
    host.send(&good[..good.len() - 1]);
    host.send(&[good.clone(), vec![0]].concat());
    assert_eq!(host.heard(), Vec::<u32>::new());
    // The unchanged datagram still arrives: the changed ones did not use up its counter
    host.send(&good);
    assert_eq!(host.heard(), vec![1]);
    host.link.duplex.close();
}

#[test]
fn a_replayed_datagram_is_dropped_and_a_reordered_one_is_not() {
    let host = serve();
    let first = host.seal(1);
    let second = host.seal(2);
    let third = host.seal(3);
    // Reordered inside the window: all three arrive
    host.send(&third);
    host.send(&first);
    host.send(&second);
    assert_eq!(host.heard(), vec![3, 1, 2]);
    // The same datagrams again
    for datagram in [&first, &second, &third, &third] {
        host.send(datagram);
    }
    assert_eq!(host.heard(), Vec::<u32>::new());
    assert_eq!(host.rejected(), 4);
    host.link.duplex.close();
}

#[test]
fn a_datagram_older_than_the_window_is_dropped() {
    let host = serve();
    let old = host.seal(1);
    // Far more datagrams follow than the window spans. Only the last one is sent.
    let mut newest = vec![];
    for id in 2..6_000 {
        newest = host.seal(id);
    }
    host.send(&newest);
    assert_eq!(host.heard(), vec![5_999]);
    host.send(&old);
    assert_eq!(host.heard(), Vec::<u32>::new());
    assert_eq!(host.rejected(), 1);
    host.link.duplex.close();
}

#[test]
fn a_forged_datagram_with_a_far_counter_does_not_move_the_window() {
    let host = serve();
    let mut forged = host.seal(9);
    forged[..8].copy_from_slice(&(1u64 << 40).to_le_bytes());
    host.send(&forged);
    // A forger without the key cannot even make a packet that costs more than the failed tag check
    let junk = Rng(3).bytes(64);
    host.send(&junk);
    let genuine = host.seal(10);
    host.send(&genuine);
    assert_eq!(host.heard(), vec![10]);
    assert_eq!(host.rejected(), 2);
    host.link.duplex.close();
}

#[test]
fn a_datagram_of_the_wrong_direction_is_dropped() {
    let host = serve();
    // The host's own key seals what it sends to the client. A packet of that key does not open at
    // the host: an attacker cannot bounce the host's datagrams back at it.
    let (to_client, client_side) = mpsc::channel();
    // Receive what the host sends to the client by UDP, then send it back unchanged
    host.udp.set_read_timeout(Some(WAIT)).unwrap();
    let pong = HostDatagram::Pong(Pong { id: 1, ping_sent: 2, received: 3, sent: 4 });
    host.link.duplex.send_datagram(&pong).unwrap();
    let mut buf = [0u8; 2048];
    let n = host.udp.recv(&mut buf).unwrap();
    to_client.send(buf[..n].to_vec()).unwrap();
    let echoed = client_side.recv().unwrap();
    host.send(&echoed);
    assert_eq!(host.heard(), Vec::<u32>::new());
    assert_eq!(host.rejected(), 1);
    host.link.duplex.close();
}

#[test]
fn what_the_host_sends_does_not_show_in_clear_text() {
    let host = serve();
    host.udp.set_read_timeout(Some(WAIT)).unwrap();
    // A sound datagram with a marker that an eavesdropper could look for
    let marker = b"SECRET-PCM-MARKER";
    host.link.duplex.send_datagram(&HostDatagram::Audio(Audio { sequence: 1, host_time: 2, pcm: marker.to_vec() })).unwrap();
    let mut buf = [0u8; 2048];
    let n = host.udp.recv(&mut buf).unwrap();
    assert!(n > marker.len() + OVERHEAD);
    assert!(!buf[..n].windows(marker.len()).any(|w| w == marker));
    host.link.duplex.close();
}

#[test]
fn a_record_that_was_changed_on_the_tcp_path_ends_the_session() {
    let mut host = serve();
    let mut buf = PacketBuf::new();
    encode(&ClientMsg::RequestKeyFrame { stream: 0, frame: 7 }, buf.begin(KIND_CONTROL)).unwrap();
    let mut record = host.raw.channels.tcp_tx.seal(&mut buf).unwrap().to_vec();
    let last = record.len() - 1;
    record[last] ^= 1;
    host.raw.tcp.write_all(&record).unwrap();
    assert!(matches!(host.ended.recv_timeout(WAIT).unwrap(), Error::Crypto(_)));
}

#[test]
fn a_record_that_was_repeated_on_the_tcp_path_ends_the_session() {
    let mut host = serve();
    let mut buf = PacketBuf::new();
    encode(&ClientMsg::Bye, buf.begin(KIND_CONTROL)).unwrap();
    // An attacker that sits in the stream sends the same valid record twice
    let record = host.raw.channels.tcp_tx.seal(&mut buf).unwrap().to_vec();
    host.raw.tcp.write_all(&record).unwrap();
    host.raw.tcp.write_all(&record).unwrap();
    assert!(matches!(host.ended.recv_timeout(WAIT).unwrap(), Error::Crypto(_)));
}

#[test]
fn a_client_cannot_claim_the_key_of_another_client() {
    let listener = HostListener::bind("127.0.0.1:0", host_identity()).unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || listener.accept().err());
    // The client names the key of `identity()` and signs with another seed
    let other = Identity::from_seed([8; KEY_LEN]);
    let mut raw = Raw::handshake(addr, &identity(), &other).unwrap();
    raw.send_control(&ClientMsg::Hello(caps()));
    assert!(matches!(host.join().unwrap(), Some(Error::Crypto(_))));
}

#[test]
fn the_proof_of_one_session_does_not_serve_in_another() {
    // Record the signature that a client made in a first session
    let listener = HostListener::bind("127.0.0.1:0", host_identity()).unwrap();
    let addr = listener.local_addr().unwrap();
    let first = thread::spawn({
        let listener = listener;
        move || {
            let first = listener.accept();
            let second = listener.accept();
            (first.err(), second.err())
        }
    });
    let mut one = Raw::handshake(addr, &identity(), &identity()).unwrap();
    one.send_control(&ClientMsg::Hello(caps()));
    let signature = sign_as_client(&identity(), &one.transcript);
    // A second session in which the attacker repeats the first session's signature
    let mut tcp = TcpStream::connect(addr).unwrap();
    tcp.set_read_timeout(Some(WAIT)).unwrap();
    let client_pre = client_preamble(VersionRange::OURS);
    tcp.write_all(&client_pre).unwrap();
    let mut host_pre = [0u8; HOST_PREAMBLE_LEN];
    tcp.read_exact(&mut host_pre).unwrap();
    let fresh = Ephemeral::new().unwrap();
    tcp.write_all(&[&33u32.to_le_bytes()[..], &[KIND_HANDSHAKE], &fresh.public()].concat()).unwrap();
    let mut buf = Vec::new();
    read_tcp_packet(&mut tcp, &mut buf).unwrap();
    let hello = HostHello::parse(&buf[1..]).unwrap();
    let transcript = transcript_hash(&client_pre, &host_pre, &fresh.public(), &hello.ephemeral, &hello.key);
    assert_ne!(transcript, one.transcript);
    let mut channels = derive(Role::Client, &fresh.agree(&hello.ephemeral).unwrap(), &transcript);
    let mut auth = PacketBuf::new();
    let body = auth.begin(KIND_HANDSHAKE);
    body.extend_from_slice(&identity().public_key());
    body.extend_from_slice(&signature);
    tcp.write_all(channels.tcp_tx.seal(&mut auth).unwrap()).unwrap();
    let mut hello_packet = PacketBuf::new();
    encode(&ClientMsg::Hello(caps()), hello_packet.begin(KIND_CONTROL)).unwrap();
    tcp.write_all(channels.tcp_tx.seal(&mut hello_packet).unwrap()).unwrap();
    let (first, second) = first.join().unwrap();
    assert!(first.is_none(), "the honest first session passes");
    assert!(matches!(second, Some(Error::Crypto(_))), "{second:?}");
}

#[test]
fn a_client_refuses_a_host_that_is_not_the_one_it_paired_with() {
    let listener = HostListener::bind("127.0.0.1:0", host_identity()).unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || listener.accept().err());
    // The client remembers another key for this host
    let pinned = Identity::from_seed([42; KEY_LEN]).public_key();
    let result = ClientLink::connect(addr, &caps(), &identity(), Some(pinned), WAIT);
    assert!(matches!(result, Err(Error::HostKeyChanged)), "{:?}", result.err());
    // The client said nothing about itself: the host never got a Hello
    assert!(host.join().unwrap().is_some());
}

#[test]
fn a_client_accepts_the_host_that_it_paired_with_and_reports_its_key() {
    let listener = HostListener::bind("127.0.0.1:0", host_identity()).unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || {
        let pending = listener.accept().unwrap();
        let stream = negotiate(&wishes(), &pending.caps).unwrap();
        pending.accept(stream, MediaMode::Tcp, 60_000).unwrap()
    });
    let pinned = host_identity().public_key();
    let client = ClientLink::connect(addr, &caps(), &identity(), Some(pinned), WAIT).unwrap();
    assert_eq!(client.host_key, pinned);
    let host = host.join().unwrap();
    assert_eq!(host.client_key, identity().public_key());
}

#[test]
fn a_man_in_the_middle_that_swaps_the_fresh_key_breaks_the_host_signature() {
    // A fake host that signs over a transcript with another fresh key than the one that it sends.
    // A relay between a client and the real host would have to do that.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let fake = thread::spawn(move || {
        let (mut tcp, _) = listener.accept().unwrap();
        let mut client_pre = [0u8; CLIENT_PREAMBLE_LEN];
        tcp.read_exact(&mut client_pre).unwrap();
        let host_pre = host_preamble(Some(VersionRange::OURS.max), VersionRange::OURS);
        tcp.write_all(&host_pre).unwrap();
        let mut buf = Vec::new();
        read_tcp_packet(&mut tcp, &mut buf).unwrap();
        let client_fresh: [u8; 32] = buf[1..].try_into().unwrap();
        let signed_fresh = Ephemeral::new().unwrap();
        let sent_fresh = Ephemeral::new().unwrap();
        let host = host_identity();
        let transcript = transcript_hash(&client_pre, &host_pre, &client_fresh, &signed_fresh.public(), &host.public_key());
        let hello = HostHello {
            ephemeral: sent_fresh.public(),
            key: host.public_key(),
            signature: cranvr_proto::secure::sign_as_host(&host, &transcript),
        };
        let body = hello.to_bytes();
        tcp.write_all(&[&(1 + body.len() as u32).to_le_bytes()[..], &[KIND_HANDSHAKE], &body].concat()).unwrap();
        // The client closes without a word
        let mut rest = Vec::new();
        tcp.read_to_end(&mut rest).unwrap();
        rest
    });
    let result = ClientLink::connect(addr, &caps(), &identity(), Some(host_identity().public_key()), WAIT);
    assert!(matches!(result, Err(Error::Crypto(_))), "{:?}", result.err());
    assert!(fake.join().unwrap().is_empty(), "the client sent nothing after the bad flight");
}

#[test]
fn a_host_of_version_3_is_refused_by_the_client() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let old = thread::spawn(move || {
        let (mut tcp, _) = listener.accept().unwrap();
        let mut pre = [0u8; CLIENT_PREAMBLE_LEN];
        tcp.read_exact(&mut pre).unwrap();
        // The old host's answer: no common version, as its range is 3 to 3. It speaks in clear text.
        tcp.write_all(&host_preamble(None, VersionRange { min: 3, max: 3 })).unwrap();
    });
    let result = ClientLink::connect(addr, &caps(), &identity(), None, WAIT);
    assert!(matches!(result, Err(Error::Version { theirs: (3, 3), .. })), "{:?}", result.err());
    old.join().unwrap();
}

#[test]
fn a_flight_of_the_wrong_size_is_refused_before_it_sizes_a_buffer() {
    let listener = HostListener::bind("127.0.0.1:0", host_identity()).unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || listener.accept().err());
    let mut raw = TcpStream::connect(addr).unwrap();
    raw.write_all(&client_preamble(VersionRange::OURS)).unwrap();
    let mut answer = [0u8; HOST_PREAMBLE_LEN];
    raw.read_exact(&mut answer).unwrap();
    // A length of 16 MB from a peer that has proved nothing
    raw.write_all(&(16u32 << 20).to_le_bytes()).unwrap();
    assert!(matches!(host.join().unwrap(), Some(Error::Malformed(_))));
}

#[test]
fn a_unit_crosses_sealed_by_udp_and_by_tcp_with_loss_and_reordering_covered() {
    for mode in [MediaMode::Udp, MediaMode::Tcp] {
        let listener = HostListener::bind("127.0.0.1:0", host_identity()).unwrap();
        let addr = listener.local_addr().unwrap();
        let host = thread::spawn(move || {
            let pending = listener.accept().unwrap();
            let stream = negotiate(&wishes(), &pending.caps).unwrap();
            let shard = if mode == MediaMode::Tcp { 60_000 } else { DEFAULT_SHARD_LEN };
            pending.accept(stream, mode, shard).unwrap()
        });
        let mut c = caps();
        c.tcp_only = mode == MediaMode::Tcp;
        let client = Arc::new(ClientLink::connect(addr, &c, &identity(), None, WAIT).unwrap());
        let host = host.join().unwrap();
        let (to_test, inbox) = mpsc::channel();
        let reader = Arc::clone(&client);
        thread::spawn(move || {
            let mut receiver = FrameReceiver::new();
            let mut handler = |incoming: Incoming<'_, _>| {
                if let Incoming::Media(packet) = incoming {
                    if let Some(unit) = receiver.push(packet, 1).unwrap() {
                        to_test.send(unit.payload().to_vec()).ok();
                    }
                }
            };
            if mode == MediaMode::Udp {
                reader.duplex.read_udp(&mut handler);
            } else {
                reader.duplex.read_tcp(&mut handler);
            }
        });
        let payload = Rng(21).bytes(120_000);
        let mut got = None;
        for _ in 0..20 {
            host.send_unit(&meta(4), &payload, &ParityController::new(0.2, 0.5)).unwrap();
            if let Ok(unit) = inbox.recv_timeout(Duration::from_millis(500)) {
                got = Some(unit);
                break;
            }
        }
        assert_eq!(got.expect("the unit arrived"), payload);
        let stats = client.duplex.crypto_stats();
        // A 120 kB unit is 3 packets over TCP and about 100 over UDP
        let least = if mode == MediaMode::Udp { 50 } else { 2 };
        assert!(stats.opened > least && stats.open_ns > 0 && stats.rejected == 0, "{stats:?}");
        assert!(host.duplex.crypto_stats().sealed > least);
        host.duplex.close();
        client.duplex.close();
    }
}

/// The cost of sealing and opening at the size of a frame of the stream. Run it in release mode:
/// `cargo test --release -p cranvr-proto --test secure seal_cost -- --ignored --nocapture`
/// The Mac's other work moves the numbers, so the test prints the median and the fastest frame.
#[test]
#[ignore]
fn seal_cost() {
    use cranvr_proto::{
        fec::parity_for,
        media::FrameSender,
        secure::{derive, Role},
    };
    use std::time::Instant;
    let shared = [1u8; 32];
    let transcript = [2u8; 32];
    let host = derive(Role::Host, &shared, &transcript);
    let mut client = derive(Role::Client, &shared, &transcript);
    let mut sender = FrameSender::new(DEFAULT_SHARD_LEN).unwrap();
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], v[0])
    };
    // An average frame of 100 Mbps at 120 Hz is 104 kB. A 1080p frame at 50 Mbps and 60 Hz is as
    // large. A key frame is several times that. The parity is 10 percent.
    for (name, bytes) in [("test scene frame, 12 kB", 12_000usize), ("average frame, 104 kB", 104_000usize), ("key frame, 600 kB", 600_000)] {
        let payload = Rng(5).bytes(bytes);
        let (mut send_plain, mut send_sealed, mut seal, mut open) = (vec![], vec![], vec![], vec![]);
        let (mut packets, mut wire_bytes) = (0usize, 0usize);
        let mut sealed = Vec::<Vec<u8>>::new();
        for round in 0..600u32 {
            // The sender's work without sealing: cutting the unit and the parity
            let t = Instant::now();
            sender.send_unit(&meta(round), &payload, |n| parity_for(n, 0.1), |_| Ok(())).unwrap();
            let plain = t.elapsed().as_nanos() as f64;
            // The same with each packet sealed in its buffer and nothing else
            let t = Instant::now();
            sender.send_unit(&meta(round), &payload, |n| parity_for(n, 0.1), |packet| host.udp_tx.seal(packet)).unwrap();
            let sealed_total = t.elapsed().as_nanos() as f64;
            // The sealing alone, packet by packet. The sealed datagrams go to the receiver.
            sealed.clear();
            let mut sealing = 0u128;
            sender
                .send_unit(&meta(round), &payload, |n| parity_for(n, 0.1), |packet| {
                    let t = Instant::now();
                    host.udp_tx.seal(packet)?;
                    sealing += t.elapsed().as_nanos();
                    sealed.push(packet.datagram().to_vec());
                    Ok(())
                })
                .unwrap();
            let t = Instant::now();
            for datagram in &mut sealed {
                client.udp_rx.open(datagram).unwrap();
            }
            let opening = t.elapsed().as_nanos() as f64;
            if round >= 100 {
                send_plain.push(plain / 1e3);
                send_sealed.push(sealed_total / 1e3);
                seal.push(sealing as f64 / 1e3);
                open.push(opening / 1e3);
                packets = sealed.len();
                wire_bytes = sealed.iter().map(Vec::len).sum();
            }
        }
        let (plain, plain_min) = median(&mut send_plain);
        let (with, with_min) = median(&mut send_sealed);
        let (seal, seal_min) = median(&mut seal);
        let (open, open_min) = median(&mut open);
        println!("{name}: {packets} packets, {wire_bytes} bytes on the wire (median, fastest, microseconds)");
        println!("  send_unit without sealing {plain:.0}, {plain_min:.0}; with sealing {with:.0}, {with_min:.0}");
        println!("  sealing alone {seal:.0}, {seal_min:.0}; opening {open:.0}, {open_min:.0}");
        println!("  per packet: seal {:.2}, open {:.2}; open throughput {:.2} GB/s", seal / packets as f64, open / packets as f64, wire_bytes as f64 / open / 1e3);
    }
}

/// What the codes of the two legs of a connection through a middle machine show. The middle machine
/// ends the client's connection with a host of its own and opens a connection to the real host as a
/// client. Each leg has its own keys.
#[test]
fn a_man_in_the_middle_that_ends_both_legs_shows_two_different_codes() {
    struct Show(mpsc::Sender<(u32, String)>);
    impl ConnectObserver for Show {
        fn waiting(&mut self, code: u32, host_name: &str) {
            self.0.send((code, host_name.to_owned())).unwrap();
        }
        fn cancelled(&self) -> bool {
            // The test ends the waits by dropping the sockets
            false
        }
    }
    let real = HostListener::bind("127.0.0.1:0", host_identity()).unwrap();
    let real_addr = real.local_addr().unwrap();
    let middle = HostListener::bind("127.0.0.1:0", Identity::from_seed([66; KEY_LEN])).unwrap();
    let middle_addr = middle.local_addr().unwrap();
    // The real host asks its user and learns the code of its leg
    let real_host = thread::spawn(move || {
        let mut pending = real.accept().unwrap();
        let code = pending.hold_for_approval("Real Mac").unwrap();
        pending.reject(RejectReason::Denied);
        code
    });
    // The middle machine takes the client's connection and opens its own to the real host
    let middle_host = thread::spawn(move || {
        let mut pending = middle.accept().unwrap();
        let (to_test, from_leg_two) = mpsc::channel();
        let leg_two = thread::spawn(move || {
            ClientLink::connect_with(real_addr, &caps(), &Identity::from_seed([77; KEY_LEN]), None, WAIT, &mut Show(to_test)).err()
        });
        let (code_two, _) = from_leg_two.recv_timeout(WAIT).expect("the real host holds the middle machine");
        let code_one = pending.hold_for_approval("Real Mac").unwrap();
        pending.reject(RejectReason::Denied);
        assert!(matches!(leg_two.join().unwrap(), Some(Error::Rejected(RejectReason::Denied))));
        (code_one, code_two)
    });
    let (to_test, from_client) = mpsc::channel();
    let client = ClientLink::connect_with(middle_addr, &caps(), &identity(), None, WAIT, &mut Show(to_test));
    assert!(matches!(client, Err(Error::Rejected(RejectReason::Denied))));
    let (client_code, host_name) = from_client.recv_timeout(WAIT).unwrap();
    assert_eq!(host_name, "Real Mac");
    let real_code = real_host.join().unwrap();
    let (middle_code_one, middle_code_two) = middle_host.join().unwrap();
    // Each leg agrees with itself
    assert_eq!(client_code, middle_code_one);
    assert_eq!(real_code, middle_code_two);
    // The user compares the headset's code with the Mac's code: they differ. They agree with a
    // chance of one in a million.
    assert_ne!(client_code, real_code, "the two legs showed the same code");
}

#[test]
fn a_client_of_version_4_gets_no_approval_and_the_host_says_why() {
    let listener = HostListener::bind("127.0.0.1:0", host_identity()).unwrap();
    let addr = listener.local_addr().unwrap();
    let host = thread::spawn(move || {
        let mut pending = listener.accept().unwrap();
        let version = pending.version;
        let held = pending.hold_for_approval("Mac");
        (version, held.err())
    });
    let mut raw = Raw::handshake_with_versions(addr, &identity(), &identity(), VersionRange { min: 4, max: 4 }).unwrap();
    raw.send_control(&ClientMsg::Hello(caps()));
    let (version, held) = host.join().unwrap();
    assert_eq!(version, 4);
    assert!(matches!(held, Some(Error::Unexpected(_))), "{held:?}");
}

/// A host written by hand. It runs the handshake as `versions` say, reads the client's key and
/// its Hello, and returns the sealed channel for the test to continue.
struct FakeHost {
    tcp: TcpStream,
    channels: Channels,
    transcript: [u8; 32],
    client_key: [u8; 32],
}

impl FakeHost {
    fn accept(listener: &TcpListener, versions: VersionRange) -> FakeHost {
        let (mut tcp, _) = listener.accept().unwrap();
        let mut client_pre = [0u8; CLIENT_PREAMBLE_LEN];
        tcp.read_exact(&mut client_pre).unwrap();
        let host_pre = host_preamble(Some(versions.max), versions);
        tcp.write_all(&host_pre).unwrap();
        let mut buf = Vec::new();
        read_tcp_packet(&mut tcp, &mut buf).unwrap();
        let client_fresh: [u8; 32] = buf[1..].try_into().unwrap();
        let fresh = Ephemeral::new().unwrap();
        let host = host_identity();
        let transcript = transcript_hash(&client_pre, &host_pre, &client_fresh, &fresh.public(), &host.public_key());
        let hello = HostHello { ephemeral: fresh.public(), key: host.public_key(), signature: sign_as_host(&host, &transcript) };
        let body = hello.to_bytes();
        tcp.write_all(&[&(1 + body.len() as u32).to_le_bytes()[..], &[KIND_HANDSHAKE], &body].concat()).unwrap();
        let mut channels = derive(Role::Host, &fresh.agree(&client_fresh).unwrap(), &transcript);
        // The client's key, then its Hello
        read_tcp_packet(&mut tcp, &mut buf).unwrap();
        let client_key: [u8; 32] = channels.tcp_rx.open(&mut buf).unwrap()[1..33].try_into().unwrap();
        read_tcp_packet(&mut tcp, &mut buf).unwrap();
        channels.tcp_rx.open(&mut buf).unwrap();
        FakeHost { tcp, channels, transcript, client_key }
    }

    fn send(&mut self, msg: &HostMsg) {
        let mut out = PacketBuf::new();
        encode(msg, out.begin(KIND_CONTROL)).unwrap();
        self.tcp.write_all(self.channels.tcp_tx.seal(&mut out).unwrap()).unwrap();
    }

    fn read(&mut self) -> ClientMsg {
        let mut buf = Vec::new();
        read_tcp_packet(&mut self.tcp, &mut buf).unwrap();
        let content = self.channels.tcp_rx.open(&mut buf).unwrap();
        cranvr_proto::wire::decode(&content[1..]).unwrap()
    }
}

#[test]
fn a_host_that_reveals_another_number_than_it_committed_to_is_caught() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let fake = thread::spawn(move || {
        let mut host = FakeHost::accept(&listener, VersionRange::OURS);
        // The commitment is to one number, and the reveal shows another
        let commit = approval_commit(&[1; 32], &host.transcript, &host.client_key);
        host.send(&HostMsg::AwaitApproval { host_name: "Mac".into(), commit });
        assert!(matches!(host.read(), ClientMsg::PairNonce(_)));
        host.send(&HostMsg::PairReveal([2; 32]));
    });
    struct Never;
    impl ConnectObserver for Never {
        fn waiting(&mut self, _code: u32, _host_name: &str) {
            panic!("the client showed a code for a commitment that does not hold");
        }
    }
    let result = ClientLink::connect_with(addr, &caps(), &identity(), None, WAIT, &mut Never);
    assert!(matches!(result, Err(Error::Crypto(_))), "{:?}", result.err());
    fake.join().unwrap();
}

#[test]
fn a_host_of_version_4_that_does_not_know_the_client_is_the_side_to_update() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let old = thread::spawn(move || {
        let mut host = FakeHost::accept(&listener, VersionRange { min: 4, max: 4 });
        // What a version 4 host did for a key that it did not know
        host.send(&HostMsg::Reject(RejectReason::NotPaired));
    });
    let result = ClientLink::connect(addr, &caps(), &identity(), None, WAIT);
    let error = result.err().expect("the old host refuses");
    assert!(matches!(error, Error::Version { ours: (4, 6), theirs: (4, 4) }), "{error:?}");
    assert_eq!(error.peer_must_update(), Some(true));
    old.join().unwrap();
}

#[test]
fn the_older_side_is_the_one_to_update() {
    let ours = VersionRange { min: 4, max: 5 };
    assert!(ours.peer_is_older(VersionRange { min: 4, max: 4 }));
    assert!(ours.peer_is_older(VersionRange { min: 3, max: 3 }));
    assert!(!ours.peer_is_older(VersionRange { min: 6, max: 7 }));
    assert!(!ours.peer_is_older(VersionRange { min: 4, max: 5 }));
}
