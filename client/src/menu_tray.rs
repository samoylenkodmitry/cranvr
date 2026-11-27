//! The hand menu in 3D (#111): the ball as a small glowing crystal and the menu as a hologram
//! (`hologram_ui.rs`), with a few sparks around it.
//!
//! The parts stand where the flat ones stand (`hand_menu.rs`), with the same widths, so every place,
//! carry and scripted aim of the flat menu holds. The rows are the flat menu's rows (`menu_model.rs`).
//! The menu's name stands on a band of glass beside an emblem. A button that shows or hides something
//! (a panel, the display on the hand) is a capsule, which is on while the thing shows, with its name
//! in the middle of its row. The links to the console's pages are gems over their names, in rows
//! that are taller than the flat menu's (`menu_model::LINKS_3D`). The other buttons are pills of
//! glass. The title row carries the switch of the 3D interface itself, so one press there gives the
//! flat menus back.
//!
//! The menu rises as it opens and sinks as it closes. After the hide it composes nothing: no panel,
//! no objects, no clock and no look at the displays, so a closed menu costs nothing. The crystal
//! holds still while the menu is closed: it draws again only when a ray rests on it or presses it.
//! Its mesh joins the scene that the controllers' models draw. While no model shows and the menu is
//! closed, the scene would draw for the crystal alone, so the flat ball stands in for it
//! (`ball_3d`), in a game and outside one.

use crate::{
    console::Interface,
    displays::displays,
    hand_menu::{menu_start, perform, toggle_hand_display, toggle_menu, Adding, HandFrame, HandState, Stand, BALL, LIFT, MENU_POLL},
    hologram_ui::{colors, Capsule, Header, Label, Words},
    hover,
    hub::hub,
    menu_model::{self, ButtonKind, MenuAction, MenuInput, MenuRow},
    panel_layout::{layer_rank, PanelId},
    place,
    spatial_ui::KEY_DANGER,
    ui::{text_style, MUTED_COLOR, TEXT_COLOR},
};
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{ButtonShape, ChoiceForm, HologramColors, HologramStage, Pose, SpatialButton, SpatialChoiceGroup, SpatialHologram, SubspaceModifier};

/// The sparks around the open menu
const MENU_STAGE: HologramStage = HologramStage { projector: false, ribbons: 0, particles: 12 };
/// The side of the menu's emblem
const EMBLEM: f32 = 40.0;

/// The ball, and beside it the ball of the hand's display when one rides with the menu, as glowing
/// crystals. They stand where the flat strip stands.
#[composable]
pub fn Strip3d(state: HandState, frame: HandFrame, pinned: Option<u8>, look: crate::hand_menu::Look) {
    let colors = colors(look.theme);
    let base = frame.base;
    let width = if pinned.is_some() { BALL * 2.0 + 12.0 } else { BALL };
    let shown = pinned.is_some_and(|stream| state.layout.get().shown(PanelId::Display(stream)));
    let open = state.menu_open.get();
    let side = match frame.stand {
        Stand::Hand { side } => side,
        Stand::Front => 1.0,
    };
    let x = base[0] + if pinned.is_some() { side * (BALL + 12.0) / 2.0 } else { 0.0 };
    let modifier = SubspaceModifier::new().size(width, BALL).offset(x, base[1] + LIFT, base[2]).layer_priority(layer_rank::HAND_MENU);
    SpatialHologram(modifier, true, colors, HologramStage::NONE, move || {
        let hovered = hover::hover_modifier(hover::STRIP);
        Row(Modifier::empty().fill_max_size().then(hovered), RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(12.0)), move || {
            let lit = move |on: bool| if on { colors.chosen } else { colors.glass };
            let ball = move || Crystal(lit(open), move || toggle_menu(state));
            let display = move |stream| Crystal(lit(shown), move || toggle_hand_display(state, stream));
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
        });
    });
}

/// A ball of the strip: a crystal of `color` that opens the menu or shows the hand's display.
#[composable]
fn Crystal(color: Color, on_click: impl Fn() + 'static) {
    SpatialButton(on_click, Modifier::empty().width(BALL).height(BALL), true, ButtonShape::Ball, color, || {});
}

