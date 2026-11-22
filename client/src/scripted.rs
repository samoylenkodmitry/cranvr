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
//! - `scripted_pointer=hand` plays the left controller alone. It holds its ray level and straight
//!   ahead for 4 seconds, turns it 35 degrees to the right and 25 degrees up in 1 second, holds that
//!   for 4 seconds and turns back in 1 second, again and again. With `hand_menu=open` the menu at the
//!   left hand shows that the hand's panels move and turn with the controller.
//! - `scripted_pointer=models` holds both controllers still in front of the head, their rays away
//!   from the panels. The right controller presses its inputs in turn, each for 4 seconds with 5
//!   seconds of rest between: the trigger, A, the grip, the thumbstick forward and the thumbstick to
//!   the right. The log says each change of the right controller's inputs ("scripted models:"), so a
//!   screen capture can be matched with the press that it shows.
//! - `scripted_pointer=console` holds the left controller still. The right controller points at the
//!   console for 4 seconds and away from every panel for 4 seconds, again and again. Once a game runs
//!   the script opens the console, which a game's start closes. The log then shows the right
//!   controller's model as it shows and hides (see `models.rs`).
//! - `scripted_pointer=menu` checks where the hand menu stands for the controllers that a user has
//!   (`menu_place.rs`). `scripted_hands` names them: `right` or `left` plays that controller alone,
//!   and the other one is missing; `both` plays both, and the left one goes missing for 4 s in the
//!   middle. The present controller waits 6 s for the link and its mapping, holds the button that
//!   opens the menu (B on the right, the menu button on the left) for 1.2 s, then aims its ray at
//!   the middle of the menu where the code puts it, then at the ball, and pulls its trigger on the
//!   ball, which closes the menu. The log names each place, and each ray that enters the menu or the
//!   ball with the point that it hit, so a run reads the panel's place in numbers. Launch it with
//!   `default_places`, so the user's carries do not move the panels, and with a link to a host that
//!   has the standard mapping.
//! - `scripted_pointer=none` plays both controllers as missing, for the look that opens the menu
//!   (`gaze.rs`). With `gaze_pitch=0` the ball stands straight ahead, where a headset on a desk looks.
//! - `scripted_pointer=press` checks that a press is never a move, on the hand menu in front of the
//!   user (launch it with `hand_menu=open` and `default_places`). The left controller is missing. The
//!   right one stands 60 cm from the menu. It points beside the menu, then at the Show button of the
//!   stream's numbers, and pulls with a ray that strays 3 mm on the panel: the press clicks and moves
//!   nothing. It then points at Recenter on the last row, by the handle below the menu, and its pull
//!   tips the ray onto the handle: the press clicks Recenter and moves nothing. Last it points at
//!   the handle, pulls and moves 10 cm to the right: the menu moves. The log says each frame in
//!   which the ray entered a button of the menu ("scripted press:"). `press_wait_ms` sets how long
//!   the ray points beside the menu first (3000 by default).
//! - `scripted_pointer=pages` walks the pages of the Mac's settings (launch it with `default_places`).
//!   The left controller is missing, so the hand menu stands in front of the user. The script waits
//!   for the host's menu and reads its pages. For each page that the hand menu lists, the right
//!   controller holds B, which opens the hand menu, and presses the page's button there: the console
//!   opens on that page. It presses the first option of the page's first choice that restarts nothing
//!   and does not hold now, or the other state of its first such toggle. It presses no action. After
//!   the last page it goes Back to the main page, opens the first game's Options and presses a change
//!   there in the same way. Each press waits 1 s with the ray at rest, as the pointer smooths the
//!   ray. The log names each aim and each choice ("scripted pages:"), and the host's app saves each
//!   change in its `app.json`.
//! - `scripted_pointer=palm` plays two hands and no controller: the left hand holds its open palm
//!   35 cm ahead of the eyes and 15 cm below them, turned to the eyes, and never pinches. The right
//!   hand points at the ball that shows in front of that palm and pinches every 2 s, which opens and
//!   closes the menu in turn.

