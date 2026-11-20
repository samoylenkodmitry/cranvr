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
//! - `scripted_pointer=desktop` plays both controllers over the Mac's first display instead, with
//!   buttons: it shows that both hands point at once and that right clicks, middle clicks and scroll
//!   reach the host as mouse events. `scripted_pitch` (degrees, default 0) tilts the head that the
//!   panel's place follows from, because a headset on a desk looks down.

use crate::{place, ui};
use cranpose::launch_args;
use cranpose_xr::{script_controller, script_hand, Hand, PinchCycle, Pose, ScriptedButtons, ScriptedController, ScriptedHand, SubspaceModifier};
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
        if args.string("scripted_pointer") == Some("desktop") {
            let pitch = args.string("scripted_pitch").and_then(|text| text.parse::<f32>().ok()).unwrap_or(0.0);
            desktop_pointers(pitch);
            return;
        }
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

/// One step of a controller's script: how long it lasts, and the buttons that are down.
fn step(seconds: f32, buttons: ScriptedButtons) -> (Duration, ScriptedButtons) {
    (Duration::from_secs_f32(seconds), buttons)
}

/// Plays both controllers over the desktop panel, in a turn of 12 seconds that repeats. The left
/// controller points at the panel's left half and the right one at its right half. Each press has a
/// time of its own, and the left X goes down while the right trigger is held, so the host must keep
/// the mouse with the right controller and ignore that press:
///
/// | Seconds | Left controller | Right controller |
/// | --- | --- | --- |
/// | 1.0 | X: the right button | |
/// | 2.0 | the thumbstick forward: scroll up | |
/// | 3.5 | the trigger: the left button | |
/// | 4.5 | the thumbstick to the right: scroll to the right | |
/// | 6.0 | the thumbstick's press: the middle button | |
/// | 7.0 | | A: the right button |
/// | 8.0 | | the thumbstick's press: the middle button |
/// | 9.0 to 10.5 | X at 9.5, which the host ignores | the trigger held, so the left button drags |
/// | 11.0 | | the thumbstick back: scroll down |
fn desktop_pointers(pitch: f32) {
    let (start, width) = crate::display_panel::start_panel();
    // The panel stands in a subspace that follows the head: its origin lies a meter ahead of the
    // head, which the room's origin is
    let head = SubspaceModifier::new().rotate(pitch, 0.0, 0.0).pose();
    let panel = head.then(&Pose::at([0.0, 0.0, -1.0])).then(&start);
    // The picture is 16 by 9, as a Mac's display is
    let size_m = [width, width * 9.0 / 16.0];
    let on_panel = |u: f32, v: f32| panel.then(&Pose::at([(u - 0.5) * size_m[0], (0.5 - v) * size_m[1], 0.0])).position;
    let hand_at = |x: f32| head.then(&Pose::at([x, -0.35, -0.3])).position;
    let rest = ScriptedButtons::default();
    let hold = |controller: ScriptedController, steps: &[(Duration, ScriptedButtons)]| steps.iter().fold(controller, |c, (length, buttons)| c.holding(*length, *buttons));
    let left_at = (hand_at(-0.25), on_panel(0.25, 0.45));
    let left = hold(
        ScriptedController::new(Hand::Left, left_at.0, left_at.1),
        &[
            step(1.0, rest),
            step(0.5, ScriptedButtons { primary: true, ..rest }),
            step(0.5, rest),
            step(1.0, ScriptedButtons { thumbstick: [0.0, 0.8], ..rest }),
            step(0.5, rest),
            step(0.4, ScriptedButtons { trigger: 1.0, ..rest }),
            step(0.6, rest),
            step(1.0, ScriptedButtons { thumbstick: [0.8, 0.0], ..rest }),
            step(0.5, rest),
            step(0.4, ScriptedButtons { thumbstick_click: true, ..rest }),
            step(3.1, rest),
            step(0.4, ScriptedButtons { primary: true, ..rest }),
            step(2.6, rest),
        ],
    )
    .repeating();
    let right_at = (hand_at(0.25), on_panel(0.75, 0.55));
    let right = hold(
        ScriptedController::new(Hand::Right, right_at.0, right_at.1),
        &[
            step(7.0, rest),
            step(0.5, ScriptedButtons { primary: true, ..rest }),
            step(0.5, rest),
            step(0.4, ScriptedButtons { thumbstick_click: true, ..rest }),
            step(0.6, rest),
            step(1.5, ScriptedButtons { trigger: 1.0, ..rest }),
            step(0.5, rest),
            step(1.0, ScriptedButtons { thumbstick: [0.0, -0.8], ..rest }),
        ],
    )
    .repeating();
    log::info!("scripted controllers point at the desktop panel: left {left_at:?}, right {right_at:?}");
    script_controller(left);
    script_controller(right);
}
