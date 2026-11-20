//! What the headset does with the controllers' buttons, from the mapping of `cranvr_common::controls`.
//!
//! The mapping says what each button does in the headset's own interface (the panels, the Mac's
//! desktop panel and the headset itself). A game still gets every button through the tracking
//! input, so this file never hides a button from a game.
//!
//! [`Runner::update`] reads one frame's input and gives the headset actions that fired in this
//! frame ([`Fire`]): show or hide the panels, open the console, recenter, the keyboard. While no
//! link streams, the press fires the action. While the link streams, only a hold of [`HOLD_NS`]
//! does, so a short press stays with the game, as the menu button's hold always did. The host's
//! word that a game runs is not enough for the rule: a game that the host does not report would
//! lose its buttons to a short press.
//!
//! The mouse actions belong to the desktop panel (`desktop.rs`). Cranpose's pointer turns each
//! controller input into one of Compose's five pointer buttons, and `pointer_mapping` here fixes
//! which. The desktop panel then reads which input a button press came from and which hand it came
//! from, and looks up the action in the mapping. Cranpose's own panels take the trigger as their
//! click whatever the mapping says.
//!
//! Apart from `pointer_mapping`, which makes a Cranpose value, the file has no Cranpose state, so a
//! test drives it with plain numbers.

use cranpose::PointerButton;
use cranpose_xr::{ControllerInput, PointerMapping};
use cranvr_common::controls::{Action, Control, Controls, Hand};
use cranvr_proto::message::{buttons as bit, HandInput, Input};

/// How long a button must stay down to fire a headset action while the link streams. A shorter press
/// goes to the game only: games use the buttons for their own menus.
pub const HOLD_NS: u64 = 800_000_000;
/// An analog input counts as down above `PRESS` and as up below `RELEASE`, as Cranpose's pointer
/// counts the trigger, so a trigger held near one value does not flicker.
const PRESS: f32 = 0.6;
const RELEASE: f32 = 0.4;

/// A headset action that fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fire {
    /// The controller whose button fired it, for the haptic click that tells the user
    pub hand: Hand,
    pub action: Action,
}

/// The speed of Cranpose's scroll at a full lean of the thumbstick, in dp each second. Cranpose's
/// own panels scroll at this speed when any thumbstick axis scrolls.
const SCROLL_SPEED: f32 = 800.0;

/// The pointer button that Cranpose gives each input that can click, and no other input gets the
/// same one: the desktop panel then knows from the button which input was pressed. The trigger is
/// the primary button, because the trigger also takes the handles of panels and clicks their
/// buttons. The pointer has five buttons, so the menu button has none.
pub fn button_of(control: Control) -> Option<PointerButton> {
    match control {
        Control::Trigger => Some(PointerButton::Primary),
        Control::Primary => Some(PointerButton::Secondary),
        Control::StickClick => Some(PointerButton::Middle),
        Control::Grip => Some(PointerButton::Back),
        Control::Secondary => Some(PointerButton::Forward),
        Control::Menu | Control::StickVertical | Control::StickHorizontal => None,
    }
}

/// The input whose press Cranpose reports as `button`.
pub fn control_of(button: PointerButton) -> Control {
    match button {
        PointerButton::Primary => Control::Trigger,
        PointerButton::Secondary => Control::Primary,
        PointerButton::Middle => Control::StickClick,
        PointerButton::Back => Control::Grip,
        PointerButton::Forward => Control::Secondary,
    }
}

