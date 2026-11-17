//! Fragmentation, reassembly and forward error correction, without sockets.

mod common;

use common::{meta, packets, Rng};
use cranvr_proto::{
    fec::{parity_for, ParityController},
    media::{FrameReceiver, FrameSender, MediaHeader, DEFAULT_SHARD_LEN, HEADER_LEN},
    message::LossReport,
    secure::OVERHEAD,
    MTU,
};

/// Header bytes that a packet of a unit adds to its payload: the 2-byte meta length and the meta
/// travel inside the unit, so a test needs the unit size from the packets themselves.
fn unit_len(packets: &[Vec<u8>]) -> usize {
    MediaHeader::parse(&packets[0]).unwrap().0.unit_len as usize
}

fn push_all(receiver: &mut FrameReceiver, packets: &[Vec<u8>]) -> Vec<(u32, Vec<u8>, u16)> {
    let mut done = Vec::new();
    for p in packets {
        if let Some(r) = receiver.push(p, 1).unwrap() {
            done.push((r.meta.frame, r.payload().to_vec(), r.packets_recovered));
        }
    }
    done
}

#[test]
fn a_packet_fits_the_mtu() {
    let payload = Rng(1).bytes(10_000);
    // The packets are not sealed here. Sealing adds the counter and the tag.
    for p in packets(&meta(0), &payload, 0.1) {
        assert!(p.len() + OVERHEAD <= MTU, "{} bytes", p.len());
    }
    assert_eq!(DEFAULT_SHARD_LEN + HEADER_LEN + OVERHEAD, MTU);
}

#[test]
fn units_of_every_shape_arrive_whole() {
    let mut rng = Rng(7);
    let m = meta(3);
    let meta_len = packets(&m, &[1], 0.0).len(); // a one-byte payload makes one packet
    assert_eq!(meta_len, 1);
    // One byte, around one and two shards, an exact multiple of the shard, a large unit
    let overhead = unit_len(&packets(&m, &[0], 0.0)) - 1;
    for size in [1, 2, DEFAULT_SHARD_LEN - overhead - 1, DEFAULT_SHARD_LEN - overhead, DEFAULT_SHARD_LEN - overhead + 1, 5 * DEFAULT_SHARD_LEN - overhead, 100_000, 700_000] {
        let payload = rng.bytes(size);
        for share in [0.0, 0.1] {
            let sent = packets(&m, &payload, share);
            let mut receiver = FrameReceiver::new();
            let done = push_all(&mut receiver, &sent);
            assert_eq!(done.len(), 1, "size {size}, share {share}");
            assert_eq!(done[0].1, payload, "size {size}, share {share}");
            assert_eq!(done[0].0, 3);
            assert_eq!(done[0].2, 0);
            let report = receiver.take_report();
            assert_eq!((report.units_complete, report.units_lost), (1, 0));
            assert_eq!(report.packets_expected as usize, sent.len());
        }
    }
}

#[test]
fn the_unit_length_is_a_multiple_of_the_shard_only_when_the_payload_makes_it() {
    let overhead = unit_len(&packets(&meta(0), &[0], 0.0)) - 1;
    let exact = packets(&meta(0), &vec![9; 4 * DEFAULT_SHARD_LEN - overhead], 0.0);
    assert_eq!(exact.len(), 4);
    // Sealing adds the counter and the tag, which fills the datagram
    assert!(exact.iter().all(|p| p.len() + OVERHEAD == MTU));
}

#[test]
fn packets_in_any_order_with_duplicates_arrive_once() {
    let mut rng = Rng(11);
    let payload = rng.bytes(60_000);
    let mut sent = packets(&meta(5), &payload, 0.2);
    let copies = sent.clone();
    sent.extend(copies);
    rng.shuffle(&mut sent);
    let mut receiver = FrameReceiver::new();
    let done = push_all(&mut receiver, &sent);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].1, payload);
}

#[test]
fn parity_restores_as_many_lost_packets_as_it_has() {
    let mut rng = Rng(21);
    for size in [1_000, 30_000, 200_000] {
        let payload = rng.bytes(size);
        let sent = packets(&meta(1), &payload, 0.1);
        let header = MediaHeader::parse(&sent[0]).unwrap().0;
        let parity = header.parity_count as usize;
        assert!(parity >= 1);
        for round in 0..20 {
            // Lose `parity` packets, from any place in the unit
            let mut lost: Vec<usize> = (0..sent.len()).collect();
            rng.shuffle(&mut lost);
            lost.truncate(parity);
            let mut kept: Vec<Vec<u8>> = sent.iter().enumerate().filter(|(i, _)| !lost.contains(i)).map(|(_, p)| p.clone()).collect();
            rng.shuffle(&mut kept);
            let mut receiver = FrameReceiver::new();
            let done = push_all(&mut receiver, &kept);
            assert_eq!(done.len(), 1, "size {size}, round {round}, lost {lost:?}");
            assert_eq!(done[0].1, payload, "size {size}, round {round}");
            let data_lost = lost.iter().filter(|&&i| i < header.data_count as usize).count();
            assert_eq!(done[0].2 as usize, if data_lost > 0 { data_lost } else { 0 });
        }
    }
}

