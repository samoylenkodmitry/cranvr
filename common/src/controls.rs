//! What each button of each controller does in the headset's own interface: the panels, the Mac's
//! desktop panel and the headset itself.
//!
//! The cranvr app on the Mac keeps the mapping in `app.json` and writes it into `vr-menu.json`. The
//! host sends that file to the headset, and the headset's console changes it with a request that
//! goes back the same way. The app, the host and the headset all read the one type here, so the
//! names in the file are the names in the three programs.
//!
//! A running game still gets every button. The mapping only decides what the headset does for the
//! panels and itself. The Meta button belongs to the system and has no entry.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// What a button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// A left mouse click on the Mac's desktop panel
    LeftClick,
    /// A right mouse click on the desktop panel
    RightClick,
    /// A middle mouse click on the desktop panel
    MiddleClick,
    /// Scrolls the desktop panel. Only a thumbstick axis offers it.
    Scroll,
    /// Opens or closes the hand menu, wherever the menu stands: on a hand or in front of the user.
    /// The menu lists every panel, the console among them.
    Menu,
    /// Shows or hides the connect and stats panels
    Panels,
    /// Opens or closes the console
    Console,
    /// Brings the panels back in front of the user. The front of a game's room is the system's:
    /// the Meta button's hold recenters it, and the headset passes that on to the game.
    Recenter,
    /// Opens or closes the address field with its keyboard, in the connect panel. The field shows
    /// while no link runs.
    Keyboard,
    Nothing,
}

impl Action {
    pub const ALL: [Action; 10] = [
        Action::LeftClick,
        Action::RightClick,
        Action::MiddleClick,
        Action::Scroll,
        Action::Menu,
        Action::Panels,
        Action::Console,
        Action::Recenter,
        Action::Keyboard,
        Action::Nothing,
    ];

    /// The name in the files and messages
    pub fn key(self) -> &'static str {
        match self {
            Action::LeftClick => "left_click",
            Action::RightClick => "right_click",
            Action::MiddleClick => "middle_click",
            Action::Scroll => "scroll",
            Action::Menu => "menu",
            Action::Panels => "panels",
            Action::Console => "console",
            Action::Recenter => "recenter",
            Action::Keyboard => "keyboard",
            Action::Nothing => "nothing",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.key() == key.trim())
    }

    /// The text of the button that picks the action
    pub fn label(self) -> &'static str {
        match self {
            Action::LeftClick => "Left click",
            Action::RightClick => "Right click",
            Action::MiddleClick => "Middle click",
            Action::Scroll => "Scroll",
            Action::Menu => "Open the menu",
            Action::Panels => "Show or hide panels",
            Action::Console => "Open the console",
            Action::Recenter => "Recenter",
            Action::Keyboard => "Keyboard",
            Action::Nothing => "Nothing",
        }
    }

    /// One plain sentence about what the action does
    pub fn describe(self) -> &'static str {
        match self {
            Action::LeftClick => "Clicks the left mouse button on the Mac's desktop panel.",
            Action::RightClick => "Clicks the right mouse button on the Mac's desktop panel.",
            Action::MiddleClick => "Clicks the middle mouse button on the Mac's desktop panel.",
            Action::Scroll => "Scrolls the Mac's desktop panel.",
            Action::Menu => "Opens or closes the hand menu, which shows and hides every panel and opens the console.",
            Action::Panels => "Shows or hides the connect and stats panels.",
            Action::Console => "Opens or closes the console.",
            Action::Recenter => "Brings the panels in front of you.",
            Action::Keyboard => "Opens or closes the address field and its keyboard while no link runs.",
            Action::Nothing => "Does nothing.",
        }
    }

    /// A mouse button of the Mac's desktop panel
    pub fn is_click(self) -> bool {
        matches!(self, Action::LeftClick | Action::RightClick | Action::MiddleClick)
    }

    /// The headset does it with no regard for where the controller points
    pub fn is_headset(self) -> bool {
        matches!(self, Action::Menu | Action::Panels | Action::Console | Action::Recenter | Action::Keyboard)
    }
}