use crate::{hand_menu, menu_model, place, ui};
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
        if args.string("scripted_pointer") == Some("hand") {
            turning_left_hand();
            return;
        }
        if args.string("scripted_pointer") == Some("models") {
            pressing_right_hand();
            return;
        }
        if args.string("scripted_pointer") == Some("console") {
            pointing_at_console();
            return;
        }
        if args.string("scripted_pointer") == Some("none") {
            log::info!("scripted: both controllers are missing");
            script_controller(missing(Hand::Left));
            script_controller(missing(Hand::Right));
            return;
        }
        if args.string("scripted_pointer") == Some("palm") {
            palm_check();
            return;
        }
        if args.string("scripted_pointer") == Some("press") {
            press_check();
            return;
        }
        if args.string("scripted_pointer") == Some("pages") {
            pages_check();
            return;
        }
        if args.string("scripted_pointer") == Some("menu") {
            menu_check(args.string("scripted_hands").unwrap_or("both"));
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

/// Where the left controller of `scripted_pointer=hand` stands, in meters from the head's start, and
/// where its ray points in the turn's two halves: level and straight ahead, then 35 degrees to the
/// right and 25 degrees up.
const LEFT_AT: [f32; 3] = [-0.15, -0.3, -0.45];
const LEVEL_TO: [f32; 3] = [-0.15, -0.3, -1.45];
const TURNED_AT: [f32; 3] = [-0.05, -0.25, -0.45];
const TURNED_TO: [f32; 3] = [-0.05 + 0.520, -0.25 + 0.423, -0.45 - 0.742];

/// Plays the left controller of `scripted_pointer=hand`, in a turn of 10 seconds that repeats.
fn turning_left_hand() {
    let rest = ScriptedButtons::default();
    let left = ScriptedController::new(Hand::Left, LEFT_AT, LEVEL_TO)
        .holding(Duration::from_secs(4), rest)
        .moving_to(Duration::from_secs(1), (TURNED_AT, TURNED_TO), rest)
        .holding(Duration::from_secs(4), rest)
        .moving_to(Duration::from_secs(1), (LEFT_AT, LEVEL_TO), rest)
        .repeating();
    log::info!("a scripted left controller turns from {LEFT_AT:?} toward {LEVEL_TO:?} to {TURNED_AT:?} toward {TURNED_TO:?} and back every 10 s");
    script_controller(left);
}

/// Where the left controller of `scripted_pointer=models` and `console` stands, in meters from the
/// head's start, with its ray down and ahead, away from the panels. A ray straight down has no
/// heading, which gives the scripted controller no pose.
const LEFT_STILL: [f32; 3] = [-0.2, -0.3, -0.35];
const LEFT_DOWN: [f32; 3] = [-0.4, -1.3, -0.85];
/// Where the right controller of `scripted_pointer=models` stands: at the height of the eyes, a
/// quarter meter ahead, with its ray level to the right, away from the panels. The head then sees the
/// controller's top face edge on, so the trigger, the face buttons and the thumbstick move in its
/// outline.
const RIGHT_AT: [f32; 3] = [0.0, 0.0, -0.25];
const RIGHT_AWAY: [f32; 3] = [1.0, 0.0, -0.25];

/// Plays `scripted_pointer=models`: both controllers stand still, and the right one presses its inputs
/// in turn, in a cycle of 45 seconds that repeats.
fn pressing_right_hand() {
    let rest = ScriptedButtons::default();
    let left = ScriptedController::new(Hand::Left, LEFT_STILL, LEFT_DOWN).holding(Duration::from_secs(45), rest).repeating();
    let presses = [
        ScriptedButtons { trigger: 1.0, ..rest },
        ScriptedButtons { primary: true, ..rest },
        ScriptedButtons { squeeze: 1.0, ..rest },
        ScriptedButtons { thumbstick: [0.0, 1.0], ..rest },
        ScriptedButtons { thumbstick: [1.0, 0.0], ..rest },
    ];
    let right = presses
        .iter()
        .fold(ScriptedController::new(Hand::Right, RIGHT_AT, RIGHT_AWAY), |script, press| script.holding(Duration::from_secs(5), rest).holding(Duration::from_secs(4), *press))
        .repeating();
    log::info!("scripted models: the right controller at {RIGHT_AT:?} presses the trigger, A, the grip and the thumbstick in turn, 4 s each with 5 s of rest");
    script_controller(left);
    script_controller(right);
}

/// Plays `scripted_pointer=console`: the right controller points at the console and away from it, in
/// a cycle of 8 seconds that repeats. Once a game runs, the script opens the console.
fn pointing_at_console() {
    let rest = ScriptedButtons::default();
    let left = ScriptedController::new(Hand::Left, LEFT_STILL, LEFT_DOWN).holding(Duration::from_secs(8), rest).repeating();
    let at = [0.15, -0.3, -0.3];
    // The console's subspace has its origin a meter ahead of the head when it opens, and the head on a
    // desk stays where it started
    let console = Pose::at([0.0, 0.0, -1.0]).then(&crate::console_panel::start_place()).position;
    let right = ScriptedController::new(Hand::Right, at, console)
        .holding(Duration::from_millis(4000), rest)
        .moving_to(Duration::from_millis(200), (at, RIGHT_AWAY), rest)
        .holding(Duration::from_millis(3800), rest)
        .moving_to(Duration::from_millis(200), (at, console), rest)
        .repeating();
    log::info!("scripted console: the right controller points at the console's middle {console:?} and away from it, 4 s each");
    script_controller(left);
    script_controller(right);
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_millis(250));
        if crate::hub::hub().backdrop().game_runs {
            if !crate::hub::hub().console().open {
                log::info!("scripted console: a game runs, the script opens the console");
                crate::hub::hub().open_console();
            }
            return;
        }
    });
}

