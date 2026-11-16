//! The gaps between the headset's tracking packets, as the host receives them. The Quest client
//! sends a pose three times per refresh. A gap longer than a refresh means that the network held
//! packets back: the host then has no new pose for a frame and skips that frame.

use serde_json::{json, Value};
use std::{sync::Mutex, time::Instant};

/// The arrival of the last packet, and the gaps since the last report in milliseconds
static GAPS: Mutex<(Option<Instant>, Vec<f32>)> = Mutex::new((None, Vec::new()));

/// Call it for every tracking packet.
pub fn record() {
    record_at(Instant::now());
}

fn record_at(now: Instant) {
    let mut gaps = GAPS.lock().unwrap();
    if let Some(last) = gaps.0.replace(now) {
        gaps.1.push((now - last).as_secs_f32() * 1000.0);
    }
}

/// The gaps since the last call, over `secs` seconds: packets per second, the median, the 90th and
/// 99th percentile and the longest gap in milliseconds. Null without gaps.
pub fn take(secs: f64) -> Value {
    let mut gaps = std::mem::take(&mut GAPS.lock().unwrap().1);
    if gaps.is_empty() {
        return Value::Null;
    }
    gaps.sort_by(f32::total_cmp);
    let at = |share: f32| gaps[((gaps.len() - 1) as f32 * share).round() as usize];
    json!({
        "hz": gaps.len() as f64 / secs,
        "p50": at(0.5),
        "p90": at(0.9),
        "p99": at(0.99),
        "max": at(1.0),
    })
}

#[cfg(test)]
#[path = "tests/pose_gaps.rs"]
mod tests;
