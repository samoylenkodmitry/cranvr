//! The game's loading screen and its message to the user (`HostMsg::GameScreen`): plain text on a
//! panel in front of the user.
//!
//! A game that speaks OpenVR tells SteamVR what to show between its scenes, and SteamVR draws it. The
//! host makes a screen of words from the game's messages (host/src/game_screen.rs). Without it the user
//! sees black while a game loads, and does not learn that the game waits for a press.
//!
//! The panel stands 1.5 m from the user at the height of the head. A screen with a direction stands
//! in the game's room at that direction, as SteamVR puts the game's own screen: the game's middle is
//! the room's origin, and its forward direction is the room's. A screen without one stands in front
//! of the head where it looked when the screen showed. The panel goes when the game ends its load or
//! the link ends. Nothing is composed while no screen shows.

use crate::{
    hub::hub,
    panel_layout::layer_rank,
    ui::{text_style, MUTED_COLOR, PANEL_COLOR, TEXT_COLOR, WARN_COLOR},
};
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{FollowMode, FollowTarget, FollowingSubspace, SpatialPanel, SubspaceModifier, TrackedDimensions};
use cranvr_proto::message::GameScreen;

/// The panel's width and height in dp, a millimeter each
const SIZE: [f32; 2] = [1200.0, 560.0];
/// How far the panel stands from the user, in meters
const DISTANCE: f32 = 1.5;
/// The origin of the room's subspace in the room (Cranpose's `Subspace`): a meter ahead of where the
/// head was when the app started. The game's middle is the room's origin.
const ROOM_SUBSPACE_Z: f32 = -1.0;
/// The origin of a subspace that follows the view: a meter ahead of the head
const VIEW_SUBSPACE_Z: f32 = -1.0;

/// Where the panel stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place {
    /// In the room: the panel's middle in dp from the origin of the room's subspace, and its turn
    /// about the up axis in degrees, toward the left
    Room { offset: [f32; 3], turn_deg: f32 },
    /// In front of the head, at this offset in dp from the origin of a subspace that follows the view
    View { offset: [f32; 3] },
}

/// Where the screen stands for a direction of the game (`facing_deg`) and the height of the head in
/// the room in meters, when the headset tracks it.
pub fn place(facing_deg: Option<f32>, head_height: Option<f32>) -> Place {
    let Some(turn_deg) = facing_deg else {
        return Place::View { offset: [0.0, 0.0, (-DISTANCE - VIEW_SUBSPACE_Z) * 1000.0] };
    };
    // A turn toward the left about the up axis moves the forward direction (-z) toward -x
    let turn = turn_deg.to_radians();
    let at = [-DISTANCE * turn.sin(), head_height.unwrap_or(0.0), -DISTANCE * turn.cos()];
    Place::Room { offset: [at[0] * 1000.0, at[1] * 1000.0, (at[2] - ROOM_SUBSPACE_Z) * 1000.0], turn_deg }
}

/// The game's screen while one shows, in the room or in front of the head.
#[composable]
pub fn GameScreenPanel(screen: MutableState<Option<GameScreen>>) {
    let Some(shown) = screen.get() else { return };
    // The place stays while one direction shows: a new message does not move the panel
    let facing = shown.facing_deg;
    key(facing.map(f32::to_bits), move || ScreenPanel(screen, facing));
}

#[composable]
fn ScreenPanel(screen: MutableState<Option<GameScreen>>, facing: Option<f32>) {
    let place = remember(|| place(facing, hub().feed.head().map(|head| head.pose.position[1]))).with(|place| *place);
    let panel = move |modifier: SubspaceModifier| {
        SpatialPanel(modifier.size(SIZE[0], SIZE[1]).layer_priority(layer_rank::TEXT), move || {
            ScreenText(screen, Modifier::empty().fill_max_size().background(PANEL_COLOR).rounded_corners(32.0).padding(48.0));
        });
    };
    match place {
        Place::Room { offset, turn_deg } => panel(SubspaceModifier::new().offset(offset[0], offset[1], offset[2]).rotate(0.0, turn_deg, 0.0)),
        Place::View { offset } => FollowingSubspace(FollowTarget::View(FollowMode::Snap(TrackedDimensions::TranslationAndYaw)), move || {
            panel(SubspaceModifier::new().offset(offset[0], offset[1], offset[2]));
        }),
    }
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
