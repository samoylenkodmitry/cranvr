//! Sockets for a session: the TCP connection, the optional UDP socket, the handshake and the two
//! ends ([`HostLink`] and [`ClientLink`]). All calls block. A caller runs one thread per read loop
//! ([`Duplex::read_tcp`] and [`Duplex::read_udp`]). Any thread may send.
//!
//! The client dials the host. The host listens on one TCP port. After the version exchange both
//! sides run the key exchange of [`crate::secure`]. Everything after it is sealed. With UDP media
//! the host opens a UDP socket per session on an ephemeral port and names it in the welcome. The
//! client's first datagram teaches the host the client's address. The datagram is sealed, so only
//! the client that finished the handshake can teach it.

use crate::{
    fec::ParityController,
    identity::{random_bytes, Identity, KEY_LEN},
    media::{FrameMeta, FrameSender, SendStats},
    message::*,
    secure::{
        approval_code, approval_commit, derive, sign_as_client, sign_as_host, transcript_hash, verify_client, verify_host, Channels,
        ClientAuth, Ephemeral, HostHello, Role, TcpRx, TcpTx, UdpRx, UdpTx, CLIENT_HELLO_LEN, HOST_HELLO_LEN, OVERHEAD,
    },
    wire::{decode, encode, read_tcp_packet, read_tcp_packet_limited, Error, PacketBuf, KIND_CONTROL, KIND_DATAGRAM, KIND_HANDSHAKE, KIND_MEDIA},
    MTU, PROTOCOL_VERSION,
};
use serde::{de::DeserializeOwned, Serialize};
use socket2::{Domain, Protocol, SockRef, Socket, Type};
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    io::{self, Read, Write},
    marker::PhantomData,
    net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// The time that handshake reads and writes may take.
/// The longest time that a handshake takes in all
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a client that waits for its approval looks at its observer
const WAIT_POLL: Duration = Duration::from_millis(200);
/// How much longer than the host a client waits for the host's answer
const APPROVAL_SLACK: Duration = Duration::from_secs(15);
/// The longest packet that the handshake reads. The Hello with the capabilities is the largest.
const HANDSHAKE_PACKET_LIMIT: usize = 64 << 10;
/// A write to the TCP connection that takes longer than this ends the session: the peer is gone.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// The UDP read loop wakes this often to see that the session closed.
const UDP_POLL: Duration = Duration::from_millis(100);
/// The socket buffers. A key frame is a burst of a few hundred packets. The operating systems'
/// defaults are smaller than that: macOS 786 KB receive, Android about 200 KB.
const SOCKET_BUFFER: usize = 4 << 20;

/// The message types of one end of a session.
pub trait Side {
    type TxMsg: Serialize;
    type TxDatagram: Serialize;
    type RxMsg: DeserializeOwned;
    type RxDatagram: DeserializeOwned;
}

pub struct HostSide;
pub struct ClientSide;

impl Side for HostSide {
    type TxMsg = HostMsg;
    type TxDatagram = HostDatagram;
    type RxMsg = ClientMsg;
    type RxDatagram = ClientDatagram;
}

impl Side for ClientSide {
    type TxMsg = ClientMsg;
    type TxDatagram = ClientDatagram;
    type RxMsg = HostMsg;
    type RxDatagram = HostDatagram;
}

/// What a read loop hands to its caller.
pub enum Incoming<'a, S: Side> {
    Control(S::RxMsg),
    Datagram(S::RxDatagram),
    /// A media packet with its kind byte, for [`crate::media::FrameReceiver::push`]
    Media(&'a [u8]),
}

/// The TCP stream and the key that seals what it carries. One lock holds both, so the counter
/// of a record and its place in the stream agree.
struct TcpWriter {
    stream: TcpStream,
    tx: TcpTx,
}

/// What sealing and opening cost, summed since the session began. The client shows the cost of a
/// frame by the difference between two readings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CryptoStats {
    /// Packets that this side sealed, their bytes on the wire and the nanoseconds that sealing took
    pub sealed: u64,
    pub sealed_bytes: u64,
    pub seal_ns: u64,
    /// Packets that this side opened, their bytes on the wire and the nanoseconds that opening took
    pub opened: u64,
    pub opened_bytes: u64,
    pub open_ns: u64,
    /// UDP packets that failed to open: changed, replayed, too old or from another key
    pub rejected: u64,
}

#[derive(Default)]
struct CryptoCounters {
    sealed: AtomicU64,
    sealed_bytes: AtomicU64,
    seal_ns: AtomicU64,
    opened: AtomicU64,
    opened_bytes: AtomicU64,
    open_ns: AtomicU64,
    rejected: AtomicU64,
}