/// The headset frames of a scripted run so far: the frame loop counts them, and a log names the frame
/// of an event with it.
pub static FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Logs each change of the right controller's inputs in a run of `scripted_pointer=models`, so that a
/// screen capture can be matched with the press that it shows. Counts the frame.
pub fn note_input(input: &cranvr_proto::message::Input) {
    FRAME.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    use cranvr_proto::message::buttons;
    use std::sync::{Mutex, OnceLock};
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    static PRESSING: OnceLock<bool> = OnceLock::new();
    if !*PRESSING.get_or_init(|| launch_args().string("scripted_pointer") == Some("models")) {
        return;
    }
    let hand = &input.hands[1];
    let state = format!(
        "trigger {:.1}, A {}, grip {:.1}, thumbstick ({:.1}, {:.1})",
        hand.trigger,
        hand.buttons & buttons::PRIMARY != 0,
        hand.grip,
        hand.stick[0],
        hand.stick[1]
    );
    let mut last = LAST.lock().unwrap();
    if last.as_deref() != Some(state.as_str()) {
        log::info!("scripted models: right {state}");
        *last = Some(state);
    }
}

/// Logs the grip and aim poses of the controllers every 2 seconds, with the launch argument
/// `log_poses`, so a run with the real controllers lying on a desk shows where the runtime puts
/// them ("scripted poses:").
pub fn note_poses(frame: &crate::tracking::HeadsetFrame) {
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    };
    static LOGGING: OnceLock<bool> = OnceLock::new();
    static LAST: AtomicU64 = AtomicU64::new(0);
    if !*LOGGING.get_or_init(|| launch_args().boolean("log_poses").unwrap_or(false)) {
        return;
    }
    let now = frame.sample_time;
    if now.saturating_sub(LAST.load(Ordering::Relaxed)) < 2_000_000_000 {
        return;
    }
    LAST.store(now, Ordering::Relaxed);
    let show = |motion: &Option<cranvr_proto::wire::Motion>| match motion {
        Some(motion) => format!("{:?} {:?}", motion.pose.position, motion.pose.orientation),
        None => "none".to_string(),
    };
    log::info!(
        "scripted poses: head {}; left grip {}; left aim {}; right grip {}; right aim {}",
        show(&Some(frame.head)),
        show(&frame.left_grip),
        show(&frame.left_aim),
        show(&frame.right_grip),
        show(&frame.right_aim)
    );
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

