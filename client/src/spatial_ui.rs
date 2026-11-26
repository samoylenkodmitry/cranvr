//! The parts of the 3D interface (#111) that the holograms (`hologram_ui.rs`) share: the glass of a
//! key that stops or removes something, and a choice of numbers as a `SpatialSlider` with a stop
//! for each value and the values' words under it. The rows are those of the flat form
//! (`page_model.rs`), with the same sizes and places, so a scripted aim at a key of the flat form
//! presses the same control in 3D.

use crate::ui::{text_style, MUTED_COLOR, TEXT_COLOR};
use cranpose::prelude::*;
use cranpose_xr::{SliderColors, SpatialSlider};

/// The glass of a key that stops or removes something
pub const KEY_DANGER: Color = Color(0.78, 0.22, 0.25, 1.0);
/// The light of a switch that is on, and the fill and the knob of a slider
const LIGHT_ON: Color = Color(0.35, 0.95, 1.0, 1.0);
const KNOB: Color = Color(0.82, 0.86, 0.96, 1.0);
/// The height of the line of values under a slider, in dp
pub const STEP_LABELS: f32 = 18.0;
/// The width of a slider's knob in Cranpose, in dp: the stops lie half of it in from each end
const SLIDER_KNOB: f32 = 32.0;

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
