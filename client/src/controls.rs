//! What the headset does with the controllers' buttons, from the mapping of `cranvr_common::controls`.
//!
//! The mapping says what each button does in the headset's own interface (the panels, the Mac's
//! desktop panel and the headset itself). A game still gets every button through the tracking
//! input, so this file never hides a button from a game.
//!
//! [`Runner::update`] reads one frame's input and gives two things:
//! - the headset actions that fired in this frame ([`Fire`]): show or hide the panels, open the
//!   console, recenter, the keyboard. While no game runs, the press fires the action. While a game
//!   runs, only a hold of [`HOLD_NS`] does, so a short press stays with the game, as the menu
//!   button's hold always did;
//! - the pointer state of each hand ([`HandPointer`]): the mouse buttons that the hand's buttons
//!   hold, whether it grabs and how its thumbstick leans. The desktop panel sends it to the Mac.
//!
//! The file has no Cranpose state, so a test drives it with plain numbers.

use cranvr_common::controls::{mouse, Action, Control, Controls, Hand};
use cranvr_proto::message::{buttons as bit, HandInput, Input};

/// How long a button must stay down to fire a headset action while a game runs. A shorter press goes
/// to the game only: games use the buttons for their own menus.
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

/// What a controller does to the panel that it points at, from its buttons and the mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HandPointer {
    /// The mouse buttons that the hand holds, as the bits of `cranvr_common::controls::mouse`
    pub buttons: u8,
    /// A button that grabs a window is down
    pub grab: bool,
    /// How far the thumbstick leans across (to the right) and forward, each from -1 to 1. An axis
    /// that the mapping leaves to nothing gives 0.
    pub scroll: [f32; 2],
}

impl HandPointer {
    /// Whether the hand does anything to a panel: a button is down, a window is grabbed or the
    /// thumbstick scrolls.
    pub fn active(&self) -> bool {
        self.buttons != 0 || self.grab || self.scroll != [0.0; 2]
    }
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
        Control::StickVertical => hand.stick[1],
        Control::StickHorizontal => hand.stick[0],
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
    pointers: [HandPointer; 2],
}

impl Runner {
    /// Reads one frame of input. `now` is on the client's clock. `game_runs` says that a game runs
    /// on the host. The returned actions fired in this frame.
    pub fn update(&mut self, controls: &Controls, input: &Input, now: u64, game_runs: bool) -> Vec<Fire> {
        let mut fires = Vec::new();
        for hand in Hand::BOTH {
            let (inputs, mapped) = (&input.hands[hand.index()], controls.hand(hand));
            let mut pointer = HandPointer::default();
            for (i, control) in Control::ALL.into_iter().enumerate() {
                if !control.exists(hand) {
                    continue;
                }
                let (action, level) = (mapped.get(control), value(inputs, control));
                if control.is_axis() {
                    if action == Action::Scroll {
                        pointer.scroll[usize::from(control == Control::StickVertical)] = level;
                    }
                    continue;
                }
                let slot = &mut self.slots[hand.index()][i];
                let down = if slot.down { level > RELEASE } else { level > PRESS };
                if down && !slot.down {
                    *slot = Slot { down, since: now, fired: false };
                } else if !down {
                    *slot = Slot::default();
                }
                if !down {
                    continue;
                }
                match action {
                    Action::LeftClick => pointer.buttons |= mouse::LEFT,
                    Action::RightClick => pointer.buttons |= mouse::RIGHT,
                    Action::MiddleClick => pointer.buttons |= mouse::MIDDLE,
                    Action::Grab => pointer.grab = true,
                    action if action.is_headset() && !slot.fired && (!game_runs || now.saturating_sub(slot.since) >= HOLD_NS) => {
                        slot.fired = true;
                        fires.push(Fire { hand, action });
                    }
                    _ => {}
                }
            }
            self.pointers[hand.index()] = pointer;
        }
        fires
    }

    /// What a hand does to the panel that it points at, as of the last `update`
    pub fn pointer(&self, hand: Hand) -> HandPointer {
        self.pointers[hand.index()]
    }
}

#[cfg(test)]
#[path = "tests/controls.rs"]
mod tests;
