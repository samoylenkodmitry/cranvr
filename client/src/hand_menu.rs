//! The hand menu: a ball that opens a small menu at the hand, or in front of the user.
//!
//! The ball, the menu and the display that rides on the hand stand where the user's input reaches
//! them (`menu_place.rs`). With two controllers they ride on the menu hand, the left one unless the
//! setting "Menu hand" or its Auto choice says otherwise, in a `FollowingSubspace` of that
//! controller, so they stay at hand wherever the user stands. A controller's own ray passes through
//! them, so the other controller's ray presses them. With one controller, or none, they stand in
//! front of the user at arm's reach, below the eyes, in a subspace that follows the head softly
//! (`FollowMode::soft`): they hold still for the small moves of the head that come with pointing at
//! them, and they come back into view after a turn. A look down does not move them. A controller that
//! stops tracking for 2 s gives them up, and they come back to it when it returns. A button of either
//! controller opens the menu too, as the mapping says (`Action::Menu`).
//!
//! A press on the ball opens the menu, which lists every panel of the app with a button to show or
//! hide it, each display of the Mac with its cost, and the actions that add a display, put the
//! panels in front of the user again and open the page of the controls (see `menu_model.rs` for the
//! rows).
//!
//! A display can ride with the menu instead of standing in the room. Then the ball becomes a pill
//! with a second round button, which shows or hides that display with one press. This works inside
//! a game, for quick actions on the Mac.
//!
//! On a hand, the ball, the menu and the hand's display hold their place against the controller:
//! they move and turn with it in every frame (`TrackedDimensions::All`). Cranpose hands their layers
//! to the compositor in the space of the controller's aim, so the compositor places them with its
//! newest pose of the controller. The menu and the hand's display have a sphere under them that the
//! other controller carries. A carry sets the panel's offset from the controller, which `place.rs`
//! keeps for the next start under a name of each place, so a carry on the right hand or in front of
//! the user moves nothing on the left hand. After the carry the panel follows the hand at the new
//! offset.
//!
//! What the user sees, in a Quest 3:
//! - the ball is 5 cm wide and 11 cm above the controller's grip, as the earlier ball was;
//! - the menu is 47 cm wide, with text 22 to 26 mm tall, and its bottom edge 3 cm above the ball. On
//!   the left hand it stands 21 cm to the right of the hand, toward the user's body, and 7 cm nearer
//!   to the face than the ball, so the right controller reaches it without crossing the left arm.
//!   On the right hand it stands 21 cm to the left. These places hold in the controller's own frame:
//!   beside the controller, above its top and back along its ray. With the ray level and ahead they
//!   are beside, up and toward the face;
//! - in front of the user the menu stands 55 cm ahead of the eyes, its top edge 6 cm below them,
//!   tilted to face them. The ball stands 26 cm to the left of the nose, 30 cm below the eyes and
//!   45 cm ahead of them, left of the menu and nearer, and the hand's display stands to the right of
//!   the menu, turned toward the eyes;
//! - the hand's display is 42 cm wide by default. A 1920 point wide Mac display then shows 13 point
//!   text 3 mm tall, which a Quest 3 resolves at the 45 cm that a hand holds it from the eyes. The user
//!   can resize it with the corner sphere;
//! - a press on a button gives a short click in the right hand.
//!
//! Nothing of the menu composes while it is closed. The strip is the one panel that a game with no
//! panel shown carries. A line under the ball names the ways to open the menu at the first start and
//! when the tracking falls to one controller (`menu_hint.rs`). It is a panel only while it shows.
//!
//! A launch can fix where the panels ride, for a test on a desk: the extra `hand_anchor` with the
//! value `view` puts the strip, the menu and the hand's display in a subspace that follows the head,
//! at the place where a left hand would hold them. Their carried places there have names of their own, so they never move
//! the panels on the controller. The value `head` puts them at the same place in a subspace that
//! follows every move of the head (`TrackedDimensions::All`), so Cranpose hands their layers to the
//! compositor in the head's space, as it does with the controller's aim. A headset on a desk tracks
//! the head, so this runs that path. The extra `hand_menu` with the value `open` opens the menu at the
//! start and keeps it open, for a test with no hand that would keep a ray over it.

