//! The game's loading screen and its message to the user (`HostMsg::GameScreen`): plain text on a
//! panel in front of the user.
//!
//! A game that speaks OpenVR tells SteamVR what to show between its scenes, and SteamVR draws it. The
//! host makes a screen of words from the game's messages (host/src/game_screen.rs). Without it the user
//! sees black while a game loads, and does not learn that the game waits for a press.
//!
//! The panel stands 1.5 m from the user at the height of the head, in front of where the head looked
//! when the screen showed. A game turns its screen by its direction (`facing_deg`) from the game's
//! forward direction, which the runtime puts where the head looked when the game's session started
//! (BasaltVR's LOCAL origin). The client does not know that moment, so it turns the panel by the angle
//! from where the head looks when the screen shows: the two agree while the user faces as at the
//! game's start. Half-Life: Alyx sends 0. The panel keeps its place while one direction shows, so a
//! new message does not move it. It goes when the game ends its load or the link ends. Nothing is
//! composed while no screen shows.

use crate::{
    panel_layout::layer_rank,
    ui::{text_style, MUTED_COLOR, PANEL_COLOR, TEXT_COLOR, WARN_COLOR},
};
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{FollowMode, FollowTarget, FollowingSubspace, SpatialPanel, SubspaceModifier, TrackedDimensions};
use cranvr_proto::message::GameScreen;

/// The panel's width and height in dp, a millimeter each
const SIZE: [f32; 2] = [1200.0, 560.0];
/// How far the panel stands from the head, in meters
const DISTANCE: f32 = 1.5;
/// How far the head stands behind the origin of a subspace that follows the view, in meters
const HEAD_BEHIND_ORIGIN: f32 = 1.0;

/// The panel's middle in dp from the origin of a subspace that follows the view, and its turn about
/// the up axis in degrees, toward the left, for the game's direction `facing_deg`.
pub fn place(facing_deg: Option<f32>) -> ([f32; 3], f32) {
    let turn_deg = facing_deg.unwrap_or(0.0);
    // A turn toward the left about the up axis moves the forward direction (-z) toward -x
    let turn = turn_deg.to_radians();
    ([-DISTANCE * turn.sin() * 1000.0, 0.0, (HEAD_BEHIND_ORIGIN - DISTANCE * turn.cos()) * 1000.0], turn_deg)
}

/// The game's screen while one shows, in front of the head.
#[composable]
pub fn GameScreenPanel(screen: MutableState<Option<GameScreen>>) {
    let Some(shown) = screen.get() else { return };
    let facing = shown.facing_deg;
    key(facing.map(f32::to_bits), move || {
        let (offset, turn_deg) = place(facing);
        FollowingSubspace(FollowTarget::View(FollowMode::Snap(TrackedDimensions::TranslationAndYaw)), move || {
            let modifier = SubspaceModifier::new().size(SIZE[0], SIZE[1]).offset(offset[0], offset[1], offset[2]).rotate(0.0, turn_deg, 0.0);
            SpatialPanel(modifier.layer_priority(layer_rank::TEXT), move || {
                ScreenText(screen, Modifier::empty().fill_max_size().background(PANEL_COLOR).rounded_corners(32.0).padding(48.0));
            });
        });
    });
}

/// The words of the screen: the load's words, its lines and the message. `modifier` gives the size
/// and the look. The phone shows the same words over its picture.
#[composable]
pub fn ScreenText(screen: MutableState<Option<GameScreen>>, modifier: Modifier) {
    let Some(shown) = screen.get() else { return };
    Column(modifier, ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(20.0)), move || {
        if let Some(loading) = shown.loading.clone() {
            Text(loading, Modifier::empty().fill_max_width(), text_style(64.0, TEXT_COLOR, Some(FontWeight::BOLD)));
        }
        for line in shown.lines.clone() {
            Text(line, Modifier::empty().fill_max_width(), text_style(34.0, MUTED_COLOR, None));
        }
        if let Some(message) = shown.message.clone() {
            Text(message, Modifier::empty().fill_max_width(), text_style(48.0, WARN_COLOR, Some(FontWeight::BOLD)));
        }
    });
}

#[cfg(test)]
#[path = "tests/game_screen.rs"]
mod tests;
