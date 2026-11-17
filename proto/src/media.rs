//! Media packets: how one encoded slice of a frame (a "unit") crosses the network.
//!
//! A unit is `[u16 meta length][FrameMeta in postcard][encoded bytes]`. The sender cuts it into
//! data packets of `shard_len` bytes (the last one may be shorter) and adds parity packets. Each
//! packet starts with a 24-byte [`MediaHeader`] that says which unit it belongs to and how the
//! unit is cut. A receiver needs no packet in particular to start: any packet creates the unit's
//! state.
//!
//! One packet on the wire:
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 1 | kind (`KIND_MEDIA`) |
//! | 1 | 1 | flags: bit 0 parity packet, bit 1 key frame |
//! | 2 | 1 | stream id |
//! | 3 | 1 | slice index |
//! | 4 | 4 | frame index |
//! | 8 | 1 | slice count of the frame |
//! | 9 | 1 | reserved, 0 |
//! | 10 | 2 | fragment index: among the data packets, or among the parity packets |
//! | 12 | 2 | data packet count |
//! | 14 | 2 | parity packet count |
//! | 16 | 4 | unit length in bytes |
//! | 20 | 2 | shard length: payload bytes of a full packet |
//! | 22 | 2 | reserved, 0 |
//!
//! All numbers are little-endian. The slices of a frame are separate units. A slice can go on the
//! wire when its encoder is done. Its parity covers that slice only.
//!
//! The parity is computed over the plain packets. The sender then seals each packet on its own
//! (see [`crate::secure`]): a datagram is the counter, the packet and the tag. A receiver opens
//! each packet that arrives and feeds the plain packet to [`FrameReceiver::push`]. A packet that
//! fails to open counts as lost, so parity restores it. Sealed parity would need the lost packet's
//! tag to open the restored one.

use crate::{
    clock::now_ns,
    fec::{Fec, MAX_PARITY},
    secure::OVERHEAD,
    wire::{decode, encode, Error, Nanos, PacketBuf, Pose, Rect, KIND_MEDIA},
    MTU,
};
use serde::{Deserialize, Serialize};

pub const HEADER_LEN: usize = 24;
/// The payload bytes of a packet that fills a datagram of [`MTU`] bytes once it is sealed.
pub const DEFAULT_SHARD_LEN: usize = MTU - HEADER_LEN - OVERHEAD;
/// The largest unit in bytes.
pub const MAX_UNIT_LEN: usize = 8 << 20;
/// The largest payload of one packet. Over TCP a packet may be much larger than a datagram.
pub const MAX_SHARD_LEN: usize = 60_000;

const FLAG_PARITY: u8 = 1;
const FLAG_KEY: u8 = 2;

/// What the client needs to know about a slice besides its bytes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameMeta {
    pub stream: u8,
    pub frame: u32,
    /// The index of this slice
    pub slice: u8,
    /// The client can start decoding at this frame
    pub key: bool,
    /// The sample time of the tracking data that the frame was rendered for, on the client's clock
    pub tracking_sample: Nanos,
    /// When the host wants the frame on the display, on the client's clock. The client's
    /// compositor may use it as the frame's display time.
    pub target_display: Nanos,
    /// The host's clock when it started to send the frame
    pub sent: Nanos,
    /// The pose that the host rendered each eye from
    pub eye_poses: [Pose; 2],
    /// Where each slice of the frame lies in the decoded image, in pixels
    pub slices: Vec<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaHeader {
    pub parity: bool,
    pub key: bool,
    pub stream: u8,
    pub slice: u8,
    pub slice_count: u8,
    pub frame: u32,
    pub index: u16,
    pub data_count: u16,
    pub parity_count: u16,
    pub unit_len: u32,
    pub shard_len: u16,
}

