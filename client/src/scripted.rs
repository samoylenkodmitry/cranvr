//! A hand that types or points, for a test on a headset where no hand can be tracked. Cranpose's
//! `scripted-hand` feature plays a hand in the frame in place of the cameras' hand. The pointer
//! treats it as a real one: a ray, a pinch and a press. Only a headset build with the feature has
//! it: `scripts/build-client.sh --scripted-hand` builds `cranvr-client-scripted.apk`, which installs
//! over the plain app. Install the plain build again after the test.
//!
//! Launch arguments:
//! - `scripted_text` is the text that the hand types into the address field of the connect panel
//!   and then on the keyboard. `address_entry` opens the address field at the start, so the field
//!   exists when the hand presses it.
//! - `scripted_point` is `ball`: the hand points at the hand menu's ball and pinches in a cycle of
//!   2.5 seconds. It needs `hand_anchor=view`, which puts the ball where a head-mounted test can
//!   reach it. A pinch that closes while the ray is on the ball presses it and opens or closes the
//!   menu. The hand also shows that a hand on a panel is held back from the game (see `hold.rs`).
//! - `scripted_hand` is the hand (`left` or `right`, default right).

use crate::{place, ui};
use cranpose::launch_args;
use cranpose_xr::{script_hand, Hand, PinchCycle, Pose, ScriptedHand};
use std::{sync::Once, time::Duration};

/// Where the hand's aim pose stands: right of the head's start, below it and ahead of it, in meters
const HAND_AT: [f32; 3] = [0.15, -0.25, -0.3];
/// The address field's center from the top of the connect panel, in dp. It follows from the panel's
/// padding, its title and the label above the field.
const FIELD_FROM_TOP: f32 = 208.0;

/// The ball in a headset test with `hand_anchor=view`: the subspace's origin lies a meter ahead of
/// the head at the start, the hand's panels stand 30 cm to the left, 35 cm below it and 60 cm nearer
/// (see `hand_menu.rs`), and the ball is 11 cm above that.
const BALL_AT: [f32; 3] = [-0.30, -0.35 + 0.11, -1.0 + 0.60];

/// Installs the script that the launch asks for. The first call does the work.
pub fn install_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let args = launch_args();
        let hand = if args.string("scripted_hand") == Some("left") { Hand::Left } else { Hand::Right };
        let cycle = PinchCycle { rest: Duration::from_millis(1500), closing: Duration::from_millis(150), held: Duration::from_millis(250), opening: Duration::from_millis(150) };
        if let Some(text) = args.string("scripted_text").map(str::to_owned) {
            // The panel stands in the subspace that follows the head: its origin lies a meter ahead of the
            // head at the start, and the room's origin is the head at the start
            let subspace = Pose::at([0.0, 0.0, -1.0]);
            let panel = subspace.then(&place::start_pose("connect", ui::side_pose(-1.0)));
            let size = place::start_size("connect", ui::CONNECT_SIZE);
            log::info!("a scripted {hand:?} hand types {text:?} on the connect panel");
            script_hand(ScriptedHand::typing(hand, HAND_AT, panel, size, [size[0] / 2.0, FIELD_FROM_TOP], &text, cycle));
        } else if args.string("scripted_point") == Some("ball") {
            log::info!("a scripted {hand:?} hand points at the hand menu's ball");
            script_hand(ScriptedHand::pointing(hand, HAND_AT, BALL_AT, cycle));
        }
    });
}