#[test]
fn a_unit_with_too_few_packets_is_reported_lost_and_never_delivered_corrupt() {
    let mut rng = Rng(31);
    let payload = rng.bytes(50_000);
    let sent = packets(&meta(9), &payload, 0.1);
    let header = MediaHeader::parse(&sent[0]).unwrap().0;
    let parity = header.parity_count as usize;
    let mut receiver = FrameReceiver::new();
    // Keep one packet less than the data count needs
    let need = header.data_count as usize;
    let kept: Vec<Vec<u8>> = sent.iter().take(need - 1).cloned().collect();
    assert!(push_all(&mut receiver, &kept).is_empty());
    assert!(parity >= 1 && sent.len() > need);
    receiver.expire(1_000_000_000, 100_000_000);
    let lost = receiver.take_lost();
    assert_eq!(lost.len(), 1);
    assert_eq!((lost[0].frame, lost[0].slice), (9, 0));
    let report = receiver.take_report();
    assert_eq!((report.units_lost, report.units_complete), (1, 0));
    assert_eq!(report.packets_expected as usize, sent.len());
    assert_eq!(report.packets_received as usize, need - 1);
    // A straggler of the lost unit does not start a new unit
    assert!(receiver.push(&sent[need], 2_000_000_000).unwrap().is_none());
    receiver.expire(9_000_000_000, 100_000_000);
    assert!(receiver.take_lost().is_empty());
}

#[test]
fn a_unit_without_parity_loses_to_one_missing_packet() {
    let mut rng = Rng(41);
    let sent = packets(&meta(2), &rng.bytes(20_000), 0.0);
    let mut receiver = FrameReceiver::new();
    assert!(push_all(&mut receiver, &sent[1..]).is_empty());
    receiver.expire(10, 1);
    assert_eq!(receiver.take_lost().len(), 1);
}

#[test]
fn losing_a_share_of_packets_is_covered_by_a_larger_parity_share() {
    // 600 frames of 100 packets with 5 percent random loss. A 5 percent share leaves some frames
    // short, a 20 percent share leaves almost none. Whatever arrives equals what was sent.
    let payload = Rng(5).bytes(100 * DEFAULT_SHARD_LEN - 200);
    let mut results = Vec::new();
    for share in [0.0f32, 0.05, 0.2] {
        let mut rng = Rng(77);
        let mut sender = FrameSender::new(DEFAULT_SHARD_LEN).unwrap();
        let mut receiver = FrameReceiver::new();
        let (mut delivered, mut sent_frames) = (0, 0);
        for frame in 0..600u32 {
            let mut m = meta(frame);
            m.key = false;
            let mut wire = Vec::new();
            sender.send_unit(&m, &payload, |n| parity_for(n, share), |packet| { wire.push(packet.content().to_vec()); Ok(()) }).unwrap();
            sent_frames += 1;
            for p in &wire {
                if rng.unit() < 0.05 {
                    continue;
                }
                if let Some(r) = receiver.push(p, frame as u64 * 8_000_000).unwrap() {
                    assert_eq!(r.payload(), &payload[..], "frame {frame}");
                    delivered += 1;
                    receiver.recycle(r);
                }
            }
            receiver.expire(frame as u64 * 8_000_000, 50_000_000);
        }
        results.push((share, delivered as f64 / sent_frames as f64));
    }
    let [(_, none), (_, small), (_, large)] = results[..] else { unreachable!() };
    // Without parity almost every 100-packet frame loses a packet: 0.95^100 is 0.6 percent
    assert!(none < 0.05, "{results:?}");
    assert!(small > none && small < 0.95, "{results:?}");
    assert!(large > 0.99, "{results:?}");
}

#[test]
fn units_of_a_frame_are_independent() {
    let mut rng = Rng(3);
    let mut all = Vec::new();
    let mut payloads = Vec::new();
    for slice in 0..3u8 {
        let mut m = meta(8);
        m.slice = slice;
        m.slices = vec![Default::default(); 3];
        let payload = rng.bytes(5_000 + slice as usize * 7_000);
        all.extend(packets(&m, &payload, 0.1));
        payloads.push(payload);
    }
    rng.shuffle(&mut all);
    let mut receiver = FrameReceiver::new();
    let mut got = Vec::new();
    for p in &all {
        if let Some(r) = receiver.push(p, 1).unwrap() {
            got.push((r.meta.slice, r.payload().to_vec()));
        }
    }
    got.sort();
    assert_eq!(got.len(), 3);
    for (slice, bytes) in got {
        assert_eq!(bytes, payloads[slice as usize]);
    }
}

