//! Forward error correction. Each unit has its own Reed-Solomon code over its packets: any
//! `data_count` of the `data_count + parity_count` packets restore the unit. The code is
//! [`reed_solomon_simd`], which works in a field of 65536 elements. A key frame of a few hundred
//! packets therefore fits in one block. The crate needs no `unsafe` code from us and picks AVX2,
//! SSSE3 or NEON at run time.
//!
//! The sender sets the parity count per unit with [`ParityController`], which adapts to the loss
//! that the client reports.

use crate::{message::LossReport, wire::Error};
use reed_solomon_simd::{ReedSolomonDecoder, ReedSolomonEncoder};

/// The most parity packets in one unit.
pub const MAX_PARITY: usize = 2048;

/// The parity packets for a unit of `data_count` packets at a parity `share` (0.1 means one
/// parity packet per ten data packets, rounded up). A share above zero gives at least one.
pub fn parity_for(data_count: usize, share: f32) -> usize {
    if share.is_nan() || share <= 0.0 || data_count == 0 {
        return 0;
    }
    let mut parity = ((data_count as f32 * share).ceil() as usize).clamp(1, MAX_PARITY.min(data_count));
    while parity > 0 && !ReedSolomonEncoder::supports(data_count, parity) {
        parity -= 1;
    }
    parity
}

fn fec_error(_: reed_solomon_simd::Error) -> Error {
    Error::Malformed("FEC parameters")
}

/// An encoder and a decoder that keep their work space between units.
#[derive(Default)]
pub struct Fec {
    encoder: Option<ReedSolomonEncoder>,
    decoder: Option<ReedSolomonDecoder>,
}

impl Fec {
    pub fn new() -> Self {
        Self::default()
    }

    /// An encoder set up for a unit. `shard_len` must be even.
    pub fn encoder(&mut self, data_count: usize, parity_count: usize, shard_len: usize) -> Result<&mut ReedSolomonEncoder, Error> {
        match &mut self.encoder {
            Some(e) => e.reset(data_count, parity_count, shard_len).map_err(fec_error)?,
            slot @ None => *slot = Some(ReedSolomonEncoder::new(data_count, parity_count, shard_len).map_err(fec_error)?),
        }
        Ok(self.encoder.as_mut().expect("set above"))
    }

    pub fn decoder(&mut self, data_count: usize, parity_count: usize, shard_len: usize) -> Result<&mut ReedSolomonDecoder, Error> {
        match &mut self.decoder {
            Some(d) => d.reset(data_count, parity_count, shard_len).map_err(fec_error)?,
            slot @ None => *slot = Some(ReedSolomonDecoder::new(data_count, parity_count, shard_len).map_err(fec_error)?),
        }
        Ok(self.decoder.as_mut().expect("set above"))
    }
}

/// The chance that a unit of `packets` packets (data and parity) loses more than `parity` of
/// them when each packet is lost on its own with the probability `rate`. The sum is the binomial
/// tail.
fn unrecoverable(packets: usize, parity: usize, rate: f64) -> f64 {
    if rate <= 0.0 {
        return 0.0;
    }
    if rate >= 1.0 {
        return 1.0;
    }
    let ratio = rate / (1.0 - rate);
    let mut term = (1.0 - rate).powi(packets as i32);
    let mut covered = term;
    for k in 0..parity {
        term *= (packets - k) as f64 / (k + 1) as f64 * ratio;
        covered += term;
    }
    (1.0 - covered).max(0.0)
}

/// Sets the parity of each unit from the client's loss reports. The estimate of the loss rate
/// rises at once when the loss rises. It falls slowly after that. A burst of loss after a calm
/// stretch costs a frame, while a few spare packets cost little.
///
/// A unit gets the fewest parity packets that keep the chance of losing it below
/// [`TARGET`], if each packet is lost on its own at the estimated rate. A small unit needs more
/// parity per data packet than a large one: a unit of 8 packets at a loss of 3 percent needs 2
/// parity packets, which is 25 percent.
#[derive(Clone, Debug)]
pub struct ParityController {
    min: f32,
    max: f32,
    /// The loss rate that the controller follows
    loss: f32,
    /// A rate added after units that parity could not save, whatever the rate says
    boost: f32,
}

/// The chance of losing a unit that the parity may leave.
const TARGET: f64 = 1e-3;
/// The estimated rate is raised by this factor. Losses come in bursts. A unit sees more than the
/// average.
const HEADROOM: f32 = 1.25;
/// How much of the old loss rate stays after a report with less loss.
const DECAY: f32 = 0.9;

impl ParityController {
    /// `min` is the parity share with no loss at all, `max` the largest share.
    pub fn new(min: f32, max: f32) -> Self {
        Self { min, max, loss: 0.0, boost: 0.0 }
    }

    /// The parity packets for a unit of `data_count` data packets.
    pub fn parity_count(&self, data_count: usize) -> usize {
        let floor = parity_for(data_count, self.min);
        let rate = self.loss * HEADROOM + self.boost;
        if rate <= 0.0 {
            return floor;
        }
        let rate = (rate + 0.002) as f64;
        let cap = parity_for(data_count, self.max).max(floor);
        // The tail falls as the parity grows, so a binary search finds the smallest parity
        let (mut low, mut high) = (floor, cap);
        while low < high {
            let mid = (low + high) / 2;
            if unrecoverable(data_count + mid, mid, rate) <= TARGET {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        low
    }

    /// The parity per data packet for a unit of 100 packets.
    pub fn share(&self) -> f32 {
        self.parity_count(100) as f32 / 100.0
    }

    /// The loss rate that the controller follows now.
    pub fn loss(&self) -> f32 {
        self.loss
    }

    pub fn on_report(&mut self, report: &LossReport) {
        if report.packets_expected == 0 {
            return;
        }
        let lost = report.packets_expected.saturating_sub(report.packets_received);
        let rate = lost as f32 / report.packets_expected as f32;
        self.loss = if rate > self.loss { rate } else { self.loss * DECAY + rate * (1.0 - DECAY) };
        // A rate below a thousandth of a percent counts as none, so the parity can fall back.
        if self.loss < 1e-5 {
            self.loss = 0.0;
        }
        self.boost = if report.units_lost > 0 { (self.boost * 1.5 + 0.01).min(self.max) } else { self.boost * DECAY };
        if self.boost < 1e-4 {
            self.boost = 0.0;
        }
    }
}