impl CryptoCounters {
    fn sealed(&self, bytes: usize, since: Instant) {
        self.sealed.fetch_add(1, Ordering::Relaxed);
        self.sealed_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        self.seal_ns.fetch_add(since.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    fn opened(&self, bytes: usize, since: Instant) {
        self.opened.fetch_add(1, Ordering::Relaxed);
        self.opened_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        self.open_ns.fetch_add(since.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

/// The connection of one session, as seen from one side.
pub struct Duplex<S: Side> {
    tcp_write: Mutex<TcpWriter>,
    tcp_read: TcpStream,
    tcp_rx: Mutex<TcpRx>,
    udp: Option<UdpSocket>,
    udp_tx: UdpTx,
    udp_rx: Mutex<UdpRx>,
    /// Builds control messages and datagrams. Media packets use the sender's own buffer.
    out: Mutex<PacketBuf>,
    closed: AtomicBool,
    dropped_sends: AtomicU64,
    crypto: CryptoCounters,
    side: PhantomData<fn() -> S>,
}

/// A send error that a datagram path counts as loss.
fn is_transient(e: &io::Error) -> bool {
    // ENOBUFS: macOS 55, Linux and Android 105. A burst of packets can fill the send buffer.
    #[cfg(target_os = "macos")]
    const ENOBUFS: i32 = 55;
    #[cfg(not(target_os = "macos"))]
    const ENOBUFS: i32 = 105;
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::ConnectionRefused) || e.raw_os_error() == Some(ENOBUFS)
}

impl<S: Side> Duplex<S> {
    fn new(tcp: TcpStream, udp: Option<UdpSocket>, channels: Channels) -> Result<Self, Error> {
        tcp.set_nodelay(true)?;
        tcp.set_read_timeout(None)?;
        tcp.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let Channels { tcp_tx, tcp_rx, udp_tx, udp_rx } = channels;
        Ok(Self {
            tcp_read: tcp.try_clone()?,
            tcp_write: Mutex::new(TcpWriter { stream: tcp, tx: tcp_tx }),
            tcp_rx: Mutex::new(tcp_rx),
            udp,
            udp_tx,
            udp_rx: Mutex::new(udp_rx),
            out: Mutex::new(PacketBuf::new()),
            closed: AtomicBool::new(false),
            dropped_sends: AtomicU64::new(0),
            crypto: CryptoCounters::default(),
            side: PhantomData,
        })
    }

    /// Whether datagrams and media use UDP. Without it they share the TCP connection.
    pub fn has_udp(&self) -> bool {
        self.udp.is_some()
    }

    /// Sends a control message over TCP.
    pub fn send_control(&self, msg: &S::TxMsg) -> Result<(), Error> {
        let mut out = self.out.lock().unwrap();
        encode(msg, out.begin(KIND_CONTROL))?;
        self.write_tcp(&mut out)
    }

    /// Sends a datagram message. It gets lost when the path loses it.
    pub fn send_datagram(&self, msg: &S::TxDatagram) -> Result<(), Error> {
        let mut out = self.out.lock().unwrap();
        encode(msg, out.begin(KIND_DATAGRAM))?;
        self.send_wire(&mut out)
    }

    /// Seals a packet that [`PacketBuf::begin`] started and sends it by UDP, or through TCP when
    /// there is no UDP socket. The packet is sealed in its own buffer. A UDP send that the system
    /// refuses for lack of buffer space counts as a lost packet.
    pub fn send_wire(&self, buf: &mut PacketBuf) -> Result<(), Error> {
        match &self.udp {
            Some(udp) => {
                if buf.content().len() + OVERHEAD > MTU {
                    return Err(Error::TooLarge(buf.content().len() + OVERHEAD));
                }
                let started = Instant::now();
                self.udp_tx.seal(buf)?;
                self.crypto.sealed(buf.datagram().len(), started);
                match udp.send(buf.datagram()) {
                    Ok(_) => Ok(()),
                    Err(e) if is_transient(&e) => {
                        self.dropped_sends.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    }
                    Err(e) => Err(e.into()),
                }
            }
            None => self.write_tcp(buf),
        }
    }

    /// One lock holds the counter, the seal and the write of a whole record, so records of
    /// different threads never interleave and the counters follow the order of the stream.
    fn write_tcp(&self, buf: &mut PacketBuf) -> Result<(), Error> {
        let mut writer = self.tcp_write.lock().unwrap();
        let TcpWriter { stream, tx } = &mut *writer;
        let started = Instant::now();
        let record = tx.seal(buf)?;
        self.crypto.sealed(record.len(), started);
        stream.write_all(record)?;
        Ok(())
    }

    /// The UDP sends that the system refused, since the session began.
    pub fn dropped_sends(&self) -> u64 {
        self.dropped_sends.load(Ordering::Relaxed)
    }

    /// The cost of sealing and opening since the session began.
    pub fn crypto_stats(&self) -> CryptoStats {
        let c = &self.crypto;
        CryptoStats {
            sealed: c.sealed.load(Ordering::Relaxed),
            sealed_bytes: c.sealed_bytes.load(Ordering::Relaxed),
            seal_ns: c.seal_ns.load(Ordering::Relaxed),
            opened: c.opened.load(Ordering::Relaxed),
            opened_bytes: c.opened_bytes.load(Ordering::Relaxed),
            open_ns: c.open_ns.load(Ordering::Relaxed),
            rejected: c.rejected.load(Ordering::Relaxed),
        }
    }

    /// Hands an opened packet to the handler. `content` is the kind byte and the body.
    fn dispatch(content: &[u8], over_udp: bool, handler: &mut impl FnMut(Incoming<'_, S>)) {
        let Some((&kind, body)) = content.split_first() else { return };
        match kind {
            // A control message must come through TCP, so the peer cannot inject one by UDP
            KIND_CONTROL if !over_udp => {
                if let Ok(msg) = decode(body) {
                    handler(Incoming::Control(msg));
                }
            }
            KIND_DATAGRAM => {
                if let Ok(msg) = decode(body) {
                    handler(Incoming::Datagram(msg));
                }
            }
            KIND_MEDIA => handler(Incoming::Media(content)),
            _ => {}
        }
    }

    /// Reads the TCP connection until it ends. It returns the reason: [`Error::Closed`] for a peer
    /// that left, or after [`Duplex::close`]. A record that fails to open ends the session with
    /// [`Error::Crypto`]: TCP delivers every record, so a bad one means an attacker.
    pub fn read_tcp(&self, mut handler: impl FnMut(Incoming<'_, S>)) -> Error {
        let mut buf = Vec::new();
        let mut reader = &self.tcp_read;
        let mut rx = self.tcp_rx.lock().unwrap();
        loop {
            if let Err(e) = read_tcp_packet(&mut reader, &mut buf) {
                return e;
            }
            let started = Instant::now();
            let wire_len = buf.len() + 4;
            match rx.open(&mut buf) {
                Ok(content) => {
                    self.crypto.opened(wire_len, started);
                    Self::dispatch(content, false, &mut handler);
                }
                Err(e) => {
                    self.crypto.rejected.fetch_add(1, Ordering::Relaxed);
                    return e;
                }
            }
        }
    }

    /// Reads the UDP socket until the session closes or the socket fails. A datagram that fails to
    /// open is dropped, as a lost one is.
    pub fn read_udp(&self, mut handler: impl FnMut(Incoming<'_, S>)) -> Error {
        let Some(udp) = &self.udp else { return Error::Unexpected("no UDP path") };
        let mut buf = vec![0u8; 2048];
        while !self.closed.load(Ordering::Relaxed) {
            match udp.recv(&mut buf) {
                Ok(n) => {
                    let started = Instant::now();
                    match self.udp_rx.lock().unwrap().open(&mut buf[..n]) {
                        Ok(content) => {
                            self.crypto.opened(n, started);
                            Self::dispatch(content, true, &mut handler);
                        }
                        Err(_) => {
                            self.crypto.rejected.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::ConnectionRefused) => {}
                Err(e) => return e.into(),
            }
        }
        Error::Closed
    }

    /// Ends the session. Both read loops return.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.tcp_read.shutdown(std::net::Shutdown::Both).ok();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

/// How a client opens its sockets. [`AnyRoute`] lets the system choose the route. A link over one
/// network, such as a headset's USB network to the Mac, opens its sockets on that network.
pub trait Route {
    /// A TCP connection to `host`. `timeout` bounds the connect.
    fn connect_tcp(&self, host: SocketAddr, timeout: Duration) -> io::Result<TcpStream>;
    /// A UDP socket bound to `local`
    fn bind_udp(&self, local: SocketAddr) -> io::Result<UdpSocket>;
}

/// The route that the system chooses for each socket.
pub struct AnyRoute;

impl Route for AnyRoute {
    fn connect_tcp(&self, host: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        TcpStream::connect_timeout(&host, timeout)
    }

    fn bind_udp(&self, local: SocketAddr) -> io::Result<UdpSocket> {
        UdpSocket::bind(local)
    }
}

/// A UDP socket from `route` with large buffers and a read timeout.
fn udp_socket(route: &dyn Route, bind: SocketAddr) -> io::Result<UdpSocket> {
    let udp = route.bind_udp(bind)?;
    let socket = SockRef::from(&udp);
    // A system may cap the size or refuse it. The default size still works.
    socket.set_recv_buffer_size(SOCKET_BUFFER).ok();
    socket.set_send_buffer_size(SOCKET_BUFFER).ok();
    udp.set_read_timeout(Some(UDP_POLL))?;
    Ok(udp)
}

/// A number the client repeats in its UDP hello. It stops a datagram of an earlier session from
/// claiming this one. The hello is sealed, so the number needs no secrecy of its own.
fn random_session() -> u64 {
    RandomState::new().build_hasher().finish()
}

/// Writes one control message, sealed, to a TCP stream that no [`Duplex`] owns yet.
fn write_control<T: Serialize>(tcp: &mut impl Write, tx: &mut TcpTx, msg: &T) -> Result<(), Error> {
    let mut buf = PacketBuf::new();
    encode(msg, buf.begin(KIND_CONTROL))?;
    tcp.write_all(tx.seal(&mut buf)?)?;
    Ok(())
}

fn read_control<T: DeserializeOwned>(tcp: &mut impl Read, rx: &mut TcpRx, buf: &mut Vec<u8>) -> Result<T, Error> {
    read_tcp_packet_limited(tcp, buf, HANDSHAKE_PACKET_LIMIT)?;
    match rx.open(buf)?.split_first() {
        Some((&KIND_CONTROL, body)) => decode(body),
        _ => Err(Error::Unexpected("expected a control message")),
    }
}

/// Writes a flight of the key exchange: a TCP packet of the handshake kind. Nothing in it is
/// secret. The exchange's signatures make a changed flight fail.
fn write_flight(tcp: &mut impl Write, body: &[u8]) -> Result<(), Error> {
    let mut packet = Vec::with_capacity(5 + body.len());
    packet.extend_from_slice(&(1 + body.len() as u32).to_le_bytes());
    packet.push(KIND_HANDSHAKE);
    packet.extend_from_slice(body);
    tcp.write_all(&packet)?;
    Ok(())
}

/// Reads a flight of the key exchange whose body has `len` bytes. The length is checked before
/// the read, so a peer that has proved nothing cannot size a buffer.
fn read_flight<'a>(tcp: &mut impl Read, buf: &'a mut Vec<u8>, len: usize) -> Result<&'a [u8], Error> {
    read_tcp_packet_limited(tcp, buf, 1 + len)?;
    match buf.split_first() {
        Some((&KIND_HANDSHAKE, body)) if body.len() == len => Ok(body),
        _ => Err(Error::Unexpected("expected a handshake flight")),
    }
}

// ---------------------------------------------------------------------------------------------
// Host

/// The host's listening socket, its key and its switch for the USB cable.
pub struct HostListener {
    listener: TcpListener,
    identity: Identity,
    /// Whether the host takes a link over a USB cable. The preamble tells it to every client.
    cable: Arc<AtomicBool>,
}

/// How the host wants to send media to a client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaMode {
    Udp,
    Tcp,
}

/// A client that passed the version check and the key exchange and sent its capabilities. The
/// host looks at them and accepts or rejects.
pub struct Pending {
    tcp: TcpStream,
    channels: Channels,
    pub peer: SocketAddr,
    pub caps: ClientCaps,
    /// The client's public key. The client proved that it holds the key, so the host may look it
    /// up in its pairings. The host does that after this call: a client on this Mac needs no
    /// pairing.
    pub client_key: [u8; KEY_LEN],
    /// The hash that both sides signed. The approval code comes from it.
    transcript: [u8; 32],
}

impl HostListener {
    /// Listens on `addr`. `identity` is the host's key: the client checks it against the key that
    /// it paired with.
    pub fn bind(addr: impl ToSocketAddrs, identity: Identity) -> Result<Self, Error> {
        Ok(Self { listener: TcpListener::bind(addr)?, identity, cable: Arc::new(AtomicBool::new(true)) })
    }

    /// Listens on `port` on every IPv6 address, beside a listener of [`HostListener::bind`] on the
    /// same port for IPv4. A headset's USB network to the Mac reaches the host here.
    pub fn bind_ipv6(port: u16, identity: Identity) -> Result<Self, Error> {
        let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
        // macOS makes an IPv6 socket take IPv4 too, which the IPv4 listener's port refuses
        socket.set_only_v6(true)?;
        socket.set_reuse_address(true)?;
        socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into())?;
        socket.listen(128)?;
        Ok(Self { listener: socket.into(), identity, cable: Arc::new(AtomicBool::new(true)) })
    }

    /// Shares the switch that says whether this host takes a link over a USB cable. The host's
    /// user turns it off with the choice of Wi-Fi. The preamble of each later connection carries
    /// the switch's state. A listener without a shared switch takes the cable.
    pub fn share_cable_switch(mut self, cable: Arc<AtomicBool>) -> Self {
        self.cable = cable;
        self
    }

    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        Ok(self.listener.local_addr()?)
    }

    /// The host's public key
    pub fn public_key(&self) -> [u8; KEY_LEN] {
        self.identity.public_key()
    }

    /// Waits for the next TCP connection and reads nothing from it. A host that serves a network
    /// runs [`HostListener::handshake`] for each connection on a thread of its own, so a peer that
    /// connects and sends nothing holds back no other client.
    pub fn connecting(&self) -> Result<Connecting, Error> {
        let (tcp, peer) = self.listener.accept()?;
        Ok(Connecting { tcp, peer })
    }

    /// Waits for a client and runs [`HostListener::handshake`] with it, for a host of one client at
    /// a time such as a test.
    pub fn accept(&self) -> Result<Pending, Error> {
        self.handshake(self.connecting()?)
    }

    /// Runs the version exchange and the key exchange with a connection, within
    /// [`HANDSHAKE_TIMEOUT`] in all. A client that sends bad bytes, speaks another version, cannot
    /// prove its key or is too slow gives an error.
    pub fn handshake(&self, connecting: Connecting) -> Result<Pending, Error> {
        let Connecting { tcp, peer } = connecting;
        tcp.set_nodelay(true)?;
        let mut timed = Timed { tcp: &tcp, until: Instant::now() + HANDSHAKE_TIMEOUT };
        let mut client_pre = [0u8; CLIENT_PREAMBLE_LEN];
        timed.read_exact(&mut client_pre)?;
        let theirs = parse_client_preamble(&client_pre)?;
        // The host answers with its own version, so a client of another one can say which side to update
        let host_pre = host_preamble(PROTOCOL_VERSION, self.cable.load(Ordering::Relaxed));
        timed.write_all(&host_pre)?;
        if theirs != PROTOCOL_VERSION {
            return Err(Error::Version { ours: PROTOCOL_VERSION, theirs });
        }
        let mut buf = Vec::new();
        // Flight 1: the client's fresh key. Flight 2: the host's fresh key, its key and its
        // signature over both preambles and both fresh keys.
        let client_fresh: [u8; KEY_LEN] = read_flight(&mut timed, &mut buf, CLIENT_HELLO_LEN)?.try_into().expect("checked length");
        let fresh = Ephemeral::new()?;
        let host_key = self.identity.public_key();
        let transcript = transcript_hash(&client_pre, &host_pre, &client_fresh, &fresh.public(), &host_key);
        let hello = HostHello { ephemeral: fresh.public(), key: host_key, signature: sign_as_host(&self.identity, &transcript) };
        write_flight(&mut timed, &hello.to_bytes())?;
        let mut channels = derive(Role::Host, &*fresh.agree(&client_fresh)?, &transcript);
        // Flight 3, sealed: the client's key and its signature over the same transcript
        read_tcp_packet_limited(&mut timed, &mut buf, HANDSHAKE_PACKET_LIMIT)?;
        let auth = match channels.tcp_rx.open(&mut buf)?.split_first() {
            Some((&KIND_HANDSHAKE, body)) => ClientAuth::parse(body)?,
            _ => return Err(Error::Unexpected("expected the client's key")),
        };
        if !verify_client(&auth.key, &transcript, &auth.signature) {
            return Err(Error::Crypto("the client does not hold the key that it claims"));
        }
        let caps = match read_control::<ClientMsg>(&mut timed, &mut channels.tcp_rx, &mut buf)? {
            ClientMsg::Hello(caps) => caps,
            _ => return Err(Error::Unexpected("the client's first message is not Hello")),
        };
        // Each later exchange of a pending client waits up to the same time for each read and write
        tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        tcp.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        Ok(Pending { tcp, channels, peer, caps, client_key: auth.key, transcript })
    }
}

/// A TCP connection that [`HostListener::connecting`] took, before any byte of the handshake
pub struct Connecting {
    tcp: TcpStream,
    pub peer: SocketAddr,
}

/// The connection of a handshake, whose reads and writes all end at one deadline. A timeout for
/// each read alone lets a client that sends a byte now and then hold the handshake for as long as
/// it likes.
struct Timed<'a> {
    tcp: &'a TcpStream,
    until: Instant,
}

impl Timed<'_> {
    /// The time until the deadline, or a timeout once it passed
    fn left(&self) -> io::Result<Duration> {
        self.until.checked_duration_since(Instant::now()).filter(|left| !left.is_zero()).ok_or_else(|| io::ErrorKind::TimedOut.into())
    }
}

impl Read for Timed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.tcp.set_read_timeout(Some(self.left()?))?;
        (&*self.tcp).read(buf)
    }
}

impl Write for Timed<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tcp.set_write_timeout(Some(self.left()?))?;
        (&*self.tcp).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.tcp).flush()
    }
}

