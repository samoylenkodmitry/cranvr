//! The ball above the left controller. A press on it with the right controller's ray hides the
//! connect and stats panels or shows them again, also while a game runs under them.
//!
//! The ball rides on the left controller in a `FollowingSubspace` of its own, so it stays at hand
//! wherever the user stands. The left controller's own ray passes through it. The ball stays while
//! the panels are hidden, so it can bring them back. The choice is kept for the next start.

use crate::{
    hub::hub,
    settings,
    ui::{ACCENT_COLOR, KEY_COLOR, TEXT_COLOR},
};
use cranpose::prelude::*;
use cranpose_xr::{local_spatial_capabilities, FollowMode, FollowTarget, FollowingSubspace, Hand, SpatialPanel, SubspaceModifier, Vibration};
use std::time::Duration;

/// The ball's diameter in dp, a millimeter each
const BALL: f32 = 50.0;
/// The ball's center above the controller's grip, in dp
const LIFT: f32 = 110.0;
/// The width of the ball's ring, in dp
const RING: f32 = 5.0;

/// The ball, in a headset. `shown` says whether the connect and stats panels show.
#[composable]
pub fn PanelsBall(shown: MutableState<bool>) {
    if !local_spatial_capabilities().current().is_spatial_ui_enabled() {
        return;
    }
    FollowingSubspace(FollowTarget::Controller(Hand::Left, FollowMode::Tight), move || {
        SpatialPanel(SubspaceModifier::new().size(BALL, BALL).offset(0.0, LIFT, 0.0), move || {
            // A ring around the ball keeps it visible against a bright game
            let fill = if shown.get() { ACCENT_COLOR } else { KEY_COLOR };
            Box(
                Modifier::empty().fill_max_size().background(TEXT_COLOR).rounded_corners(BALL / 2.0).padding(RING),
                BoxSpec::default(),
                move || {
                    Box(
                        Modifier::empty().fill_max_size().background(fill).rounded_corners(BALL / 2.0 - RING).clickable(move |_| toggle(shown)),
                        BoxSpec::default(),
                        || {},
                    );
                },
            );
        });
    });
}

/// Hides or shows the panels. A click on the right hand tells the user that the press took.
fn toggle(shown: MutableState<bool>) {
    let now = !shown.get();
    shown.set(now);
    settings::remember_panels_shown(now);
    if let Some(haptics) = hub().headset().and_then(|headset| headset.haptics) {
        haptics.vibrate(Hand::Right, Vibration::new(0.3, Duration::from_millis(30)));
    }
}
