use super::*;

const MS: u64 = 1_000_000;

fn input(left: HandInput, right: HandInput) -> Input {
    Input { sample_time: 0, hands: [left, right] }
}

fn pressed(mask: u32) -> HandInput {
    HandInput { buttons: mask, ..HandInput::default() }
}

fn trigger(value: f32) -> HandInput {
    HandInput { trigger: value, ..HandInput::default() }
}

#[test]
fn the_standard_mapping_turns_each_button_into_its_mouse_button() {
    let controls = Controls::default();
    let mut runner = Runner::default();
    let at = |hand, runner: &Runner| runner.pointer(hand);
    // The right trigger is the left mouse button, as it always was
    runner.update(&controls, &input(HandInput::default(), trigger(0.9)), 0, false);
    assert_eq!(at(Hand::Right, &runner).buttons, mouse::LEFT);
    assert_eq!(at(Hand::Left, &runner), HandPointer::default());
    // A and the thumbstick's press are the right and the middle button. The grip grabs.
    let mut held = pressed(bit::PRIMARY | bit::STICK_CLICK);
    held.grip = 0.9;
    runner.update(&controls, &input(HandInput::default(), held), 10 * MS, false);
    assert_eq!(at(Hand::Right, &runner), HandPointer { buttons: mouse::RIGHT | mouse::MIDDLE, grab: true, scroll: [0.0; 2] });
    // Both hands at once, each with its own buttons
    runner.update(&controls, &input(pressed(bit::PRIMARY), trigger(0.9)), 20 * MS, false);
    assert_eq!(at(Hand::Left, &runner).buttons, mouse::RIGHT);
    assert_eq!(at(Hand::Right, &runner).buttons, mouse::LEFT);
}

#[test]
fn the_thumbstick_scrolls_in_the_axes_that_the_mapping_gives_it() {
    let mut controls = Controls::default();
    let mut runner = Runner::default();
    let lean = HandInput { stick: [0.4, -0.8], ..HandInput::default() };
    runner.update(&controls, &input(HandInput::default(), lean), 0, false);
    assert_eq!(runner.pointer(Hand::Right).scroll, [0.4, -0.8]);
    // The left stick scrolls too, and its lean does not reach the right hand's pointer
    assert_eq!(runner.pointer(Hand::Left).scroll, [0.0, 0.0]);
    // The user turns the sideways scroll off
    controls.set(Hand::Right, Control::StickHorizontal, Action::Nothing);
    runner.update(&controls, &input(HandInput::default(), lean), 10 * MS, false);
    assert_eq!(runner.pointer(Hand::Right).scroll, [0.0, -0.8]);
    controls.set(Hand::Right, Control::StickVertical, Action::Nothing);
    runner.update(&controls, &input(HandInput::default(), lean), 20 * MS, false);
    assert_eq!(runner.pointer(Hand::Right).scroll, [0.0, 0.0]);
    assert!(!runner.pointer(Hand::Right).active());
}

#[test]
fn a_button_does_what_the_user_mapped_to_it() {
    let mut controls = Controls::default();
    controls.set(Hand::Right, Control::Trigger, Action::RightClick);
    controls.set(Hand::Right, Control::Secondary, Action::MiddleClick);
    controls.set(Hand::Right, Control::Grip, Action::Nothing);
    let mut runner = Runner::default();
    let mut held = pressed(bit::SECONDARY);
    held.trigger = 1.0;
    held.grip = 1.0;
    runner.update(&controls, &input(HandInput::default(), held), 0, false);
    assert_eq!(runner.pointer(Hand::Right), HandPointer { buttons: mouse::RIGHT | mouse::MIDDLE, grab: false, scroll: [0.0; 2] });
}

