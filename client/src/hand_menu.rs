//! The hand menu: a ball above the left controller that opens a small menu at the hand.
//!
//! The ball rides on the left controller in a `FollowingSubspace` of its own, so it stays at hand
//! wherever the user stands. The left controller's own ray passes through it, so the right
//! controller's ray presses it. A press opens the menu, which stands above the hand and lists every
//! panel of the app with a button to show or hide it, each display of the Mac with its cost, and
//! the actions that add a display, put the panels in front of the user again and open the page of
//! the controls (see `menu_model.rs` for the rows).
//!
//! A display can ride on the left hand instead of standing in the room. Then the ball becomes a
//! pill with a second round button, which shows or hides that display with one press. This works
//! inside a game, for quick actions on the Mac.
//!
//! The ball, the menu and the hand's display hold their place against the left controller: they
//! move and turn with it in every frame (`TrackedDimensions::All`). Cranpose hands their layers to
//! the compositor in the space of the controller's aim, so the compositor places them with its
//! newest pose of the controller. The menu and the hand's display have a sphere under them that the
//! right controller carries. A carry sets the panel's offset from the controller, which `place.rs`
//! keeps for the next start. After the carry the panel follows the hand at the new offset.
//!
//! What the user sees, in a Quest 3:
//! - the ball is 5 cm wide and 11 cm above the controller's grip, as the earlier ball was;
//! - the menu is 47 cm wide, with text 22 to 26 mm tall, and its bottom edge 3 cm above the ball. It
//!   stands 21 cm to the right of the hand, toward the user's body, and 7 cm nearer to the face than
//!   the ball, so the right controller reaches it without crossing the left arm. These places hold
//!   in the controller's own frame: right of the controller, above its top and back along its ray.
//!   With the ray level and ahead they are right, up and toward the face;
//! - the hand's display is 42 cm wide by default. A 1920 point wide Mac display then shows 13 point
//!   text 3 mm tall, which a Quest 3 resolves at the 45 cm that a hand holds it from the eyes. The user
//!   can resize it with the corner sphere;
//! - a press on a button gives a short click in the right hand.
//!
//! Nothing of the menu composes while it is closed. The strip is the one panel that a game with no
//! panel shown carries.
//!
//! A launch can change where the panels ride, for a headset on a desk that tracks no controller (the
//! controller's subspace shows nothing then): the extra `hand_anchor` with the value `view` puts the
//! strip, the menu and the hand's display in a subspace that follows the head, at the place where a
//! left hand would hold them. Their carried places there have names of their own, so they never move
//! the panels on the controller. The extra `hand_menu` with the value `open` opens the menu at the
//! start.

use crate::{
    displays::displays,
    hover,
    hub::hub,
    menu_model::{self, ButtonKind, MenuAction, MenuInput, MenuRow},
    panel_layout::{layer_rank, Layout, PanelId},
    ui::{text_style, ACCENT_COLOR, KEY_COLOR, MUTED_COLOR, PANEL_COLOR, STOP_COLOR, TEXT_COLOR},
};
use crate::place;
use cranpose::launch_args;
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{local_spatial_capabilities, FollowMode, FollowTarget, FollowingSubspace, Hand, Pose, SpatialPanel, SubspaceModifier, TrackedDimensions, Vibration};
use std::time::Duration;

/// The ball's diameter in dp, a millimeter each
pub const BALL: f32 = 50.0;
/// The ball's center above the controller's grip, in dp
pub const LIFT: f32 = 110.0;
/// The width of the ball's ring, in dp
const RING: f32 = 5.0;
/// The distance between the ball and the display's or menu's bottom edge, in dp
pub const CLEARANCE: f32 = 30.0;
/// The menu's center, to the right of the hand and nearer to the face, in dp
const MENU_X: f32 = 210.0;
const MENU_Z: f32 = 70.0;
/// The hand display's center to the right of the hand, in dp
pub const HAND_DISPLAY_X: f32 = 90.0;
/// The menu closes when no pointer was over a panel for this long: a user who forgot it open in a
/// game must not lose the right hand's buttons
const MENU_IDLE: Duration = Duration::from_secs(20);
/// How often the menu looks at the displays' list while it is open
const MENU_POLL: Duration = Duration::from_millis(250);

/// Where the hand's panels stand in the subspace that holds them, and the names under which
/// `place.rs` keeps the places that the user carried the menu and the hand's display to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandFrame {
    /// The offset in dp from the subspace's origin to where a hand holds the panels
    pub base: [f32; 3],
    pub menu: &'static str,
    pub display: &'static str,
}

