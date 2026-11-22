use super::*;

const BOTH: [bool; 2] = [true, true];
const LEFT_ONLY: [bool; 2] = [true, false];
const RIGHT_ONLY: [bool; 2] = [false, true];
const NONE: [bool; 2] = [false, false];

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn frame(time: u64, tracked: [bool; 2]) -> PlaceFrame {
    PlaceFrame { time: ms(time), tracked, ..PlaceFrame::default() }
}

#[test]
fn two_controllers_put_the_menu_on_the_menu_hand_and_one_or_none_put_it_in_front_of_the_user() {
    assert_eq!(place(MenuHand::Left, BOTH, Hand::Right), MenuPlace::Hand(Hand::Left));
    assert_eq!(place(MenuHand::Right, BOTH, Hand::Right), MenuPlace::Hand(Hand::Right));
    // Auto takes the hand that does not point
    assert_eq!(place(MenuHand::Auto, BOTH, Hand::Right), MenuPlace::Hand(Hand::Left));
    assert_eq!(place(MenuHand::Auto, BOTH, Hand::Left), MenuPlace::Hand(Hand::Right));
    // With one controller its own ray could not press a menu on it, whatever the setting says
    for setting in MenuHand::ALL {
        for present in [LEFT_ONLY, RIGHT_ONLY, NONE] {
            assert_eq!(place(setting, present, Hand::Right), MenuPlace::View, "{setting:?} {present:?}");
        }
    }
}

#[test]
fn the_hand_that_works_the_menu_is_the_other_hand_or_the_one_that_is_present() {
    assert_eq!(workers(MenuPlace::Hand(Hand::Left), BOTH), [false, true]);
    assert_eq!(workers(MenuPlace::Hand(Hand::Right), BOTH), [true, false]);
    assert_eq!(workers(MenuPlace::View, LEFT_ONLY), [true, false]);
    assert_eq!(workers(MenuPlace::View, RIGHT_ONLY), [false, true]);
}

#[test]
fn a_lost_controller_takes_the_menu_away_after_two_seconds_and_gives_it_back_when_it_returns() {
    let mut placer = Placer::default();
    // Both controllers count as present from the start: the menu is on the left hand at once
    assert_eq!(placer.update(MenuHand::Auto, &frame(0, BOTH)), MenuPlace::Hand(Hand::Left));
    assert_eq!(placer.update(MenuHand::Auto, &frame(300, BOTH)), MenuPlace::Hand(Hand::Left));
    // The left controller stops tracking. For 2 s nothing moves.
    assert_eq!(placer.update(MenuHand::Auto, &frame(1_000, RIGHT_ONLY)), MenuPlace::Hand(Hand::Left));
    assert_eq!(placer.update(MenuHand::Auto, &frame(2_999, RIGHT_ONLY)), MenuPlace::Hand(Hand::Left));
    // Then the menu stands in front of the user, where the right controller points at it
    assert_eq!(placer.update(MenuHand::Auto, &frame(3_000, RIGHT_ONLY)), MenuPlace::View);
    assert_eq!(placer.present(), RIGHT_ONLY);
    // A flicker of the left controller shorter than the return's wait moves nothing
    assert_eq!(placer.update(MenuHand::Auto, &frame(4_000, BOTH)), MenuPlace::View);
    assert_eq!(placer.update(MenuHand::Auto, &frame(4_200, RIGHT_ONLY)), MenuPlace::View);
    assert_eq!(placer.update(MenuHand::Auto, &frame(4_400, BOTH)), MenuPlace::View);
    // Tracked for the whole wait, it takes the menu back
    assert_eq!(placer.update(MenuHand::Auto, &frame(4_700, BOTH)), MenuPlace::Hand(Hand::Left));
    // A gap of the right controller that is shorter than 2 s moves nothing
    assert_eq!(placer.update(MenuHand::Auto, &frame(5_000, LEFT_ONLY)), MenuPlace::Hand(Hand::Left));
    assert_eq!(placer.update(MenuHand::Auto, &frame(6_900, BOTH)), MenuPlace::Hand(Hand::Left));
    assert_eq!(placer.update(MenuHand::Auto, &frame(7_500, BOTH)), MenuPlace::Hand(Hand::Left));
}