impl Pending {
    /// Tells a client that the host's user must allow it, and runs the exchange of numbers that
    /// gives both sides the approval code (see [`approval_code`]). It returns the code. The caller
    /// shows it to the host's user, then calls [`Pending::accept`] or [`Pending::reject`]. The call
    /// takes up to the handshake timeout, for a client that does not answer.
    pub fn hold_for_approval(&mut self, host_name: &str) -> Result<u32, Error> {
        let nonce = random_bytes()?;
        let commit = approval_commit(&nonce, &self.transcript, &self.client_key);
        write_control(&mut self.tcp, &mut self.channels.tcp_tx, &HostMsg::AwaitApproval { host_name: host_name.to_owned(), commit })?;
        let mut buf = Vec::new();
        let client_nonce = match read_control::<ClientMsg>(&mut self.tcp, &mut self.channels.tcp_rx, &mut buf)? {
            ClientMsg::PairNonce(client_nonce) => client_nonce,
            _ => return Err(Error::Unexpected("expected the client's number for the approval code")),
        };
        write_control(&mut self.tcp, &mut self.channels.tcp_tx, &HostMsg::PairReveal(nonce))?;
        Ok(approval_code(&self.transcript, &self.client_key, &nonce, &client_nonce))
    }

    /// Whether the client closed the connection, or sent anything, while the host waits for its
    /// user. A waiting client sends nothing except its goodbye.
    pub fn client_left(&mut self) -> bool {
        if self.tcp.set_nonblocking(true).is_err() {
            return true;
        }
        let mut byte = [0u8; 1];
        let left = match self.tcp.peek(&mut byte) {
            Ok(_) => true,
            Err(e) => e.kind() != io::ErrorKind::WouldBlock,
        };
        self.tcp.set_nonblocking(false).ok();
        left
    }

