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
//! Each display keeps its place by its stream: shown or hidden, in the room or on the hand. Its pose
//! and its width stay in `place.rs`. A host that offers fewer displays, such as a test host, changes
//! none of it: a display that the host does not offer only shows nothing. The place comes back when
//! a host offers that display again. A display goes away from the layout only when the user removes
//! it in the hand menu.
//!
//! The console's own state is the hub's (see `console.rs`): its opening follows the host's menu and
//! the buttons that the mapping gives the console. This file only names it, so the menu can list it.
//!
//! The ranks of the panels for the headset's layers are here too (`layer_rank`).

use cranpose::preferences;
use cranvr_proto::message::DESKTOP_STREAM;
use std::collections::BTreeSet;

/// Which panels keep a layer of the headset's compositor of their own when a frame needs more
/// layers than the headset takes (`SubspaceModifier::layer_priority`).
///
/// A Quest 3 takes 16 layers in a frame. Four displays with their handles, the console, the connect
/// panel and the hand menu need more. Cranpose then draws the panels of the lowest rank into the
/// projection layer of the 3D content, where their text shows softer. The handles go there first.
/// The rule: a panel ranks higher when the user reads more text on it, and smaller text.
/// - The Mac's displays rank first. Their pictures are surfaces, which Cranpose keeps on layers of
///   their own before every panel, so the rank does nothing to them. The Mac's cursor ring over a
///   display is a panel. It takes the displays' rank: on a runtime with no depth test, a ring in the
///   projection layer can draw under the layer of its display. The host draws the cursor into the
///   pictures by default, so the ring shows only with `CRANVR_DESKTOP_CURSOR_IN_VIDEO=0`.
/// - The console and the connect panel rank next. Most of their text is 20 to 22 mm tall, and it
///   stands 0.6 to 1 m from the head: 1.3 to 2 degrees.
/// - The hand menu ranks after them: its ball and its panel. The text of the menu is 20 to 26 mm
///   tall at the 45 cm from which a hand holds it: 2.5 to 3.3 degrees. It stays readable when it
///   shows softer.
/// - Every other panel keeps Cranpose's default of 0: the stream's numbers, which the user reads only
///   to check the link.
///
/// Panels of one rank keep their layers by Cranpose's own order: the panel that a hand points at
/// first, then the nearer one.
pub mod layer_rank {
    /// The cursor ring over a display of the Mac
    pub const DISPLAY: i32 = 3;
    /// The console and the connect panel
    pub const TEXT: i32 = 2;
    /// The hand menu's ball and its panel
    pub const HAND_MENU: i32 = 1;
}

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
    /// A test's panels (the launch extra `shown`): they show in the room and in each game, and nothing
    /// is saved
    test: Option<BTreeSet<PanelId>>,
    /// The displays that the host offers now and the number of the session that offers them. `None`
    /// while no session lists displays. It is not saved.
    offered: Option<(u64, BTreeSet<u8>)>,
}