/// The mapping that Cranpose's pointer gets: an input that some hand maps to a mouse button has a
/// button of its own (`button_of`), and no other input presses a pointer button, so a grip or a
/// face button that does something else reaches no panel as a click. The trigger always has the
/// primary button. The thumbstick scrolls Cranpose's panels while any axis of the mapping scrolls. Cranpose has one
/// mapping for both hands, and the user's mapping has one for each hand. The desktop panel applies
/// the user's mapping to each hand's presses (`desktop.rs`).
pub fn pointer_mapping(controls: &Controls) -> PointerMapping {
    let pairs = [
        (ControllerInput::Trigger, Control::Trigger),
        (ControllerInput::Squeeze, Control::Grip),
        (ControllerInput::Primary, Control::Primary),
        (ControllerInput::Secondary, Control::Secondary),
        (ControllerInput::ThumbstickClick, Control::StickClick),
        (ControllerInput::Menu, Control::Menu),
    ];
    let clicks = |control: Control| control == Control::Trigger || Hand::BOTH.into_iter().any(|hand| controls.action(hand, control).is_click());
    let mapping = pairs
        .into_iter()
        .fold(PointerMapping::new(), |mapping, (input, control)| mapping.with_button(input, button_of(control).filter(|_| clicks(control))));
    let scrolls = Hand::BOTH.into_iter().any(|hand| {
        [Control::StickVertical, Control::StickHorizontal].into_iter().any(|axis| controls.action(hand, axis) == Action::Scroll)
    });
    mapping.with_scroll_speed(if scrolls { SCROLL_SPEED } else { 0.0 })
}

/// Whether the beams of the controllers should show: while at least one of the app's own panels
/// shows. A game draws its own rays, so with every panel hidden the beams would draw over them. The
/// ball above the left controller does not count: it is a control that the other hand presses
/// without a beam, and the beam that it needs would draw over the game.
///
/// - `room_for_panels`: the connect and stats panels may stand (the hands do not belong to a game)
/// - `panels_shown`: the user did not hide the connect and stats panels
/// - `streaming`: the link streams. The connect panel shows anyway while it does not.
/// - `console_open`, `desktop_shown`: the console is open, and the host's desktop panel shows
pub fn beams_wanted(room_for_panels: bool, panels_shown: bool, streaming: bool, console_open: bool, desktop_shown: bool) -> bool {
    let connect = room_for_panels && (panels_shown || !streaming);
    let stats = room_for_panels && panels_shown;
    connect || stats || console_open || desktop_shown
}

/// The value of an input of a hand as 0 or 1 for a button and from 0 to 1 for the trigger and the
/// grip.
fn value(hand: &HandInput, control: Control) -> f32 {
    let button = |mask: u32| if hand.buttons & mask != 0 { 1.0 } else { 0.0 };
    match control {
        Control::Trigger => hand.trigger.max(button(bit::TRIGGER_CLICK)),
        Control::Grip => hand.grip.max(button(bit::GRIP_CLICK)),
        Control::Primary => button(bit::PRIMARY),
        Control::Secondary => button(bit::SECONDARY),
        Control::StickClick => button(bit::STICK_CLICK),
        Control::Menu => button(bit::MENU),
        Control::StickVertical | Control::StickHorizontal => 0.0,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    down: bool,
    /// The client time at which the input went down
    since: u64,
    /// A headset action fired for this press
    fired: bool,
}

/// Follows the buttons of both controllers from one frame to the next.
#[derive(Debug, Default)]
pub struct Runner {
    slots: [[Slot; Control::ALL.len()]; 2],
}

impl Runner {
    /// Reads one frame of input. `now` is on the client's clock. `streams` says that the link
    /// streams, so a game may run. The returned actions fired in this frame.
    pub fn update(&mut self, controls: &Controls, input: &Input, now: u64, streams: bool) -> Vec<Fire> {
        let mut fires = Vec::new();
        for hand in Hand::BOTH {
            let (inputs, mapped) = (&input.hands[hand.index()], controls.hand(hand));
            for (i, control) in Control::ALL.into_iter().enumerate() {
                if !control.exists(hand) || control.is_axis() {
                    continue;
                }
                let (action, level) = (mapped.get(control), value(inputs, control));
                let slot = &mut self.slots[hand.index()][i];
                let down = if slot.down { level > RELEASE } else { level > PRESS };
                if down && !slot.down {
                    *slot = Slot { down, since: now, fired: false };
                } else if !down {
                    *slot = Slot::default();
                }
                if down && action.is_headset() && !slot.fired && (!streams || now.saturating_sub(slot.since) >= HOLD_NS) {
                    slot.fired = true;
                    fires.push(Fire { hand, action });
                }
            }
        }
        fires
    }
}

#[cfg(test)]
#[path = "tests/controls.rs"]
mod tests;
