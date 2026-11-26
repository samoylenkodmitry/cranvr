//! The 3D interface as holograms (#111): the console's pages and the hand menu as objects of
//! glowing glass that float with no panel behind them (Cranpose's `SpatialHologram`).
//!
//! The rows are those of `page_model.rs`, with the same heights and places, so a scripted aim at a
//! key of the model presses the same object. A choice whose options have shapes (`ChoiceShape`),
//! which the Mac gives the Stream page's settings, draws each option as an object whose form shows
//! its value, with its name over it. A key is a pill of glass, a toggle a capsule whose lit half
//! slides and a choice of numbers a groove with a crystal knob. A page's title stands on a band of
//! glass beside an emblem, with a wave of light points at the band's end.
//!
//! The words float on the hologram's panel at the compositor's full resolution, with no plate
//! behind them. In passthrough a faint dark glass stands behind each line of words, so they stay
//! readable over a bright room.

use crate::{
    console::{size, Pending, Room, Shown, Theme},
    console_panel::{run, tone_color},
    page_model::{Form, Key, Line, Look, Name, Row, LABEL},
    spatial_ui::{Steps3d, KEY_DANGER},
    ui::{text_style, MUTED_COLOR, TEXT_COLOR},
};
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{
    ButtonShape, ChoiceForm, HologramColors, HologramStage, SpatialButton, SpatialChoiceGroup, SpatialEmblem, SpatialHologram, SpatialParticleWave, SpatialRibbon, SpatialSwitch, SubspaceModifier,
    SwitchColors,
};
use cranvr_common::menu::ChoiceShape;

/// The glass behind words in passthrough
const BACKING: Color = Color(0.0, 0.0, 0.0, 0.55);
/// The side of a page's emblem, in dp
const EMBLEM: f32 = 76.0;
/// The height of the band of glass behind a page's title, and the size of the wave at its end
const BAND: f32 = 64.0;
const WAVE: [f32; 2] = [190.0, 56.0];
/// The wave's width for each dp of a header's height: a small header, such as the hand menu's, gets
/// a short wave, so its title keeps room
const WAVE_PER_HEIGHT: f32 = 2.2;

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
pub fn HologramConsole(modifier: SubspaceModifier, rows: Vec<Row>, page: MutableState<Shown>, (pending, notice): (MutableState<Option<Pending>>, Option<String>), (theme, words): (Theme, Words)) {
    let colors = colors(theme);
    SpatialHologram(modifier, true, colors, HologramStage::PAGE, move || {
        // The pointer over the console holds the pointing hand back from the game (see `hold.rs`)
        let hovered = crate::hover::hover_modifier(crate::hover::CONSOLE);
        let rows = rows.clone();
        let notice = notice.clone();
        Column(Modifier::empty().fill_max_size().then(hovered).padding(size::PADDING), ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(size::GAP)), move || {
            for row in rows.clone() {
                HologramRow(row, page, (pending, notice.clone()), (colors, words));
            }
        });
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

/// A title on a band of glass beside an emblem, with the stream's state under it and a wave of light
/// points at the band's end.
#[composable]
pub fn Header(text: String, status: String, (height, emblem): (f32, f32), (colors, words): (HologramColors, Words)) {
    Row(
        Modifier::empty().fill_max_width().height(height),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(16.0)).vertical_alignment(VerticalAlignment::CenterVertically),
        move || {
            SpatialEmblem(Modifier::empty().width(emblem).height(emblem), move || PlaySign(colors.chosen, emblem * 0.34));
            let (text, status) = (text.clone(), status.clone());
            let band = (height * 0.7).min(BAND);
            SpatialRibbon(Modifier::empty().weight(1.0).height(band).padding_symmetric(18.0, 0.0), move || {
                let (text, status) = (text.clone(), status.clone());
                Row(Modifier::empty().fill_max_size(), RowSpec::default().vertical_alignment(VerticalAlignment::CenterVertically), move || {
                    let (text, status) = (text.clone(), status.clone());
                    Column(Modifier::empty().weight(1.0), ColumnSpec::default(), move || {
                        Label(words, text.clone(), text_style((height * 0.36).clamp(24.0, 32.0), TEXT_COLOR, Some(FontWeight::BOLD)));
                        if !status.is_empty() {
                            Label(words, status.clone(), text_style(17.0, MUTED_COLOR, None));
                        }
                    });
                    SpatialParticleWave(Modifier::empty().width((height * WAVE_PER_HEIGHT).min(WAVE[0])).height(band.min(WAVE[1])));
                });
            });
        },
    );
}

#[composable]
fn HologramRow(row: Row, page: MutableState<Shown>, (pending, notice): (MutableState<Option<Pending>>, Option<String>), (colors, words): (HologramColors, Words)) {
    let height = row.height;
    match row.line {
        Line::Header { text, status } | Line::Title { text, status } => Header(text, status, (height, EMBLEM), (colors, words)),
        // The name of a choice stands at the bottom of its row, over its objects, clear of the row above
        Line::Label(text) => {
            Box(Modifier::empty().fill_max_width().height(height), BoxSpec::default().content_alignment(Alignment::BOTTOM_START), move || {
                Label(words, text.clone(), text_style(23.0, TEXT_COLOR, None));
            });
        }
        Line::Heading(text) => {
            Box(Modifier::empty().height(height), BoxSpec::default().content_alignment(Alignment::BOTTOM_START), move || {
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
                            let size = if shape == ChoiceShape::Stepper { 20.0 } else { 18.0 };
                            Label(words, key.label.clone(), text_style(size, color, chosen.then_some(FontWeight::BOLD)));
                        }
                    },
                );
            }
            Form::Switch if keys.len() == 2 => {
                // A toggle stands on a band of glass: its name, and its switch at the band's end
                SpatialRibbon(Modifier::empty().fill_max_width().height(height), move || {
                    let (name, keys, notice) = (name.clone(), keys.clone(), notice.clone());
                    Row(
                        Modifier::empty().fill_max_size(),
                        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)).vertical_alignment(VerticalAlignment::CenterVertically),
                        move || {
                            let width = HologramName(&name, words);
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
                });
            }
            Form::Steps => {
                Row(Modifier::empty().fill_max_width().height(height), RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)), move || {
                    HologramName(&name, words);
                    let labels = keys.iter().map(|key| key.label.clone()).collect();
                    let selected = keys.iter().position(|key| key.look == Look::Selected);
                    let (keys, notice) = (keys.clone(), notice.clone());
                    Steps3d(labels, selected, Modifier::empty().weight(1.0), move |index| {
                        if let Some(key) = keys.get(index) {
                            run(&key.press, page, pending, notice.clone());
                        }
                    });
                });
            }
            _ => {
                Row(
                    Modifier::empty().fill_max_width().height(height),
                    RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)).vertical_alignment(VerticalAlignment::CenterVertically),
                    move || {
                        let width = HologramName(&name, words);
                        for key in keys.clone() {
                            GlassKey(key, width, colors, page, (pending, notice.clone()));
                        }
                    },
                );
            }
        },
    }
}