    pub fn reject(mut self, reason: RejectReason) {
        write_control(&mut self.tcp, &mut self.channels.tcp_tx, &HostMsg::Reject(reason)).ok();
    }

    /// Sends the welcome, waits until the media path works and returns the session.
    /// `shard_len` is the payload bytes of a media packet.
    pub fn accept(mut self, stream: StreamParams, mode: MediaMode, shard_len: usize) -> Result<HostLink, Error> {
        let session = random_session();
        let sender = FrameSender::new(shard_len)?;
        let udp = match mode {
            MediaMode::Udp => {
                if shard_len + crate::media::HEADER_LEN + OVERHEAD > MTU {
                    return Err(Error::TooLarge(shard_len + crate::media::HEADER_LEN + OVERHEAD));
                }
                // The TCP connection's own address keeps the scope of an IPv6 link-local one
                let mut local = self.tcp.local_addr()?;
                local.set_port(0);
                Some(udp_socket(&AnyRoute, local)?)
            }
            MediaMode::Tcp => None,
        };
        let media = match &udp {
            Some(udp) => MediaPlan::Udp { port: udp.local_addr()?.port() },
            None => MediaPlan::Tcp,
        };
        let welcome = Welcome { session, stream: stream.clone(), media, shard_len: shard_len as u16 };
        write_control(&mut self.tcp, &mut self.channels.tcp_tx, &HostMsg::Welcome(welcome))?;
        if let Some(udp) = &udp {
            // The client repeats its hello until the host answers, so a lost datagram costs 50 ms
            let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
            let mut buf = [0u8; 2048];
            loop {
                match udp.recv_from(&mut buf) {
                    Ok((n, from)) => {
                        // Only a datagram that opens with the session's key can name the client's
                        // address
                        let hello = match self.channels.udp_rx.open(&mut buf[..n]).map(|content| content.split_first()) {
                            Ok(Some((&KIND_DATAGRAM, body))) => decode::<ClientDatagram>(body).ok(),
                            _ => None,
                        };
                        if matches!(hello, Some(ClientDatagram::Hello { session: s }) if s == session) {
                            udp.connect(from)?;
                            break;
                        }
                    }
                    Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                    Err(e) => return Err(e.into()),
                }
                if Instant::now() > deadline {
                    return Err(Error::Timeout);
                }
            }
        }
        write_control(&mut self.tcp, &mut self.channels.tcp_tx, &HostMsg::MediaReady)?;
        Ok(HostLink {
            duplex: Duplex::new(self.tcp, udp, self.channels)?,
            sender: Mutex::new(sender),
            peer: self.peer,
            client_key: self.client_key,
            session,
            stream,
            media,
        })
    }
}

