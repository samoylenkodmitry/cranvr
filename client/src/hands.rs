//! Who owns the user's hands: the panels or the game.
//!
//! Cranpose's backend points a tracked hand at the panels and presses with its pinch, for an app
//! that declares `Use::hand_tracking()` (see `build.rs`). The same cameras serve the game: the host
//! asks the headset for the hands (`HostMsg::TrackingWish`) when a game uses them, and for a game
//! that has no hand code it makes a controller of a hand (`host/src/tracking.rs`), so that a pinch
//! is the game's trigger. Both readings come from one frame, so they do not disturb each other.
//! The user's pinch is the problem: it would press a panel behind the game's picture in the same
//! moment, and a press on a button such as Stop ends what the user plays.
//!
//! The rule: **while the host asks for the hands and a tracked hand points at the panels, the hands
//! belong to the game, and the panels that stand in the room do not show.** That covers the connect
//! panel and the stats panel. It covers the ball above the left controller as well. Cranpose gives a
//! panel that is not composed no hit, so no pinch can press it, and the app needs no switch of the
//! pointer. The console stays. The user opens it on purpose with a hold of the left menu button.
//! Its Close button hides it again.
//!
//! Why this rule and not another one:
//! - The wish is the host's own word for "a pinch is input of the game". It comes from the runtime
//!   of the game, so the client names no game.
//! - Cranpose lets a controller point first, and a hand points only on a side with no controller in
//!   use. A user who holds both controllers has no hand pointer, so nothing fights, and the panels
//!   stay. A game with hand code that also takes controllers (OpenComposite asks for hands for
//!   every OpenVR game) then keeps its panels and its ball.
//! - Without a game the host asks for no hands, so the panels work with a pinch before a game
//!   starts and after it ends.
//! - The wish ends with the game or the link, and a hand that the cameras lose for a moment keeps
//!   the answer for a second and a half. The panels come back after that.
//!
//! The way out for a user who plays with hands only: the panels stay hidden, and the console opens
//! with the hold of the left menu button of a controller. The user picks one up, and its ray points
//! first. Without a controller the user ends the game in the cranvr app on the Mac, or leaves the
//! headset app through the system's menu.

use cranvr_proto::tracking::Features;

/// Whether the hands belong to the game now: the host asked for the hands (`wish`), and a tracked
/// hand points at the panels (`hand_points`).
pub fn belong_to_game(wish: Features, hand_points: bool) -> bool {
    hand_points && wish.contains(Features::HANDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hands_belong_to_the_game_only_while_the_host_asks_for_them_and_a_hand_points() {
        assert!(!belong_to_game(Features::NONE, true));
        assert!(belong_to_game(Features::HANDS, true));
        assert!(belong_to_game(Features::HANDS.union(Features::EYES), true));
        // A game that reads the face or the eyes leaves the hands to the panels
        assert!(!belong_to_game(Features::FACE.union(Features::EYES), true));
        assert!(!belong_to_game(Features::BODY, true));
        // Both controllers in the hands: no hand points, so a game that asks for hands keeps the panels
        assert!(!belong_to_game(Features::HANDS, false));
    }
}