/// The frame of the panels on the left controller: its origin is the controller's grip, turned as
/// its aim is turned.
pub const ON_CONTROLLER: HandFrame = HandFrame { base: [0.0; 3], menu: "hand-menu", display: "hand-display" };

/// The frame of the test on a desk: the head's subspace has its origin a meter ahead of the head. A
/// left hand lies 30 cm left of the nose, 35 cm below it and 40 cm ahead of it.
pub const ON_VIEW: HandFrame = HandFrame { base: [-300.0, -350.0, 600.0], menu: "hand-menu-view", display: "hand-display-view" };

/// Where the hand's panels ride: the left controller, which they follow in all six dimensions, or the
/// head for a test on a desk.
fn anchor() -> (FollowTarget, HandFrame) {
    if launch_args().string("hand_anchor") == Some("view") {
        (FollowTarget::View(FollowMode::Snap(TrackedDimensions::TranslationAndYaw)), ON_VIEW)
    } else {
        (FollowTarget::Controller(Hand::Left, FollowMode::Tight(TrackedDimensions::All)), ON_CONTROLLER)
    }
}

/// The menu's pose before the user carries it: its bottom edge above the ball, to the right of the
/// hand and nearer to the face. `height` is the menu's height in dp.
pub fn menu_start(frame: HandFrame, height: f32) -> Pose {
    let base = frame.base;
    let y = base[1] + LIFT + BALL / 2.0 + CLEARANCE + height / 2.0;
    SubspaceModifier::new().offset(base[0] + MENU_X, y, base[2] + MENU_Z).pose()
}

/// Whether the launch opens the menu at the start (the extra `hand_menu` with the value `open`).
pub fn open_on_launch() -> bool {
    launch_args().string("hand_menu") == Some("open")
}

/// Whether a flat window shows the menu: a test asks for it with the extra `hand_menu`. The menu then
/// stands among the panels of the room, so a screenshot of the window shows its rows and words.
pub fn flat_menu() -> bool {
    !local_spatial_capabilities().current().is_spatial_ui_enabled() && launch_args().string("hand_menu").is_some()
}

/// A click in the right hand tells the user that a press took.
fn click() {
    if let Some(haptics) = hub().headset().and_then(|headset| headset.haptics) {
        haptics.vibrate(Hand::Right, Vibration::new(0.3, Duration::from_millis(30)));
    }
}

/// What the hand's panels share with the app.
#[derive(Clone, Copy, PartialEq)]
pub struct HandState {
    pub layout: MutableState<Layout>,
    pub menu_open: MutableState<bool>,
    /// Counts up at each press on Recenter: the room's panels compose again around the head
    pub recenter: MutableState<u32>,
}

/// The ball with the menu and the hand's display. In a headset only.
#[composable]
pub fn HandPanels(state: HandState, console_open: bool) {
    // A flat window shows no ball. A test that asks for the menu gets it beside the panels instead
    // (see `flat_menu`).
    if !local_spatial_capabilities().current().is_spatial_ui_enabled() {
        return;
    }
    let (target, frame) = anchor();
    let layout = state.layout.get();
    let pinned_shown = layout.pinned().filter(|stream| layout.shown(PanelId::Display(*stream)));
    let open = state.menu_open.get();
    // The menu closes itself after a while without a pointer over a panel
    LaunchedEffect(open, async move |_| {
        // The time counts from the last pointer over a panel, or from the opening
        let opened_ms = cranvr_proto::clock::now_ns() / 1_000_000;
        while open {
            delay(Duration::from_secs(1)).await;
            let now_ms = cranvr_proto::clock::now_ns() / 1_000_000;
            let since_opened = Duration::from_millis(now_ms.saturating_sub(opened_ms));
            let idle = hover::idle_for(now_ms).map_or(since_opened, |idle| idle.min(since_opened));
            if idle > MENU_IDLE && state.menu_open.get_non_reactive() {
                log::info!("the hand menu closes: no pointer was over a panel for {} s", MENU_IDLE.as_secs());
                state.menu_open.set(false);
            }
        }
    });
    FollowingSubspace(target, move || {
        Strip(state, frame.base, layout.pinned());
        if open {
            Menu(state, console_open, frame);
        }
        if let Some(stream) = pinned_shown {
            key(stream, move || crate::display_panel::HandDisplay(stream, frame));
        }
    });
}

