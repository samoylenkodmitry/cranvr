//! The size of the pictures that the client asks the host for.
//!
//! The headset reports the size of its eyes' pictures that it recommends. The hardware decoder has
//! limits: the largest picture and the frame rate at each size. The client asks for the
//! recommended eye size unless the frame that the host would encode for it lies beyond the
//! decoder's limits at the refresh rate. It then takes a smaller eye.

use crate::hub::StreamLayout;
use cranpose::DecoderCapabilities;
use cranvr_common::foveation::FoveationLevel;

/// The foveation that the client sizes its eye for. The user picks the level on the Mac, and the
/// client learns it only with the stream. The weakest level, Low, gives the largest frame of the
/// foveated ones, so a decoder that takes it takes every level. Off has a smaller eye of the host's
/// own choice.
const SIZING_LEVEL: FoveationLevel = FoveationLevel::Low;

/// The smallest share of the recommended size that the client asks for.
const LEAST_SHARE: f64 = 0.5;
/// Each try takes this share of the one before.
const STEP: f64 = 0.9;

/// The size of the decoded frame that the host encodes for an eye of `eye` pixels.
pub fn frame_for(eye: [u32; 2], foveation: bool) -> [u32; 2] {
    StreamLayout::new(eye, foveation.then(|| SIZING_LEVEL.config().expect("a foveated level").into())).frame()
}

/// The eye size to ask for: `recommended`, or the largest smaller size that `fits`. `fits` says
/// whether the decoder takes a frame of the given size. A size is even, as the host's encoder
/// wants.
pub fn choose_eye(recommended: [u32; 2], foveation: bool, fits: impl Fn([u32; 2]) -> bool) -> [u32; 2] {
    let mut share: f64 = 1.0;
    loop {
        let used = share.max(LEAST_SHARE);
        let eye = recommended.map(|side| ((f64::from(side) * used).round() as u32) & !1);
        if fits(frame_for(eye, foveation)) || used <= LEAST_SHARE {
            return eye;
        }
        share *= STEP;
    }
}

/// Whether the decoder takes a frame of the size at `refresh_hz`. A platform that gives no limits
/// takes every size here: the decoder fails at start if it cannot.
pub fn decoder_takes(capabilities: &DecoderCapabilities, frame: [u32; 2], refresh_hz: f32) -> bool {
    let [width, height] = frame;
    if capabilities.max_width.is_some_and(|max| width > max) || capabilities.max_height.is_some_and(|max| height > max) {
        return false;
    }
    // Rates are known where the sizes are
    capabilities.max_width.is_none() || capabilities.max_frame_rate(width, height).is_some_and(|rate| rate >= refresh_hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quest_3_eye_is_sized_for_the_largest_foveated_frame() {
        let [w, h] = frame_for([1680, 1760], true);
        assert_eq!(frame_for([1680, 1760], false), [3360, 1760]);
        assert!(w * h < 3_400_000 && w * h > 3_000_000, "{w} x {h}");
        for level in FoveationLevel::ALL {
            let Some(config) = level.config() else { continue };
            let [lw, lh] = StreamLayout::new([1680, 1760], Some(config.into())).frame();
            assert!(lw <= w && lh <= h, "{level:?} gives {lw} x {lh}");
        }
    }

    #[test]
    fn the_recommended_size_stays_when_the_decoder_takes_it() {
        assert_eq!(choose_eye([1680, 1760], true, |_| true), [1680, 1760]);
    }

    #[test]
    fn a_decoder_that_takes_less_gets_a_smaller_eye_that_fits() {
        // A decoder of at most 1,200,000 pixels in a frame
        let fits = |frame: [u32; 2]| frame[0] * frame[1] <= 1_200_000;
        let eye = choose_eye([1680, 1760], true, fits);
        assert!(fits(frame_for(eye, true)) && eye[0] < 1680 && eye[0] % 2 == 0 && eye[1] % 2 == 0);
        // The step is 10 percent, so the eye is not much smaller than it must be
        let next = choose_eye([1680, 1760], true, |frame| fits(frame) || frame[0] * frame[1] > 1_200_000 * 5);
        assert!(next[0] >= eye[0]);
        // The same decoder with no foveation takes a smaller eye still
        assert!(choose_eye([1680, 1760], false, fits)[0] < eye[0]);
    }

    #[test]
    fn a_decoder_that_takes_nothing_gets_the_smallest_share() {
        assert_eq!(choose_eye([1680, 1760], true, |_| false), [840, 880]);
    }
}
