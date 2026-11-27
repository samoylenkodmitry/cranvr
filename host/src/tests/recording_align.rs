use super::{find, mono, RATE};

// A sound like a game's: noise whose loudness changes every few tens of milliseconds
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
            let noise = (next() % 20_000) as f32 - 10_000.0;
            (noise * level) as i16
        })
        .collect()
}

// What the headset heard of `played`: from its sample `at` on, quieter, with a little noise of its own
fn heard(played: &[i16], at: usize, gain: f32) -> Vec<i16> {
    let mut state = 7_u32;
    played[at..]
        .iter()
        .map(|sample| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let hiss = (state >> 24) as f32 - 128.0;
            (f32::from(*sample) * gain + hiss) as i16
        })
        .collect()
}

// The headset's recorder started 1.234 s after the Mac's sound: the search finds that sample
#[test]
fn the_lag_of_a_quieter_noisy_copy_is_found_to_the_sample() {
    let played = game_sound(8, 1);
    let at = 59_232;
    let heard = heard(&played, at, 0.6);
    let lag = find(&heard[..4 * RATE], &played, 1.0, 1.0).expect("a lag");
    assert_eq!(lag.samples, at as i64, "{lag:?}");
    assert!(lag.matched > 0.9, "{lag:?}");
}

// A recording with no sound gives no lag: the caller falls back to the times of the start
#[test]
fn silence_finds_no_lag() {
    let played = game_sound(4, 2);
    let silence = vec![0_i16; 2 * RATE];
    assert_eq!(find(&silence, &played, 0.5, 0.5), None);
}

// A heard sound that is another sound matches nothing well enough
#[test]
fn another_sound_finds_no_lag() {
    let played = game_sound(4, 3);
    let other = game_sound(2, 4);
    assert_eq!(find(&other, &played, 1.0, 0.5), None);
}

#[test]
fn stereo_becomes_the_mean_of_its_channels() {
    assert_eq!(mono(&[100, 300, -50, 50, 7]), vec![200, 0]);
}