/// The host's end of a session.
pub struct HostLink {
    pub duplex: Duplex<HostSide>,
    sender: Mutex<FrameSender>,
    pub peer: SocketAddr,
    /// The key that the client proved
    pub client_key: [u8; KEY_LEN],
    pub session: u64,
    pub stream: StreamParams,
    pub media: MediaPlan,
}

impl HostLink {
    /// Sends one slice with the parity that the controller asks for. Over TCP the parity is
    /// zero, because TCP loses nothing.
    pub fn send_unit(&self, meta: &FrameMeta, payload: &[u8], parity: &ParityController) -> Result<SendStats, Error> {
        let udp = self.duplex.has_udp();
        self.sender.lock().unwrap().send_unit(meta, payload, |n| if udp { parity.parity_count(n) } else { 0 }, |packet| self.duplex.send_wire(packet))
    }
}

// ---------------------------------------------------------------------------------------------
// Client

/// What [`ClientLink::connect_with`] tells its caller while the host's user decides.
pub trait ConnectObserver {
    /// The host does not know this client and asks its user. `code` is the approval code: the
    /// host's user sees the same one. `host_name` is the name that the host gave itself.
    fn waiting(&mut self, code: u32, host_name: &str);

    /// Whether the caller gave up. The wait then ends with [`Error::Closed`].
    fn cancelled(&self) -> bool {
        false
    }
}

