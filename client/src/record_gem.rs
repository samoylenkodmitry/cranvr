//! The gem on the left hand that records what the headset shows (the Mac's recording, see
//! app/src/recorder.rs): a 3D rhombus of glowing glass, an octahedron 25 mm across, a few
//! centimeters above the back of the left controller or of the left hand. It turns slowly
//! (Cranpose's `SpatialGem`).
//!
//! Its color says what a press does:
//! - the theme's glass (blue in the cyan theme, violet in the amber one) while nothing points at it;
//! - green while the ray or the hand's pointer rests on it: a press starts a recording;
//! - red with a slow pulse while a recording runs, pointed at or not: a press stops it;
//! - the theme's glass, dim, while no recording can start, such as with no USB cable. A press then
//!   shows the reason beside the gem for 3 s.
//!
//! A line beside the gem also shows the Mac's notes for 3 s, such as "Saved: NAME", and "Starting..."
//! or "Stopping..." after a press until the Mac's state changes. The gem shows in games too, as the
//! ball does, and hides while neither the left controller nor the left hand is tracked. A hidden gem
//! composes nothing. The hand menu's row "Record what I see" and the Mac window's Record button
//! share the state.

use crate::{
    console::Theme,
    hand_menu::{self, HandState},
    hologram_ui, hover,
    hub::hub,
    panel_layout::layer_rank,
    ui::{text_style, PANEL_COLOR, TEXT_COLOR},
};
use cranpose::launch_args;
use cranpose::prelude::*;
use cranpose_xr::{local_spatial_capabilities, FollowMode, FollowTarget, FollowingSubspace, GemGlow, Hand, HandJointType, HologramStage, SpatialGem, SpatialHologram, SpatialPanel, SubspaceModifier, TrackedDimensions};
use cranvr_proto::message::Recording;
use std::time::Duration;

/// The side of the gem's place in dp, a millimeter each: the gem is 86% of it, 25 mm across
pub const GEM: f32 = 30.0;
/// The gem's middle in the left controller's frame (its grip, turned as its aim): 5.5 cm out to the
/// left of the hand, 5 cm above the grip and 1.5 cm toward the face, clear of the ball 11 cm above
/// the grip
pub const ON_CONTROLLER: [f32; 3] = [-55.0, 50.0, 15.0];
/// The gem's middle above the back of the left hand, in dp along the palm joint's +y
const ABOVE_HAND: f32 = 45.0;
/// How long a line beside the gem shows
const NOTE_TIME: Duration = Duration::from_secs(3);
/// How often the gem looks at the Mac's state
const POLL: Duration = Duration::from_millis(250);
/// The line beside the gem, in dp
const NOTE_SIZE: [f32; 2] = [300.0, 34.0];

const GREEN: Color = Color(0.25, 1.0, 0.42, 1.0);
const RED: Color = Color(1.0, 0.16, 0.12, 1.0);
/// The share of the theme's light of a gem that cannot record now
const DIM: f32 = 0.35;

/// What the gem rides on
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Anchor {
    Controller,
    Hand,
    /// In front of the head, where a left hand holds the ball, for a headset on a desk
    /// (`hand_anchor=view`)
    View,
}

/// What the gem shows now: its color and whether its glow pulses
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub color: Color,
    pub glow: GemGlow,
}

/// The gem's look for the Mac's state, the hover and the theme's glass.
pub fn look(recording: Option<&Recording>, hovered: bool, glass: Color) -> Look {
    match recording {
        Some(state) if state.running => Look { color: RED, glow: GemGlow::Pulse },
        Some(state) if state.blocked.is_none() && hovered => Look { color: GREEN, glow: GemGlow::Steady },
        Some(state) if state.blocked.is_none() => Look { color: glass, glow: GemGlow::Steady },
        _ => Look { color: Color(glass.0 * DIM, glass.1 * DIM, glass.2 * DIM, 1.0), glow: GemGlow::Steady },
    }
}

/// Why a press cannot start or stop a recording now, or `None` while it can.
pub fn blocked(recording: Option<&Recording>) -> Option<String> {
    match recording {
        None => Some("Connect to the Mac to record".into()),
        Some(state) if state.running => None,
        Some(state) => state.blocked.clone(),
    }
}