use crate::{
    console::Interface,
    displays::displays,
    hover,
    hub::hub,
    menu_model::{self, ButtonKind, MenuAction, MenuInput, MenuRow},
    menu_place::MenuPlace,
    panel_layout::{layer_rank, Layout, PanelId},
    ui::{text_style, ACCENT_COLOR, KEY_COLOR, MUTED_COLOR, PANEL_COLOR, STOP_COLOR, TEXT_COLOR},
};
use crate::place;
use cranpose::launch_args;
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{local_spatial_capabilities, FollowMode, FollowTarget, FollowingSubspace, Hand, HandJointType, Pose, SpatialPanel, SubspaceModifier, TrackedDimensions};
use cranvr_common::controls::{Controls, Hand as ControlHand};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

/// The ball's diameter in dp, a millimeter each
pub const BALL: f32 = 50.0;
/// The ball's center above the controller's grip, in dp
pub const LIFT: f32 = 110.0;
/// The width of the ball's ring, in dp
const RING: f32 = 5.0;
/// The distance between the ball and the display's or menu's bottom edge, in dp
pub const CLEARANCE: f32 = 30.0;
/// The menu's center beside the hand and nearer to the face, in dp
const MENU_X: f32 = 210.0;
const MENU_Z: f32 = 70.0;
/// The hand display's center beside the hand, in dp
pub const HAND_DISPLAY_X: f32 = 90.0;
/// In front of the user: the subspace's origin lies a meter ahead of the eyes. The menu stands 55 cm
/// ahead of the eyes with its top edge 6 cm below them. The hand's display stands 3 cm to the right
/// of the menu. All in dp.
const FRONT_Z: f32 = 450.0;
const FRONT_TOP: f32 = -60.0;
const FRONT_GAP: f32 = 30.0;
/// The distance from the eyes to the panels in front of the user, in dp
const FRONT_REACH: f32 = 1000.0 - FRONT_Z;
/// The menu closes when no pointer was over a panel for this long: a user who forgot it open in a
/// game must not lose the right hand's buttons
const MENU_IDLE: Duration = Duration::from_secs(20);
/// How often the menu looks at the displays' list while it is open
pub(crate) const MENU_POLL: Duration = Duration::from_millis(250);

/// How the menu and the hand's display stand around the ball.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stand {
    /// Above the ball, beside the hand toward the body: `side` is 1 for the right of the ball (the
    /// left hand) and -1 for its left (the right hand)
    Hand { side: f32 },
    /// In front of the eyes: the menu in the middle, the hand's display to its right
    Front,
}

/// Where the hand's panels stand in the subspace that holds them, and the names under which
/// `place.rs` keeps the places that the user carried the menu and the hand's display to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandFrame {
    /// The offset in dp from the subspace's origin to the point below the ball where a hand holds it
    pub base: [f32; 3],
    pub stand: Stand,
    pub menu: &'static str,
    pub display: &'static str,
}

/// The frame of the panels on the left controller: its origin is the controller's grip, turned as
/// its aim is turned. The names are those that the user's carries were saved under before the menu
/// had other places.
pub const ON_LEFT: HandFrame = HandFrame { base: [0.0; 3], stand: Stand::Hand { side: 1.0 }, menu: "hand-menu", display: "hand-display" };

/// The frame of the panels on the right controller, a mirror of the left one.
pub const ON_RIGHT: HandFrame = HandFrame { base: [0.0; 3], stand: Stand::Hand { side: -1.0 }, menu: "hand-menu-right", display: "hand-display-right" };

/// The frame of the panels in front of the user: the head's soft subspace has its origin a meter
/// ahead of the eyes. The ball stands 26 cm left of the nose and 30 cm below the eyes, 45 cm ahead of
/// them: 10 cm nearer than the menu, so a ray from the right hand passes in front of the open menu
/// to reach it.
pub const IN_VIEW: HandFrame = HandFrame { base: [-260.0, -300.0 - LIFT, FRONT_Z + 100.0], stand: Stand::Front, menu: "view-menu", display: "view-display" };

/// The frame of the test on a desk: the head's subspace has its origin a meter ahead of the head. A
/// left hand lies 30 cm left of the nose, 35 cm below it and 40 cm ahead of it.
pub const ON_VIEW: HandFrame = HandFrame { base: [-300.0, -350.0, 600.0], stand: Stand::Hand { side: 1.0 }, menu: "hand-menu-view", display: "hand-display-view" };

/// The menu's place now, as the frame loop chose it (`menu_place.rs`): 0 is the left hand, 1 the
/// right hand and 2 the view
static PLACE: AtomicU8 = AtomicU8::new(0);