/// An observer that watches nothing.
pub struct NoObserver;

impl ConnectObserver for NoObserver {
    fn waiting(&mut self, _code: u32, _host_name: &str) {}
}

/// What a host shows to anyone who opens a connection, before it knows who asks: the protocol
/// version and, when it matches ours, its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostProbe {
    /// The protocol version of the host
    pub version: u16,
    /// The host's key. `None` when the host speaks another version than this side: it then ends
    /// the exchange after the version exchange.
    pub key: Option<[u8; KEY_LEN]>,
    /// The host takes a link over a USB cable. It is false when the host's user chose Wi-Fi. A host
    /// of another version says nothing about it, and the probe gives true for it.
    pub cable: bool,
}

/// Reads the host's preamble: the version first, and the rest only from a host of this side's
/// version. A host of another version ends the exchange after its version.
fn read_host_preamble(tcp: &mut TcpStream) -> Result<(u16, [u8; HOST_PREAMBLE_LEN]), Error> {
    let mut host_pre = [0u8; HOST_PREAMBLE_LEN];
    tcp.read_exact(&mut host_pre[..HOST_VERSION_LEN])?;
    let theirs = parse_host_preamble(&host_pre)?;
    if theirs == PROTOCOL_VERSION {
        tcp.read_exact(&mut host_pre[HOST_VERSION_LEN..])?;
    }
    Ok((theirs, host_pre))
}