impl MediaHeader {
    /// Appends the 23 bytes after the kind byte.
    pub fn append_after_kind(&self, out: &mut Vec<u8>) {
        out.push((u8::from(self.parity) * FLAG_PARITY) | (u8::from(self.key) * FLAG_KEY));
        out.push(self.stream);
        out.push(self.slice);
        out.extend_from_slice(&self.frame.to_le_bytes());
        out.push(self.slice_count);
        out.push(0);
        out.extend_from_slice(&self.index.to_le_bytes());
        out.extend_from_slice(&self.data_count.to_le_bytes());
        out.extend_from_slice(&self.parity_count.to_le_bytes());
        out.extend_from_slice(&self.unit_len.to_le_bytes());
        out.extend_from_slice(&self.shard_len.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
    }

    /// Reads the header of a packet (kind byte included) and returns it with the payload.
    pub fn parse(packet: &[u8]) -> Result<(MediaHeader, &[u8]), Error> {
        if packet.len() < HEADER_LEN || packet[0] != KIND_MEDIA {
            return Err(Error::Malformed("media packet too short"));
        }
        let u16_at = |at: usize| u16::from_le_bytes([packet[at], packet[at + 1]]);
        let header = MediaHeader {
            parity: packet[1] & FLAG_PARITY != 0,
            key: packet[1] & FLAG_KEY != 0,
            stream: packet[2],
            slice: packet[3],
            frame: u32::from_le_bytes([packet[4], packet[5], packet[6], packet[7]]),
            slice_count: packet[8],
            index: u16_at(10),
            data_count: u16_at(12),
            parity_count: u16_at(14),
            unit_len: u32::from_le_bytes([packet[16], packet[17], packet[18], packet[19]]),
            shard_len: u16_at(20),
        };
        let payload = &packet[HEADER_LEN..];
        header.validate(payload.len())?;
        Ok((header, payload))
    }

    /// Checks that the numbers agree with each other. A packet that fails is dropped before it
    /// can size a buffer.
    fn validate(&self, payload_len: usize) -> Result<(), Error> {
        let shard = self.shard_len as usize;
        let (data, parity, unit) = (self.data_count as usize, self.parity_count as usize, self.unit_len as usize);
        if shard == 0 || !shard.is_multiple_of(2) || shard > MAX_SHARD_LEN {
            return Err(Error::Malformed("shard length"));
        }
        if data == 0 || unit == 0 || unit > MAX_UNIT_LEN || unit <= (data - 1) * shard || unit > data * shard {
            return Err(Error::Malformed("unit length"));
        }
        // At most one parity packet per data packet, so the parity buffer never exceeds the unit
        if parity > data || parity > MAX_PARITY {
            return Err(Error::Malformed("parity count"));
        }
        let count = if self.parity { parity } else { data };
        if self.index as usize >= count || self.slice_count == 0 || self.slice >= self.slice_count {
            return Err(Error::Malformed("fragment index"));
        }
        let expected = if self.parity || (self.index as usize) + 1 < data { shard } else { unit - (data - 1) * shard };
        if payload_len != expected {
            return Err(Error::Malformed("payload length"));
        }
        Ok(())
    }
}

/// What a sender produced for one unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SendStats {
    pub data_packets: u32,
    pub parity_packets: u32,
    pub bytes: usize,
}

/// Cuts units into packets. It keeps its buffers between units, so a unit costs no allocation
/// once the buffers have grown to the size of the largest unit.
pub struct FrameSender {
    shard_len: usize,
    fec: Fec,
    unit: Vec<u8>,
    pad: Vec<u8>,
    packet: PacketBuf,
}

impl FrameSender {
    /// `shard_len` is the payload bytes of a full packet: [`DEFAULT_SHARD_LEN`] for UDP.
    pub fn new(shard_len: usize) -> Result<Self, Error> {
        if shard_len == 0 || !shard_len.is_multiple_of(2) || shard_len > MAX_SHARD_LEN {
            return Err(Error::Malformed("shard length"));
        }
        Ok(Self { shard_len, fec: Fec::new(), unit: Vec::new(), pad: Vec::new(), packet: PacketBuf::new() })
    }

