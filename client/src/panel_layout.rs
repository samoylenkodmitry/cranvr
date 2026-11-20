//! Which panels show, and where a display stands.
//!
//! The hand menu lists every panel of the app: the console, the connection panel, the stream's
//! numbers and each display of the Mac. Each has a choice to show or hide it. The choice differs
//! between the room and a game:
//! - Without a game the user's choice is saved, so the next start shows the same panels. The first
//!   start shows the connection panel, the stream's numbers and the first display.
//! - While a game runs no panel shows until the user picks one, and the pick ends with the game.
//!   A panel that is not shown costs nothing: a display that is not shown has no stream (the host
//!   runs no capture and no encoder for it), no decoder and no surface in the headset.
//!
//! One display can ride on the left hand instead of standing in the room (see `hand_menu.rs`). The
//! choice to show it is the same as for any display, and it is saved.
//!
//! The console's own state is the hub's (see `console.rs`): its opening follows the host's menu and
//! the hold of the left menu button. This file only names it, so the menu can list it.

use cranpose::preferences;
use cranvr_proto::message::DESKTOP_STREAM;
use std::collections::BTreeSet;

/// A panel that the menu lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PanelId {
    Console,
    Connection,
    Stats,
    /// The display that carries this stream
    Display(u8),
}

/// What the app shows now: the saved choice of the room, or the choice of the game that runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    room: BTreeSet<PanelId>,
    game: BTreeSet<PanelId>,
    in_game: bool,
    /// The display that rides on the left hand
    pinned: Option<u8>,
}

impl Default for Layout {
    fn default() -> Self {
        Self { room: [PanelId::Connection, PanelId::Stats, PanelId::Display(DESKTOP_STREAM)].into(), game: BTreeSet::new(), in_game: false, pinned: None }
    }
}

const ROOM_KEY: &str = "panels:room";
const PINNED_KEY: &str = "panels:pinned";

fn name_of(id: PanelId) -> Option<String> {
    match id {
        PanelId::Console => None,
        PanelId::Connection => Some("connection".to_string()),
        PanelId::Stats => Some("stats".to_string()),
        PanelId::Display(stream) => Some(format!("display{stream}")),
    }
}

fn from_name(name: &str) -> Option<PanelId> {
    match name.trim() {
        "connection" => Some(PanelId::Connection),
        "stats" => Some(PanelId::Stats),
        other => other.strip_prefix("display").and_then(|n| n.parse().ok()).map(PanelId::Display),
    }
}

impl Layout {
    /// The layout from the saved text of the room's choice and of the pinned display. A text that is
    /// none gives the defaults. `old_panels_shown` is the choice that the ball made before the menu:
    /// it hid the connection panel and the stats panel together.
    pub fn from_saved(room: Option<&str>, pinned: Option<&str>, old_panels_shown: Option<bool>) -> Self {
        let mut layout = Self::default();
        match room {
            Some(text) => layout.room = text.split(',').filter_map(from_name).collect(),
            None if old_panels_shown == Some(false) => {
                layout.room.remove(&PanelId::Connection);
                layout.room.remove(&PanelId::Stats);
            }
            None => {}
        }
        layout.pinned = pinned.and_then(|text| text.trim().parse().ok()).filter(|stream| (DESKTOP_STREAM..DESKTOP_STREAM + cranvr_proto::message::MAX_DISPLAYS).contains(stream));
        layout
    }

    /// The saved text of the room's choice
    pub fn room_text(&self) -> String {
        self.room.iter().filter_map(|id| name_of(*id)).collect::<Vec<_>>().join(",")
    }

    pub fn pinned_text(&self) -> String {
        self.pinned.map_or(String::new(), |stream| stream.to_string())
    }

    /// Reads the saved layout from the preferences.
    pub fn load(old_panels_shown: Option<bool>) -> Self {
        let get = |key: &str| preferences().get(key);
        Self::from_saved(get(ROOM_KEY).as_deref(), get(PINNED_KEY).as_deref(), old_panels_shown)
    }

    /// Keeps the room's choice and the pinned display for the next start.
    pub fn save(&self) {
        for (key, text) in [(ROOM_KEY, self.room_text()), (PINNED_KEY, self.pinned_text())] {
            if let Err(e) = preferences().set(key, &text) {
                log::warn!("the panels' layout was not saved: {e}");
            }
        }
    }

    fn set(&self) -> &BTreeSet<PanelId> {
        if self.in_game {
            &self.game
        } else {
            &self.room
        }
    }

    fn set_mut(&mut self) -> &mut BTreeSet<PanelId> {
        if self.in_game {
            &mut self.game
        } else {
            &mut self.room
        }
    }

    /// Whether the panel shows now. The console is the hub's, so it never counts here.
    pub fn shown(&self, id: PanelId) -> bool {
        self.set().contains(&id)
    }

    /// Shows the panel or hides it. A game's pick is not saved.
    pub fn set_shown(&mut self, id: PanelId, shown: bool) {
        if shown {
            self.set_mut().insert(id);
        } else {
            self.set_mut().remove(&id);
        }
    }

    pub fn toggle(&mut self, id: PanelId) {
        let shown = self.shown(id);
        self.set_shown(id, !shown);
    }

