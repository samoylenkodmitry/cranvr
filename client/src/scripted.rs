//! A hand that types, for a test on a headset where no hand can be tracked. Cranpose's
//! `scripted-hand` feature plays a hand in the frame in place of the cameras' hand. The pointer
//! treats it as a real one: a ray, a pinch and a press. This file installs a hand that presses the
//! address field of the connect panel and then the keys of a text, so that a headset on a desk
//! shows that the keyboard opens and types. Only a headset build with the feature has it:
//! `scripts/build-client.sh --scripted-hand` builds `cranvr-client-scripted.apk`, which installs over
//! the plain app. Install the plain build again after the test.
//!
//! Launch arguments: `scripted_text` is the text to type, `scripted_hand` the hand (`left` or
//! `right`, default right). `address_entry` opens the address field at the start, so the field
//! exists when the hand presses it.

use crate::{place, ui};
use cranpose::launch_args;
use cranpose_xr::{script_hand, Hand, PinchCycle, Pose, ScriptedHand};
use std::{sync::Once, time::Duration};

/// Where the hand's aim pose stands: right of the head's start, below it and ahead of it, in meters
const HAND_AT: [f32; 3] = [0.15, -0.25, -0.3];
/// The address field's center from the top of the connect panel, in dp. It follows from the panel's
/// padding, its title and the label above the field.
const FIELD_FROM_TOP: f32 = 208.0;

/// Installs the script that the launch asks for. The first call does the work.
pub fn install_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let args = launch_args();
        let Some(text) = args.string("scripted_text").map(str::to_owned) else { return };
        let hand = if args.string("scripted_hand") == Some("left") { Hand::Left } else { Hand::Right };
        // The panel stands in the subspace that follows the head: its origin lies a meter ahead of the
        // head at the start, and the room's origin is the head at the start
        let subspace = Pose::at([0.0, 0.0, -1.0]);
        let panel = subspace.then(&place::start_pose("connect", ui::side_pose(-1.0)));
        let size = place::start_size("connect", ui::CONNECT_SIZE);
        log::info!("a scripted {hand:?} hand types {text:?} on the connect panel");
        script_hand(ScriptedHand::typing(
            hand,
            HAND_AT,
            panel,
            size,
            [size[0] / 2.0, FIELD_FROM_TOP],
            &text,
            PinchCycle { rest: Duration::from_millis(1500), closing: Duration::from_millis(150), held: Duration::from_millis(250), opening: Duration::from_millis(150) },
        ));
    });
}