impl Serialize for Action {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.key())
    }
}

/// A name that is no action is an error.
impl<'de> Deserialize<'de> for Action {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let key = String::deserialize(deserializer)?;
        Self::from_key(&key).ok_or_else(|| serde::de::Error::custom(format!("no action is named {key}")))
    }
}

/// When the headset draws its own models of the controllers while a game runs. A game draws its
/// own controllers, so the headset's models stand at the same place as a second copy. Outside a
/// game the headset's models show with the rays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ModelsInGame {
    /// Both models show for the whole game
    Always,
    /// A hand's model shows while its ray points at a cranvr panel
    #[default]
    WhilePointing,
    /// No model shows while a game runs
    Never,
}

impl ModelsInGame {
    pub const ALL: [ModelsInGame; 3] = [ModelsInGame::Always, ModelsInGame::WhilePointing, ModelsInGame::Never];

    /// The name in the files and messages
    pub fn key(self) -> &'static str {
        match self {
            ModelsInGame::Always => "always",
            ModelsInGame::WhilePointing => "while_pointing",
            ModelsInGame::Never => "never",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }

    /// The text of the choice's button
    pub fn label(self) -> &'static str {
        match self {
            ModelsInGame::Always => "Always",
            ModelsInGame::WhilePointing => "While pointing at a cranvr panel",
            ModelsInGame::Never => "Never",
        }
    }
}

impl Serialize for ModelsInGame {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.key())
    }
}

/// A name that is no choice is an error.
impl<'de> Deserialize<'de> for ModelsInGame {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let key = String::deserialize(deserializer)?;
        Self::from_key(&key).ok_or_else(|| serde::de::Error::custom(format!("no choice of the controller models is named {key}")))
    }
}

/// The hand on which the hand menu rides while the headset tracks both controllers. The other hand
/// points at it. With one controller the menu stands in front of the user, whatever the choice says
/// (see `client/src/menu_place.rs`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MenuHand {
    Left,
    Right,
    /// The hand that does not point: the one that did not last pull its trigger on a panel
    #[default]
    Auto,
}

impl MenuHand {
    pub const ALL: [MenuHand; 3] = [MenuHand::Left, MenuHand::Right, MenuHand::Auto];

    /// The name in the files and messages
    pub fn key(self) -> &'static str {
        match self {
            MenuHand::Left => "left",
            MenuHand::Right => "right",
            MenuHand::Auto => "auto",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }

    /// The text of the choice's button
    pub fn label(self) -> &'static str {
        match self {
            MenuHand::Left => "Left hand",
            MenuHand::Right => "Right hand",
            MenuHand::Auto => "Auto",
        }
    }
}

impl Serialize for MenuHand {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.key())
    }
}

/// A name that is no choice is an error.
impl<'de> Deserialize<'de> for MenuHand {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let key = String::deserialize(deserializer)?;
        Self::from_key(&key).ok_or_else(|| serde::de::Error::custom(format!("no menu hand is named {key}")))
    }
}

/// The two controllers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hand {
    Left,
    Right,
}

impl Hand {
    pub const BOTH: [Hand; 2] = [Hand::Left, Hand::Right];

    pub fn label(self) -> &'static str {
        match self {
            Hand::Left => "Left controller",
            Hand::Right => "Right controller",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Hand::Left => 0,
            Hand::Right => 1,
        }
    }
}

/// The inputs of a Touch controller that the mapping covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    Trigger,
    Grip,
    /// A on the right controller, X on the left
    Primary,
    /// B on the right controller, Y on the left
    Secondary,
    /// The press of the thumbstick
    StickClick,
    /// The thumbstick's lean forward and back
    StickVertical,
    /// The thumbstick's lean to the sides
    StickHorizontal,
    /// Only the left controller has it
    Menu,
}