/// Notes the menu's place for the code that runs outside the composition.
pub fn set_place(place: MenuPlace) {
    PLACE.store(
        match place {
            MenuPlace::Hand(ControlHand::Left) => 0,
            MenuPlace::Hand(ControlHand::Right) => 1,
            MenuPlace::View => 2,
        },
        Ordering::Relaxed,
    );
}

/// The menu's place now.
pub fn current_place() -> MenuPlace {
    match PLACE.load(Ordering::Relaxed) {
        0 => MenuPlace::Hand(ControlHand::Left),
        1 => MenuPlace::Hand(ControlHand::Right),
        _ => MenuPlace::View,
    }
}

/// The frame of the hand's panels now.
pub fn frame() -> HandFrame {
    anchor(current_place()).1
}

/// Where the hand's panels ride at `place`: a controller, which they follow in all six dimensions,
/// or the view, which they follow softly. A launch may fix the head for a test on a desk (see the
/// file's notes).
fn anchor(place: MenuPlace) -> (FollowTarget, HandFrame) {
    match launch_args().string("hand_anchor") {
        Some("view") => (FollowTarget::View(FollowMode::Snap(TrackedDimensions::TranslationAndYaw)), ON_VIEW),
        Some("head") => (FollowTarget::View(FollowMode::Tight(TrackedDimensions::All)), ON_VIEW),
        _ => match place {
            MenuPlace::Hand(ControlHand::Left) => (FollowTarget::Controller(Hand::Left, FollowMode::Tight(TrackedDimensions::All)), ON_LEFT),
            MenuPlace::Hand(ControlHand::Right) => (FollowTarget::Controller(Hand::Right, FollowMode::Tight(TrackedDimensions::All)), ON_RIGHT),
            MenuPlace::View => (FollowTarget::View(FollowMode::soft(TrackedDimensions::TranslationAndYaw)), IN_VIEW),
        },
    }
}

/// The pose of a panel in front of the eyes with its center `x` dp to the right of the nose and its
/// top edge at `FRONT_TOP`: tilted and turned so that it faces the eyes. `height` is its height in dp.
fn front_pose(x: f32, height: f32) -> Pose {
    let y = FRONT_TOP - height / 2.0;
    let pitch = (-y).atan2(FRONT_REACH).to_degrees();
    let yaw = x.atan2(FRONT_REACH).to_degrees();
    SubspaceModifier::new().offset(x, y, FRONT_Z).rotate(-pitch, -yaw, 0.0).pose()
}

/// The menu's pose before the user carries it. On a hand its bottom edge stands above the ball,
/// beside the hand and nearer to the face. In front of the user it stands in the middle, below the
/// eyes. `height` is the menu's height in dp.
pub fn menu_start(frame: HandFrame, height: f32) -> Pose {
    let base = frame.base;
    match frame.stand {
        Stand::Hand { side } => {
            let y = base[1] + LIFT + BALL / 2.0 + CLEARANCE + height / 2.0;
            SubspaceModifier::new().offset(base[0] + side * MENU_X, y, base[2] + MENU_Z).pose()
        }
        Stand::Front => front_pose(0.0, height),
    }
}

/// The hand's display's pose before the user carries it. On a hand it stands above the ball, a
/// little beside the hand. In front of the user it stands to the right of the menu. `size` is the
/// display's width and height in dp.
pub fn hand_display_start(frame: HandFrame, size: [f32; 2]) -> Pose {
    let base = frame.base;
    match frame.stand {
        Stand::Hand { side } => {
            let y = base[1] + LIFT + BALL / 2.0 + CLEARANCE + size[1] / 2.0;
            SubspaceModifier::new().offset(base[0] + side * HAND_DISPLAY_X, y, base[2]).pose()
        }
        Stand::Front => front_pose(menu_model::size::WIDTH / 2.0 + FRONT_GAP + size[0] / 2.0, size[1]),
    }
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

/// What the hand's panels share with the app.
#[derive(Clone, Copy, PartialEq)]
pub struct HandState {
    pub layout: MutableState<Layout>,
    pub menu_open: MutableState<bool>,
    /// Where the menu stands now (`menu_place.rs`)
    pub place: MutableState<MenuPlace>,
    /// The controllers that count as present (left, right)
    pub present: MutableState<[bool; 2]>,
    /// The line under the ball shows (`menu_hint.rs`)
    pub hint: MutableState<bool>,
    /// The palm that carries a ball of its own, while it faces the eyes (`palm.rs`)
    pub palm: MutableState<Option<ControlHand>>,
    /// The cameras track a hand
    pub hands: MutableState<bool>,
    /// No controller is present and the cameras track no hand: a look opens the menu (`gaze.rs`)
    pub gaze: MutableState<bool>,
    /// A look rests on that ball and counts toward a press: the ball shows it
    pub looking: MutableState<bool>,
    /// Counts up at each press on Recenter: the room's panels compose again around the head
    pub recenter: MutableState<u32>,
}

/// How the hand's panels draw: the interface, whether the ball is a 3D crystal (see `menu_tray.rs`),
/// the 3D theme and the room behind the words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    pub interface: Interface,
    pub ball_3d: bool,
    pub theme: crate::console::Theme,
    pub room: crate::console::Room,
}

