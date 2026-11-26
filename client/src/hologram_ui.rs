//! The hologram form of the 3D interface (#111): the Stream page and the hand menu as objects of
//! glowing glass that float with no panel behind them (Cranpose's `SpatialHologram`).
//!
//! A page draws as a hologram when its choices carry shapes (`ChoiceShape`), which the Mac gives the
//! Stream page's settings: each option is an object whose form shows its value. The rows are those
//! of `page_model.rs`, with the same heights and places, so a scripted aim at a key of the model
//! presses the same object. The words float on the hologram's panel at the compositor's full
//! resolution, with no plate behind them. In passthrough a faint dark glass stands behind each line
//! of words, so they stay readable over a bright room.

use crate::{
    console::{size, Pending, Room, Shown, Theme},
    console_panel::{run, tone_color, RowName},
    page_model::{Form, Key, Line, Look, Row},
    ui::{text_style, MUTED_COLOR, TEXT_COLOR},
};
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{
    ButtonShape, ChoiceForm, HologramColors, HologramStage, SpatialButton, SpatialChoiceGroup, SpatialEmblem, SpatialHologram, SpatialRibbon, SpatialSwitch,
    SubspaceModifier, SwitchColors,
};
use cranvr_common::menu::ChoiceShape;

/// The glass behind words in passthrough
const BACKING: Color = Color(0.0, 0.0, 0.0, 0.55);
/// The side of a hologram page's emblem, in dp
const EMBLEM: f32 = 64.0;

/// The colors of a theme.
pub fn colors(theme: Theme) -> HologramColors {
    match theme {
        Theme::Cyan => HologramColors::CYAN,
        Theme::Amber => HologramColors::VIOLET_GOLD,
    }
}

/// How the words of a hologram draw: with a dark glass behind them in passthrough.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Words {
    pub backed: bool,
}

impl Words {
    pub fn of(room: Room) -> Self {
        Self { backed: room == Room::Passthrough }
    }
}

/// A line of words of a hologram.
#[composable]
pub fn Label(words: Words, text: String, style: TextStyle) {
    let modifier = if words.backed { Modifier::empty().background(BACKING).rounded_corners(8.0).padding_symmetric(8.0, 2.0) } else { Modifier::empty() };
    Text(text, modifier, style);
}

/// The console's page as a hologram, at the place and size that `modifier` gives.
#[composable]
pub fn HologramConsole(
    modifier: SubspaceModifier,
    rows: Vec<Row>,
    page: MutableState<Shown>,
    (pending, notice): (MutableState<Option<Pending>>, Option<String>),
    (theme, words): (Theme, Words),
) {
    let colors = colors(theme);
    SpatialHologram(modifier, true, colors, HologramStage::PAGE, move || {
        // The pointer over the console holds the pointing hand back from the game (see `hold.rs`)
        let hovered = crate::hover::hover_modifier(crate::hover::CONSOLE);
        let rows = rows.clone();
        let notice = notice.clone();
        Column(
            Modifier::empty().fill_max_size().then(hovered).padding(size::PADDING),
            ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(size::GAP)),
            move || {
                for row in rows.clone() {
                    HologramRow(row, page, (pending, notice.clone()), (colors, words));
                }
            },
        );
    });
}

/// The form of the objects of a choice of `shape`.
fn form_of(shape: ChoiceShape) -> ChoiceForm {
    match shape {
        ChoiceShape::Keys | ChoiceShape::Gems => ChoiceForm::Gems,
        ChoiceShape::Cubes => ChoiceForm::Cubes,
        ChoiceShape::Petals => ChoiceForm::Petals,
        ChoiceShape::Arc => ChoiceForm::Arc,
        ChoiceShape::Stepper => ChoiceForm::Stepper,
    }
}

