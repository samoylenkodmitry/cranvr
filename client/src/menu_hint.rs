//! The line under the hand menu's ball that names the ways to open the menu.
//!
//! A user learns them once. The line shows at the first start and again when the tracking falls to
//! one controller, because the menu then moves and the other hand's buttons are gone. It goes when
//! the menu opens, whichever way. The first opening is saved, so the next start shows no line. The
//! line names what works now: the buttons of the controllers that are present and that the mapping
//! gives the menu, a hold while a link streams and a press while none does, else the ball.
//!
//! A launch with the extra `first_start` shows the line as at a first start and saves nothing, for a
//! test on a headset whose user saw the line already.

use crate::menu_place::{other, MenuPlace};
use cranpose::{launch_args, preferences};
use cranvr_common::controls::{Action, Control, Controls, Hand};

const SEEN_KEY: &str = "menu_hint_seen";

fn first_start_on_launch() -> bool {
    launch_args().boolean("first_start").unwrap_or(false)
}

/// Whether the line shows at the start: the user never opened the menu.
pub fn wanted_at_start() -> bool {
    first_start_on_launch() || preferences().get(SEEN_KEY).is_none()
}

/// Keeps for the next starts that the user opened the menu once.
pub fn mark_seen() {
    if first_start_on_launch() {
        return;
    }
    if let Err(e) = preferences().set(SEEN_KEY, "1") {
        log::warn!("the menu hint's state was not saved: {e}");
    }
}

/// The short name of an input of `hand` in the line
fn input_name(hand: Hand, control: Control) -> String {
    let side = match hand {
        Hand::Left => "left",
        Hand::Right => "right",
    };
    match control {
        Control::Primary | Control::Secondary => control.label(hand).trim_end_matches(" button").to_string(),
        Control::Menu => "the menu button".to_string(),
        Control::Trigger => format!("the {side} trigger"),
        Control::Grip => format!("the {side} grip"),
        Control::StickClick => format!("the {side} stick"),
        Control::StickVertical | Control::StickHorizontal => String::new(),
    }
}

/// The line for the mapping `controls`, the controllers that are present (left, right), whether the
/// cameras track a hand, the menu's place and whether a link streams.
pub fn text(controls: &Controls, present: [bool; 2], hands: bool, place: MenuPlace, streaming: bool) -> String {
    let names: Vec<String> =
        Hand::BOTH.into_iter().filter(|hand| present[hand.index()]).flat_map(|hand| controls.inputs_of(hand, Action::Menu).map(move |control| input_name(hand, control))).collect();
    if !names.is_empty() {
        let verb = if streaming { "Hold" } else { "Press" };
        return format!("{verb} {} to open the menu", names.join(" or "));
    }
    // The ball works for a hand that points at it: the other hand of a menu on a hand, or the one
    // that is present
    let pointer = match place {
        MenuPlace::Hand(hand) => present[other(hand).index()].then_some(other(hand)),
        MenuPlace::View => Hand::BOTH.into_iter().find(|hand| present[hand.index()]),
    };
    match pointer {
        Some(_) => "Point at the ball and pull the trigger".to_string(),
        // A palm turned to the eyes carries a ball, which the other hand's pinch presses
        None if hands => "Show a palm and pinch it with the other hand".to_string(),
        // No input: a look that rests on the ball below (see `gaze.rs`)
        None => "Look down at this ball for a second".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_names_the_buttons_of_the_controllers_that_are_present() {
        let controls = Controls::default();
        let left = MenuPlace::Hand(Hand::Left);
        assert_eq!(text(&controls, [true, true], false, left, true), "Hold the menu button or B to open the menu");
        assert_eq!(text(&controls, [false, true], false, MenuPlace::View, true), "Hold B to open the menu");
        assert_eq!(text(&controls, [true, false], false, MenuPlace::View, false), "Press the menu button to open the menu");
        // A mapping without the menu leaves the ball
        let mut none = controls;
        none.set(Hand::Left, Control::Menu, Action::Console);
        none.set(Hand::Right, Control::Secondary, Action::Panels);
        assert_eq!(text(&none, [true, true], false, left, true), "Point at the ball and pull the trigger");
        assert_eq!(text(&none, [false, true], false, MenuPlace::View, true), "Point at the ball and pull the trigger");
        // No controller: the Mac
        assert_eq!(text(&none, [false, false], false, MenuPlace::View, true), "Look down at this ball for a second");
        assert_eq!(text(&controls, [false, false], false, MenuPlace::View, true), "Look down at this ball for a second");
        // Hands without controllers: the palm
        assert_eq!(text(&none, [false, false], true, MenuPlace::View, true), "Show a palm and pinch it with the other hand");
        // A trigger that opens the menu has its side in the name
        let mut trigger = none;
        trigger.set(Hand::Right, Control::Trigger, Action::Menu);
        assert_eq!(text(&trigger, [true, true], false, left, false), "Press the right trigger to open the menu");
    }

    #[test]
    fn every_line_fits_under_the_ball() {
        let controls = Controls::default();
        for present in [[true, true], [true, false], [false, true], [false, false]] {
            let line = text(&controls, present, false, MenuPlace::View, true);
            assert!(line.len() <= 46, "{line}");
        }
    }
}