/// The gem and its line, on the left controller or the left hand while one is tracked.
#[composable]
pub fn RecordGem(state: HandState, theme: Theme) {
    if !local_spatial_capabilities().current().is_spatial_ui_enabled() {
        return;
    }
    // A launch may leave the gem out, for a measure of what it costs
    if launch_args().boolean("record_gem") == Some(false) {
        return;
    }
    let anchor = if launch_args().string("hand_anchor") == Some("view") {
        Some(Anchor::View)
    } else if state.present.get()[0] {
        Some(Anchor::Controller)
    } else if state.left_hand.get() {
        Some(Anchor::Hand)
    } else {
        None
    };
    let Some(anchor) = anchor else { return };
    let recording = rememberMutableStateOf(|| hub().recording());
    LaunchedEffect((), async move |_| {
        let mut seen = hub().recording_changes();
        loop {
            delay(POLL).await;
            let now = hub().recording_changes();
            if now != seen {
                seen = now;
                recording.set(hub().recording());
            }
        }
    });
    // The line beside the gem: its words and a count that names each new line
    let note = rememberMutableStateOf(|| None::<(String, u32)>);
    // A new note of the Mac shows. The note that was there when the gem came into view does not.
    let mac_note = recording.get().filter(|state| state.note.is_some()).map(|state| state.note_id);
    let first_note = remember(|| mac_note).with(|first| *first);
    LaunchedEffect(mac_note, async move |_| {
        if mac_note != first_note {
            if let Some(text) = recording.get_non_reactive().and_then(|state| state.note) {
                show(note, text);
            }
        }
    });
    let running = recording.get().is_some_and(|state| state.running);
    // A note of a press ("Starting...") ends when the Mac's state changes
    LaunchedEffect(running, async move |_| {
        if note.get_non_reactive().is_some_and(|(text, _)| text.ends_with("...")) {
            note.set(None);
        }
    });
    let shown = note.get();
    LaunchedEffect(shown.as_ref().map(|(_, id)| *id), async move |_| {
        if let Some((_, id)) = note.get_non_reactive() {
            delay(NOTE_TIME).await;
            if note.get_non_reactive().is_some_and(|(_, now)| now == id) {
                note.set(None);
            }
        }
    });
    let glass = hologram_ui::colors(theme).glass;
    key(anchor, move || {
        let (target, at, turn) = place(anchor);
        FollowingSubspace(target, move || {
            let modifier = SubspaceModifier::new().size(GEM, GEM).offset(at[0], at[1], at[2]).rotate(turn, 0.0, 0.0).layer_priority(layer_rank::HAND_MENU);
            SpatialHologram(modifier, true, hologram_ui::colors(theme), HologramStage::NONE, move || {
                let source = rememberMutableInteractionSource();
                let hovered = source.collectIsHoveredAsState().get();
                let current = recording.get();
                let look = look(current.as_ref(), hovered, glass);
                Box(Modifier::empty().fill_max_size().then(hover::hover_modifier(hover::GEM)), BoxSpec::default(), move || {
                    SpatialGem(move || press(recording, note), Modifier::empty().fill_max_size(), true, look.color, look.glow, source);
                });
            });
            if let Some((text, _)) = note.get() {
                // The line stands to the left of the gem, away from the ball and the menu
                let x = at[0] - GEM / 2.0 - 8.0 - NOTE_SIZE[0] / 2.0;
                let modifier = SubspaceModifier::new().size(NOTE_SIZE[0], NOTE_SIZE[1]).offset(x, at[1], at[2]).rotate(turn, 0.0, 0.0).layer_priority(layer_rank::HAND_MENU);
                SpatialPanel(modifier, move || {
                    let text = text.clone();
                    Box(Modifier::empty().fill_max_size().background(PANEL_COLOR).rounded_corners(NOTE_SIZE[1] / 2.0).padding_symmetric(12.0, 5.0), BoxSpec::default().content_alignment(Alignment::CENTER_END), move || {
                        Text(text.clone(), Modifier::empty(), text_style(17.0, TEXT_COLOR, None));
                    });
                });
            }
        });
    });
}

/// The subspace that the gem rides in, its middle there in dp, and its turn about x in degrees.
fn place(anchor: Anchor) -> (FollowTarget, [f32; 3], f32) {
    match anchor {
        Anchor::Controller => (FollowTarget::Controller(Hand::Left, FollowMode::Tight(TrackedDimensions::All)), ON_CONTROLLER, 0.0),
        // A hand joint's +y points out of the back of the hand: a turn of -90 degrees about x makes
        // the gem's panel face along it, toward the eyes that look at the back of the hand
        Anchor::Hand => (FollowTarget::Hand(Hand::Left, HandJointType::Palm, FollowMode::Tight(TrackedDimensions::All)), [0.0, ABOVE_HAND, 0.0], -90.0),
        Anchor::View => {
            let base = hand_menu::ON_VIEW.base;
            (FollowTarget::View(FollowMode::Snap(TrackedDimensions::TranslationAndYaw)), [base[0] + ON_CONTROLLER[0], base[1] + ON_CONTROLLER[1], base[2] + ON_CONTROLLER[2]], 0.0)
        }
    }
}

/// Shows `text` beside the gem for 3 s.
fn show(note: MutableState<Option<(String, u32)>>, text: String) {
    let id = note.get_non_reactive().map_or(0, |(_, id)| id.wrapping_add(1));
    note.set(Some((text, id)));
}

/// A press on the gem: the reason why nothing can happen now, or the request to the Mac.
fn press(recording: MutableState<Option<Recording>>, note: MutableState<Option<(String, u32)>>) {
    let current = recording.get_non_reactive();
    match blocked(current.as_ref()) {
        Some(why) => {
            log::info!("the gem cannot record: {why}");
            show(note, why);
        }
        None => {
            let running = current.is_some_and(|state| state.running);
            log::info!("the gem {} the recording", if running { "stops" } else { "starts" });
            show(note, if running { "Stopping..." } else { "Starting..." }.into());
            hub().toggle_recording();
        }
    }
}

#[cfg(test)]
#[path = "tests/record_gem.rs"]
mod tests;