impl Control {
    pub const ALL: [Control; 8] = [
        Control::Trigger,
        Control::Grip,
        Control::Primary,
        Control::Secondary,
        Control::StickClick,
        Control::StickVertical,
        Control::StickHorizontal,
        Control::Menu,
    ];

    /// The name of the input on the controller of `hand`
    pub fn label(self, hand: Hand) -> &'static str {
        match (self, hand) {
            (Control::Trigger, _) => "Trigger",
            (Control::Grip, _) => "Grip",
            (Control::Primary, Hand::Left) => "X button",
            (Control::Primary, Hand::Right) => "A button",
            (Control::Secondary, Hand::Left) => "Y button",
            (Control::Secondary, Hand::Right) => "B button",
            (Control::StickClick, _) => "Stick press",
            (Control::StickVertical, _) => "Stick up and down",
            (Control::StickHorizontal, _) => "Stick left and right",
            (Control::Menu, _) => "Menu button",
        }
    }

    /// Whether the controller of `hand` has the input. The right Touch controller has no menu
    /// button.
    pub fn exists(self, hand: Hand) -> bool {
        !(self == Control::Menu && hand == Hand::Right)
    }

    /// Whether the input leans from -1 to 1 and is no button
    pub fn is_axis(self) -> bool {
        matches!(self, Control::StickVertical | Control::StickHorizontal)
    }

    /// The actions that make sense for the input: an axis scrolls or does nothing. The menu button
    /// has no mouse button, because the pointer of the headset's panels has five buttons and the
    /// five other inputs use them (see `client/src/controls.rs`). A button does the rest.
    pub fn offers(self) -> &'static [Action] {
        const AXIS: [Action; 2] = [Action::Scroll, Action::Nothing];
        const MENU: [Action; 6] = [Action::Menu, Action::Panels, Action::Console, Action::Recenter, Action::Keyboard, Action::Nothing];
        const BUTTON: [Action; 9] = [
            Action::LeftClick,
            Action::RightClick,
            Action::MiddleClick,
            Action::Menu,
            Action::Panels,
            Action::Console,
            Action::Recenter,
            Action::Keyboard,
            Action::Nothing,
        ];
        if self.is_axis() {
            &AXIS
        } else if self == Control::Menu {
            &MENU
        } else {
            &BUTTON
        }
    }
}

/// The actions of one controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HandControls {
    pub trigger: Action,
    pub grip: Action,
    pub primary: Action,
    pub secondary: Action,
    pub stick_click: Action,
    pub stick_vertical: Action,
    pub stick_horizontal: Action,
    pub menu: Action,
}

impl HandControls {
    /// The mapping that a controller has before the user changes anything:
    ///
    /// - The trigger is the left mouse button, as it was.
    /// - The grip does nothing: a window is carried by its handle, as it was.
    /// - The first face button (A or X) is the right mouse button.
    /// - Y on the left controller shows or hides the panels.
    /// - The thumbstick scrolls, and its press is the middle mouse button, as a mouse wheel's.
    /// - The left menu button and B on the right controller open the hand menu. A user with only
    ///   one controller in hand still has a button for the menu. The menu holds the console.
    ///
    /// The two controllers are the same otherwise, so a user may point with either. A mouse button
    /// works only where the controller points at the Mac's desktop panel, so it does nothing while a
    /// game is the only thing in view.
    pub fn standard(hand: Hand) -> Self {
        Self {
            trigger: Action::LeftClick,
            grip: Action::Nothing,
            primary: Action::RightClick,
            secondary: if hand == Hand::Left { Action::Panels } else { Action::Menu },
            stick_click: Action::MiddleClick,
            stick_vertical: Action::Scroll,
            stick_horizontal: Action::Scroll,
            menu: if hand == Hand::Left { Action::Menu } else { Action::Nothing },
        }
    }

    pub fn get(&self, control: Control) -> Action {
        match control {
            Control::Trigger => self.trigger,
            Control::Grip => self.grip,
            Control::Primary => self.primary,
            Control::Secondary => self.secondary,
            Control::StickClick => self.stick_click,
            Control::StickVertical => self.stick_vertical,
            Control::StickHorizontal => self.stick_horizontal,
            Control::Menu => self.menu,
        }
    }

