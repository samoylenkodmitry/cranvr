//! What cranvr sets when it starts a Windows program. Nothing here depends on which game it is: a
//! game's own needs are options that the user sees and changes in the Play tab, and the options for
//! every program sit in its Wine card.

use serde::{Deserialize, Serialize};

/// The launch options of one program.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GameOptions {
    /// The runtime refuses swapchains with two slices (BASALT_MAX_ARRAY_SIZE=1, cranvr's runtime
    /// patch 0003), so the game draws each eye in a pass of its own. A game that renders both eyes
    /// in one pass into a two-slice swapchain shows a black right eye, because DXMT leaves the
    /// second slice empty. SUPERHOT VR needed it and then made about 82 frames a second instead of
    /// 119. The host sees the black right eye (host/src/eye_check.rs) and the app turns the option
    /// on (black_eye.rs).
    pub eye_per_pass: bool,
    /// Why `eye_per_pass` is on, when cranvr turned it on (see black_eye.rs). The Options panel
    /// shows it. A change by the user clears it.
    pub eye_per_pass_reason: String,
    /// Wine DLL overrides as WINEDLLOVERRIDES takes them, such as "winhttp=n,b" to load a
    /// winhttp.dll from the game's folder before Wine's own
    pub dll_overrides: String,
    /// More environment variables: NAME=value entries separated by spaces
    pub env: String,
    /// The program's arguments, separated by spaces
    pub args: String,
}

impl GameOptions {
    pub fn is_default(&self) -> bool {
        // A reason belongs to the option: with the option off it is a leftover
        let mut plain = self.clone();
        if !plain.eye_per_pass {
            plain.eye_per_pass_reason.clear();
        }
        plain == Self::default()
    }
}

/// The options of every Windows program that cranvr starts. Steam does not get them.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WineOptions {
    /// The program's own window on the Mac shows its picture. Off by default: a VR game draws
    /// into the headset, and an empty window costs no GPU time (DXMT_SKIP_PRESENT=1, cranvr's DXMT
    /// patch 0008).
    pub mac_window: bool,
    /// Wine DLL overrides for every program, as in [`GameOptions::dll_overrides`]
    pub dll_overrides: String,
    /// Environment variables for every program, as in [`GameOptions::env`]
    pub env: String,
}

/// The environment that the options add to a program's start, in the order that it is set. A
/// program's own variable comes after the one for every program, so it wins.
pub fn environment(wine: &WineOptions, game: &GameOptions) -> Vec<(String, String)> {
    let mut vars = Vec::new();
    if !wine.mac_window {
        vars.push(("DXMT_SKIP_PRESENT".to_owned(), "1".to_owned()));
    }
    if game.eye_per_pass {
        vars.push(("BASALT_MAX_ARRAY_SIZE".to_owned(), "1".to_owned()));
    }
    let overrides: Vec<&str> = [wine.dll_overrides.trim(), game.dll_overrides.trim()].into_iter().filter(|o| !o.is_empty()).collect();
    if !overrides.is_empty() {
        vars.push(("WINEDLLOVERRIDES".to_owned(), overrides.join(";")));
    }
    vars.extend(variables(&wine.env));
    vars.extend(variables(&game.env));
    vars
}

/// The NAME=value entries of `text`. An entry with no name or no `=` is left out.
pub fn variables(text: &str) -> impl Iterator<Item = (String, String)> + '_ {
    text.split_whitespace().filter_map(|entry| {
        let (name, value) = entry.split_once('=')?;
        (!name.is_empty()).then(|| (name.to_owned(), value.to_owned()))
    })
}

/// The program's arguments.
pub fn arguments(game: &GameOptions) -> impl Iterator<Item = &str> {
    game.args.split_whitespace()
}

/// The options as one line that the UI shows: the environment, then the arguments.
pub fn summary(wine: &WineOptions, game: &GameOptions) -> String {
    let mut parts: Vec<String> = environment(wine, game).into_iter().map(|(name, value)| format!("{name}={value}")).collect();
    parts.extend(arguments(game).map(str::to_owned));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_without_its_option_leaves_the_options_at_their_defaults() {
        let reason = GameOptions { eye_per_pass_reason: "because".into(), ..Default::default() };
        assert!(reason.is_default());
        assert!(!GameOptions { eye_per_pass: true, ..reason.clone() }.is_default());
        assert!(!GameOptions { args: "-x".into(), ..reason }.is_default());
    }

    #[test]
    fn default_options_only_keep_the_mac_window_empty() {
        assert_eq!(environment(&WineOptions::default(), &GameOptions::default()), [("DXMT_SKIP_PRESENT".to_owned(), "1".to_owned())]);
        let wine = WineOptions { mac_window: true, ..Default::default() };
        assert!(environment(&wine, &GameOptions::default()).is_empty());
        assert_eq!(arguments(&GameOptions::default()).count(), 0);
    }

    #[test]
    fn a_game_s_options_reach_its_environment_and_arguments() {
        let game = GameOptions {
            eye_per_pass: true,
            dll_overrides: "winhttp=n,b".into(),
            env: "FOO=1  BAR=two =skipped noequals".into(),
            args: "fpfc  --verbose".into(),
            ..Default::default()
        };
        let vars = environment(&WineOptions { mac_window: true, ..Default::default() }, &game);
        assert_eq!(
            vars,
            [
                ("BASALT_MAX_ARRAY_SIZE".to_owned(), "1".to_owned()),
                ("WINEDLLOVERRIDES".to_owned(), "winhttp=n,b".to_owned()),
                ("FOO".to_owned(), "1".to_owned()),
                ("BAR".to_owned(), "two".to_owned()),
            ]
        );
        assert_eq!(arguments(&game).collect::<Vec<_>>(), ["fpfc", "--verbose"]);
    }

    #[test]
    fn overrides_for_every_program_and_for_one_compose_and_its_variables_come_last() {
        let wine = WineOptions { mac_window: true, dll_overrides: "dxgi=n".into(), env: "LEVEL=1".into() };
        let game = GameOptions { dll_overrides: " winhttp=n,b ".into(), env: "LEVEL=2".into(), ..Default::default() };
        let vars = environment(&wine, &game);
        assert_eq!(vars[0], ("WINEDLLOVERRIDES".to_owned(), "dxgi=n;winhttp=n,b".to_owned()));
        assert_eq!(vars.iter().rev().find(|(name, _)| name == "LEVEL").map(|(_, value)| value.as_str()), Some("2"));
        assert_eq!(summary(&wine, &game), "WINEDLLOVERRIDES=dxgi=n;winhttp=n,b LEVEL=1 LEVEL=2");
    }
}