    /// Sends one unit through `emit`, data packets first, then parity packets. `parity` says how
    /// many parity packets a unit of a given number of data packets gets (see
    /// [`crate::fec::parity_for`] and [`crate::fec::ParityController::parity_count`]). `emit` gets
    /// each packet in the buffer that it was built in. The caller seals it there and sends it, or
    /// takes [`PacketBuf::content`] where nothing is sealed.
    pub fn send_unit(
        &mut self,
        meta: &FrameMeta,
        payload: &[u8],
        parity: impl FnOnce(usize) -> usize,
        mut emit: impl FnMut(&mut PacketBuf) -> Result<(), Error>,
    ) -> Result<SendStats, Error> {
        self.unit.clear();
        self.unit.extend_from_slice(&[0, 0]);
        encode(meta, &mut self.unit)?;
        let meta_len = u16::try_from(self.unit.len() - 2).map_err(|_| Error::TooLarge(self.unit.len()))?;
        self.unit[..2].copy_from_slice(&meta_len.to_le_bytes());
        self.unit.extend_from_slice(payload);
        let unit_len = self.unit.len();
        if unit_len > MAX_UNIT_LEN {
            return Err(Error::TooLarge(unit_len));
        }
        let shard = self.shard_len;
        let data_count = unit_len.div_ceil(shard);
        let parity_count = parity(data_count).min(data_count).min(MAX_PARITY);
        let mut header = MediaHeader {
            parity: false,
            key: meta.key,
            stream: meta.stream,
            slice: meta.slice,
            slice_count: u8::try_from(meta.slices.len()).unwrap_or(u8::MAX).max(meta.slice.saturating_add(1)),
            frame: meta.frame,
            index: 0,
            data_count: data_count as u16,
            parity_count: parity_count as u16,
            unit_len: unit_len as u32,
            shard_len: shard as u16,
        };
        let mut stats = SendStats::default();
        for (i, chunk) in self.unit.chunks(shard).enumerate() {
            header.index = i as u16;
            let buf = self.packet.begin(KIND_MEDIA);
            header.append_after_kind(buf);
            buf.extend_from_slice(chunk);
            stats.bytes += self.packet.content().len() + OVERHEAD;
            emit(&mut self.packet)?;
            stats.data_packets += 1;
        }
        if parity_count > 0 {
            let encoder = self.fec.encoder(data_count, parity_count, shard)?;
            for chunk in self.unit.chunks(shard) {
                let added = if chunk.len() == shard {
                    encoder.add_original_shard(chunk)
                } else {
                    self.pad.clear();
                    self.pad.extend_from_slice(chunk);
                    self.pad.resize(shard, 0);
                    encoder.add_original_shard(&self.pad)
                };
                added.map_err(|_| Error::Malformed("FEC input"))?;
            }
            let result = encoder.encode().map_err(|_| Error::Malformed("FEC encode"))?;
            header.parity = true;
            for (i, recovery) in result.recovery_iter().enumerate() {
                header.index = i as u16;
                let buf = self.packet.begin(KIND_MEDIA);
                header.append_after_kind(buf);
                buf.extend_from_slice(recovery);
                stats.bytes += self.packet.content().len() + OVERHEAD;
                emit(&mut self.packet)?;
                stats.parity_packets += 1;
            }
        }
        Ok(stats)
    }
}

/// A unit that arrived whole.
pub struct Received {
    pub meta: FrameMeta,
    /// The data packets that FEC restored
    pub packets_recovered: u16,
    /// When the unit's first packet arrived, as the caller's `now` said
    pub first_packet: Nanos,
    /// When the unit became complete
    pub completed: Nanos,
    buf: Vec<u8>,
    payload_start: usize,
}

impl Received {
    /// The encoded bytes of the slice.
    pub fn payload(&self) -> &[u8] {
        &self.buf[self.payload_start..]
    }
}

/// A unit that did not arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LostUnit {
    pub stream: u8,
    pub frame: u32,
    pub slice: u8,
}

/// Units in flight at the same time. A slice of a frame is a unit, so 32 hold several frames of
/// several slices.
const SLOTS: usize = 32;
/// Finished units whose late packets the receiver drops.
const CLOSED: usize = 128;

struct Slot {
    active: bool,
    key: u64,
    /// Counts up per unit, so the oldest slot is the lowest
    age: u64,
    first_at: Nanos,
    header: MediaHeader,
    got: Vec<bool>,
    data_got: usize,
    parity_got: usize,
    data: Vec<u8>,
    parity: Vec<u8>,
}

impl Slot {
    fn new() -> Self {
        Self {
            active: false,
            key: 0,
            age: 0,
            first_at: 0,
            header: MediaHeader {
                parity: false,
                key: false,
                stream: 0,
                slice: 0,
                slice_count: 1,
                frame: 0,
                index: 0,
                data_count: 0,
                parity_count: 0,
                unit_len: 0,
                shard_len: 0,
            },
            got: Vec::new(),
            data_got: 0,
            parity_got: 0,
            data: Vec::new(),
            parity: Vec::new(),
        }
    }

    fn packets(&self) -> u32 {
        self.header.data_count as u32 + self.header.parity_count as u32
    }
}