/// The time that a menu check waits for the link and the host's mapping before its first press
const MENU_WAIT: Duration = Duration::from_secs(6);
/// Where the menu check's right controller stands, in meters from the head's start, and where its
/// left controller stands when it is alone: 25 cm ahead of the head, nearer than the ball, so its ray
/// meets the ball's face and not its edge
const MENU_RIGHT_AT: [f32; 3] = [0.15, -0.3, -0.3];
const MENU_LEFT_AT: [f32; 3] = [-0.1, -0.35, -0.25];

/// The height of the menu that a host with no display gives: the console, the connection, the
/// stream's numbers and the row of actions.
fn empty_menu_height() -> f32 {
    use crate::{displays::DisplaysView, menu_place::MenuPlace, panel_layout::Layout};
    let layout = Layout::default();
    let displays = DisplaysView::default();
    let input = menu_model::MenuInput { layout: &layout, console_open: false, displays: &displays, wishes: Default::default(), place: MenuPlace::View, adding: false, pages: &[] };
    menu_model::height(&menu_model::rows(&input))
}

/// A place straight down and ahead of `at`, where a ray hits no panel
fn away_from(at: [f32; 3]) -> [f32; 3] {
    [at[0], at[1] - 1.0, at[2] - 0.3]
}

/// The script of a controller that is missing for the whole run.
fn missing(hand: Hand) -> ScriptedController {
    ScriptedController::new(hand, [0.0; 3], [0.0, 0.0, -1.0]).untracked(Duration::from_millis(1))
}

/// Plays `scripted_pointer=menu` (see the file's notes).
fn menu_check(hands: &str) {
    let rest = ScriptedButtons::default();
    let pull = ScriptedButtons { trigger: 1.0, ..rest };
    let height = empty_menu_height();
    // The soft subspace in front of the user has its origin a meter ahead of the head, and the head on
    // a desk stays where it started
    let view = Pose::at([0.0, 0.0, -1.0]);
    let view_menu = view.then(&hand_menu::menu_start(hand_menu::IN_VIEW, height)).position;
    let base = hand_menu::IN_VIEW.base;
    let view_ball = view.then(&Pose::at([base[0], base[1] + hand_menu::LIFT, base[2]].map(|dp| dp / 1000.0))).position;
    // Cranpose's pointer smooths the ray, so a press comes 1 s after each aim, on the point where
    // the ray rests. The middle of the empty menu is the name of a row, which a press leaves alone.
    let alone = |hand: Hand, at: [f32; 3], open: ScriptedButtons| {
        ScriptedController::new(hand, at, away_from(at))
            .holding(MENU_WAIT, rest)
            .holding(Duration::from_millis(1200), open)
            .holding(Duration::from_millis(800), rest)
            .moving_to(Duration::from_millis(1), (at, view_menu), rest)
            .holding(Duration::from_millis(1000), rest)
            .holding(Duration::from_millis(200), pull)
            .holding(Duration::from_millis(300), rest)
            .moving_to(Duration::from_millis(1), (at, view_ball), rest)
            .holding(Duration::from_millis(1000), rest)
            .holding(Duration::from_millis(200), pull)
            .holding(Duration::from_millis(1000), rest)
            .moving_to(Duration::from_millis(1), (at, away_from(at)), rest)
    };
    log::info!("scripted menu check with {hands}: a menu {height:.0} dp tall, in front of the user at {view_menu:?} with its ball at {view_ball:?}");
    match hands {
        "right" => {
            script_controller(missing(Hand::Left));
            script_controller(alone(Hand::Right, MENU_RIGHT_AT, ScriptedButtons { secondary: true, ..rest }));
        }
        "left" => {
            script_controller(missing(Hand::Right));
            script_controller(alone(Hand::Left, MENU_LEFT_AT, ScriptedButtons { menu: true, ..rest }));
        }
        _ => {
            // The left controller's ray is level and straight ahead, so its pose has no turn
            let left = Pose::at(LEFT_AT);
            let hand_menu = left.then(&hand_menu::menu_start(hand_menu::ON_LEFT, height)).position;
            let hand_ball = left.then(&Pose::at([0.0, hand_menu::LIFT / 1000.0, 0.0])).position;
            log::info!("scripted menu check: the menu on the left hand at {hand_menu:?} with its ball at {hand_ball:?}; the left controller goes missing from 10 s to 14 s");
            script_controller(ScriptedController::new(Hand::Left, LEFT_AT, LEVEL_TO).holding(Duration::from_secs(10), rest).untracked(Duration::from_secs(4)).holding(Duration::from_secs(60), rest));
            let at = MENU_RIGHT_AT;
            let right = ScriptedController::new(Hand::Right, at, away_from(at))
                .holding(MENU_WAIT, rest)
                .holding(Duration::from_millis(1200), ScriptedButtons { secondary: true, ..rest })
                .holding(Duration::from_millis(800), rest)
                // 8 s: the menu on the left hand
                .moving_to(Duration::from_millis(1), (at, hand_menu), rest)
                .holding(Duration::from_millis(1000), rest)
                .holding(Duration::from_millis(200), pull)
                .holding(Duration::from_millis(300), rest)
                .moving_to(Duration::from_millis(1), (at, away_from(at)), rest)
                // The left controller goes missing at 10 s, and the menu stands in front of the user from 12 s
                .holding(Duration::from_millis(3000), rest)
                .moving_to(Duration::from_millis(1), (at, view_menu), rest)
                .holding(Duration::from_millis(800), rest)
                .holding(Duration::from_millis(200), pull)
                .moving_to(Duration::from_millis(1), (at, away_from(at)), rest)
                // The left controller is back at 14 s, and the menu on its hand from 14.3 s
                .holding(Duration::from_millis(1500), rest)
                .moving_to(Duration::from_millis(1), (at, hand_ball), rest)
                .holding(Duration::from_millis(1000), rest)
                .holding(Duration::from_millis(200), pull)
                .holding(Duration::from_millis(1000), rest)
                .moving_to(Duration::from_millis(1), (at, away_from(at)), rest);
            script_controller(right);
        }
    }
}