    pub fn set(&mut self, control: Control, action: Action) {
        let slot = match control {
            Control::Trigger => &mut self.trigger,
            Control::Grip => &mut self.grip,
            Control::Primary => &mut self.primary,
            Control::Secondary => &mut self.secondary,
            Control::StickClick => &mut self.stick_click,
            Control::StickVertical => &mut self.stick_vertical,
            Control::StickHorizontal => &mut self.stick_horizontal,
            Control::Menu => &mut self.menu,
        };
        *slot = action;
    }
}

/// The actions of both controllers, when the headset draws its models of them in a game, and the
/// hand that carries the hand menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Controls {
    pub left: HandControls,
    pub right: HandControls,
    pub models_in_game: ModelsInGame,
    pub menu_hand: MenuHand,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            left: HandControls::standard(Hand::Left),
            right: HandControls::standard(Hand::Right),
            models_in_game: ModelsInGame::default(),
            menu_hand: MenuHand::default(),
        }
    }
}

/// What a text says about one controller. An entry that the text leaves out keeps the action that
/// the controller has before the user changes anything. The standard action depends on the hand, so
/// the type reads through this one and not through a `Default`.
#[derive(Deserialize, Default)]
struct PartialHand {
    trigger: Option<Action>,
    grip: Option<Action>,
    primary: Option<Action>,
    secondary: Option<Action>,
    stick_click: Option<Action>,
    stick_vertical: Option<Action>,
    stick_horizontal: Option<Action>,
    menu: Option<Action>,
}

impl PartialHand {
    fn over(self, base: HandControls) -> HandControls {
        HandControls {
            trigger: self.trigger.unwrap_or(base.trigger),
            grip: self.grip.unwrap_or(base.grip),
            primary: self.primary.unwrap_or(base.primary),
            secondary: self.secondary.unwrap_or(base.secondary),
            stick_click: self.stick_click.unwrap_or(base.stick_click),
            stick_vertical: self.stick_vertical.unwrap_or(base.stick_vertical),
            stick_horizontal: self.stick_horizontal.unwrap_or(base.stick_horizontal),
            menu: self.menu.unwrap_or(base.menu),
        }
    }
}

#[derive(Deserialize, Default)]
struct PartialControls {
    left: Option<PartialHand>,
    right: Option<PartialHand>,
    models_in_game: Option<ModelsInGame>,
    menu_hand: Option<MenuHand>,
}

impl<'de> Deserialize<'de> for Controls {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let partial = PartialControls::deserialize(deserializer)?;
        Ok(Self {
            left: partial.left.unwrap_or_default().over(HandControls::standard(Hand::Left)),
            right: partial.right.unwrap_or_default().over(HandControls::standard(Hand::Right)),
            models_in_game: partial.models_in_game.unwrap_or_default(),
            menu_hand: partial.menu_hand.unwrap_or_default(),
        })
    }
}

impl Controls {
    pub fn hand(&self, hand: Hand) -> &HandControls {
        match hand {
            Hand::Left => &self.left,
            Hand::Right => &self.right,
        }
    }

    pub fn hand_mut(&mut self, hand: Hand) -> &mut HandControls {
        match hand {
            Hand::Left => &mut self.left,
            Hand::Right => &mut self.right,
        }
    }

    pub fn action(&self, hand: Hand, control: Control) -> Action {
        self.hand(hand).get(control)
    }

    /// Sets one input. An action that the input does not offer, such as a scroll on a button,
    /// leaves the mapping as it was and returns `false`.
    pub fn set(&mut self, hand: Hand, control: Control, action: Action) -> bool {
        if !control.offers().contains(&action) {
            return false;
        }
        self.hand_mut(hand).set(control, action);
        true
    }