fn unit_key(stream: u8, slice: u8, frame: u32) -> u64 {
    (stream as u64) << 40 | (slice as u64) << 32 | frame as u64
}

/// Reassembles units from packets that arrive in any order, with gaps and duplicates.
pub struct FrameReceiver {
    slots: Vec<Slot>,
    closed: [u64; CLOSED],
    closed_next: usize,
    fec: Fec,
    pool: Vec<Vec<u8>>,
    next_age: u64,
    lost: Vec<LostUnit>,
    counters: crate::message::LossReport,
}

impl Default for FrameReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameReceiver {
    pub fn new() -> Self {
        Self {
            slots: (0..SLOTS).map(|_| Slot::new()).collect(),
            closed: [u64::MAX; CLOSED],
            closed_next: 0,
            fec: Fec::new(),
            pool: Vec::new(),
            next_age: 0,
            lost: Vec::new(),
            counters: Default::default(),
        }
    }

    /// Takes one packet (kind byte included). It returns the unit when this packet completes it,
    /// with the help of parity if some data packets are missing. `now` is the caller's clock.
    pub fn push(&mut self, packet: &[u8], now: Nanos) -> Result<Option<Received>, Error> {
        let (header, payload) = MediaHeader::parse(packet)?;
        let key = unit_key(header.stream, header.slice, header.frame);
        if self.closed.contains(&key) {
            // A parity packet after its unit completed, or a straggler of a unit given up
            self.counters.packets_received += 1;
            return Ok(None);
        }
        let at = match self.slots.iter().position(|s| s.active && s.key == key) {
            Some(at) => at,
            None => self.open(header, key, now),
        };
        let slot = &mut self.slots[at];
        let same = slot.header.data_count == header.data_count
            && slot.header.parity_count == header.parity_count
            && slot.header.unit_len == header.unit_len
            && slot.header.shard_len == header.shard_len;
        if !same {
            return Err(Error::Malformed("packet disagrees with its unit"));
        }
        let data_count = header.data_count as usize;
        let shard = header.shard_len as usize;
        let place = if header.parity { data_count + header.index as usize } else { header.index as usize };
        if slot.got[place] {
            return Ok(None);
        }
        slot.got[place] = true;
        self.counters.packets_received += 1;
        if header.parity {
            let from = header.index as usize * shard;
            slot.parity[from..from + shard].copy_from_slice(payload);
            slot.parity_got += 1;
        } else {
            let from = header.index as usize * shard;
            slot.data[from..from + payload.len()].copy_from_slice(payload);
            slot.data_got += 1;
        }
        let recovered = if slot.data_got == data_count {
            0
        } else if slot.data_got + slot.parity_got >= data_count {
            self.recover(at)?
        } else {
            return Ok(None);
        };
        Ok(self.finish(at, recovered, now))
    }

    /// Starts a unit in a free slot, or in the oldest slot when none is free (its unit is lost).
    fn open(&mut self, header: MediaHeader, key: u64, now: Nanos) -> usize {
        let at = match self.slots.iter().position(|s| !s.active) {
            Some(at) => at,
            None => {
                let oldest = (0..SLOTS).min_by_key(|&i| self.slots[i].age).expect("slots exist");
                self.give_up(oldest);
                oldest
            }
        };
        let (data_count, parity_count, shard) = (header.data_count as usize, header.parity_count as usize, header.shard_len as usize);
        let spare = self.pool.pop();
        let slot = &mut self.slots[at];
        slot.active = true;
        slot.key = key;
        slot.age = self.next_age;
        self.next_age += 1;
        slot.first_at = now;
        slot.header = header;
        slot.got.clear();
        slot.got.resize(data_count + parity_count, false);
        slot.data_got = 0;
        slot.parity_got = 0;
        if let Some(spare) = spare {
            slot.data = spare;
        }
        slot.data.clear();
        // The tail of the last packet must be zero: it is part of the code's last data shard
        slot.data.resize(data_count * shard, 0);
        slot.parity.clear();
        slot.parity.resize(parity_count * shard, 0);
        at
    }

    fn close(&mut self, key: u64) {
        self.closed[self.closed_next] = key;
        self.closed_next = (self.closed_next + 1) % CLOSED;
    }