impl Default for Layout {
    fn default() -> Self {
        Self { room: [PanelId::Connection, PanelId::Stats, PanelId::Display(DESKTOP_STREAM)].into(), game: BTreeSet::new(), in_game: false, pinned: None, test: None, offered: None }
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
    /// none gives the defaults.
    pub fn from_saved(room: Option<&str>, pinned: Option<&str>) -> Self {
        let mut layout = Self::default();
        if let Some(text) = room {
            layout.room = text.split(',').filter_map(from_name).collect();
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
    pub fn load() -> Self {
        let get = |key: &str| preferences().get(key);
        let mut layout = Self::from_saved(get(ROOM_KEY).as_deref(), get(PINNED_KEY).as_deref());
        log::info!("the panels' saved layout: {}", layout.saved_text());
        // A test chooses the panels and the pinned display with launch extras and saves nothing:
        // `shown` is a list such as `display1,stats` and `pinned` a display's number
        let args = crate::launch::args();
        if let Some(list) = args.string("shown") {
            let set: BTreeSet<PanelId> = list.split(',').filter_map(from_name).collect();
            layout.room.clone_from(&set);
            layout.game.clone_from(&set);
            layout.test = Some(set);
        }
        // The launch asks for every panel hidden (`panels=hidden`), as a test of the beams needs
        if crate::settings::panels_hidden_on_launch() && layout.test.is_none() {
            layout.room.clear();
            layout.game.clear();
            layout.test = Some(BTreeSet::new());
        }
        if let Some(stream) = args.int("pinned").and_then(|n| u8::try_from(n).ok()) {
            layout.pinned = Some(stream);
            layout.test.get_or_insert_with(|| layout.room.clone());
        }
        layout
    }

    /// Keeps the room's choice and the pinned display for the next start.
    pub fn save(&self) {
        if self.test.is_some() {
            return;
        }
        for (key, text) in [(ROOM_KEY, self.room_text()), (PINNED_KEY, self.pinned_text())] {
            if let Err(e) = preferences().set(key, &text) {
                log::warn!("the panels' layout was not saved: {e}");
            }
        }
        log::info!("the panels' layout is saved: {}", self.saved_text());
    }

    /// The saved choice in words, for the log
    fn saved_text(&self) -> String {
        format!("room [{}], hand [{}]", self.room_text(), self.pinned_text())
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
            self.game = self.test.clone().unwrap_or_default();
        }
        self.in_game = in_game;
    }

    pub fn in_game(&self) -> bool {
        self.in_game
    }

    /// The displays that show now, in the order of their streams: the shown ones that the host offers.
    pub fn displays_shown(&self) -> Vec<u8> {
        self.set().iter().filter_map(|id| if let PanelId::Display(stream) = id { Some(*stream) } else { None }).filter(|stream| self.offers(*stream)).collect()
    }

    /// The display that the user put on the left hand, offered by the host or not
    pub fn pinned(&self) -> Option<u8> {
        self.pinned
    }

    /// The display on the left hand while the host offers it
    pub fn hand_display(&self) -> Option<u8> {
        self.pinned.filter(|stream| self.offers(*stream))
    }

    fn offers(&self, stream: u8) -> bool {
        self.offered.as_ref().is_some_and(|(_, streams)| streams.contains(&stream))
    }

    /// Puts the display on the left hand, or back in the room when it is on the hand already.
    pub fn toggle_pinned(&mut self, stream: u8) {
        self.pinned = if self.pinned == Some(stream) { None } else { Some(stream) };
    }

    /// Follows the host's list of displays in a session, given as `(session, streams)`, or `None`
    /// while no session lists any. Each display keeps its place whether the host offers it or not,
    /// and nothing is saved for it. A display that a later list of the same session adds, such as
    /// one that the user asked for, shows at once: the room's choice is then saved. The first list of
    /// a session shows nothing new. Returns whether the layout changed.
    pub fn follow_host(&mut self, offer: Option<(u64, Vec<u8>)>) -> bool {
        let next = offer.map(|(session, streams)| (session, streams.into_iter().collect::<BTreeSet<u8>>()));
        if next == self.offered {
            return false;
        }
        let added: Vec<u8> = match (&self.offered, &next) {
            (Some((was, before)), Some((session, now))) if was == session => now.difference(before).copied().collect(),
            _ => Vec::new(),
        };
        self.offered = next;
        for stream in &added {
            self.set_shown(PanelId::Display(*stream), true);
        }
        if !added.is_empty() && !self.in_game {
            self.save();
        }
        true
    }

    /// The user removed the display: its place goes from the room, the game and the hand.
    pub fn forget(&mut self, stream: u8) {
        self.room.remove(&PanelId::Display(stream));
        self.game.remove(&PanelId::Display(stream));
        if self.pinned == Some(stream) {
            self.pinned = None;
        }
    }
}

/// What the app composes as panels and surfaces, each of which is a layer of the headset's compositor.
/// The log shows it, so a run can check that a game with nothing shown carries the ball and nothing
/// else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Layers {
    /// Panels with composables: the connection, the stats, the console, the menu and the hand's ball
    pub panels: usize,
    /// Surfaces that a decoder writes into: the displays
    pub surfaces: usize,
}

/// The layers that the app composes. `with_video` names the displays whose video the host sends: a
/// display without it composes nothing. `connected` says that the link streams: the connection panel
/// then shows only when the user picked it. `room_free` is false while a tracked hand points at the
/// panels in a game that asks for hands, which hides the room's panels and the ball.
pub fn layers(layout: &Layout, connected: bool, console_open: bool, menu_open: bool, room_free: bool, with_video: &[u8]) -> Layers {
    let mut out = Layers::default();
    if room_free {
        out.panels += usize::from(layout.shown(PanelId::Connection) || !connected) + usize::from(layout.shown(PanelId::Stats));
    }
    out.panels += usize::from(console_open);
    let shown: Vec<u8> = layout.displays_shown().into_iter().filter(|stream| with_video.contains(stream)).collect();
    out.surfaces = shown.len();
    // A display that is not on the hand has a cursor ring when the host leaves the cursor out of the
    // pictures. The host puts it into them, so no ring counts here.
    if room_free {
        // The ball, which is the pill with a second button while a display rides on the hand
        out.panels += 1 + usize::from(menu_open);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout of the first start in a session in which the host offers `streams`
    fn offering(streams: &[u8]) -> Layout {
        let mut layout = Layout::default();
        assert!(layout.follow_host(Some((1, streams.to_vec()))));
        layout
    }

    #[test]
    fn a_game_with_nothing_shown_carries_the_ball_and_nothing_else() {
        let mut layout = offering(&[1, 2]);
        layout.set_in_game(true);
        // The same count as the earlier ball: one panel and no surface
        assert_eq!(layers(&layout, true, false, false, true, &[1, 2]), Layers { panels: 1, surfaces: 0 });
        // The menu is one more panel while it is open. A display on the hand is one more surface.
        assert_eq!(layers(&layout, true, false, true, true, &[1]), Layers { panels: 2, surfaces: 0 });
        layout.set_shown(PanelId::Display(2), true);
        assert_eq!(layers(&layout, true, false, false, true, &[1, 2]), Layers { panels: 1, surfaces: 1 });
        // A display that the host sends no video for composes nothing
        assert_eq!(layers(&layout, true, false, false, true, &[1]), Layers { panels: 1, surfaces: 0 });
    }

    #[test]
    fn the_room_shows_the_connection_the_stats_and_the_first_display() {
        let layout = offering(&[1]);
        assert_eq!(layers(&layout, true, true, false, true, &[1]), Layers { panels: 4, surfaces: 1 });
        // Before the link streams the connection panel shows even when the user hid it
        let mut hidden = Layout::default();
        hidden.toggle(PanelId::Connection);
        assert_eq!(layers(&hidden, false, false, false, true, &[]).panels, 3);
        assert_eq!(layers(&hidden, true, false, false, true, &[]).panels, 2);
    }

    #[test]
    fn the_first_start_shows_the_connection_the_stats_and_the_first_display() {
        let layout = offering(&[1, 2]);
        assert!(layout.shown(PanelId::Connection) && layout.shown(PanelId::Stats) && layout.shown(PanelId::Display(1)));
        assert!(!layout.shown(PanelId::Display(2)));
        assert_eq!(layout.displays_shown(), vec![1]);
        assert_eq!(layout.pinned(), None);
    }

    #[test]
    fn a_game_starts_with_nothing_shown_and_the_room_comes_back_when_it_ends() {
        let mut layout = offering(&[1, 2]);
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
        let mut layout = offering(&[1]);
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
        let back = Layout::from_saved(Some(&layout.room_text()), Some(&layout.pinned_text()));
        assert!(back.shown(PanelId::Connection) && !back.shown(PanelId::Stats) && back.shown(PanelId::Display(3)) && back.shown(PanelId::Display(1)));
        assert!(!back.shown(PanelId::Display(2)));
        assert_eq!(back.pinned(), Some(3));
    }

    #[test]
    fn a_text_that_means_nothing_is_ignored() {
        let layout = Layout::from_saved(Some("connection,rubbish,display9x,display2"), Some("99"));
        assert!(layout.shown(PanelId::Connection) && layout.shown(PanelId::Display(2)) && !layout.shown(PanelId::Stats));
        assert_eq!(layout.pinned(), None);
    }

    #[test]
    fn a_host_without_the_display_keeps_its_place_and_the_place_comes_back() {
        // The user's room: display 1 on the left hand, display 2 in the room
        let mut layout = offering(&[1, 2]);
        layout.set_shown(PanelId::Display(2), true);
        layout.toggle_pinned(1);
        let saved = (layout.room_text(), layout.pinned_text());
        assert_eq!(saved, ("connection,stats,display1,display2".to_string(), "1".to_string()));
        assert_eq!((layout.hand_display(), layout.displays_shown()), (Some(1), vec![1, 2]));
        // A test host with no displays: nothing shows, and nothing of the place changes
        assert!(layout.follow_host(Some((2, vec![]))));
        assert_eq!((layout.hand_display(), layout.displays_shown()), (None, vec![]));
        assert_eq!(layout.pinned(), Some(1));
        assert_eq!((layout.room_text(), layout.pinned_text()), saved);
        // A host with only display 2, and no session at all
        assert!(layout.follow_host(Some((3, vec![2]))));
        assert_eq!((layout.hand_display(), layout.displays_shown()), (None, vec![2]));
        assert!(layout.follow_host(None));
        assert!(!layout.follow_host(None));
        assert_eq!((layout.room_text(), layout.pinned_text()), saved);
        // The user's host offers both again: display 1 is back on the hand
        assert!(layout.follow_host(Some((4, vec![1, 2]))));
        assert_eq!((layout.hand_display(), layout.displays_shown()), (Some(1), vec![1, 2]));
        assert_eq!((layout.room_text(), layout.pinned_text()), saved);
        // A start that reads the saved text gets the same place
        let mut back = Layout::from_saved(Some(&saved.0), Some(&saved.1));
        back.follow_host(Some((1, vec![1, 2])));
        assert_eq!((back.hand_display(), back.displays_shown()), (Some(1), vec![1, 2]));
    }

    #[test]
    fn a_hidden_display_stays_hidden_when_a_host_offers_it_again() {
        let mut layout = offering(&[1, 2]);
        layout.set_shown(PanelId::Display(1), false);
        layout.follow_host(Some((2, vec![])));
        // The first list of a session shows nothing new, though the session before had no display 1
        layout.follow_host(Some((3, vec![1, 2])));
        assert!(layout.displays_shown().is_empty());
        assert_eq!(layout.room_text(), "connection,stats");
    }

    /// The only test that saves: the preferences are one store for the whole test process.
    #[test]
    fn the_saved_layout_changes_only_by_the_user_or_by_a_display_that_the_session_adds() {
        let path = std::env::temp_dir().join(format!("cranvr-client-layout-{}", std::process::id()));
        // The order of the keys differs from the order that a save writes, so any save changes the bytes
        std::fs::write(&path, "panels:room=connection,stats,display1,display2\npanels:pinned=1\n").unwrap();
        cranpose::set_platform_preferences(std::sync::Arc::new(cranpose::FilePreferences::at_path(&path)));
        let bytes = || std::fs::read_to_string(&path).unwrap();
        let before = bytes();
        let mut layout = Layout::load();
        // Hosts with fewer displays or none, as the client's loop follows the host's lists
        for offer in [Some((1, vec![])), None, Some((2, vec![2])), Some((3, vec![])), Some((4, vec![1, 2]))] {
            layout.follow_host(offer);
            assert_eq!(bytes(), before);
        }
        assert_eq!((layout.hand_display(), layout.displays_shown()), (Some(1), vec![1, 2]));
        // A display that the user adds in the session shows at once. The room's choice is saved.
        assert!(layout.follow_host(Some((4, vec![1, 2, 3]))));
        assert_eq!(layout.displays_shown(), vec![1, 2, 3]);
        assert_eq!(bytes(), "panels:pinned=1\npanels:room=connection,stats,display1,display2,display3\n");
        // In a game the added display shows in the game's pick only, and nothing is saved
        layout.set_in_game(true);
        let before = bytes();
        assert!(layout.follow_host(Some((4, vec![1, 2, 3, 4]))));
        assert_eq!(layout.displays_shown(), vec![4]);
        assert_eq!(bytes(), before);
        // The user removes display 1 in the hand menu: its place goes and the choice is saved
        layout.forget(1);
        layout.save();
        assert_eq!(bytes(), "panels:pinned=\npanels:room=connection,stats,display2,display3\n");
        cranpose::clear_platform_preferences();
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_display_that_the_user_removes_leaves_the_room_the_game_and_the_hand() {
        let mut layout = offering(&[1, 2]);
        layout.set_shown(PanelId::Display(2), true);
        layout.toggle_pinned(2);
        layout.set_in_game(true);
        layout.set_shown(PanelId::Display(2), true);
        layout.forget(2);
        assert_eq!(layout.pinned(), None);
        assert!(layout.displays_shown().is_empty());
        layout.set_in_game(false);
        assert_eq!(layout.displays_shown(), vec![1]);
        assert_eq!((layout.room_text(), layout.pinned_text()), ("connection,stats,display1".to_string(), String::new()));
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