/// The ball, and beside it the round button of the hand's display when one rides on the hand.
#[composable]
fn Strip(state: HandState, base: [f32; 3], pinned: Option<u8>) {
    let width = if pinned.is_some() { BALL * 2.0 + 12.0 } else { BALL };
    let shown = pinned.is_some_and(|stream| state.layout.get().shown(PanelId::Display(stream)));
    let open = state.menu_open.get();
    // The pill's middle lies half a button to the right of the ball's
    let x = base[0] + if pinned.is_some() { (BALL + 12.0) / 2.0 } else { 0.0 };
    SpatialPanel(SubspaceModifier::new().size(width, BALL).offset(x, base[1] + LIFT, base[2]).layer_priority(layer_rank::HAND_MENU), move || {
        let hovered = hover::hover_modifier(hover::STRIP);
        Row(
            Modifier::empty().fill_max_size().then(hovered),
            RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(12.0)),
            move || {
                Round(if open { ACCENT_COLOR } else { KEY_COLOR }, RoundPress::Menu, state);
                if let Some(stream) = pinned {
                    Round(if shown { ACCENT_COLOR } else { KEY_COLOR }, RoundPress::HandDisplay(stream), state);
                }
            },
        );
    });
}

/// What a press on a round button of the strip does.
#[derive(Clone, Copy, PartialEq)]
enum RoundPress {
    /// Opens or closes the menu
    Menu,
    /// Shows or hides the display on the hand
    HandDisplay(u8),
}

/// A round button with a ring around it, so it shows against a bright game.
#[composable]
fn Round(fill: Color, press: RoundPress, state: HandState) {
    Box(
        Modifier::empty().width(BALL).height(BALL).background(TEXT_COLOR).rounded_corners(BALL / 2.0).padding(RING),
        BoxSpec::default(),
        move || {
            Box(
                Modifier::empty().fill_max_size().background(fill).rounded_corners(BALL / 2.0 - RING).clickable(move |_| match press {
                    RoundPress::Menu => toggle_menu(state),
                    RoundPress::HandDisplay(stream) => toggle_hand_display(state, stream),
                }),
                BoxSpec::default(),
                || {},
            );
        },
    );
}

fn toggle_menu(state: HandState) {
    let open = !state.menu_open.get_non_reactive();
    log::info!("the hand menu {}", if open { "opens" } else { "closes" });
    state.menu_open.set(open);
    click();
}

fn toggle_hand_display(state: HandState, stream: u8) {
    let mut layout = state.layout.get_non_reactive();
    layout.toggle(PanelId::Display(stream));
    log::info!("the display on the hand {}", if layout.shown(PanelId::Display(stream)) { "shows" } else { "hides" });
    commit(state, layout);
    click();
}

/// Takes a new layout. The room's choice is saved. A game's pick is not.
fn commit(state: HandState, layout: Layout) {
    if !layout.in_game() {
        layout.save();
    }
    state.layout.set(layout);
}