/// The menu as a hologram. It shows while `open` and composes nothing once it has hidden.
#[composable]
pub fn MenuTray(state: HandState, console_open: bool, frame: HandFrame, (open, look): (bool, crate::hand_menu::Look)) {
    let (colors, words) = (colors(look.theme), Words::of(look.room));
    let view = rememberMutableStateOf(|| displays().menu_view());
    let pages = rememberMutableStateOf(|| hub().listed_pages());
    let recording = rememberMutableStateOf(|| hub().recording());
    // The menu looks at the displays, the pages and the recording only while it is open
    LaunchedEffect(open, async move |_| {
        let mut seen = hub().recording_changes();
        while open {
            let changes = hub().recording_changes();
            if changes != seen {
                seen = changes;
                recording.set(hub().recording());
            }
            let now = displays().menu_view();
            if view.get_non_reactive() != now {
                view.set(now);
            }
            let listed = hub().listed_pages();
            if pages.get_non_reactive() != listed {
                pages.set(listed);
            }
            delay(MENU_POLL).await;
        }
    });
    let adding = rememberMutableStateOf(|| None::<Adding>);
    let layout = state.layout.get();
    let wishes = crate::displays::wishes_of(&layout);
    let displays_view = view.get();
    let busy = adding.get().is_some_and(|add| add.runs(&displays_view));
    let listed = pages.get();
    let recorded = recording.get();
    let input = MenuInput { layout: &layout, console_open, displays: &displays_view, wishes, place: state.place.get(), adding: busy, pages: &listed, costs: true, recording: recorded.as_ref() };
    let rows = menu_model::rows(&input);
    let height = menu_model::height(&rows, Interface::ThreeD);
    // The place that the user carried the menu to, else its start. Cranpose keeps a carry's place
    // while the menu stays open.
    let pose: Pose = place::saved_pose(frame.menu).unwrap_or_else(|| menu_start(frame, height));
    let modifier = SubspaceModifier::new().size(menu_model::size::WIDTH, height).layer_priority(layer_rank::HAND_MENU);
    SpatialHologram(place::movable(frame.menu, modifier, pose, |_| {}), open, colors, MENU_STAGE, move || {
        let hovered = hover::hover_modifier(hover::MENU);
        let rows = rows.clone();
        Column(
            Modifier::empty().fill_max_size().then(hovered).padding(menu_model::size::PADDING),
            ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(menu_model::size::GAP)),
            move || {
                TitleRow(colors, words);
                for row in rows.clone() {
                    RowView3d(row, state, adding, (colors, words));
                }
            },
        );
    });
}

/// The menu's name on a band of glass beside an emblem, and the switch of the 3D interface: a press
/// gives the flat menus back.
#[composable]
fn TitleRow(colors: HologramColors, words: Words) {
    use menu_model::size::*;
    Row(
        Modifier::empty().fill_max_width().height(TITLE),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(GAP)).vertical_alignment(VerticalAlignment::CenterVertically),
        move || {
            Box(Modifier::empty().weight(1.0).fill_max_height(), BoxSpec::default(), move || {
                Header("cranvr".into(), String::new(), (TITLE, EMBLEM), (colors, words));
            });
            Label(words, "3D".into(), text_style(20.0, MUTED_COLOR, Some(FontWeight::BOLD)));
            Capsule(true, Modifier::empty().width(BUTTON).height(TITLE), colors, |on| {
                if !on {
                    hub().pick_interface(Interface::Flat);
                }
            });
        },
    );
}

/// Whether a button of the menu shows or hides something, so it is a switch in 3D.
fn is_switch(action: MenuAction) -> bool {
    matches!(action, MenuAction::TogglePanel(_) | MenuAction::ToggleHand(_))
}

#[composable]
fn RowView3d(row: MenuRow, state: HandState, adding: MutableState<Option<Adding>>, (colors, words): (HologramColors, Words)) {
    use menu_model::size::*;
    if row.is_links() {
        Links(row, state, adding, words);
        return;
    }
    Column(Modifier::empty().fill_max_width(), ColumnSpec::default(), move || {
        let (title, buttons) = (row.title.clone(), row.buttons.clone());
        Row(
            Modifier::empty().fill_max_width().height(ROW),
            RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(GAP)).vertical_alignment(VerticalAlignment::CenterVertically),
            move || {
                if !title.is_empty() {
                    let title = title.clone();
                    // The name stands in the middle of its row, level with the row's switch
                    Box(Modifier::empty().weight(1.0).fill_max_height(), BoxSpec::default().content_alignment(Alignment::CENTER_START), move || {
                        Label(words, title.clone(), text_style(25.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                    });
                }
                for button in buttons.clone() {
                    let action = button.action;
                    // Each button of a row without a title shares the row's width
                    let modifier = if title.is_empty() { Modifier::empty().weight(1.0) } else { Modifier::empty().width(BUTTON) }.height(ROW);
                    if is_switch(action) {
                        Capsule(button.kind == ButtonKind::On, modifier, colors, move |_| perform(action, state, Some(adding)));
                    } else {
                        let color = match button.kind {
                            ButtonKind::Normal => colors.glass,
                            ButtonKind::On => colors.chosen,
                            ButtonKind::Danger => KEY_DANGER,
                        };
                        let label = button.label.clone();
                        SpatialButton(
                            move || perform(action, state, Some(adding)),
                            modifier,
                            true,
                            ButtonShape::Key,
                            color,
                            move || {
                                Text(label.clone(), Modifier::empty(), text_style(20.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                            },
                        );
                    }
                }
            },
        );
        for detail in row.details.clone() {
            Box(Modifier::empty().height(DETAIL), BoxSpec::default().content_alignment(Alignment::CENTER_START), move || {
                Label(words, detail.clone(), text_style(20.0, MUTED_COLOR, None));
            });
        }
    });
}

/// A row of links to the console's pages: a gem over each page's name. A press opens the console on
/// that page.
#[composable]
fn Links(row: MenuRow, state: HandState, adding: MutableState<Option<Adding>>, words: Words) {
    let buttons = row.buttons.clone();
    let names = row.buttons;
    SpatialChoiceGroup(
        None,
        move |index| {
            if let Some(button) = buttons.get(index) {
                perform(button.action, state, Some(adding));
            }
        },
        names.len(),
        ChoiceForm::Gems,
        Modifier::empty().fill_max_width().height(menu_model::LINKS_3D),
        move |index| {
            if let Some(button) = names.get(index) {
                Label(words, button.label.clone(), text_style(19.0, TEXT_COLOR, Some(FontWeight::BOLD)));
            }
        },
    );
}