#[composable]
fn HologramRow(row: Row, page: MutableState<Shown>, (pending, notice): (MutableState<Option<Pending>>, Option<String>), (colors, words): (HologramColors, Words)) {
    let height = row.height;
    match row.line {
        Line::Header(text) | Line::Title { text, .. } => {
            Row(
                Modifier::empty().fill_max_width().height(height),
                RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(18.0)).vertical_alignment(VerticalAlignment::CenterVertically),
                move || {
                    SpatialEmblem(Modifier::empty().width(EMBLEM).height(EMBLEM), move || PlaySign(colors.chosen));
                    let text = text.clone();
                    SpatialRibbon(Modifier::empty().weight(1.0).height(52.0).padding_symmetric(18.0, 0.0), move || {
                        Label(words, text.clone(), text_style(34.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                    });
                },
            );
        }
        Line::Heading(text) => {
            Box(Modifier::empty().height(height), BoxSpec::default().content_alignment(Alignment::CENTER_START), move || {
                Label(words, text.clone(), text_style(24.0, MUTED_COLOR, Some(FontWeight::BOLD)));
            });
        }
        Line::Text { text, tone } => {
            Box(Modifier::empty().fill_max_width().height(height), BoxSpec::default().content_alignment(Alignment::CENTER_START), move || {
                Label(words, text.clone(), text_style(22.0, tone_color(tone), None));
            });
        }
        Line::Keys { name, keys, form } => match form {
            Form::Object(shape) => {
                let selected = keys.iter().position(|key| key.look == Look::Selected);
                let count = keys.len();
                let (pressed, said) = (keys.clone(), keys.clone());
                let notice = notice.clone();
                SpatialChoiceGroup(
                    selected,
                    move |index| {
                        if let Some(key) = pressed.get(index) {
                            run(&key.press, page, pending, notice.clone());
                        }
                    },
                    count,
                    form_of(shape),
                    Modifier::empty().fill_max_width().height(height),
                    move |index| {
                        if let Some(key) = said.get(index) {
                            let chosen = key.look == Look::Selected;
                            let color = if chosen { TEXT_COLOR } else { MUTED_COLOR };
                            // A stepper shows one option's words over its crystal, on dark glass so the
                            // crystal's bright edges do not cross them
                            let over_crystal = shape == ChoiceShape::Stepper;
                            let size = if over_crystal { 20.0 } else { 17.0 };
                            let words = Words { backed: words.backed || over_crystal };
                            Label(words, key.label.clone(), text_style(size, color, chosen.then_some(FontWeight::BOLD)));
                        }
                    },
                );
            }
            Form::Switch if keys.len() == 2 => {
                Row(
                    Modifier::empty().fill_max_width().height(height),
                    RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
                    move || {
                        let width = RowName(&name);
                        let span = width.map_or(Modifier::empty().weight(1.0), |width| Modifier::empty().width(2.0 * width + size::GAP));
                        let checked = keys[0].look == Look::Selected;
                        let keys = keys.clone();
                        let notice = notice.clone();
                        Capsule(checked, span.height(size::BUTTON), colors, move |on| {
                            let key = &keys[if on { 0 } else { 1 }];
                            run(&key.press, page, pending, notice.clone());
                        });
                    },
                );
            }
            _ => {
                Row(
                    Modifier::empty().fill_max_width().height(height),
                    RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
                    move || {
                        let width = RowName(&name);
                        for key in keys.clone() {
                            GlassKey(key, width, colors, page, (pending, notice.clone()));
                        }
                    },
                );
            }
        },
    }
}

/// A glowing triangle that points right, the sign of the stream.
#[composable]
fn PlaySign(color: Color) {
    Canvas(Modifier::empty().width(24.0).height(26.0), move |scope| {
        let size = scope.size();
        let mut path = Path::new();
        path.move_to(Point::new(size.width * 0.12, 0.0));
        path.line_to(Point::new(size.width, size.height / 2.0));
        path.line_to(Point::new(size.width * 0.12, size.height));
        path.close();
        scope.draw_path(&path, Brush::solid(color), DrawStyle::Fill);
    });
}

/// A switch as a capsule of glass whose lit half slides to the chosen side, with "Off" and "On" over
/// its halves.
#[composable]
pub fn Capsule(checked: bool, modifier: Modifier, colors: HologramColors, on_change: impl Fn(bool) + 'static) {
    let on_change = std::rc::Rc::new(on_change);
    Box(modifier, BoxSpec::default(), move || {
        let on_change = on_change.clone();
        SpatialSwitch(checked, move |on| on_change(on), Modifier::empty().fill_max_size(), true, SwitchColors { face: colors.glass, checked: colors.chosen });
        Row(Modifier::empty().fill_max_size(), RowSpec::default(), move || {
            for (words, lit) in [("Off", !checked), ("On", checked)] {
                Box(Modifier::empty().weight(1.0).fill_max_height(), BoxSpec::default().content_alignment(Alignment::CENTER), move || {
                    let color = if lit { TEXT_COLOR } else { MUTED_COLOR };
                    Text(words, Modifier::empty(), text_style(18.0, color, lit.then_some(FontWeight::BOLD)));
                });
            }
        });
    });
}

/// A key of glass with its words, `width` wide or a share of the row.
#[composable]
fn GlassKey(key: Key, width: Option<f32>, colors: HologramColors, page: MutableState<Shown>, (pending, notice): (MutableState<Option<Pending>>, Option<String>)) {
    let modifier = match width {
        Some(width) => Modifier::empty().width(width),
        None => Modifier::empty().weight(1.0),
    }
    .height(size::BUTTON);
    let press = key.press.clone();
    let color = if key.look == Look::Selected { colors.chosen } else { colors.glass };
    let label = key.label.clone();
    SpatialButton(move || run(&press, page, pending, notice.clone()), modifier, true, ButtonShape::Key, color, move || {
        Text(label.clone(), Modifier::empty(), text_style(19.0, TEXT_COLOR, Some(FontWeight::BOLD)));
    });
}