/// The place in the room of the point `x`, `y` dp from the top left corner of the console, whose
/// rows `rows` are. The console stands at its default place in a subspace a meter ahead of the head.
fn on_console(rows: &[crate::page_model::Row], x: f32, y: f32) -> [f32; 3] {
    use crate::console::size::WIDTH;
    let height = crate::page_model::height(rows);
    let console = Pose::at([0.0, 0.0, -1.0]).then(&crate::console_panel::start_place());
    console.then(&Pose::at([(x - WIDTH / 2.0) / 1000.0, (height / 2.0 - y) / 1000.0, 0.0])).position
}

/// The first change of a page that restarts nothing: its first option, or toggle state, that does not
/// hold now.
fn first_change(page: &cranvr_common::menu::Page) -> Option<cranvr_common::menu::Request> {
    use cranvr_common::menu::{Item, Request, OFF, ON};
    page.items.iter().find_map(|item| match item {
        Item::Choice(choice) if !choice.restarts => choice
            .options
            .iter()
            .find(|option| option.value != choice.value)
            .map(|option| Request::Set { id: choice.id.clone(), subject: choice.subject.clone(), value: option.value.clone() }),
        Item::Toggle(toggle) if !toggle.restarts => Some(Request::Set { id: toggle.id.clone(), subject: toggle.subject.clone(), value: if toggle.on { OFF } else { ON }.into() }),
        _ => None,
    })
}