    /// The same mapping with every entry that its input does not offer set to nothing. A file that
    /// the user edited by hand may hold such an entry.
    pub fn sanitized(mut self) -> Self {
        for hand in Hand::BOTH {
            for control in Control::ALL {
                if !control.offers().contains(&self.action(hand, control)) {
                    self.hand_mut(hand).set(control, Action::Nothing);
                }
            }
        }
        self
    }

    /// The mapping from the JSON text of `vr-menu.json`'s `controls` value or of a console
    /// request. A text that is no mapping gives `None`.
    pub fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str::<Self>(text).ok().map(Self::sanitized)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The inputs of `hand` that do `action`
    pub fn inputs_of(&self, hand: Hand, action: Action) -> impl Iterator<Item = Control> + '_ {
        Control::ALL.into_iter().filter(move |control| control.exists(hand) && self.action(hand, *control) == action)
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The mouse buttons that the headset's pointer reports to the host, as bits of
/// `Mouse::buttons`.
pub mod mouse {
    pub const LEFT: u8 = 1 << 0;
    pub const RIGHT: u8 = 1 << 1;
    pub const MIDDLE: u8 = 1 << 2;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_models_in_a_game_show_while_pointing_unless_the_text_names_another_choice() {
        assert_eq!(Controls::default().models_in_game, ModelsInGame::WhilePointing);
        assert_eq!(Controls::from_json("{}").unwrap().models_in_game, ModelsInGame::WhilePointing);
        for choice in ModelsInGame::ALL {
            let controls = Controls { models_in_game: choice, ..Controls::default() };
            assert_eq!(Controls::from_json(&controls.to_json()), Some(controls));
            assert_eq!(ModelsInGame::from_key(choice.key()), Some(choice));
        }
        assert_eq!(Controls::from_json(r#"{"models_in_game":"never"}"#).unwrap().models_in_game, ModelsInGame::Never);
        // A name that is no choice makes the mapping unreadable, as a wrong action does
        assert_eq!(Controls::from_json(r#"{"models_in_game":"sometimes"}"#), None);
    }

    #[test]
    fn the_menu_hand_is_auto_unless_the_text_names_another_choice() {
        assert_eq!(Controls::default().menu_hand, MenuHand::Auto);
        assert_eq!(Controls::from_json("{}").unwrap().menu_hand, MenuHand::Auto);
        for choice in MenuHand::ALL {
            let controls = Controls { menu_hand: choice, ..Controls::default() };
            assert_eq!(Controls::from_json(&controls.to_json()), Some(controls));
            assert_eq!(MenuHand::from_key(choice.key()), Some(choice));
        }
        assert_eq!(Controls::from_json(r#"{"menu_hand":"right"}"#).unwrap().menu_hand, MenuHand::Right);
        assert_eq!(Controls::from_json(r#"{"menu_hand":"middle"}"#), None);
    }

    #[test]
    fn a_name_that_is_no_action_makes_the_mapping_unreadable() {
        assert_eq!(Action::from_key("grab"), None);
        assert_eq!(Controls::from_json(r#"{"right":{"grip":"grab"}}"#), None);
    }

    #[test]
    fn the_standard_mapping_opens_the_menu_from_either_controller() {
        let controls = Controls::default();
        // The trigger clicks and the thumbstick scrolls on both
        for hand in Hand::BOTH {
            assert_eq!(controls.action(hand, Control::Trigger), Action::LeftClick);
            assert_eq!(controls.action(hand, Control::StickVertical), Action::Scroll);
        }
        // The left menu button and B open the menu, so one controller alone opens it
        assert_eq!(controls.inputs_of(Hand::Left, Action::Menu).collect::<Vec<_>>(), vec![Control::Menu]);
        assert_eq!(controls.inputs_of(Hand::Right, Action::Menu).collect::<Vec<_>>(), vec![Control::Secondary]);
        assert_eq!(controls.action(Hand::Left, Control::Secondary), Action::Panels);
        assert_eq!(controls.action(Hand::Right, Control::Menu), Action::Nothing);
        // The menu holds the console, so no button of the standard mapping opens it
        assert!(Hand::BOTH.iter().all(|hand| controls.inputs_of(*hand, Action::Console).count() == 0));
        // Every button and the menu button may open the menu. An axis may not.
        for control in Control::ALL {
            assert_eq!(control.offers().contains(&Action::Menu), !control.is_axis(), "{control:?}");
        }
        assert!(Action::Menu.is_headset());
    }

    #[test]
    fn every_input_of_the_standard_mapping_is_offered_to_it() {
        let controls = Controls::default();
        for hand in Hand::BOTH {
            for control in Control::ALL {
                assert!(control.offers().contains(&controls.action(hand, control)), "{hand:?} {control:?}");
            }
        }
        assert_eq!(controls, controls.sanitized());
    }

    #[test]
    fn an_axis_offers_scroll_and_nothing_and_a_button_offers_no_scroll() {
        for control in Control::ALL {
            let scrolls = control.offers().contains(&Action::Scroll);
            assert_eq!(scrolls, control.is_axis(), "{control:?}");
            assert!(control.offers().contains(&Action::Nothing));
        }
        // Every action is on offer somewhere
        for action in Action::ALL {
            assert!(Control::ALL.iter().any(|control| control.offers().contains(&action)), "{action:?}");
        }
    }

    #[test]
    fn a_mapping_survives_its_json() {
        let mut controls = Controls::default();
        assert!(controls.set(Hand::Right, Control::Secondary, Action::Keyboard));
        assert!(controls.set(Hand::Left, Control::StickHorizontal, Action::Nothing));
        let text = controls.to_json();
        assert!(text.contains("\"keyboard\"") && text.contains("\"left_click\""), "{text}");
        assert_eq!(Controls::from_json(&text), Some(controls));
    }

    #[test]
    fn a_mapping_with_missing_entries_keeps_the_standard_action_of_each() {
        let text = r#"{"right":{"trigger":"right_click","grip":"nothing"}}"#;
        let controls = Controls::from_json(text).unwrap();
        assert_eq!(controls.right.trigger, Action::RightClick);
        assert_eq!(controls.right.grip, Action::Nothing);
        assert_eq!(controls.right.primary, Action::RightClick);
        assert_eq!(controls.left, HandControls::standard(Hand::Left));
        assert_eq!(Controls::from_json("not json"), None);
        assert_eq!(Controls::from_json("{}"), Some(Controls::default()));
    }

    #[test]
    fn an_entry_that_its_input_does_not_offer_is_refused_or_cleared() {
        let mut controls = Controls::default();
        assert!(!controls.set(Hand::Left, Control::Trigger, Action::Scroll));
        assert!(!controls.set(Hand::Left, Control::StickVertical, Action::LeftClick));
        // The menu button has no mouse button
        assert!(!controls.set(Hand::Left, Control::Menu, Action::RightClick));
        assert_eq!(controls, Controls::default());
        let loaded = Controls::from_json(r#"{"left":{"trigger":"scroll","stick_vertical":"left_click"}}"#).unwrap();
        assert_eq!((loaded.left.trigger, loaded.left.stick_vertical), (Action::Nothing, Action::Nothing));
    }

    #[test]
    fn the_names_of_the_actions_are_unique_and_read_back() {
        for action in Action::ALL {
            assert_eq!(Action::from_key(action.key()), Some(action));
            assert!(!action.label().is_empty() && !action.describe().is_empty());
        }
        let mut keys: Vec<_> = Action::ALL.iter().map(|action| action.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), Action::ALL.len());
    }

    #[test]
    fn the_inputs_that_do_an_action_are_listed_for_the_controller_that_has_them() {
        let controls = Controls::default();
        let menus: Vec<_> = controls.inputs_of(Hand::Left, Action::Menu).collect();
        assert_eq!(menus, vec![Control::Menu]);
        assert_eq!(controls.inputs_of(Hand::Right, Action::Console).count(), 0);
        assert_eq!(controls.inputs_of(Hand::Right, Action::Scroll).count(), 2);
    }
}
