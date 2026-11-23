//! The 3D form of the app's menus (#111): the looks and the controls that the hand menu and the
//! console share when the setting Interface is 3D.
//!
//! Each menu is a Cranpose `SpatialTray`: a panel that carries the words and takes the input, on a
//! base of 3D geometry with light and soft shadows. Its keys are `SpatialButton`s that sink while a
//! ray presses them, its toggles `SpatialSwitch` rockers that flip, and a choice of numbers a
//! `SpatialSlider` with a stop for each value. The rows are those of the flat form
//! (`menu_model.rs`, `page_model.rs`), with the same sizes and places, so a scripted aim at a key
//! of the flat form presses the same control in 3D.

use crate::ui::{text_style, MUTED_COLOR, TEXT_COLOR};
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{ButtonShape, SliderColors, SpatialButton, SpatialSlider, SpatialSwitch, SwitchColors};

/// The base of a tray: a deep blue that keeps its color in the light
pub const TRAY_COLOR: Color = Color(0.07, 0.1, 0.28, 1.0);
/// A key's face, a key that is on, a key of the main action and a key that stops or removes
pub const KEY_FACE: Color = Color(0.16, 0.25, 0.55, 1.0);
pub const KEY_ON: Color = Color(0.2, 0.5, 1.0, 1.0);
pub const KEY_PRIMARY: Color = Color(0.12, 0.55, 0.34, 1.0);
pub const KEY_DANGER: Color = Color(0.78, 0.22, 0.25, 1.0);
/// The light of a switch that is on, and the fill and the knob of a slider
const LIGHT_ON: Color = Color(0.35, 0.95, 1.0, 1.0);
const KNOB: Color = Color(0.82, 0.86, 0.96, 1.0);
/// The height of the line of values under a slider, in dp
pub const STEP_LABELS: f32 = 18.0;
/// The width of a slider's knob in Cranpose, in dp: the stops lie half of it in from each end
const SLIDER_KNOB: f32 = 32.0;

/// A key of a menu: a 3D key cap of `color` with its words on it.
#[composable]
pub fn Key3d(label: String, color: Color, modifier: Modifier, on_click: impl Fn() + 'static) {
    SpatialButton(on_click, modifier, true, ButtonShape::Key, color, move || {
        Text(label.clone(), Modifier::empty(), text_style(20.0, TEXT_COLOR, Some(FontWeight::BOLD)));
    });
}

/// A ball of the strip: a 3D ball of `color` that opens the menu or shows the hand's display.
#[composable]
pub fn Ball3d(color: Color, modifier: Modifier, on_click: impl Fn() + 'static) {
    SpatialButton(on_click, modifier, true, ButtonShape::Ball, color, || {});
}

/// A toggle: a rocker that flips at a press anywhere on it. Its start half reads "Off" and its end
/// half "On". While on, the end half lies deeper, its face is the color of a key that is on and its
/// light shines.
#[composable]
pub fn Rocker3d(checked: bool, modifier: Modifier, on_change: impl Fn(bool) + 'static) {
    let colors = SwitchColors { face: if checked { KEY_ON } else { KEY_FACE }, checked: LIGHT_ON };
    let on_change = std::rc::Rc::new(on_change);
    Box(modifier, BoxSpec::default(), move || {
        let on_change = on_change.clone();
        SpatialSwitch(checked, move |on| on_change(on), Modifier::empty().fill_max_size(), true, colors);
        Row(Modifier::empty().fill_max_size(), RowSpec::default(), || {
            for words in ["Off", "On"] {
                Box(Modifier::empty().weight(1.0).fill_max_height(), BoxSpec::default().content_alignment(Alignment::CENTER), move || {
                    Text(words, Modifier::empty(), text_style(18.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                });
            }
        });
    });
}

/// A choice of values in order, such as numbers: a slider with a stop for each value and the
/// values' words under the stops. A drag shows the value that the knob takes and picks it at the
/// release, so a setting that restarts the host changes once. `selected` is the value that holds
/// now.
#[composable]
pub fn Steps3d(labels: Vec<String>, selected: Option<usize>, modifier: Modifier, on_pick: impl Fn(usize) + 'static) {
    let count = labels.len().max(2);
    let last = (count - 1) as f32;
    let held = selected.map_or(0.0, |index| index as f32 / last);
    // The value that a drag shows until the Mac's menu says the new one
    let dragged = rememberMutableStateOf(|| None::<f32>);
    LaunchedEffect(selected, async move |_| dragged.set(None));
    let value = dragged.get().unwrap_or(held);
    let at = (value * last).round() as usize;
    let on_pick = std::rc::Rc::new(on_pick);
    Column(modifier, ColumnSpec::default(), move || {
        let on_pick = on_pick.clone();
        SpatialSlider(
            value,
            move |value| dragged.set(Some(value)),
            Modifier::empty().fill_max_width().height(crate::console::size::BUTTON),
            true,
            (count - 2) as u32,
            move || {
                if let Some(value) = dragged.get_non_reactive() {
                    on_pick((value * last).round() as usize);
                }
            },
            SliderColors { thumb: KNOB, active_track: LIGHT_ON },
        );
        let labels = labels.clone();
        BoxWithConstraints(Modifier::empty().fill_max_width().height(STEP_LABELS), move |scope| {
            let width = scope.constraints().max_width;
            let track = (width - SLIDER_KNOB).max(1.0);
            for (index, label) in labels.iter().enumerate() {
                let x = SLIDER_KNOB / 2.0 + track * index as f32 / last;
                let color = if index == at { TEXT_COLOR } else { MUTED_COLOR };
                Box(
                    Modifier::empty().offset(x - 45.0, 0.0).width(90.0).fill_max_height(),
                    BoxSpec::default().content_alignment(Alignment::CENTER),
                    {
                        let label = label.clone();
                        move || {
                            Text(label.clone(), Modifier::empty(), text_style(15.0, color, None));
                        }
                    },
                );
            }
        });
    });
}