#[test]
fn auto_moves_the_menu_to_the_hand_that_does_not_pull_on_a_panel_and_only_while_the_menu_rests() {
    let mut placer = Placer::default();
    placer.update(MenuHand::Auto, &frame(0, BOTH));
    assert_eq!(placer.update(MenuHand::Auto, &frame(300, BOTH)), MenuPlace::Hand(Hand::Left));
    let pull = |time: u64, hand: usize, menu_busy: bool| {
        let mut frame = frame(time, BOTH);
        frame.trigger[hand] = 1.0;
        frame.on_panel[hand] = true;
        frame.menu_busy = menu_busy;
        frame
    };
    // The left hand pulls on a panel while the menu is open: the menu stays where the user works it
    assert_eq!(placer.update(MenuHand::Auto, &pull(400, 0, true)), MenuPlace::Hand(Hand::Left));
    assert_eq!(placer.update(MenuHand::Auto, &frame(500, BOTH)), MenuPlace::Hand(Hand::Left));
    // A held trigger is no new pull
    assert_eq!(placer.update(MenuHand::Auto, &pull(600, 0, true)), MenuPlace::Hand(Hand::Left));
    assert_eq!(placer.update(MenuHand::Auto, &pull(700, 0, false)), MenuPlace::Hand(Hand::Left));
    // A pull of the left hand on a panel with the menu closed: the left hand points, so the menu
    // goes to the right hand
    placer.update(MenuHand::Auto, &frame(800, BOTH));
    assert_eq!(placer.update(MenuHand::Auto, &pull(900, 0, false)), MenuPlace::Hand(Hand::Right));
    // A pull away from every panel is a game's
    let mut away = pull(1_100, 1, false);
    away.on_panel = [false; 2];
    placer.update(MenuHand::Auto, &frame(1_000, BOTH));
    assert_eq!(placer.update(MenuHand::Auto, &away), MenuPlace::Hand(Hand::Right));
    // A fixed setting ignores the hand that points
    assert_eq!(placer.update(MenuHand::Left, &frame(1_200, BOTH)), MenuPlace::Hand(Hand::Left));
    // The right hand pulls on a panel: Auto gives the left hand the menu again
    placer.update(MenuHand::Auto, &frame(1_300, BOTH));
    assert_eq!(placer.update(MenuHand::Auto, &pull(1_400, 1, false)), MenuPlace::Hand(Hand::Left));
}

#[test]
fn a_start_without_controllers_puts_the_menu_in_front_of_the_user_after_two_seconds() {
    let mut placer = Placer::default();
    assert_eq!(placer.update(MenuHand::Auto, &frame(0, NONE)), MenuPlace::Hand(Hand::Left));
    assert_eq!(placer.update(MenuHand::Auto, &frame(2_000, NONE)), MenuPlace::View);
    // One controller wakes up: the menu stays in front of the user, where it points
    assert_eq!(placer.update(MenuHand::Auto, &frame(3_000, RIGHT_ONLY)), MenuPlace::View);
    assert_eq!(placer.update(MenuHand::Auto, &frame(3_500, RIGHT_ONLY)), MenuPlace::View);
    assert_eq!(placer.present(), RIGHT_ONLY);
}

#[test]
fn every_place_has_its_own_words() {
    let labels = [MenuPlace::Hand(Hand::Left), MenuPlace::Hand(Hand::Right), MenuPlace::View].map(MenuPlace::label);
    assert!(labels[0] != labels[1] && labels[1] != labels[2] && labels[0] != labels[2]);
}