#[test]
fn bad_packets_are_refused_without_harm() {
    let mut rng = Rng(9);
    let sent = packets(&meta(0), &rng.bytes(5_000), 0.1);
    let mut receiver = FrameReceiver::new();
    assert!(receiver.push(&[], 0).is_err());
    assert!(receiver.push(&sent[0][..10], 0).is_err());
    // A header that claims more than it carries
    let mut cut = sent[0].clone();
    cut.truncate(cut.len() - 1);
    assert!(receiver.push(&cut, 0).is_err());
    // Random bytes with the right kind byte
    for _ in 0..1000 {
        let mut junk = rng.bytes(200);
        junk[0] = cranvr_proto::wire::KIND_MEDIA;
        let _ = receiver.push(&junk, 0);
    }
    // The receiver still works
    let done = push_all(&mut receiver, &sent);
    assert!(done.len() <= 1);
}

#[test]
fn a_header_that_claims_a_huge_parity_is_refused_before_it_sizes_a_buffer() {
    let mut rng = Rng(5);
    let sent = packets(&meta(0), &rng.bytes(5_000), 0.1);
    let mut receiver = FrameReceiver::new();
    // Bytes 14 and 15 hold the parity count. 65535 packets of 1352 bytes would be 88 MB.
    let mut hostile = sent[0].clone();
    hostile[14..16].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(receiver.push(&hostile, 0).is_err());
    // More parity packets than data packets is refused too
    let mut more = sent[0].clone();
    let data = u16::from_le_bytes([more[12], more[13]]);
    more[14..16].copy_from_slice(&(data + 1).to_le_bytes());
    assert!(receiver.push(&more, 0).is_err());
    // The unit still arrives from the real packets
    assert_eq!(push_all(&mut receiver, &sent).len(), 1);
}

#[test]
fn the_parity_count_follows_the_share() {
    assert_eq!(parity_for(100, 0.0), 0);
    assert_eq!(parity_for(100, 0.05), 5);
    assert_eq!(parity_for(100, 0.051), 6);
    assert_eq!(parity_for(1, 0.01), 1);
    assert_eq!(parity_for(0, 0.5), 0);
    assert_eq!(parity_for(10, 5.0), 10);
}

fn report(expected: u32, received: u32, units_lost: u32) -> LossReport {
    LossReport { packets_expected: expected, packets_received: received, units_lost, ..Default::default() }
}

#[test]
fn the_controller_raises_the_parity_at_once_and_lowers_it_slowly() {
    let mut c = ParityController::new(0.02, 0.5);
    assert_eq!(c.parity_count(100), 2);
    // No loss keeps the minimum
    c.on_report(&report(1000, 1000, 0));
    assert_eq!(c.parity_count(100), 2);
    // 10 percent loss: the parity covers it at the next report
    c.on_report(&report(1000, 900, 0));
    assert!(c.share() >= 0.2, "{}", c.share());
    let high = c.parity_count(100);
    // The loss stops: the parity falls in steps and reaches the minimum
    let mut steps = 0;
    while c.parity_count(100) > 2 && steps < 500 {
        c.on_report(&report(1000, 1000, 0));
        assert!(c.parity_count(100) <= high);
        steps += 1;
    }
    assert!(steps > 10, "fell in {steps} reports");
    assert_eq!(c.parity_count(100), 2);
    // A report of nothing changes nothing
    c.on_report(&report(0, 0, 0));
    assert_eq!(c.parity_count(100), 2);
}

#[test]
fn a_small_unit_needs_more_parity_per_packet_than_a_large_one() {
    let mut c = ParityController::new(0.05, 0.5);
    // 3 percent loss on the wire
    c.on_report(&report(10_000, 9_700, 0));
    let small = c.parity_count(8);
    let large = c.parity_count(200);
    assert!(small >= 2, "{small}");
    assert!(small as f32 / 8.0 > large as f32 / 200.0, "{small} of 8, {large} of 200");
    // The count leaves a unit lost with a probability near the target: at 3 percent loss a unit of
    // 8 packets with that parity survives in all but about one of a thousand
    let mut rng = Rng(99);
    let (mut survived, rounds) = (0, 20_000);
    for _ in 0..rounds {
        let lost = (0..8 + small).filter(|_| rng.unit() < 0.03).count();
        survived += (lost <= small) as u32;
    }
    assert!(survived as f64 / rounds as f64 > 0.998, "{survived} of {rounds}");
}

#[test]
fn units_that_parity_could_not_save_push_the_parity_up_even_at_a_low_rate() {
    let mut c = ParityController::new(0.02, 0.5);
    c.on_report(&report(1000, 998, 0));
    let before = c.parity_count(100);
    c.on_report(&report(1000, 998, 2));
    assert!(c.parity_count(100) > before + 1, "{} after {before}", c.parity_count(100));
    // The maximum holds
    for _ in 0..50 {
        c.on_report(&report(1000, 100, 10));
    }
    assert_eq!(c.parity_count(100), 50);
    assert_eq!(c.parity_count(1), 1);
}
