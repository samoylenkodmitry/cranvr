//! Helpers for the tests. A seeded random generator makes a failure repeat. Sample messages.

#![allow(dead_code)]

use cranvr_proto::{
    fec::parity_for,
    media::{sample_meta, FrameMeta, FrameSender, DEFAULT_SHARD_LEN},
    message::{ClientCaps, Views},
    wire::{Codec, Fov, Pose},
};

/// xorshift64*: enough for tests. It gives the same numbers on every machine.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..n`
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// A number in `0.0..1.0`
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// The client's key
pub fn identity() -> cranvr_proto::identity::Identity {
    cranvr_proto::identity::Identity::from_seed([5; 32])
}

/// The host's key
pub fn host_identity() -> cranvr_proto::identity::Identity {
    cranvr_proto::identity::Identity::from_seed([6; 32])
}

pub fn caps() -> ClientCaps {
    let fov = Fov { left: -0.9, right: 0.8, up: 0.9, down: -1.0 };
    ClientCaps {
        name: "Test headset".into(),
        eye_size: [2064, 2208],
        views: Views {
            fov: [fov, fov],
            eye_pose: [Pose { position: [-0.032, 0.0, 0.0], ..Pose::IDENTITY }, Pose { position: [0.032, 0.0, 0.0], ..Pose::IDENTITY }],
        },
        refresh_rates: vec![72.0, 90.0, 120.0],
        codecs: vec![Codec::Hevc, Codec::H264],
        decoders: 2,
        foveation: true,
        tcp_only: false,
        cable: false,
        microphone: false,
    }
}

/// The packets of one unit, as the sender builds them (not sealed), with `share` parity packets per
/// data packet.
pub fn packets(meta: &FrameMeta, payload: &[u8], share: f32) -> Vec<Vec<u8>> {
    let mut sender = FrameSender::new(DEFAULT_SHARD_LEN).unwrap();
    let mut out = Vec::new();
    sender
        .send_unit(meta, payload, |n| parity_for(n, share), |packet| {
            out.push(packet.content().to_vec());
            Ok(())
        })
        .unwrap();
    out
}

pub fn meta(frame: u32) -> FrameMeta {
    sample_meta(0, frame)
}
