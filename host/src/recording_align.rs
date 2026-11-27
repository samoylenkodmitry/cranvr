//! Where the picture of a recording sits in the Mac's sound.
//!
//! The headset's recorder records the picture and what the headset plays. The headset plays the
//! Mac's sound that the host sent, some tens of milliseconds after the Mac played it. The final file
//! takes the Mac's own sound: it has no loss, no headset volume and nothing of the headset's own
//! sounds. The lag between the two recordings of the same sound puts the Mac's sound into the
//! picture's time, as the user heard it.
//!
//! A game is often silent for a minute or more: in its menus, while it loads, between two songs. The
//! search therefore takes its windows from the whole recording, not from its start: the recording
//! falls into up to six parts, and each part gives the 8 s in which the headset's loudness changes
//! most. Each window then searches the whole Mac sound for the place where the loudness changes in
//! the same way: first in steps of 10 ms, then to the millisecond. No guess limits the search, so a
//! wrong time of the start costs nothing.
//!
//! The windows that agree within 40 ms give a line: the Mac sound's time at the picture's start
//! and how fast the two clocks part. The Mac's sound runs on the clock of its output device, the
//! picture on the headset's: they part by some parts in a million, which over 10 minutes makes some
//! milliseconds. The final file follows the line, so the sound stays where the user heard it to
//! the end.

const RATE: usize = crate::wav::SAMPLE_RATE as usize;
/// The samples in one step of the loudness: 1 ms
const STEP: usize = RATE / 1000;
/// The steps of 1 ms in one coarse step: 10 ms
const COARSE: usize = 10;
/// The length of a window, in coarse steps: 8 s
const WINDOW: usize = 800;
/// The shortest window, in coarse steps: 2 s
const LEAST_WINDOW: usize = 200;
/// The most windows: one in each part of the recording
const MOST_WINDOWS: usize = 6;
/// A window starts every 0.5 s within its part, in coarse steps
const HOP: usize = 50;
/// A window whose loudness changes by less than this, in 16-bit steps, holds no sound to match:
/// silence and a steady hum
const LEAST_CHANGE: f32 = 8.0;
/// A window matches only this well or better, from 0 to 1
const LEAST_MATCH: f32 = 0.5;
/// A lone window that agrees with no other must match this well
const LONE_MATCH: f32 = 0.7;
/// Windows whose lags differ by at most this much agree, in ms
const AGREE_MS: f64 = 40.0;
/// The fine search spans this many steps of 1 ms on each side of the coarse lag
const FINE_SPAN: i64 = 15;
/// Clocks that part faster than this are no clocks: a match that says so is wrong, in s per s
const MOST_RATE: f64 = 1e-3;
/// A line needs windows this far apart, in s
const LEAST_SPAN_S: f64 = 20.0;

/// Where the picture sits in the Mac's sound: the Mac sound's time at the picture's time `p` is
/// `offset_s + (1 + rate) * p`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Alignment {
    /// The Mac sound's time at the picture's start, in s. Negative when the Mac's sound starts after it.
    pub offset_s: f64,
    /// How much faster the Mac sound's clock runs than the picture's, in s per s
    pub rate: f64,
    /// The windows that agreed, and the windows with sound
    pub windows: usize,
    pub tried: usize,
    /// The median match of the windows that agreed, from 0 to 1
    pub matched: f32,
    /// The farthest that a window that agreed lies from the line, in ms
    pub spread_ms: f64,
}

/// One window's answer: the picture's time of its middle, the lag in ms and how well it matched
#[derive(Clone, Copy, Debug)]
struct Found {
    at_s: f64,
    lag_ms: f64,
    matched: f32,
}

/// Where the picture sits in `played`, the Mac's sound, from `heard`, the headset's own sound whose
/// first sample lies at `heard_start` s of the picture. Both are one channel at 48 kHz. `None` when
/// the headset heard nothing that matches, as in silence.
pub fn align(heard: &[i16], heard_start: f64, played: &[i16]) -> Option<Alignment> {
    let (heard_fine, played_fine) = (loudness(heard), loudness(played));
    let (heard_coarse, played_coarse) = (coarse(&heard_fine), coarse(&played_fine));
    let window = WINDOW.min(heard_coarse.len());
    if window < LEAST_WINDOW || played_coarse.len() < window {
        return None;
    }
    let played_sums = Sums::of(&played_coarse);
    let played_fine_sums = Sums::of(&played_fine);
    let starts = windows(&heard_coarse, window);
    let tried = starts.len();
    let found: Vec<Found> = starts
        .into_iter()
        .filter_map(|start| {
            let (lag, _) = best_lag(&heard_coarse[start..start + window], start, &played_coarse, &played_sums, None)?;
            let (start_fine, length_fine) = (start * COARSE, window * COARSE);
            let span = (lag * COARSE as i64 - FINE_SPAN, lag * COARSE as i64 + FINE_SPAN);
            let (fine, matched) = best_lag(&heard_fine[start_fine..start_fine + length_fine], start_fine, &played_fine, &played_fine_sums, Some(span))?;
            let at_s = heard_start + (start_fine + length_fine / 2) as f64 / 1000.0;
            Some(Found { at_s, lag_ms: fine as f64, matched })
        })
        .filter(|found| found.matched >= LEAST_MATCH)
        .collect();
    let agreed = agreeing(&found);
    let lone_ok = agreed.len() == 1 && found.len() == 1 && agreed[0].matched >= LONE_MATCH;
    if agreed.len() < 2 && !lone_ok {
        return None;
    }
    let (a, b) = line(&agreed);
    let spread_ms = agreed.iter().map(|found| (found.lag_ms - (a + b * found.at_s)).abs()).fold(0.0, f64::max);
    let mut matches: Vec<f32> = agreed.iter().map(|found| found.matched).collect();
    matches.sort_by(f32::total_cmp);
    Some(Alignment { offset_s: a / 1000.0 - heard_start, rate: b / 1000.0, windows: agreed.len(), tried, matched: matches[matches.len() / 2], spread_ms })
}

