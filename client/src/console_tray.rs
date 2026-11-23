//! The console in 3D (#111): the rows of a page on a tray, with 3D keys, switches and sliders.
//!
//! The rows are those of the flat console (`page_model.rs`), with the same heights and places, so
//! a scripted aim at a key of the flat form presses the same control here. A toggle's two keys
//! are one switch over both. A choice of numbers in one row is a slider over its keys, with a stop
//! under each key's place. The other keys are 3D keys. The words of titles, headings and lines
//! stay on the tray's panel, at the compositor's full resolution.

use crate::{
    console::{size, Pending, Shown},
    console_panel::{bold, run, tone_color, RowName},
    page_model::{Form, Key, Line, Look, Row},
    spatial_ui::{Key3d, Rocker3d, Steps3d, KEY_DANGER, KEY_FACE, KEY_ON, KEY_PRIMARY, TRAY_COLOR},
    ui::{text_style, MUTED_COLOR},
};
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{SpatialTray, SubspaceModifier};

/// The console's tray, at the place and size that `modifier` gives.
#[composable]
pub fn ConsoleTray(modifier: SubspaceModifier, rows: Vec<Row>, page: MutableState<Shown>, (pending, notice): (MutableState<Option<Pending>>, Option<String>)) {
    SpatialTray(modifier, true, Some(TRAY_COLOR), move || {
        // The pointer over the console holds the pointing hand back from the game (see `hold.rs`)
        let hovered = crate::hover::hover_modifier(crate::hover::CONSOLE);
        let rows = rows.clone();
        let notice = notice.clone();
        Column(
            Modifier::empty().fill_max_size().then(hovered).padding(size::PADDING),
            ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(size::GAP)),
            move || {
                for row in rows.clone() {
                    RowView3d(row, page, pending, notice.clone());
                }
            },
        );
    });
}

fn color_of(look: Look) -> Color {
    match look {
        Look::Normal => KEY_FACE,
        Look::Selected => KEY_ON,
        Look::Primary => KEY_PRIMARY,
        Look::Danger => KEY_DANGER,
    }
}

#[composable]
fn RowView3d(row: Row, page: MutableState<Shown>, pending: MutableState<Option<Pending>>, notice: Option<String>) {
    let height = row.height;
    match row.line {
        Line::Title { text, status } => {
            Row(Modifier::empty().fill_max_width().height(height), RowSpec::default(), move || {
                Text(text.clone(), Modifier::empty().weight(1.0), bold(34.0));
                if !status.is_empty() {
                    Text(status.clone(), Modifier::empty(), text_style(22.0, MUTED_COLOR, None));
                }
            });
        }
        Line::Heading(text) => {
            Text(text, Modifier::empty().height(height), text_style(26.0, MUTED_COLOR, Some(FontWeight::BOLD)));
        }
        Line::Text { text, tone } => {
            Text(text, Modifier::empty().fill_max_width().height(height), text_style(22.0, tone_color(tone), None));
        }
        Line::Keys { name, keys, form } => {
            Row(
                Modifier::empty().fill_max_width().height(height),
                RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
                move || {
                    let width = RowName(&name);
                    let keys = keys.clone();
                    let notice = notice.clone();
                    match form {
                        Form::Switch if keys.len() == 2 => {
                            // The keys On and Off become one switch as wide as both
                            let span = width.map_or(Modifier::empty().weight(1.0), |width| Modifier::empty().width(2.0 * width + size::GAP));
                            let checked = keys[0].look == Look::Selected;
                            Rocker3d(checked, span.height(size::BUTTON), move |on| {
                                let key = &keys[if on { 0 } else { 1 }];
                                run(&key.press, page, pending, notice.clone());
                            });
                        }
                        Form::Steps => {
                            let labels = keys.iter().map(|key| key.label.clone()).collect();
                            let selected = keys.iter().position(|key| key.look == Look::Selected);
                            Steps3d(labels, selected, Modifier::empty().weight(1.0), move |index| {
                                if let Some(key) = keys.get(index) {
                                    run(&key.press, page, pending, notice.clone());
                                }
                            });
                        }
                        _ => {
                            for key in keys {
                                KeyView3d(key, width, page, (pending, notice.clone()));
                            }
                        }
                    }
                },
            );
        }
    }
}

/// A key of a row: its width is `width`, or a share of the row.
#[composable]
fn KeyView3d(key: Key, width: Option<f32>, page: MutableState<Shown>, (pending, notice): (MutableState<Option<Pending>>, Option<String>)) {
    let modifier = match width {
        Some(width) => Modifier::empty().width(width),
        None => Modifier::empty().weight(1.0),
    }
    .height(size::BUTTON);
    let press = key.press.clone();
    Key3d(key.label.clone(), color_of(key.look), modifier, move || run(&press, page, pending, notice.clone()));
}