/// Plays `scripted_pointer=pages` (see the file's notes). The steps need the host's pages, so a thread
/// waits for the menu and installs the right controller's script then.
fn pages_check() {
    script_controller(missing(Hand::Left));
    std::thread::spawn(|| {
        let since = std::time::Instant::now();
        let menu = loop {
            if let Some(menu) = crate::hub::hub().console().menu {
                break menu;
            }
            if since.elapsed() > Duration::from_secs(30) {
                log::warn!("scripted pages: no menu came from the host in 30 s");
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        // The list of displays comes with the link too, and the hand menu lists them above the pages
        std::thread::sleep(Duration::from_millis(1500));
        match pages_script(&menu) {
            Some(right) => script_controller(right),
            None => log::warn!("scripted pages: the menu has no page with a change to press"),
        }
    });
}

fn pages_script(menu: &crate::console::Menu) -> Option<ScriptedController> {
    use crate::{
        console::{Press, Shown},
        displays::displays,
        hub::hub,
        menu_place::MenuPlace,
        page_model::{console_rows, find_key, key_point, Local},
        panel_layout::Layout,
    };
    use menu_model::size::{DETAIL, GAP, PADDING, ROW, TITLE, WIDTH};
    let rest = ScriptedButtons::default();
    let pull = ScriptedButtons { trigger: 1.0, ..rest };
    let at = MENU_RIGHT_AT;
    let view = hub().console();
    let local = Local { room: view.room, volume: view.volume, link: true };
    let now = std::time::Instant::now();
    // The hand menu in front of the user: its rows, as `hand_menu.rs` lays them out
    let pages = menu.listed();
    let layout = Layout::default();
    let displays_view = displays().menu_view();
    let input = menu_model::MenuInput { layout: &layout, console_open: false, displays: &displays_view, wishes: Default::default(), place: MenuPlace::View, adding: false, pages: &pages };
    let rows = menu_model::rows(&input);
    let height = menu_model::height(&rows);
    // The place of the button of the page `index` in the hand menu
    let page_button = |index: usize| {
        let mut top = PADDING + TITLE + GAP;
        for row in &rows {
            if let Some(at) = row.buttons.iter().position(|button| button.action == menu_model::MenuAction::OpenPage(index as u8)) {
                let n = row.buttons.len() as f32;
                let each = (WIDTH - 2.0 * PADDING - (n - 1.0) * GAP) / n;
                return Some(on_view_menu(height, PADDING + at as f32 * (each + GAP) + each / 2.0, top + ROW / 2.0));
            }
            top += ROW + GAP + row.details.len() as f32 * DETAIL;
        }
        None
    };
    // A press: the ray rests on the point for 1 s, then the trigger goes down and up
    let press = |script: ScriptedController, target: [f32; 3]| {
        script.moving_to(Duration::from_millis(1), (at, target), rest).holding(Duration::from_millis(1000), rest).holding(Duration::from_millis(200), pull).holding(Duration::from_millis(600), rest)
    };
    let key_at = |shown: &Shown, wanted: &Press| {
        let rows = console_rows(Some(menu), shown, local, "", None, now);
        let (row, key) = find_key(&rows, wanted)?;
        let [x, y] = key_point(&rows, row, key)?;
        Some(on_console(&rows, x, y))
    };
    let mut right = ScriptedController::new(Hand::Right, at, away_from(at)).holding(Duration::from_millis(500), rest);
    log::info!("scripted pages: the hand menu {height:.0} dp tall with {} pages", pages.len());
    // Each page: a hold of B opens the hand menu, the page's button opens the console there, and the
    // first change of the page that restarts nothing is pressed
    let mut last = None;
    for (index, (id, short)) in pages.iter().enumerate() {
        let target = page_button(index)?;
        right = right.holding(Duration::from_millis(1200), ScriptedButtons { secondary: true, ..rest }).holding(Duration::from_millis(800), rest);
        log::info!("scripted pages: the page {short} ({id}) at {target:?}");
        right = press(right, target);
        let page = Shown::Host(id.clone());
        if let Some(change) = menu.page(id).and_then(first_change) {
            let target = key_at(&page, &Press::Host(change.clone()))?;
            log::info!("scripted pages: on {id} the change {} at {target:?}", change.to_json());
            right = press(right, target);
        }
        last = Some(page);
    }
    let page = last?;
    // Back to the main page, and the first game's options there: the first link to a page that the
    // menus do not list
    right = press(right, key_at(&page, &Press::Back)?);
    let options = menu.main().and_then(|main| {
        main.items.iter().find_map(|item| match item {
            cranvr_common::menu::Item::Buttons(row) => row.buttons.iter().find_map(|button| match &button.press {
                cranvr_common::menu::Press::Open { page } if !pages.iter().any(|(id, _)| id == page) => Some(page.clone()),
                _ => None,
            }),
            _ => None,
        })
    });
    if let Some(game) = options {
        let target = key_at(&Shown::Main, &Press::Open(game.clone()))?;
        log::info!("scripted pages: the options {game} at {target:?}");
        right = press(right, target);
        if let Some(change) = menu.page(&game).and_then(first_change) {
            let target = key_at(&Shown::Host(game.clone()), &Press::Host(change.clone()))?;
            log::info!("scripted pages: on {game} the change {} at {target:?}", change.to_json());
            right = press(right, target);
        }
    }
    Some(right.moving_to(Duration::from_millis(1), (at, away_from(at)), rest))
}

/// Where the right controller of the press check stands, in meters from the head's start: below the
/// eyes and back by the head, about 60 cm from the menu
const PRESS_AT: [f32; 3] = [0.1, -0.35, 0.05];

/// The place in the room of the point `x`, `y` dp from the top left corner of the menu in front of
/// the user, whose height is `height` dp.
fn on_view_menu(height: f32, x: f32, y: f32) -> [f32; 3] {
    let view = Pose::at([0.0, 0.0, -1.0]);
    let menu = view.then(&hand_menu::menu_start(hand_menu::IN_VIEW, height));
    menu.then(&Pose::at([(x - menu_model::size::WIDTH / 2.0) / 1000.0, (height / 2.0 - y) / 1000.0, 0.0])).position
}

/// Plays `scripted_pointer=press` (see the file's notes).
fn press_check() {
    use menu_model::size::{BUTTON, GAP, PADDING, ROW, TITLE, WIDTH};
    let rest = ScriptedButtons::default();
    let pull = ScriptedButtons { trigger: 1.0, ..rest };
    let height = empty_menu_height();
    let row_y = |row: f32| PADDING + TITLE + GAP + row * (ROW + GAP) + ROW / 2.0;
    // The Show button of the stream's numbers, at the right end of the third row
    let show = on_view_menu(height, WIDTH - PADDING - BUTTON / 2.0, row_y(2.0));
    let menu = Pose::at([0.0, 0.0, -1.0]).then(&hand_menu::menu_start(hand_menu::IN_VIEW, height));
    let handle = ScriptedController::handle_of(&menu, [WIDTH, height]);
    let at = PRESS_AT;
    // The Show button's point 3 mm to its right: a dp is a millimeter
    let strayed = on_view_menu(height, WIDTH - PADDING - BUTTON / 2.0 + 3.0, row_y(2.0));
    // Recenter, the first of the two buttons of the last row, 8 dp inside its corner nearest the
    // handle, which stands below the middle of the menu
    let near_handle = on_view_menu(height, (WIDTH - GAP) / 2.0 - 8.0, row_y(3.0) + ROW / 2.0 - 8.0);
    let carried = [at[0] + 0.1, at[1], at[2]];
    // A run can wait longer before the first press (`press_wait_ms`), so that a system toast that
    // covers the screen capture has gone
    let wait = launch_args().int("press_wait_ms").and_then(|ms| u64::try_from(ms).ok()).unwrap_or(3000);
    let right = ScriptedController::new(Hand::Right, at, away_from(at))
        .holding(Duration::from_millis(wait), rest)
        // 3 s: the ray enters the Show button and rests there for a screen capture
        .moving_to(Duration::from_millis(1), (at, show), rest)
        .holding(Duration::from_millis(2000), rest)
        // 5 s: the pull, with the ray's point straying 3 mm and back each 40 ms
        .holding(Duration::from_millis(40), pull)
        .moving_to(Duration::from_millis(40), (at, strayed), pull)
        .moving_to(Duration::from_millis(40), (at, show), pull)
        .moving_to(Duration::from_millis(40), (at, strayed), pull)
        .moving_to(Duration::from_millis(40), (at, show), pull)
        .holding(Duration::from_millis(1000), rest)
        // 6.2 s: Recenter by the handle, then a pull whose start tips the ray onto the handle in
        // 80 ms. The trigger at 0.4 has left its rest and does not press yet, and the smoothed ray
        // reaches the handle in the next 300 ms. The press comes 380 ms after the last frame of rest.
        .moving_to(Duration::from_millis(1), (at, near_handle), rest)
        .holding(Duration::from_millis(1000), rest)
        .moving_to(Duration::from_millis(80), (at, handle), ScriptedButtons { trigger: 0.4, ..rest })
        .holding(Duration::from_millis(300), ScriptedButtons { trigger: 0.4, ..rest })
        .holding(Duration::from_millis(300), pull)
        .holding(Duration::from_millis(1000), rest)
        // 8.9 s: the handle, a pull and a carry 10 cm to the right
        .moving_to(Duration::from_millis(1), (at, handle), rest)
        .holding(Duration::from_millis(1000), rest)
        .holding(Duration::from_millis(100), pull)
        .moving_to(Duration::from_millis(500), (carried, [handle[0] + 0.1, handle[1], handle[2]]), pull)
        .holding(Duration::from_millis(200), pull)
        .holding(Duration::from_millis(500), rest)
        .moving_to(Duration::from_millis(1), (carried, away_from(carried)), rest);
    log::info!("scripted press: a menu {height:.0} dp tall; Show at {show:?}, Recenter by the handle at {near_handle:?}, the handle at {handle:?}");
    script_controller(missing(Hand::Left));
    script_controller(right);
}

/// Logs the frame in which a pointer's point on the hand menu enters one of its buttons, in a run of
/// `scripted_pointer=press`. The menu has no display and no link, so its buttons stand where
/// `press_check` aims.
pub fn note_menu_point(kind: cranpose::PointerEventKind, at: cranpose::Point) {
    use menu_model::size::{BUTTON, GAP, PADDING, ROW, TITLE, WIDTH};
    use std::sync::Mutex;
    static INSIDE: Mutex<Option<&'static str>> = Mutex::new(None);
    if launch_args().string("scripted_pointer") != Some("press") {
        return;
    }
    let row_top = |row: f32| PADDING + TITLE + GAP + row * (ROW + GAP);
    let half = (WIDTH - 2.0 * PADDING - GAP) / 2.0;
    let buttons: [(&'static str, [f32; 4]); 2] = [
        ("Show of the stream's numbers", [WIDTH - PADDING - BUTTON, row_top(2.0), BUTTON, ROW]),
        ("Recenter", [PADDING, row_top(3.0), half, ROW]),
    ];
    let over = match kind {
        cranpose::PointerEventKind::Exit => None,
        _ => buttons
            .iter()
            .find(|(_, [x, y, w, h])| (*x..x + w).contains(&at.x) && (*y..y + h).contains(&at.y))
            .map(|(name, _)| *name),
    };
    let mut inside = INSIDE.lock().unwrap();
    if *inside != over {
        if let Some(name) = over {
            log::info!("scripted press: frame {}: the ray enters {name} at {:.0}, {:.0} dp", FRAME.load(std::sync::atomic::Ordering::Relaxed), at.x, at.y);
        }
        *inside = over;
    }
}

/// Where the left palm of `scripted_pointer=palm` stands: 35 cm ahead of the head's start and 15 cm
/// below it, turned so that its palm faces back toward the head (a turn of -90 degrees about x takes
/// the joint's +y, the back of the hand, away from the head)
const PALM: Pose = Pose { position: [-0.1, -0.15, -0.35], orientation: [-std::f32::consts::FRAC_1_SQRT_2, 0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2] };

/// Plays `scripted_pointer=palm` (see the file's notes).
fn palm_check() {
    script_controller(missing(Hand::Left));
    script_controller(missing(Hand::Right));
    // The ball stands 7 cm in front of the palm, along the palm's side of the joint
    let ball = PALM.then(&Pose::at([0.0, -0.07, 0.0])).position;
    let still = PinchCycle { rest: Duration::from_secs(600), closing: Duration::from_millis(1), held: Duration::from_millis(1), opening: Duration::from_millis(1) };
    let pinch = PinchCycle { rest: Duration::from_millis(1600), closing: Duration::from_millis(150), held: Duration::from_millis(250), opening: Duration::from_millis(150) };
    log::info!("scripted palm check: the left palm at {:?}, the right hand points at the ball at {ball:?} and pinches every 2.15 s", PALM.position);
    script_hand(ScriptedHand::pointing(Hand::Left, [-0.15, -0.3, -0.3], [-0.15, -1.3, -0.6], still).with_palm(PALM));
    script_hand(ScriptedHand::pointing(Hand::Right, [0.15, -0.25, -0.2], ball, pinch));
}