/// The loudness of each step of 1 ms: the mean size of its samples.
fn loudness(samples: &[i16]) -> Vec<f32> {
    samples.as_chunks::<STEP>().0.iter().map(|step| step.iter().map(|sample| f32::from(*sample).abs()).sum::<f32>() / STEP as f32).collect()
}

/// The loudness in steps of 10 ms: the mean of ten steps of 1 ms.
fn coarse(fine: &[f32]) -> Vec<f32> {
    fine.as_chunks::<COARSE>().0.iter().map(|steps| steps.iter().sum::<f32>() / COARSE as f32).collect()
}

/// The start of each window, in coarse steps: in each part of the recording the window whose
/// loudness changes most, when it changes at all.
fn windows(heard: &[f32], window: usize) -> Vec<usize> {
    let parts = (heard.len() / window).clamp(1, MOST_WINDOWS);
    let part = heard.len() / parts;
    let latest = heard.len() - window;
    (0..parts)
        .filter_map(|index| {
            let end = if index + 1 == parts { heard.len() } else { (index + 1) * part };
            let first = (index * part).min(latest);
            let last = end.saturating_sub(window).clamp(first, latest);
            (first..=last)
                .step_by(HOP)
                .map(|start| (start, deviation(&heard[start..start + window])))
                .filter(|(_, change)| *change >= LEAST_CHANGE)
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(start, _)| start)
        })
        .collect()
}

fn deviation(values: &[f32]) -> f32 {
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    (values.iter().map(|value| (value - mean).powi(2)).sum::<f32>() / values.len() as f32).sqrt()
}

/// Running sums of the values and of their squares, for the mean and the power of any stretch
struct Sums {
    values: Vec<f64>,
    squares: Vec<f64>,
}

impl Sums {
    fn of(values: &[f32]) -> Self {
        let (mut sums, mut squares) = (vec![0.0; values.len() + 1], vec![0.0; values.len() + 1]);
        for (index, value) in values.iter().enumerate() {
            sums[index + 1] = sums[index] + f64::from(*value);
            squares[index + 1] = squares[index] + f64::from(*value).powi(2);
        }
        Self { values: sums, squares }
    }

    /// The sum and the sum of squares of `from..from + length`
    fn stretch(&self, from: usize, length: usize) -> (f64, f64) {
        (self.values[from + length] - self.values[from], self.squares[from + length] - self.squares[from])
    }
}

/// The lag at which `window`, which starts at step `start` of the heard sound, matches `played` best:
/// the step of `played` that sounds at the heard sound's first step, and the match from -1 to 1.
/// The search spans every lag that keeps the window inside `played`, or the lags of `span`.
fn best_lag(window: &[f32], start: usize, played: &[f32], sums: &Sums, span: Option<(i64, i64)>) -> Option<(i64, f32)> {
    let length = window.len();
    let mean = window.iter().sum::<f32>() / length as f32;
    let centered: Vec<f32> = window.iter().map(|value| value - mean).collect();
    let power: f64 = centered.iter().map(|value| f64::from(*value).powi(2)).sum();
    if power == 0.0 {
        return None;
    }
    let (fits_low, fits_high) = (-(start as i64), played.len() as i64 - (start + length) as i64);
    let (low, high) = match span {
        Some((low, high)) => (low.max(fits_low), high.min(fits_high)),
        None => (fits_low, fits_high),
    };
    let least_power = f64::from(LEAST_CHANGE).powi(2) * length as f64;
    let mut best: Option<(i64, f32)> = None;
    for lag in low..=high {
        let from = (start as i64 + lag) as usize;
        let (sum, squares) = sums.stretch(from, length);
        let other_power = squares - sum * sum / length as f64;
        // Silence in the Mac's sound matches nothing
        if other_power < least_power {
            continue;
        }
        let cross: f32 = centered.iter().zip(&played[from..from + length]).map(|(a, b)| a * b).sum();
        let matched = (f64::from(cross) / (power * other_power).sqrt()) as f32;
        if best.is_none_or(|(_, top)| matched > top) {
            best = Some((lag, matched));
        }
    }
    best
}