/// The menu panel. It stands where the user last carried it in `frame`, else at `menu_start`.
#[composable]
pub fn Menu(state: HandState, console_open: bool, frame: HandFrame) {
    let view = rememberMutableStateOf(|| displays().menu_view());
    LaunchedEffect((), async move |_| loop {
        let now = displays().menu_view();
        if view.get_non_reactive() != now {
            view.set(now);
        }
        delay(MENU_POLL).await;
    });
    let layout = state.layout.get();
    let wishes = crate::displays::wishes_of(&layout);
    let displays_view = view.get();
    let input = MenuInput { layout: &layout, console_open, displays: &displays_view, wishes };
    let rows = menu_model::rows(&input);
    let height = menu_model::height(&rows);
    // The place that the user carried the menu to is read once for each opening. Cranpose keeps the
    // place of a carry while the menu stays open.
    let saved = remember(move || place::saved_pose(frame.menu)).with(|pose: &Option<Pose>| *pose);
    let pose = saved.unwrap_or_else(|| menu_start(frame, height));
    let modifier = SubspaceModifier::new().size(menu_model::size::WIDTH, height).layer_priority(layer_rank::HAND_MENU);
    SpatialPanel(place::movable(frame.menu, modifier, pose, |_| {}), move || {
        let hovered = hover::hover_modifier(hover::MENU);
        let rows = rows.clone();
        Column(
            Modifier::empty().fill_max_size().then(hovered).background(PANEL_COLOR).rounded_corners(24.0).padding(menu_model::size::PADDING),
            ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(menu_model::size::GAP)),
            move || {
                Text("cranvr", Modifier::empty().height(menu_model::size::TITLE), text_style(32.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                for row in rows.clone() {
                    RowView(row, state);
                }
            },
        );
    });
}

#[composable]
fn RowView(row: MenuRow, state: HandState) {
    use menu_model::size::*;
    Column(Modifier::empty().fill_max_width(), ColumnSpec::default(), move || {
        let (title, buttons) = (row.title.clone(), row.buttons.clone());
        Row(
            Modifier::empty().fill_max_width().height(ROW),
            RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(GAP)),
            move || {
                if !title.is_empty() {
                    Text(title.clone(), Modifier::empty().weight(1.0).padding_symmetric(0.0, 10.0), text_style(26.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                }
                for button in buttons.clone() {
                    let action = button.action;
                    let fill = match button.kind {
                        ButtonKind::Normal => KEY_COLOR,
                        ButtonKind::On => ACCENT_COLOR,
                        ButtonKind::Danger => STOP_COLOR,
                    };
                    // Each button of a row without a title shares the row's width
                    let modifier = if title.is_empty() { Modifier::empty().weight(1.0) } else { Modifier::empty().width(BUTTON) };
                    Button(
                        modifier.height(ROW).background(fill).rounded_corners(12.0),
                        ButtonSpec::default(),
                        move || perform(action, state),
                        move || {
                            Text(button.label.clone(), Modifier::empty(), text_style(22.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                        },
                    );
                }
            },
        );
        for detail in row.details.clone() {
            Text(detail, Modifier::empty().height(DETAIL), text_style(20.0, MUTED_COLOR, None));
        }
    });
}

/// Does what a button of the menu says.
fn perform(action: MenuAction, state: HandState) {
    click();
    let mut layout = state.layout.get_non_reactive();
    match action {
        MenuAction::TogglePanel(PanelId::Console) => {
            if hub().console().open {
                hub().close_console();
            } else {
                hub().open_console();
            }
        }
        MenuAction::TogglePanel(id) => {
            layout.toggle(id);
            log::info!("the hand menu {} {id:?}", if layout.shown(id) { "shows" } else { "hides" });
            commit(state, layout);
        }
        MenuAction::ToggleHand(stream) => {
            layout.toggle_pinned(stream);
            // A display that moves to the hand or back shows there at once
            layout.set_shown(PanelId::Display(stream), true);
            log::info!("the hand menu puts display {} {}", stream - cranvr_proto::message::DESKTOP_STREAM + 1, if layout.pinned() == Some(stream) { "on the left hand" } else { "back in the room" });
            commit(state, layout);
        }
        MenuAction::RemoveDisplay(stream) => displays().request_remove(stream),
        MenuAction::AddDisplay => displays().request_add(),
        MenuAction::Recenter => {
            log::info!("the hand menu puts the panels in front of the user again");
            state.recenter.set(state.recenter.get_non_reactive() + 1);
        }
        MenuAction::OpenControls => hub().open_controls(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A left controller 20 cm left of the head, 30 cm below it and 40 cm ahead, with its ray level
    /// and straight ahead, as a hand holds it to look at the menu
    const LEFT: Pose = Pose::at([-0.2, -0.3, -0.4]);

    /// How straight the panel at `pose` faces the head at the room's origin: 1 when its front points
    /// right at the head
    fn facing_the_head(pose: &Pose) -> f32 {
        let normal = pose.rotate([0.0, 0.0, 1.0]);
        let to_head = pose.position.map(|v| -v);
        let length = to_head.iter().map(|v| v * v).sum::<f32>().sqrt();
        normal.iter().zip(to_head).map(|(n, t)| n * t / length).sum()
    }

    #[test]
    fn the_menu_and_the_hands_display_face_the_head_above_the_hand() {
        let height = 400.0;
        let menu = LEFT.then(&menu_start(ON_CONTROLLER, height));
        assert!(facing_the_head(&menu) > 0.8, "{menu:?}");
        // Its bottom edge is above the ball, and its middle is right of the hand
        let bottom = menu.position[1] - height / 2000.0;
        assert!(bottom > LEFT.position[1] + (LIFT + BALL / 2.0) / 1000.0, "{menu:?}");
        assert!(menu.position[0] > LEFT.position[0] + 0.15, "{menu:?}");
        let display = LEFT.then(&crate::display_panel::hand_display_start(ON_CONTROLLER, 280.0));
        assert!(facing_the_head(&display) > 0.8, "{display:?}");
    }

    #[test]
    fn the_places_on_the_controller_and_on_the_head_have_names_of_their_own() {
        let names = [ON_CONTROLLER.menu, ON_CONTROLLER.display, ON_VIEW.menu, ON_VIEW.display];
        for (i, name) in names.iter().enumerate() {
            assert!(!names[..i].contains(name), "{name}");
            assert!(!["desktop", "display2", "display3", "display4", "connect", "stats", "console"].contains(name), "{name}");
        }
    }
}