/// The name at the start of a row of keys, vertically in the middle of the row: a setting's name, or
/// a title with its detail. It gives the width of each key after a title, which stand at the row's
/// end, as `page_model.rs` lays them out.
#[composable]
fn HologramName(name: &Name, words: Words) -> Option<f32> {
    match name {
        Name::None => None,
        Name::Setting(label) => {
            let label = label.clone();
            Box(Modifier::empty().width(LABEL).fill_max_height(), BoxSpec::default().content_alignment(Alignment::CENTER_START), move || {
                Label(words, label.clone(), text_style(22.0, TEXT_COLOR, None));
            });
            None
        }
        Name::Title { title, detail, width } => {
            let (title, detail) = (title.clone(), detail.clone());
            Column(Modifier::empty().weight(1.0), ColumnSpec::default().vertical_arrangement(LinearArrangement::Center), move || {
                if !title.is_empty() {
                    Label(words, title.clone(), text_style(21.0, TEXT_COLOR, None));
                }
                if !detail.is_empty() {
                    Label(words, detail.clone(), text_style(17.0, MUTED_COLOR, None));
                }
            });
            Some(*width)
        }
    }
}

/// A glowing triangle that points right, the sign of the stream, `side` dp high.
#[composable]
fn PlaySign(color: Color, side: f32) {
    Canvas(Modifier::empty().width(side * 0.92).height(side), move |scope| {
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

/// The glass of a key of `look`.
pub fn key_color(look: Look, colors: HologramColors) -> Color {
    match look {
        Look::Normal => colors.glass,
        Look::Selected | Look::Primary => colors.chosen,
        Look::Danger => KEY_DANGER,
    }
}

/// A pill of glass with its words, `width` wide or a share of the row.
#[composable]
fn GlassKey(key: Key, width: Option<f32>, colors: HologramColors, page: MutableState<Shown>, (pending, notice): (MutableState<Option<Pending>>, Option<String>)) {
    let modifier = match width {
        Some(width) => Modifier::empty().width(width),
        None => Modifier::empty().weight(1.0),
    }
    .height(size::BUTTON);
    let press = key.press.clone();
    let label = key.label.clone();
    SpatialButton(
        move || run(&press, page, pending, notice.clone()),
        modifier,
        true,
        ButtonShape::Key,
        key_color(key.look, colors),
        move || {
            Text(label.clone(), Modifier::empty(), text_style(19.0, TEXT_COLOR, Some(FontWeight::BOLD)));
        },
    );
}
