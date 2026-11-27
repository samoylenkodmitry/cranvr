use super::{align, level, mono, place, RATE};

// A sound like a game's: noise whose loudness changes every 30 ms, with a beat every 0.5 s on top,
// as in a song whose bars repeat
fn game_sound(seconds: usize, seed: u32) -> Vec<i16> {
    let mut state = seed;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let mut level = 0.0_f32;
    (0..seconds * RATE)
        .map(|index| {
            if index % 1440 == 0 {
                level = (next() % 1000) as f32 / 1000.0;
            }
            let beat = if index % (RATE / 2) < 2400 { 1.6 } else { 1.0 };
            let noise = (next() % 20_000) as f32 - 10_000.0;
            (noise * level * beat * 0.6) as i16
        })
        .collect()
}

fn silence(seconds: f64) -> Vec<i16> {
    vec![0; (seconds * RATE as f64) as usize]
}

fn joined(parts: &[Vec<i16>]) -> Vec<i16> {
    parts.concat()
}

// What the headset heard of `played`: its sample `k` is the played sound at `at + k * (1 + rate)`,
// quieter, with a little noise of its own, `length` samples long
fn heard(played: &[i16], at: f64, rate: f64, gain: f32, length: usize) -> Vec<i16> {
    let mut state = 7_u32;
    (0..length)
        .map(|k| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let hiss = (state >> 24) as f32 - 128.0;
            let source = at + k as f64 * (1.0 + rate);
            let below = source.floor() as usize;
            let value = match (played.get(below), played.get(below + 1)) {
                (Some(a), Some(b)) => f32::from(*a) + (f32::from(*b) - f32::from(*a)) * (source - below as f64) as f32,
                _ => 0.0,
            };
            (value * gain + hiss) as i16
        })
        .collect()
}

fn close(value: f64, expected: f64, within: f64) -> bool {
    (value - expected).abs() <= within
}

// The owner's recordings of 2026-11-27: a game is silent for its first 45 s, longer than the 30 s
// that the old search compared. The search finds the lag in the sound that comes later.
#[test]
fn a_long_silence_at_the_start_still_finds_the_lag_in_the_sound_after_it() {
    let played = joined(&[silence(45.0), game_sound(20, 1), silence(5.0)]);
    let at = 1.234 * RATE as f64;
    let heard = heard(&played, at, 0.0, 0.6, played.len() - at as usize);
    let found = align(&heard, 0.0, &played).expect("a lag");
    assert!(close(found.offset_s, 1.234, 0.0015), "{found:?}");
    assert_eq!(found.rate, 0.0, "{found:?}");
    assert!(found.matched > 0.9, "{found:?}");
}

// The Mac's clock and the headset's part by 40 parts in a million: over 150 s the lag grows by 6 ms.
// The line follows it.
#[test]
fn clocks_that_part_give_a_line_that_follows_them() {
    let played = game_sound(160, 2);
    let at = 0.5 * RATE as f64;
    let heard = heard(&played, at, 40e-6, 0.5, 150 * RATE);
    let found = align(&heard, 0.0, &played).expect("a lag");
    assert!(close(found.offset_s, 0.5, 0.002), "{found:?}");
    assert!(close(found.rate, 40e-6, 12e-6), "{found:?}");
    assert!(found.windows >= 4 && found.spread_ms <= 2.0, "{found:?}");
}

// The Mac's sound starts 5 s after the picture, as when its first buffer came late: the lag is
// negative, and the windows of the first 5 s, which match nothing, do not count
#[test]
fn a_mac_sound_that_starts_after_the_picture_gives_a_negative_offset() {
    let source = game_sound(60, 3);
    let played = source[5 * RATE..].to_vec();
    let heard = heard(&source, 0.0, 0.0, 0.7, source.len());
    let found = align(&heard, 0.0, &played).expect("a lag");
    assert!(close(found.offset_s, -5.0, 0.0015), "{found:?}");
}

// An app that died during a recording left the Mac's sound of the first minute alone, while the
// headset recorded on for two more: the first minute finds the lag
#[test]
fn a_mac_sound_of_the_first_part_alone_still_finds_the_lag() {
    let source = game_sound(180, 4);
    let played = source[..60 * RATE].to_vec();
    let heard = heard(&source, 0.25 * RATE as f64, 0.0, 0.6, 170 * RATE);
    let found = align(&heard, 0.0, &played).expect("a lag");
    assert!(close(found.offset_s, 0.25, 0.0015), "{found:?}");
}

// The headset's sound starts 65 ms into the file, as in the headset's own recordings
#[test]
fn the_start_of_the_headsets_sound_in_its_file_counts() {
    let played = game_sound(20, 5);
    let heard = heard(&played, 0.3 * RATE as f64, 0.0, 0.6, 18 * RATE);
    let found = align(&heard, 0.065, &played).expect("a lag");
    assert!(close(found.offset_s, 0.3 - 0.065, 0.0015), "{found:?}");
}

// A recording with no sound gives no lag: the caller keeps another sound
#[test]
fn silence_finds_no_lag() {
    let played = game_sound(10, 6);
    assert_eq!(align(&silence(8.0), 0.0, &played), None);
    assert_eq!(align(&heard(&played, 0.0, 0.0, 0.6, 8 * RATE), 0.0, &silence(10.0)), None);
}

// A heard sound that is another sound matches nothing well enough
#[test]
fn another_sound_finds_no_lag() {
    let played = game_sound(30, 7);
    let other = game_sound(20, 8);
    assert_eq!(align(&other, 0.0, &played), None);
}

#[test]
fn the_mac_sound_moves_into_the_pictures_time_and_says_what_it_covers() {
    let stereo: Vec<i16> = (0..10).flat_map(|frame| [frame * 10, -frame * 10]).collect();
    let (out, covered) = place(&stereo, (2.0 / RATE as f64, 0.0), 10);
    assert_eq!(&out[..6], &[20, -20, 30, -30, 40, -40]);
    assert_eq!(covered.iter().filter(|c| **c).count(), 8, "the last two frames lie past its end");
    let (late, covered) = place(&stereo, (-3.0 / RATE as f64, 0.0), 5);
    assert_eq!(&late[..8], &[0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(covered, vec![false, false, false, true, true]);
}

#[test]
fn the_level_of_silence_and_of_a_sound() {
    let quiet = level(&silence(2.0), 1);
    assert_eq!((quiet["rms_db"].as_f64(), quiet["sound_s"].as_u64()), (Some(-120.0), Some(0)));
    let loud = level(&[16384_i16; 2 * RATE], 2);
    assert_eq!((loud["rms_db"].as_f64(), loud["peak_db"].as_f64(), loud["sound_s"].as_u64()), (Some(-6.0), Some(-6.0), Some(1)));
}

#[test]
fn stereo_becomes_the_mean_of_its_channels() {
    assert_eq!(mono(&[100, 300, -50, 50, 7]), vec![200, 0]);
}