/// Opens a connection to `host`, runs the version exchange and the first two flights of the key
/// exchange, checks the host's signature and closes. The client says nothing about itself: the host
/// sees a connection that ends before the client's key. The call tells a host that answers from
/// one that only accepts a connection. Behind `adb reverse` the headset's own end of the cable
/// accepts the connection whether or not a host runs on the Mac, so only bytes from the host prove
/// that it is there. `timeout` bounds each step.
pub fn probe_host(host: SocketAddr, timeout: Duration) -> Result<HostProbe, Error> {
    probe_host_via(&AnyRoute, host, timeout)
}

/// [`probe_host`] with the connection from `route`.
pub fn probe_host_via(route: &dyn Route, host: SocketAddr, timeout: Duration) -> Result<HostProbe, Error> {
    let mut tcp = route.connect_tcp(host, timeout)?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(timeout))?;
    tcp.set_write_timeout(Some(timeout))?;
    let client_pre = client_preamble(PROTOCOL_VERSION);
    tcp.write_all(&client_pre)?;
    let (theirs, host_pre) = read_host_preamble(&mut tcp)?;
    if theirs != PROTOCOL_VERSION {
        return Ok(HostProbe { version: theirs, key: None, cable: true });
    }
    let cable = host_takes_cable(&host_pre)?;
    let mut buf = Vec::new();
    let fresh = Ephemeral::new()?;
    write_flight(&mut tcp, &fresh.public())?;
    let hello = HostHello::parse(read_flight(&mut tcp, &mut buf, HOST_HELLO_LEN)?)?;
    let transcript = transcript_hash(&client_pre, &host_pre, &fresh.public(), &hello.ephemeral, &hello.key);
    if !verify_host(&hello.key, &transcript, &hello.signature) {
        return Err(Error::Crypto("the host's signature is not valid"));
    }
    Ok(HostProbe { version: theirs, key: Some(hello.key), cable })
}

/// The client's end of a session.
pub struct ClientLink {
    pub duplex: Duplex<ClientSide>,
    pub welcome: Welcome,
    pub host: SocketAddr,
    /// The host's key. The host signed the key exchange with it. The host accepted this client, so
    /// a caller that has no key of this host yet may remember this one.
    pub host_key: [u8; KEY_LEN],
}

impl ClientLink {
    /// Connects to the host, checks its version, proves its key and checks the host's, sends the
    /// capabilities and waits for the host's choices and for the media path. `expected_host` is
    /// the key that the client paired with, if it has one: a host with another key fails with
    /// [`Error::HostKeyChanged`] before the client says anything about itself. `timeout` bounds each step.
    pub fn connect(
        host: SocketAddr,
        caps: &ClientCaps,
        identity: &Identity,
        expected_host: Option<[u8; KEY_LEN]>,
        timeout: Duration,
    ) -> Result<Self, Error> {
        Self::connect_with(host, caps, identity, expected_host, timeout, &mut NoObserver)
    }

    /// [`ClientLink::connect`] with an observer. A host that does not know this client holds the
    /// connection until its user answers. The observer then gets the approval code to show, and
    /// can end the wait.
    pub fn connect_with(
        host: SocketAddr,
        caps: &ClientCaps,
        identity: &Identity,
        expected_host: Option<[u8; KEY_LEN]>,
        timeout: Duration,
        observer: &mut dyn ConnectObserver,
    ) -> Result<Self, Error> {
        Self::connect_via(&AnyRoute, host, caps, identity, expected_host, timeout, observer)
    }

