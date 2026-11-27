//! Where the picture of a recording sits in the Mac's sound.
//!
//! The headset's recorder records the picture and what the headset plays. The headset plays the
//! Mac's sound that the host sent, some tens of milliseconds after the Mac played it. The final file
//! takes the Mac's own sound: it has no loss, no headset volume and nothing of the headset's own
//! sounds. The lag between the two recordings of the same sound puts the Mac's sound into the
//! picture's time, as the user heard it.
//!
//! The search runs in two steps. The loudness of both sounds in steps of 1 ms finds the lag
//! within a millisecond over a span of seconds. The samples themselves then find it to the sample
//! within that millisecond.

const RATE: usize = crate::wav::SAMPLE_RATE as usize;
/// The samples in one step of the loudness: 1 ms
const STEP: usize = RATE / 1000;
/// The longest stretch of the heard sound that the loudness compares: 30 s
const COMPARED_STEPS: usize = 30_000;
/// The length of the samples' comparison: 1 s about the loudest second of the heard sound
const FINE: usize = RATE;
/// A match of the loudness below this share of a perfect one finds no lag
const LEAST_MATCH: f32 = 0.5;

/// A lag that the search found
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lag {
    /// The sample of the played sound that sounds at the heard sound's first sample. It is negative
    /// when the heard sound starts before the played one.
    pub samples: i64,
    /// How well the loudness of the two matches at this lag, from 0 to 1
    pub matched: f32,
}

impl Lag {
    pub fn seconds(self) -> f64 {
        self.samples as f64 / RATE as f64
    }
}

/// The lag of `heard` against `played`, both one channel at 48 kHz, searched within `span` seconds
/// of `guess` seconds. `None` when the heard sound holds too little to match, as in silence.
pub fn find(heard: &[i16], played: &[i16], guess: f64, span: f64) -> Option<Lag> {
    let heard_steps = loudness(&heard[..heard.len().min(COMPARED_STEPS * STEP)]);
    let played_steps = loudness(played);
    // Silence and a steady hum change the loudness by less than a step of the samples
    if heard_steps.iter().all(|value| value.abs() < 1.0) {
        return None;
    }
    let (low, high) = (((guess - span) * 1000.0).round() as i64, ((guess + span) * 1000.0).round() as i64);
    let mut best: Option<(i64, f32)> = None;
    for lag in low..=high {
        let matched = match_at(&heard_steps, &played_steps, lag);
        if best.is_none_or(|(_, top)| matched > top) {
            best = Some((lag, matched));
        }
    }
    let (coarse, matched) = best?;
    if matched < LEAST_MATCH {
        return None;
    }
    let samples = fine(heard, played, coarse * STEP as i64);
    Some(Lag { samples, matched })
}

/// The loudness of each step of 1 ms, less its mean, so silence and a steady hum match nothing.
fn loudness(samples: &[i16]) -> Vec<f32> {
    let steps: Vec<f32> = samples
        .chunks_exact(STEP)
        .map(|step| step.iter().map(|sample| f32::from(*sample).abs()).sum::<f32>() / STEP as f32)
        .collect();
    let mean = steps.iter().sum::<f32>() / steps.len().max(1) as f32;
    steps.into_iter().map(|step| step - mean).collect()
}

/// How well `heard` matches `played` from its step `lag` on, from -1 to 1.
fn match_at(heard: &[f32], played: &[f32], lag: i64) -> f32 {
    let (mut cross, mut heard_power, mut played_power) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (index, value) in heard.iter().enumerate() {
        let Some(other) = usize::try_from(index as i64 + lag).ok().and_then(|at| played.get(at)) else { continue };
        cross += f64::from(*value) * f64::from(*other);
        heard_power += f64::from(*value).powi(2);
        played_power += f64::from(*other).powi(2);
    }
    if heard_power == 0.0 || played_power == 0.0 {
        return 0.0;
    }
    (cross / (heard_power * played_power).sqrt()) as f32
}

/// The lag in samples within a step of `coarse`, from the samples of the loudest second of `heard`.
fn fine(heard: &[i16], played: &[i16], coarse: i64) -> i64 {
    let window = FINE.min(heard.len());
    let start = loudest(heard, window);
    let part = &heard[start..start + window];
    let mut best = (coarse, f64::MIN);
    for lag in coarse - STEP as i64..=coarse + STEP as i64 {
        let mut cross = 0.0_f64;
        let mut power = 0.0_f64;
        for (index, value) in part.iter().enumerate() {
            let Some(other) = usize::try_from((start + index) as i64 + lag).ok().and_then(|at| played.get(at)) else { continue };
            cross += f64::from(*value) * f64::from(*other);
            power += f64::from(*other).powi(2);
        }
        let score = if power > 0.0 { cross / power.sqrt() } else { f64::MIN };
        if score > best.1 {
            best = (lag, score);
        }
    }
    best.0
}

/// The start of the loudest stretch of `window` samples, in steps of a tenth of the window.
fn loudest(samples: &[i16], window: usize) -> usize {
    let hop = (window / 10).max(1);
    (0..=samples.len().saturating_sub(window))
        .step_by(hop)
        .max_by_key(|start| samples[*start..*start + window].iter().map(|sample| u64::from(sample.unsigned_abs())).sum::<u64>())
        .unwrap_or(0)
}

/// One channel from interleaved stereo: the mean of the two.
pub fn mono(stereo: &[i16]) -> Vec<i16> {
    stereo.chunks_exact(2).map(|pair| ((i32::from(pair[0]) + i32::from(pair[1])) / 2) as i16).collect()
}

#[cfg(test)]
#[path = "tests/recording_align.rs"]
mod tests;