    /// A game starts or ends. A game starts with no panel shown: the choice of the room comes back
    /// when it ends.
    pub fn set_in_game(&mut self, in_game: bool) {
        if in_game && !self.in_game {
            self.game.clear();
        }
        self.in_game = in_game;
    }

    pub fn in_game(&self) -> bool {
        self.in_game
    }

    /// The displays that show now, in the order of their streams.
    pub fn displays_shown(&self) -> Vec<u8> {
        self.set().iter().filter_map(|id| if let PanelId::Display(stream) = id { Some(*stream) } else { None }).collect()
    }

    pub fn pinned(&self) -> Option<u8> {
        self.pinned
    }

    /// Puts the display on the left hand, or back in the room when it is on the hand already.
    pub fn toggle_pinned(&mut self, stream: u8) {
        self.pinned = if self.pinned == Some(stream) { None } else { Some(stream) };
    }

    /// The host's list of displays: a display that left it is forgotten, so a new display on its
    /// stream starts hidden and in the room.
    pub fn forget_missing(&mut self, streams: &[u8]) {
        let gone = |id: &PanelId| matches!(id, PanelId::Display(stream) if !streams.contains(stream));
        self.room.retain(|id| !gone(id));
        self.game.retain(|id| !gone(id));
        if self.pinned.is_some_and(|stream| !streams.contains(&stream)) {
            self.pinned = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_start_shows_the_connection_the_stats_and_the_first_display() {
        let layout = Layout::default();
        assert!(layout.shown(PanelId::Connection) && layout.shown(PanelId::Stats) && layout.shown(PanelId::Display(1)));
        assert!(!layout.shown(PanelId::Display(2)));
        assert_eq!(layout.displays_shown(), vec![1]);
        assert_eq!(layout.pinned(), None);
    }

    #[test]
    fn a_game_starts_with_nothing_shown_and_the_room_comes_back_when_it_ends() {
        let mut layout = Layout::default();
        layout.set_in_game(true);
        assert!(!layout.shown(PanelId::Connection) && layout.displays_shown().is_empty());
        layout.set_shown(PanelId::Display(2), true);
        assert_eq!(layout.displays_shown(), vec![2]);
        layout.set_in_game(false);
        assert_eq!(layout.displays_shown(), vec![1]);
        // The next game starts empty again, whatever the last one showed
        layout.set_in_game(true);
        assert!(layout.displays_shown().is_empty());
    }

    #[test]
    fn a_game_that_is_reported_twice_keeps_the_panels_that_the_user_picked() {
        let mut layout = Layout::default();
        layout.set_in_game(true);
        layout.set_shown(PanelId::Display(1), true);
        layout.set_in_game(true);
        assert_eq!(layout.displays_shown(), vec![1]);
    }

    #[test]
    fn the_room_choice_survives_the_text_and_the_game_choice_does_not_count() {
        let mut layout = Layout::default();
        layout.toggle(PanelId::Stats);
        layout.set_shown(PanelId::Display(3), true);
        layout.toggle_pinned(3);
        layout.set_in_game(true);
        layout.set_shown(PanelId::Display(2), true);
        let back = Layout::from_saved(Some(&layout.room_text()), Some(&layout.pinned_text()), None);
        assert!(back.shown(PanelId::Connection) && !back.shown(PanelId::Stats) && back.shown(PanelId::Display(3)) && back.shown(PanelId::Display(1)));
        assert!(!back.shown(PanelId::Display(2)));
        assert_eq!(back.pinned(), Some(3));
    }

    #[test]
    fn the_old_choice_of_the_ball_hides_the_connection_and_the_stats_together() {
        let layout = Layout::from_saved(None, None, Some(false));
        assert!(!layout.shown(PanelId::Connection) && !layout.shown(PanelId::Stats) && layout.shown(PanelId::Display(1)));
        assert!(Layout::from_saved(None, None, Some(true)).shown(PanelId::Connection));
        // A saved choice wins over the old one
        assert!(Layout::from_saved(Some("connection"), None, Some(false)).shown(PanelId::Connection));
    }

    #[test]
    fn a_text_that_means_nothing_is_ignored() {
        let layout = Layout::from_saved(Some("connection,rubbish,display9x,display2"), Some("99"), None);
        assert!(layout.shown(PanelId::Connection) && layout.shown(PanelId::Display(2)) && !layout.shown(PanelId::Stats));
        assert_eq!(layout.pinned(), None);
    }

    #[test]
    fn a_display_that_leaves_the_list_is_forgotten_everywhere() {
        let mut layout = Layout::default();
        layout.set_shown(PanelId::Display(2), true);
        layout.toggle_pinned(2);
        layout.set_in_game(true);
        layout.set_shown(PanelId::Display(2), true);
        layout.forget_missing(&[1]);
        assert_eq!(layout.pinned(), None);
        assert!(layout.displays_shown().is_empty());
        layout.set_in_game(false);
        assert_eq!(layout.displays_shown(), vec![1]);
    }

    #[test]
    fn a_press_on_the_pin_toggles_it() {
        let mut layout = Layout::default();
        layout.toggle_pinned(2);
        assert_eq!(layout.pinned(), Some(2));
        layout.toggle_pinned(1);
        assert_eq!(layout.pinned(), Some(1));
        layout.toggle_pinned(1);
        assert_eq!(layout.pinned(), None);
    }
}
