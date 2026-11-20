use super::*;

const MS: u64 = 1_000_000;

fn input(left: HandInput, right: HandInput) -> Input {
    Input { sample_time: 0, hands: [left, right] }
}

fn pressed(mask: u32) -> HandInput {
    HandInput { buttons: mask, ..HandInput::default() }
}

#[test]
fn each_input_that_can_click_has_a_pointer_button_of_its_own() {
    // The desktop panel finds the input from the button, so no two inputs may share one
    let inputs = [Control::Trigger, Control::Grip, Control::Primary, Control::Secondary, Control::StickClick];
    let buttons: Vec<_> = inputs.iter().map(|control| button_of(*control).unwrap()).collect();
    for (i, a) in buttons.iter().enumerate() {
        for b in &buttons[i + 1..] {
            assert_ne!(a, b);
        }
    }
    for control in inputs {
        assert_eq!(control_of(button_of(control).unwrap()), control);
    }
    // The trigger clicks the headset's own panels, so it keeps the primary button
    assert_eq!(button_of(Control::Trigger), Some(PointerButton::Primary));
    assert!([Control::Menu, Control::StickVertical, Control::StickHorizontal].iter().all(|control| button_of(*control).is_none()));
}

#[test]
fn cranpose_gets_one_button_for_each_input_and_scrolls_while_an_axis_scrolls() {
    let mut controls = Controls::default();
    let mapping = pointer_mapping(&controls);
    for (input, control) in [
        (ControllerInput::Trigger, Control::Trigger),
        (ControllerInput::Squeeze, Control::Grip),
        (ControllerInput::Primary, Control::Primary),
        (ControllerInput::Secondary, Control::Secondary),
        (ControllerInput::ThumbstickClick, Control::StickClick),
    ] {
        assert_eq!(mapping.button(input), button_of(control), "{input:?}");
    }
    assert_eq!(mapping.button(ControllerInput::Menu), None);
    assert_eq!(mapping.scroll_speed(), 800.0);
    // One axis of one hand that scrolls keeps Cranpose's own scrolling on
    for hand in Hand::BOTH {
        for axis in [Control::StickVertical, Control::StickHorizontal] {
            controls.set(hand, axis, Action::Nothing);
        }
    }
    assert_eq!(pointer_mapping(&controls).scroll_speed(), 0.0);
    controls.set(Hand::Left, Control::StickHorizontal, Action::Scroll);
    assert_eq!(pointer_mapping(&controls).scroll_speed(), 800.0);
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
fn the_beams_show_only_while_one_of_the_apps_own_panels_shows() {
    // No link: the connect panel shows, so the beams do
    assert!(beams_wanted(true, true, false, false, false));
    assert!(beams_wanted(true, false, false, false, false), "the connect panel shows while no link streams");
    // A stream with the panels shown, and a stream with them hidden and nothing else up
    assert!(beams_wanted(true, true, true, false, false));
    assert!(!beams_wanted(true, false, true, false, false), "a game with every panel hidden has no beam");
    // The console, the desktop panel or both bring the beams back
    assert!(beams_wanted(true, false, true, true, false));
    assert!(beams_wanted(true, false, true, false, true));
    // The hands belong to a game: no panel stands, and only the console or the desktop can show
    assert!(!beams_wanted(false, true, false, false, false));
    assert!(beams_wanted(false, true, false, true, false));
}