    /// [`ClientLink::connect_with`] with the sockets from `route`.
    pub fn connect_via(
        route: &dyn Route,
        host: SocketAddr,
        caps: &ClientCaps,
        identity: &Identity,
        expected_host: Option<[u8; KEY_LEN]>,
        timeout: Duration,
        observer: &mut dyn ConnectObserver,
    ) -> Result<Self, Error> {
        let mut tcp = route.connect_tcp(host, timeout)?;
        tcp.set_nodelay(true)?;
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;
        let client_pre = client_preamble(PROTOCOL_VERSION);
        tcp.write_all(&client_pre)?;
        let (theirs, host_pre) = read_host_preamble(&mut tcp)?;
        if theirs != PROTOCOL_VERSION {
            return Err(Error::Version { ours: PROTOCOL_VERSION, theirs });
        }
        host_takes_cable(&host_pre)?;
        let mut buf = Vec::new();
        let fresh = Ephemeral::new()?;
        write_flight(&mut tcp, &fresh.public())?;
        let hello = HostHello::parse(read_flight(&mut tcp, &mut buf, HOST_HELLO_LEN)?)?;
        let transcript = transcript_hash(&client_pre, &host_pre, &fresh.public(), &hello.ephemeral, &hello.key);
        if !verify_host(&hello.key, &transcript, &hello.signature) {
            return Err(Error::Crypto("the host's signature is not valid"));
        }
        if expected_host.is_some_and(|expected| expected != hello.key) {
            return Err(Error::HostKeyChanged);
        }
        let mut channels = derive(Role::Client, &*fresh.agree(&hello.ephemeral)?, &transcript);
        let mut auth = PacketBuf::new();
        let body = auth.begin(KIND_HANDSHAKE);
        body.extend_from_slice(&identity.public_key());
        body.extend_from_slice(&sign_as_client(identity, &transcript));
        tcp.write_all(channels.tcp_tx.seal(&mut auth)?)?;
        write_control(&mut tcp, &mut channels.tcp_tx, &ClientMsg::Hello(caps.clone()))?;
        let mut waiting_since: Option<Instant> = None;
        let welcome = loop {
            match read_control::<HostMsg>(&mut tcp, &mut channels.tcp_rx, &mut buf) {
                Ok(HostMsg::Welcome(w)) => break w,
                Ok(HostMsg::Reject(reason)) => return Err(Error::Rejected(reason)),
                Ok(HostMsg::AwaitApproval { host_name, commit }) if waiting_since.is_none() => {
                    let nonce = random_bytes()?;
                    write_control(&mut tcp, &mut channels.tcp_tx, &ClientMsg::PairNonce(nonce))?;
                    let host_nonce = match read_control::<HostMsg>(&mut tcp, &mut channels.tcp_rx, &mut buf)? {
                        HostMsg::PairReveal(host_nonce) => host_nonce,
                        _ => return Err(Error::Unexpected("expected the host's number for the approval code")),
                    };
                    let key = identity.public_key();
                    if approval_commit(&host_nonce, &transcript, &key) != commit {
                        return Err(Error::Crypto("the host's number does not match its commitment"));
                    }
                    observer.waiting(approval_code(&transcript, &key, &host_nonce, &nonce), &host_name);
                    waiting_since = Some(Instant::now());
                    // The host answers when its user does. A short timeout lets the loop see a cancel.
                    tcp.set_read_timeout(Some(WAIT_POLL))?;
                }
                Ok(_) => return Err(Error::Unexpected("the host's first answer is not Welcome")),
                Err(Error::Timeout) if waiting_since.is_some_and(|since| since.elapsed() < APPROVAL_WAIT + APPROVAL_SLACK) => {
                    if observer.cancelled() {
                        return Err(Error::Closed);
                    }
                }
                Err(e) => return Err(e),
            }
        };
        let udp = match welcome.media {
            MediaPlan::Udp { port } => {
                let bind: SocketAddr = if host.is_ipv4() { ([0, 0, 0, 0], 0).into() } else { ([0u16; 8], 0).into() };
                let udp = udp_socket(route, bind)?;
                // The host's address keeps the scope of an IPv6 link-local one
                let mut media = host;
                media.set_port(port);
                udp.connect(media)?;
                Some(udp)
            }
            MediaPlan::Tcp => None,
        };
        // Repeat the hello datagram until MediaReady arrives. The read timeout on the TCP side
        // paces the repeats.
        let started = Instant::now();
        tcp.set_read_timeout(Some(Duration::from_millis(50)))?;
        let mut out = PacketBuf::new();
        loop {
            if let Some(udp) = &udp {
                encode(&ClientDatagram::Hello { session: welcome.session }, out.begin(KIND_DATAGRAM))?;
                channels.udp_tx.seal(&mut out)?;
                udp.send(out.datagram()).ok();
            }
            match read_control::<HostMsg>(&mut tcp, &mut channels.tcp_rx, &mut buf) {
                Ok(HostMsg::MediaReady) => break,
                Ok(_) => return Err(Error::Unexpected("expected MediaReady")),
                Err(Error::Timeout) if started.elapsed() < timeout => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Self { duplex: Duplex::new(tcp, udp, channels)?, welcome, host, host_key: hello.key })
    }
}
