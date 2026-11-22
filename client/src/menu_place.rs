//! Where the hand menu stands: on a hand, or in front of the user.
//!
//! The ball, the menu and the display that rides on the hand stand together (`hand_menu.rs`). Their
//! place follows from the controllers that the user has:
//!
//! - Two controllers: the menu rides on the menu hand, and the other hand points at it. The setting
//!   "Menu hand" names the hand: Left, Right or Auto. Auto takes the hand that does not point. The
//!   hand that points is the one that last pulled its trigger on a panel of the app while the menu
//!   was closed, and the right hand before that. A left-handed user who works a panel with the left
//!   hand thus gets the menu on the right hand.
//! - One controller: the menu stands in front of the user (`MenuPlace::View`), and that controller
//!   points at it. A controller's own ray passes through the panels on that controller, so a menu
//!   on the only hand could not be pressed.
//! - No controller: the menu stands in front of the user too.
//!
//! A controller that the runtime stops tracking (a flat battery, a hand out of the cameras' view)
//! counts as gone after [`LOST_AFTER`], so a short gap moves nothing. A controller that comes back
//! counts after [`BACK_AFTER`] without a gap, so a flicker of the tracking does not move the menu
//! twice. The hand that points changes only while the menu is closed and no ray is on it: the menu
//! never jumps from the hand that the user is working on.
//!
//! The file has no Cranpose state, so a test drives it with plain values.

use cranvr_common::controls::{Hand, MenuHand};
use std::time::Duration;

/// How long a controller may go untracked before the menu leaves its hand
pub const LOST_AFTER: Duration = Duration::from_secs(2);
/// How long a controller must be tracked again before the menu comes back to it
pub const BACK_AFTER: Duration = Duration::from_millis(300);
/// A trigger counts as down above `PRESS` and as up below `RELEASE`, as in Cranpose's pointer.
const PRESS: f32 = 0.6;
const RELEASE: f32 = 0.4;

/// Where the menu, its ball and the hand's display stand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuPlace {
    /// On the controller of this hand. The other hand points at it.
    Hand(Hand),
    /// In front of the user, at arm's reach and a little below the eyes
    View,
}

impl MenuPlace {
    /// The words for the log and for the menu's rows
    pub fn label(self) -> &'static str {
        match self {
            MenuPlace::Hand(Hand::Left) => "on the left hand",
            MenuPlace::Hand(Hand::Right) => "on the right hand",
            MenuPlace::View => "in front of you",
        }
    }
}

/// The other hand.
pub fn other(hand: Hand) -> Hand {
    match hand {
        Hand::Left => Hand::Right,
        Hand::Right => Hand::Left,
    }
}

/// The menu's place for the setting `setting`, the controllers that count as present (left, right)
/// and the hand that points.
pub fn place(setting: MenuHand, present: [bool; 2], pointing: Hand) -> MenuPlace {
    match present {
        [true, true] => MenuPlace::Hand(match setting {
            MenuHand::Left => Hand::Left,
            MenuHand::Right => Hand::Right,
            MenuHand::Auto => other(pointing),
        }),
        _ => MenuPlace::View,
    }
}

/// The hands that work the menu at `place`: the other hand of a menu on a hand, and every hand
/// that is present while the menu stands in front of the user.
pub fn workers(place: MenuPlace, present: [bool; 2]) -> [bool; 2] {
    match place {
        MenuPlace::Hand(hand) => {
            let mut workers = [false; 2];
            workers[other(hand).index()] = true;
            workers
        }
        MenuPlace::View => present,
    }
}

/// What the headset knows about the controllers in one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlaceFrame {
    /// The frame's time, on any clock that rises
    pub time: Duration,
    /// Whether the runtime tracks each controller now (left, right). A hand that the cameras track
    /// is no controller.
    pub tracked: [bool; 2],
    /// How far each controller's trigger is pulled, from 0 to 1
    pub trigger: [f32; 2],
    /// Whether each hand's ray is on a panel of the app other than the menu and its ball
    pub on_panel: [bool; 2],
    /// The menu is open, or a ray is on the menu or on its ball
    pub menu_busy: bool,
}

/// Follows the controllers from frame to frame and gives the menu's place.
#[derive(Clone, Copy, Debug)]
pub struct Placer {
    present: [bool; 2],
    /// Since when each controller's tracking differs from `present`
    differs_since: [Option<Duration>; 2],
    down: [bool; 2],
    /// The hand that last pulled its trigger on a panel of the app
    pointing: Option<Hand>,
}

impl Default for Placer {
    /// Both controllers count as present at the start, so the usual user with two controllers sees
    /// the ball on the hand from the first frame. A controller that is missing gives it up after
    /// [`LOST_AFTER`].
    fn default() -> Self {
        Self { present: [true; 2], differs_since: [None; 2], down: [false; 2], pointing: None }
    }
}

impl Placer {
    /// Reads one frame and returns the menu's place.
    pub fn update(&mut self, setting: MenuHand, frame: &PlaceFrame) -> MenuPlace {
        for hand in Hand::BOTH {
            let i = hand.index();
            if frame.tracked[i] == self.present[i] {
                self.differs_since[i] = None;
            } else {
                let since = *self.differs_since[i].get_or_insert(frame.time);
                let wait = if frame.tracked[i] { BACK_AFTER } else { LOST_AFTER };
                if frame.time.saturating_sub(since) >= wait {
                    self.present[i] = frame.tracked[i];
                    self.differs_since[i] = None;
                }
            }
            let was_down = self.down[i];
            self.down[i] = if was_down { frame.trigger[i] > RELEASE } else { frame.trigger[i] >= PRESS };
            if self.down[i] && !was_down && frame.on_panel[i] && !frame.menu_busy {
                self.pointing = Some(hand);
            }
        }
        place(setting, self.present, self.pointing.unwrap_or(Hand::Right))
    }

    /// The controllers that count as present now (left, right)
    pub fn present(&self) -> [bool; 2] {
        self.present
    }
}

#[cfg(test)]
#[path = "tests/menu_place.rs"]
mod tests;