    /// Counts the unit of a slot as lost and frees the slot.
    fn give_up(&mut self, at: usize) {
        let slot = &mut self.slots[at];
        if !slot.active {
            return;
        }
        slot.active = false;
        let (key, packets) = (slot.key, slot.packets());
        let h = slot.header;
        self.counters.packets_expected += packets;
        self.counters.units_lost += 1;
        self.lost.push(LostUnit { stream: h.stream, frame: h.frame, slice: h.slice });
        self.close(key);
    }

    /// Restores the missing data packets of a slot from its parity. It returns how many.
    fn recover(&mut self, at: usize) -> Result<u16, Error> {
        let slot = &mut self.slots[at];
        let (data_count, parity_count, shard) = (slot.header.data_count as usize, slot.header.parity_count as usize, slot.header.shard_len as usize);
        let decoder = self.fec.decoder(data_count, parity_count, shard)?;
        for i in (0..data_count).filter(|&i| slot.got[i]) {
            decoder.add_original_shard(i, &slot.data[i * shard..(i + 1) * shard]).map_err(|_| Error::Malformed("FEC input"))?;
        }
        for p in (0..parity_count).filter(|&p| slot.got[data_count + p]) {
            decoder.add_recovery_shard(p, &slot.parity[p * shard..(p + 1) * shard]).map_err(|_| Error::Malformed("FEC input"))?;
        }
        let result = decoder.decode().map_err(|_| Error::Malformed("FEC decode"))?;
        let mut restored = 0;
        for (i, bytes) in result.restored_original_iter() {
            slot.data[i * shard..(i + 1) * shard].copy_from_slice(bytes);
            restored += 1;
        }
        Ok(restored)
    }

    /// Turns a complete slot into a [`Received`].
    fn finish(&mut self, at: usize, recovered: u16, now: Nanos) -> Option<Received> {
        let slot = &mut self.slots[at];
        slot.active = false;
        let (key, packets, first_at, unit_len) = (slot.key, slot.packets(), slot.first_at, slot.header.unit_len as usize);
        let mut buf = std::mem::take(&mut slot.data);
        buf.truncate(unit_len);
        self.close(key);
        self.counters.packets_expected += packets;
        self.counters.units_complete += 1;
        self.counters.units_recovered += u32::from(recovered > 0);
        self.counters.packets_recovered += recovered as u32;
        let parsed = (|| {
            let meta_len = u16::from_le_bytes([*buf.first()?, *buf.get(1)?]) as usize;
            let meta: FrameMeta = decode(buf.get(2..2 + meta_len)?).ok()?;
            Some((meta, 2 + meta_len))
        })();
        match parsed {
            Some((meta, payload_start)) => Some(Received { meta, packets_recovered: recovered, first_packet: first_at, completed: now, buf, payload_start }),
            None => {
                // Bytes that complete but do not parse: the sender is broken or the link flips bits
                self.counters.units_complete -= 1;
                self.counters.units_lost += 1;
                self.recycle_buffer(buf);
                None
            }
        }
    }

    /// Gives up the units that have waited longer than `timeout` for their packets.
    pub fn expire(&mut self, now: Nanos, timeout: Nanos) {
        for at in 0..SLOTS {
            if self.slots[at].active && now.saturating_sub(self.slots[at].first_at) > timeout {
                self.give_up(at);
            }
        }
    }

    /// The units given up since the last call. The client asks the host for a key frame.
    pub fn take_lost(&mut self) -> Vec<LostUnit> {
        std::mem::take(&mut self.lost)
    }

    /// The counts since the last call.
    pub fn take_report(&mut self) -> crate::message::LossReport {
        std::mem::take(&mut self.counters)
    }

    /// Returns a unit's buffer for reuse.
    pub fn recycle(&mut self, received: Received) {
        self.recycle_buffer(received.buf);
    }

    fn recycle_buffer(&mut self, buf: Vec<u8>) {
        if self.pool.len() < 8 {
            self.pool.push(buf);
        }
    }
}

/// A [`FrameMeta`] for tests and tools that need one with plausible values.
pub fn sample_meta(stream: u8, frame: u32) -> FrameMeta {
    FrameMeta {
        stream,
        frame,
        slice: 0,
        key: frame == 0,
        tracking_sample: now_ns(),
        target_display: 0,
        sent: now_ns(),
        eye_poses: [Pose::IDENTITY; 2],
        slices: vec![Rect { x: 0, y: 0, w: 1, h: 1 }],
    }
}