#[test]
fn the_trigger_keeps_its_press_between_the_two_thresholds() {
    let controls = Controls::default();
    let mut runner = Runner::default();
    let down = |runner: &mut Runner, value: f32, at: u64| {
        runner.update(&controls, &input(HandInput::default(), trigger(value)), at * MS, false);
        runner.pointer(Hand::Right).buttons == mouse::LEFT
    };
    assert!(!down(&mut runner, 0.5, 0), "a trigger that is half pulled does not press");
    assert!(down(&mut runner, 0.7, 1));
    assert!(down(&mut runner, 0.5, 2), "it stays down until it falls below 0.4");
    assert!(!down(&mut runner, 0.3, 3));
    assert!(!down(&mut runner, 0.5, 4), "and needs 0.6 to press again");
}

#[test]
fn without_a_game_a_press_fires_the_headset_action_once() {
    let controls = Controls::default();
    let mut runner = Runner::default();
    // The left Y button shows or hides the panels in the standard mapping
    let none = |runner: &mut Runner, at: u64| runner.update(&controls, &input(HandInput::default(), HandInput::default()), at * MS, false);
    assert!(none(&mut runner, 0).is_empty());
    let fires = runner.update(&controls, &input(pressed(bit::SECONDARY), HandInput::default()), MS, false);
    assert_eq!(fires, vec![Fire { hand: Hand::Left, action: Action::Panels }]);
    // The same press, held on, fires nothing more
    let held = runner.update(&controls, &input(pressed(bit::SECONDARY), HandInput::default()), 500 * MS, false);
    assert!(held.is_empty());
    // A new press fires again
    none(&mut runner, 600);
    let again = runner.update(&controls, &input(pressed(bit::SECONDARY), HandInput::default()), 700 * MS, false);
    assert_eq!(again.len(), 1);
}

#[test]
fn while_a_game_runs_only_a_hold_fires_and_a_short_press_stays_with_the_game() {
    let controls = Controls::default();
    let mut runner = Runner::default();
    let menu = input(pressed(bit::MENU), HandInput::default());
    let released = input(HandInput::default(), HandInput::default());
    // A short press fires nothing
    assert!(runner.update(&controls, &menu, 1_000 * MS, true).is_empty());
    assert!(runner.update(&controls, &menu, 1_000 * MS + HOLD_NS - 1, true).is_empty());
    assert!(runner.update(&controls, &released, 1_000 * MS + HOLD_NS + MS, true).is_empty());
    // A hold fires once, as the menu button's hold always opened the console
    assert!(runner.update(&controls, &menu, 2_000 * MS, true).is_empty());
    assert_eq!(runner.update(&controls, &menu, 2_000 * MS + HOLD_NS, true), vec![Fire { hand: Hand::Left, action: Action::Console }]);
    assert!(runner.update(&controls, &menu, 2_000 * MS + 2 * HOLD_NS, true).is_empty(), "one action for one hold");
    // The next hold fires again
    assert!(runner.update(&controls, &released, 3_000 * MS, true).is_empty());
    assert!(runner.update(&controls, &menu, 4_000 * MS, true).is_empty());
    assert_eq!(runner.update(&controls, &menu, 4_000 * MS + HOLD_NS, true).len(), 1);
}

#[test]
fn the_right_controller_has_no_menu_button_to_map() {
    let mut controls = Controls::default();
    controls.hand_mut(Hand::Right).menu = Action::Console;
    let mut runner = Runner::default();
    // A runtime that reports a menu press on the right controller changes nothing
    let fires = runner.update(&controls, &input(HandInput::default(), pressed(bit::MENU)), 0, false);
    assert!(fires.is_empty());
}

#[test]
fn a_mouse_button_and_a_headset_action_can_share_a_controller() {
    let mut controls = Controls::default();
    controls.set(Hand::Right, Control::Primary, Action::Keyboard);
    let mut runner = Runner::default();
    let both = HandInput { buttons: bit::PRIMARY | bit::TRIGGER_CLICK, trigger: 1.0, ..HandInput::default() };
    let fires = runner.update(&controls, &input(HandInput::default(), both), 0, false);
    assert_eq!(fires, vec![Fire { hand: Hand::Right, action: Action::Keyboard }]);
    assert_eq!(runner.pointer(Hand::Right).buttons, mouse::LEFT);
}