/// The largest group of windows whose lags agree within 40 ms, the better matches first in a tie.
fn agreeing(found: &[Found]) -> Vec<Found> {
    found
        .iter()
        .map(|center| found.iter().copied().filter(|other| (other.lag_ms - center.lag_ms).abs() <= AGREE_MS).collect::<Vec<_>>())
        .max_by(|a, b| a.len().cmp(&b.len()).then(a.iter().map(|f| f.matched).sum::<f32>().total_cmp(&b.iter().map(|f| f.matched).sum::<f32>())))
        .unwrap_or_default()
}

/// The line `lag_ms = a + b * at_s` through the windows by least squares. Windows that lie close
/// together in time, or a slope that no clocks make, give a flat line at their mean.
fn line(found: &[Found]) -> (f64, f64) {
    let count = found.len() as f64;
    let mean_at = found.iter().map(|f| f.at_s).sum::<f64>() / count;
    let mean_lag = found.iter().map(|f| f.lag_ms).sum::<f64>() / count;
    let span = found.iter().map(|f| f.at_s).fold(f64::MIN, f64::max) - found.iter().map(|f| f.at_s).fold(f64::MAX, f64::min);
    if found.len() < 2 || span < LEAST_SPAN_S {
        return (mean_lag, 0.0);
    }
    let covariance: f64 = found.iter().map(|f| (f.at_s - mean_at) * (f.lag_ms - mean_lag)).sum();
    let variance: f64 = found.iter().map(|f| (f.at_s - mean_at).powi(2)).sum();
    let slope = covariance / variance;
    if slope.abs() > MOST_RATE * 1000.0 {
        return (mean_lag, 0.0);
    }
    (mean_lag - slope * mean_at, slope)
}

/// The Mac's sound in the picture's time: `frames` stereo frames at 48 kHz, frame `k` from the Mac
/// sound's time `offset_s + (1 + rate) * k / 48000`. Returns the frames and, for each frame, whether
/// the Mac's sound covers it: before its start and after its end it does not.
pub fn place(stereo: &[i16], alignment: (f64, f64), frames: usize) -> (Vec<i16>, Vec<bool>) {
    let (offset_s, rate) = alignment;
    let count = stereo.len() / 2;
    let mut out = vec![0_i16; frames * 2];
    let mut covered = vec![false; frames];
    for frame in 0..frames {
        let source = (offset_s + (1.0 + rate) * frame as f64 / RATE as f64) * RATE as f64;
        if source < 0.0 || source > (count.max(1) - 1) as f64 {
            continue;
        }
        let below = source.floor() as usize;
        let share = (source - below as f64) as f32;
        let above = (below + 1).min(count - 1);
        for channel in 0..2 {
            let (a, b) = (f32::from(stereo[below * 2 + channel]), f32::from(stereo[above * 2 + channel]));
            out[frame * 2 + channel] = (a + (b - a) * share).round() as i16;
        }
        covered[frame] = true;
    }
    (out, covered)
}

/// One channel from interleaved stereo: the mean of the two.
pub fn mono(stereo: &[i16]) -> Vec<i16> {
    stereo.as_chunks::<2>().0.iter().map(|[left, right]| ((i32::from(*left) + i32::from(*right)) / 2) as i16).collect()
}

/// The loudness of a sound of `channels` interleaved channels for the log: its RMS and its peak in
/// dB of full scale, and the seconds in which its RMS rose above -60 dB, which have sound.
pub fn level(samples: &[i16], channels: usize) -> serde_json::Value {
    let decibels = |value: f64| if value > 0.0 { (20.0 * (value / 32768.0).log10() * 10.0).round() / 10.0 } else { -120.0 };
    let power = samples.iter().map(|sample| f64::from(*sample).powi(2)).sum::<f64>() / samples.len().max(1) as f64;
    let peak = samples.iter().map(|sample| f64::from(sample.unsigned_abs())).fold(0.0, f64::max);
    let loud = samples
        .chunks(RATE * channels.max(1))
        .filter(|part| {
            let rms = (part.iter().map(|sample| f64::from(*sample).powi(2)).sum::<f64>() / part.len() as f64).sqrt();
            rms > 32768.0 * 1e-3
        })
        .count();
    serde_json::json!({"rms_db": decibels(power.sqrt()), "peak_db": decibels(peak), "sound_s": loud})
}

#[cfg(test)]
#[path = "tests/recording_align.rs"]
mod tests;