/// The ball with the menu and the hand's display. In a headset only. `controls` is the mapping of
/// the buttons, and `streaming` says that a link streams, so a button opens the menu with a hold.
#[composable]
pub fn HandPanels(state: HandState, (console_open, look): (bool, Look), controls: Controls, streaming: bool) {
    // A flat window shows no ball. A test that asks for the menu gets it beside the panels instead
    // (see `flat_menu`).
    if !local_spatial_capabilities().current().is_spatial_ui_enabled() {
        return;
    }
    let place = state.place.get();
    let (target, frame) = anchor(place);
    let layout = state.layout.get();
    // The display on the hand while the host offers it. The strip has no button for a display that the
    // host does not offer.
    let on_hand = layout.hand_display();
    let pinned_shown = on_hand.filter(|stream| layout.shown(PanelId::Display(*stream)));
    let open = state.menu_open.get();
    let gaze = state.gaze.get();
    let hint = (state.hint.get() && !open).then(|| crate::menu_hint::text(&controls, state.present.get(), state.hands.get(), place, streaming));
    let said = hint.clone();
    LaunchedEffect(said.clone(), async move |_| {
        if let Some(text) = said {
            log::info!("the menu hint says: {text}");
        }
    });
    // The menu closes itself after a while without a pointer over a panel. A test launch that opens it
    // keeps it open: no hand keeps a ray over it on a desk.
    let closes_itself = !open_on_launch();
    LaunchedEffect(open, async move |_| {
        // The time counts from the last pointer over a panel, or from the opening
        let opened_ms = cranvr_proto::clock::now_ns() / 1_000_000;
        while open && closes_itself {
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
    // An open palm that faces the eyes carries a ball of its own, which the other hand presses
    if let Some(hand) = state.palm.get() {
        key(hand, move || PalmBall(state, hand));
    }
    // With no input at all, the ball that a look presses takes the place of the ball in the view
    if gaze {
        let hint = hint.clone();
        GazeBall(state, hint);
    }
    // A new place composes its panels anew: each place has its own subspace and its own saved places
    key(place, move || {
        FollowingSubspace(target.clone(), move || {
            if !gaze {
                if look.ball_3d {
                    crate::menu_tray::Strip3d(state, frame, on_hand, look);
                } else {
                    Strip(state, frame, on_hand);
                }
                if let Some(text) = hint.clone() {
                    Hint(text, frame);
                }
            }
            if look.interface.is_3d() {
                crate::menu_tray::MenuTray(state, console_open, frame, (open, look));
            } else if open {
                Menu(state, console_open, frame);
            }
            if let Some(stream) = pinned_shown {
                key(stream, move || crate::display_panel::HandDisplay(stream, frame));
            }
        });
    });
}

/// The ball, and beside it the round button of the hand's display when one rides with the menu. The
/// button lies on the side of the menu, so the ball stays above the hand.
#[composable]
fn Strip(state: HandState, frame: HandFrame, pinned: Option<u8>) {
    let base = frame.base;
    let width = if pinned.is_some() { BALL * 2.0 + 12.0 } else { BALL };
    let shown = pinned.is_some_and(|stream| state.layout.get().shown(PanelId::Display(stream)));
    let open = state.menu_open.get();
    let side = match frame.stand {
        Stand::Hand { side } => side,
        Stand::Front => 1.0,
    };
    // The pill's middle lies half a button from the ball's, toward the menu
    let x = base[0] + if pinned.is_some() { side * (BALL + 12.0) / 2.0 } else { 0.0 };
    SpatialPanel(SubspaceModifier::new().size(width, BALL).offset(x, base[1] + LIFT, base[2]).layer_priority(layer_rank::HAND_MENU), move || {
        let hovered = hover::hover_modifier(hover::STRIP);
        Row(
            Modifier::empty().fill_max_size().then(hovered),
            RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(12.0)),
            move || {
                let ball = move || Round(if open { ACCENT_COLOR } else { KEY_COLOR }, RoundPress::Menu, state);
                let display = move |stream| Round(if shown { ACCENT_COLOR } else { KEY_COLOR }, RoundPress::HandDisplay(stream), state);
                match pinned {
                    Some(stream) if side < 0.0 => {
                        display(stream);
                        ball();
                    }
                    Some(stream) => {
                        ball();
                        display(stream);
                    }
                    None => ball(),
                }
            },
        );
    });
}

/// The ball's distance in front of the palm, in dp
const PALM_LIFT: f32 = 70.0;

/// The ball on an open palm (see `palm.rs`): it stands in front of the palm and faces away from it,
/// toward the eyes that the palm faces.
#[composable]
fn PalmBall(state: HandState, hand: ControlHand) {
    let xr_hand = match hand {
        ControlHand::Left => Hand::Left,
        ControlHand::Right => Hand::Right,
    };
    let open = state.menu_open.get();
    // A hand joint's +y points out of the back of the hand: the palm's side is -y, and a turn of 90
    // degrees about x makes the ball face along it
    let modifier = SubspaceModifier::new().size(BALL, BALL).offset(0.0, -PALM_LIFT, 0.0).rotate(90.0, 0.0, 0.0).layer_priority(layer_rank::HAND_MENU);
    FollowingSubspace(FollowTarget::Hand(xr_hand, HandJointType::Palm, FollowMode::Tight(TrackedDimensions::All)), move || {
        SpatialPanel(modifier.clone(), move || {
            let hovered = hover::hover_modifier(hover::PALM);
            Box(Modifier::empty().fill_max_size().then(hovered), BoxSpec::default(), move || {
                Round(if open { ACCENT_COLOR } else { KEY_COLOR }, RoundPress::Menu, state);
            });
        });
    });
}

/// The ball that a look presses, low in front of the user (see `gaze.rs`), with the line under it. It
/// follows the head's place and heading in every frame, so the app knows where it stands.
#[composable]
fn GazeBall(state: HandState, hint: Option<String>) {
    let pitch = crate::gaze::pitch();
    let [x, y, z] = crate::gaze::offset(pitch);
    let open = state.menu_open.get() || state.looking.get();
    // The frame of the line under the ball: its base lies `LIFT` below the ball, as on a hand
    let frame = HandFrame { base: [x, y - LIFT, z], stand: Stand::Front, menu: "gaze-menu", display: "gaze-display" };
    FollowingSubspace(FollowTarget::View(FollowMode::Tight(TrackedDimensions::TranslationAndYaw)), move || {
        // Tilted up by the ball's angle, so it faces the eyes
        let modifier = SubspaceModifier::new().size(BALL, BALL).offset(x, y, z).rotate(-pitch, 0.0, 0.0).layer_priority(layer_rank::HAND_MENU);
        SpatialPanel(modifier, move || {
            Round(if open { ACCENT_COLOR } else { KEY_COLOR }, RoundPress::Menu, state);
        });
        if let Some(text) = hint.clone() {
            Hint(text, frame);
        }
    });
}

/// The width and the height of the line under the ball, in dp
const HINT_SIZE: [f32; 2] = [460.0, 36.0];

/// The line under the ball that names the ways to open the menu.
#[composable]
fn Hint(text: String, frame: HandFrame) {
    let base = frame.base;
    let y = base[1] + LIFT - BALL / 2.0 - 8.0 - HINT_SIZE[1] / 2.0;
    SpatialPanel(SubspaceModifier::new().size(HINT_SIZE[0], HINT_SIZE[1]).offset(base[0], y, base[2]).layer_priority(layer_rank::HAND_MENU), move || {
        Box(Modifier::empty().fill_max_size().background(PANEL_COLOR).rounded_corners(HINT_SIZE[1] / 2.0).padding_symmetric(14.0, 6.0), BoxSpec::default(), {
            let text = text.clone();
            move || {
                Text(text.clone(), Modifier::empty(), text_style(20.0, TEXT_COLOR, None));
            }
        });
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

pub(crate) fn toggle_menu(state: HandState) {
    let open = !state.menu_open.get_non_reactive();
    log::info!("the hand menu {}", if open { "opens" } else { "closes" });
    state.menu_open.set(open);
}

pub(crate) fn toggle_hand_display(state: HandState, stream: u8) {
    let mut layout = state.layout.get_non_reactive();
    layout.toggle(PanelId::Display(stream));
    log::info!("the display on the hand {}", if layout.shown(PanelId::Display(stream)) { "shows" } else { "hides" });
    commit(state, layout);
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
    // The pages of the Mac's settings, as the host's menu lists them
    let pages = rememberMutableStateOf(|| hub().listed_pages());
    LaunchedEffect((), async move |_| loop {
        let now = displays().menu_view();
        if view.get_non_reactive() != now {
            view.set(now);
        }
        let listed = hub().listed_pages();
        if pages.get_non_reactive() != listed {
            pages.set(listed);
        }
        delay(MENU_POLL).await;
    });
    // A display that the user added, from the press until the host lists it or says why not
    let adding = rememberMutableStateOf(|| None::<Adding>);
    let layout = state.layout.get();
    let wishes = crate::displays::wishes_of(&layout);
    let displays_view = view.get();
    let busy = adding.get().is_some_and(|add| add.runs(&displays_view));
    let listed = pages.get();
    let input = MenuInput { layout: &layout, console_open, displays: &displays_view, wishes, place: state.place.get(), adding: busy, pages: &listed, costs: true };
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
                Row(Modifier::empty().fill_max_width().height(menu_model::size::TITLE), RowSpec::default(), || {
                    Text("cranvr", Modifier::empty().weight(1.0), text_style(32.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                    // One press gives the 3D menus back
                    Button(
                        Modifier::empty().width(menu_model::size::BUTTON).fill_max_height().background(KEY_COLOR).rounded_corners(12.0),
                        ButtonSpec::default(),
                        || hub().pick_interface(Interface::ThreeD),
                        || {
                            Text("3D", Modifier::empty(), text_style(22.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                        },
                    );
                });
                for row in rows.clone() {
                    RowView(row, state, adding);
                }
            },
        );
    });
}

/// A display that the user asked the Mac for: how many the host listed then, its notice then and
/// when.
#[derive(Clone, PartialEq)]
pub(crate) struct Adding {
    listed: usize,
    notice: Option<String>,
    since: Instant,
}

/// How long the Mac may take to add a display before the menu stops saying that it adds one
const ADD_TIME: Duration = Duration::from_secs(20);

impl Adding {
    fn new(view: &crate::displays::DisplaysView) -> Self {
        Self { listed: view.displays.len(), notice: view.notice.clone(), since: Instant::now() }
    }

    /// Whether the Mac still adds the display: the host lists no new display and says nothing new.
    pub(crate) fn runs(&self, view: &crate::displays::DisplaysView) -> bool {
        view.displays.len() <= self.listed && view.notice == self.notice && self.since.elapsed() < ADD_TIME
    }
}

#[composable]
fn RowView(row: MenuRow, state: HandState, adding: MutableState<Option<Adding>>) {
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
                        move || perform(action, state, Some(adding)),
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

/// Does what a press of the Mac on the headset's own page says (`headset_page.rs`), as the menu's
/// button does.
pub fn perform_remote(action: MenuAction, state: HandState) {
    perform(action, state, None);
}

/// Does what a button of the menu says. `adding` is the menu's note of a display that the user
/// added, when the menu is open.
pub(crate) fn perform(action: MenuAction, state: HandState, adding: Option<MutableState<Option<Adding>>>) {
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
            log::info!("the hand menu puts display {} {}", stream - cranvr_proto::message::DESKTOP_STREAM + 1, if layout.pinned() == Some(stream) { current_place().label() } else { "back in the room" });
            commit(state, layout);
        }
        MenuAction::RemoveDisplay(stream) => {
            // The display's place goes with it, in a game too
            displays().request_remove(stream);
            layout.forget(stream);
            log::info!("the hand menu removes display {}", stream - cranvr_proto::message::DESKTOP_STREAM + 1);
            layout.save();
            state.layout.set(layout);
        }
        MenuAction::AddDisplay => {
            if let Some(adding) = adding {
                adding.set(Some(Adding::new(&displays().menu_view())));
            }
            displays().request_add();
        }
        MenuAction::Recenter => {
            log::info!("the hand menu puts the panels in front of the user again");
            state.recenter.set(state.recenter.get_non_reactive() + 1);
        }
        MenuAction::OpenControls => hub().open_controls(),
        MenuAction::OpenPage(index) => {
            // The console takes the place of the menu, which closes
            if let Some((id, short)) = hub().listed_pages().into_iter().nth(usize::from(index)) {
                log::info!("the hand menu opens the console on the page {short} ({id})");
                hub().open_page(id);
                state.menu_open.set(false);
            }
        }
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
        let menu = LEFT.then(&menu_start(ON_LEFT, height));
        assert!(facing_the_head(&menu) > 0.8, "{menu:?}");
        // Its bottom edge is above the ball, and its middle is right of the hand
        let bottom = menu.position[1] - height / 2000.0;
        assert!(bottom > LEFT.position[1] + (LIFT + BALL / 2.0) / 1000.0, "{menu:?}");
        assert!(menu.position[0] > LEFT.position[0] + 0.15, "{menu:?}");
        let display = LEFT.then(&hand_display_start(ON_LEFT, [420.0, 280.0]));
        assert!(facing_the_head(&display) > 0.8, "{display:?}");
        // On the right hand the menu is the mirror: left of the hand
        let right = Pose::at([0.2, -0.3, -0.4]);
        let menu = right.then(&menu_start(ON_RIGHT, height));
        assert!(facing_the_head(&menu) > 0.8, "{menu:?}");
        assert!(menu.position[0] < right.position[0] - 0.15, "{menu:?}");
    }

    /// The subspace that follows the view, with the head at the room's origin: its origin lies a
    /// meter ahead
    const VIEW: Pose = Pose::at([0.0, 0.0, -1.0]);

    #[test]
    fn in_front_of_the_user_the_menu_stands_at_arms_reach_below_the_eyes_and_faces_them() {
        let height = 430.0;
        let menu = VIEW.then(&menu_start(IN_VIEW, height));
        assert!(facing_the_head(&menu) > 0.99, "{menu:?}");
        let distance = menu.position.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((0.55..0.7).contains(&distance), "{distance}");
        // Its top edge stands below the eyes, and its middle in front of the nose
        let top = menu.position[1] + height / 2000.0 * 0.9;
        assert!(top < 0.0 && menu.position[1] > -0.4, "{menu:?}");
        assert!(menu.position[0].abs() < 1e-4, "{menu:?}");
        // The ball stands left of the menu and below the eyes, where the menu does not cover it
        let ball = VIEW.then(&Pose::at([IN_VIEW.base[0], IN_VIEW.base[1] + LIFT, IN_VIEW.base[2]].map(|dp| dp / 1000.0)));
        // Seen from the eyes, the ball's right edge lies left of the menu's left edge
        let angle = |x: f32, z: f32| (x / -z).atan().to_degrees();
        let menu_left = angle(menu.position[0] - menu_model::size::WIDTH / 2000.0, menu.position[2]);
        assert!(angle(ball.position[0] + BALL / 2000.0, ball.position[2]) < menu_left - 3.0, "{ball:?}");
        assert!((-0.35..-0.25).contains(&ball.position[1]) && (-0.5..-0.4).contains(&ball.position[2]), "{ball:?}");
        // It stands nearer than the menu, so a ray from the right hand reaches it in front of the menu
        assert!(ball.position[2] > menu.position[2] + 0.05, "{ball:?} {menu:?}");
        // The display stands right of the menu and faces the eyes
        let display = VIEW.then(&hand_display_start(IN_VIEW, [420.0, 236.0]));
        assert!(facing_the_head(&display) > 0.99, "{display:?}");
        assert!(display.position[0] - 0.21 > menu.position[0] + menu_model::size::WIDTH / 2000.0, "{display:?}");
    }

    #[test]
    fn the_places_on_the_controller_and_on_the_head_have_names_of_their_own() {
        let names = [ON_LEFT.menu, ON_LEFT.display, ON_RIGHT.menu, ON_RIGHT.display, IN_VIEW.menu, IN_VIEW.display, ON_VIEW.menu, ON_VIEW.display];
        for (i, name) in names.iter().enumerate() {
            assert!(!names[..i].contains(name), "{name}");
            assert!(!["desktop", "display2", "display3", "display4", "connect", "stats", "console"].contains(name), "{name}");
        }
    }
}
