//! Sockets for a session: the TCP connection, the optional UDP socket, the handshake and the two
//! ends ([`HostLink`] and [`ClientLink`]). All calls block. A caller runs one thread per read loop
//! ([`Duplex::read_tcp`] and [`Duplex::read_udp`]). Any thread may send.
//!
//! The client dials the host. The host listens on one TCP port. With UDP media the host opens a
//! UDP socket per session on an ephemeral port and names it in the welcome. The client's first
//! datagram teaches the host the client's address.

use crate::{
    fec::ParityController,
    media::{FrameMeta, FrameSender, SendStats},
    message::*,
    wire::{decode, encode, read_tcp_packet, Error, PacketBuf, Wire, KIND_CONTROL, KIND_DATAGRAM, KIND_MEDIA},
    MTU,
};
use serde::{de::DeserializeOwned, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    io::{self, Read, Write},
    marker::PhantomData,
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

/// The time that handshake reads and writes may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
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

/// The connection of one session, as seen from one side.
pub struct Duplex<S: Side> {
    tcp_write: Mutex<TcpStream>,
    tcp_read: TcpStream,
    udp: Option<UdpSocket>,
    /// Builds control messages and datagrams. Media packets use the sender's own buffer.
    out: Mutex<PacketBuf>,
    closed: AtomicBool,
    dropped_sends: AtomicU64,
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
    fn new(tcp: TcpStream, udp: Option<UdpSocket>) -> Result<Self, Error> {
        tcp.set_nodelay(true)?;
        tcp.set_read_timeout(None)?;
        tcp.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(Self {
            tcp_read: tcp.try_clone()?,
            tcp_write: Mutex::new(tcp),
            udp,
            out: Mutex::new(PacketBuf::new()),
            closed: AtomicBool::new(false),
            dropped_sends: AtomicU64::new(0),
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
        let wire = out.seal()?;
        self.write_tcp(wire.framed)
    }

    /// Sends a datagram message. It gets lost when the path loses it.
    pub fn send_datagram(&self, msg: &S::TxDatagram) -> Result<(), Error> {
        let mut out = self.out.lock().unwrap();
        encode(msg, out.begin(KIND_DATAGRAM))?;
        let wire = out.seal()?;
        self.send_wire(wire)
    }

    /// Sends a finished packet by UDP, or through TCP when there is no UDP socket. A UDP send that
    /// the system refuses for lack of buffer space counts as a lost packet.
    pub fn send_wire(&self, wire: Wire<'_>) -> Result<(), Error> {
        match &self.udp {
            Some(udp) => {
                if wire.packet.len() > MTU {
                    return Err(Error::TooLarge(wire.packet.len()));
                }
                match udp.send(wire.packet) {
                    Ok(_) => Ok(()),
                    Err(e) if is_transient(&e) => {
                        self.dropped_sends.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    }
                    Err(e) => Err(e.into()),
                }
            }
            None => self.write_tcp(wire.framed),
        }
    }

    /// One lock holds a whole packet, so packets of different threads never interleave.
    fn write_tcp(&self, bytes: &[u8]) -> Result<(), Error> {
        self.tcp_write.lock().unwrap().write_all(bytes)?;
        Ok(())
    }

    /// The UDP sends that the system refused, since the session began.
    pub fn dropped_sends(&self) -> u64 {
        self.dropped_sends.load(Ordering::Relaxed)
    }

    fn dispatch(packet: &[u8], over_udp: bool, handler: &mut impl FnMut(Incoming<'_, S>)) {
        let Some((&kind, body)) = packet.split_first() else { return };
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
            KIND_MEDIA => handler(Incoming::Media(packet)),
            _ => {}
        }
    }

    /// Reads the TCP connection until it ends. It returns the reason: [`Error::Closed`] for a peer
    /// that left, or after [`Duplex::close`].
    pub fn read_tcp(&self, mut handler: impl FnMut(Incoming<'_, S>)) -> Error {
        let mut buf = Vec::new();
        let mut reader = &self.tcp_read;
        loop {
            if let Err(e) = read_tcp_packet(&mut reader, &mut buf) {
                return e;
            }
            Self::dispatch(&buf, false, &mut handler);
        }
    }

    /// Reads the UDP socket until the session closes or the socket fails.
    pub fn read_udp(&self, mut handler: impl FnMut(Incoming<'_, S>)) -> Error {
        let Some(udp) = &self.udp else { return Error::Unexpected("no UDP path") };
        let mut buf = vec![0u8; 2048];
        while !self.closed.load(Ordering::Relaxed) {
            match udp.recv(&mut buf) {
                Ok(n) => Self::dispatch(&buf[..n], true, &mut handler),
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

/// A UDP socket with large buffers and a read timeout.
fn udp_socket(bind: SocketAddr) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::for_address(bind), Type::DGRAM, Some(Protocol::UDP))?;
    // A system may cap the size or refuse it. The default size still works.
    socket.set_recv_buffer_size(SOCKET_BUFFER).ok();
    socket.set_send_buffer_size(SOCKET_BUFFER).ok();
    socket.bind(&bind.into())?;
    let udp: UdpSocket = socket.into();
    udp.set_read_timeout(Some(UDP_POLL))?;
    Ok(udp)
}

/// A number the client repeats to prove that its UDP datagram belongs to this session. It stops
/// a stray datagram from claiming the session. It is not a secret: pairing is a separate step.
fn random_session() -> u64 {
    RandomState::new().build_hasher().finish()
}

/// Writes one control message to a TCP stream that no [`Duplex`] owns yet.
fn write_control<T: Serialize>(tcp: &mut TcpStream, msg: &T) -> Result<(), Error> {
    let mut buf = PacketBuf::new();
    encode(msg, buf.begin(KIND_CONTROL))?;
    tcp.write_all(buf.seal()?.framed)?;
    Ok(())
}

fn read_control<T: DeserializeOwned>(tcp: &mut TcpStream, buf: &mut Vec<u8>) -> Result<T, Error> {
    read_tcp_packet(tcp, buf)?;
    match buf.split_first() {
        Some((&KIND_CONTROL, body)) => decode(body),
        _ => Err(Error::Unexpected("expected a control message")),
    }
}

// ---------------------------------------------------------------------------------------------
// Host

/// The host's listening socket.
pub struct HostListener {
    listener: TcpListener,
}

/// How the host wants to send media to a client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaMode {
    Udp,
    Tcp,
}

/// A client that passed the version check and sent its capabilities. The host looks at them and
/// accepts or rejects.
pub struct Pending {
    tcp: TcpStream,
    pub peer: SocketAddr,
    pub version: u16,
    pub caps: ClientCaps,
}

impl HostListener {
    pub fn bind(addr: impl ToSocketAddrs) -> Result<Self, Error> {
        Ok(Self { listener: TcpListener::bind(addr)? })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        Ok(self.listener.local_addr()?)
    }

    /// Waits for a client and runs the version exchange. A client that sends bad bytes, or speaks
    /// no common version, gives an error and the caller calls `accept` again.
    pub fn accept(&self) -> Result<Pending, Error> {
        let (mut tcp, peer) = self.listener.accept()?;
        tcp.set_nodelay(true)?;
        tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        tcp.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let mut preamble = [0u8; CLIENT_PREAMBLE_LEN];
        tcp.read_exact(&mut preamble)?;
        let theirs = parse_client_preamble(&preamble)?;
        let version = VersionRange::OURS.common(theirs);
        tcp.write_all(&host_preamble(version, VersionRange::OURS))?;
        let version = version.ok_or(Error::Version {
            ours: (VersionRange::OURS.min, VersionRange::OURS.max),
            theirs: (theirs.min, theirs.max),
        })?;
        let mut buf = Vec::new();
        match read_control::<ClientMsg>(&mut tcp, &mut buf)? {
            ClientMsg::Hello(caps) => Ok(Pending { tcp, peer, version, caps }),
            _ => Err(Error::Unexpected("the client's first message is not Hello")),
        }
    }
}

impl Pending {
    pub fn reject(mut self, reason: RejectReason) {
        write_control(&mut self.tcp, &HostMsg::Reject(reason)).ok();
    }

    /// Sends the welcome, waits until the media path works and returns the session.
    /// `shard_len` is the payload bytes of a media packet.
    pub fn accept(mut self, stream: StreamParams, mode: MediaMode, shard_len: usize) -> Result<HostLink, Error> {
        let session = random_session();
        let sender = FrameSender::new(shard_len)?;
        let udp = match mode {
            MediaMode::Udp => {
                if shard_len + crate::media::HEADER_LEN > MTU {
                    return Err(Error::TooLarge(shard_len + crate::media::HEADER_LEN));
                }
                Some(udp_socket(SocketAddr::new(self.tcp.local_addr()?.ip(), 0))?)
            }
            MediaMode::Tcp => None,
        };
        let media = match &udp {
            Some(udp) => MediaPlan::Udp { port: udp.local_addr()?.port() },
            None => MediaPlan::Tcp,
        };
        let welcome = Welcome { session, version: self.version, stream: stream.clone(), media, shard_len: shard_len as u16 };
        write_control(&mut self.tcp, &HostMsg::Welcome(welcome))?;
        if let Some(udp) = &udp {
            // The client repeats its hello until the host answers, so a lost datagram costs 50 ms
            let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
            let mut buf = [0u8; 2048];
            loop {
                match udp.recv_from(&mut buf) {
                    Ok((n, from)) => {
                        let hello = match buf[..n].split_first() {
                            Some((&KIND_DATAGRAM, body)) => decode::<ClientDatagram>(body).ok(),
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
        write_control(&mut self.tcp, &HostMsg::MediaReady)?;
        Ok(HostLink {
            duplex: Duplex::new(self.tcp, udp)?,
            sender: Mutex::new(sender),
            peer: self.peer,
            session,
            version: self.version,
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
    pub session: u64,
    pub version: u16,
    pub stream: StreamParams,
    pub media: MediaPlan,
}

impl HostLink {
    /// Sends one slice with the parity that the controller asks for. Over TCP the parity is
    /// zero, because TCP loses nothing.
    pub fn send_unit(&self, meta: &FrameMeta, payload: &[u8], parity: &ParityController) -> Result<SendStats, Error> {
        let udp = self.duplex.has_udp();
        self.sender.lock().unwrap().send_unit(meta, payload, |n| if udp { parity.parity_count(n) } else { 0 }, |wire| self.duplex.send_wire(wire))
    }
}

// ---------------------------------------------------------------------------------------------
// Client

/// The client's end of a session.
pub struct ClientLink {
    pub duplex: Duplex<ClientSide>,
    pub welcome: Welcome,
    pub host: SocketAddr,
}

impl ClientLink {
    /// Connects to the host, agrees on a version, sends the capabilities and waits for the host's
    /// choices and for the media path. `timeout` bounds each step.
    pub fn connect(host: SocketAddr, caps: &ClientCaps, timeout: Duration) -> Result<Self, Error> {
        let mut tcp = TcpStream::connect_timeout(&host, timeout)?;
        tcp.set_nodelay(true)?;
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;
        tcp.write_all(&client_preamble(VersionRange::OURS))?;
        let mut preamble = [0u8; HOST_PREAMBLE_LEN];
        tcp.read_exact(&mut preamble)?;
        let (chosen, theirs) = parse_host_preamble(&preamble)?;
        chosen.ok_or(Error::Version {
            ours: (VersionRange::OURS.min, VersionRange::OURS.max),
            theirs: (theirs.min, theirs.max),
        })?;
        write_control(&mut tcp, &ClientMsg::Hello(caps.clone()))?;
        let mut buf = Vec::new();
        let welcome = match read_control::<HostMsg>(&mut tcp, &mut buf)? {
            HostMsg::Welcome(w) => w,
            HostMsg::Reject(reason) => return Err(Error::Rejected(reason)),
            _ => return Err(Error::Unexpected("the host's first answer is not Welcome")),
        };
        let udp = match welcome.media {
            MediaPlan::Udp { port } => {
                let bind: SocketAddr = if host.is_ipv4() { ([0, 0, 0, 0], 0).into() } else { ([0u16; 8], 0).into() };
                let udp = udp_socket(bind)?;
                udp.connect(SocketAddr::new(host.ip(), port))?;
                Some(udp)
            }
            MediaPlan::Tcp => None,
        };
        // Repeat the hello datagram until MediaReady arrives. The read timeout on the TCP side
        // paces the repeats.
        let started = Instant::now();
        tcp.set_read_timeout(Some(Duration::from_millis(50)))?;
        loop {
            if let Some(udp) = &udp {
                let mut out = PacketBuf::new();
                encode(&ClientDatagram::Hello { session: welcome.session }, out.begin(KIND_DATAGRAM))?;
                udp.send(out.seal()?.packet).ok();
            }
            match read_control::<HostMsg>(&mut tcp, &mut buf) {
                Ok(HostMsg::MediaReady) => break,
                Ok(_) => return Err(Error::Unexpected("expected MediaReady")),
                Err(Error::Timeout) if started.elapsed() < timeout => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Self { duplex: Duplex::new(tcp, udp)?, welcome, host })
    }
}
