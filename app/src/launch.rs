//! What cranvr sets when it starts a Windows program. Nothing here depends on which game it is: a
//! game's own needs are options that the user sees and changes in the Play tab, and the options for
//! every program sit in its Wine card.

use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

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
    /// The user turned `eye_per_pass` off by hand. cranvr then leaves the option to the user: a
    /// black right eye gives a notice, not a change.
    pub eye_per_pass_declined: bool,
    /// Both eyes in one pass (DXMT_RT_ARRAY_INDEX_FROM_ANY_SHADER=1, cranvr's DXMT patch 0010): DXMT
    /// reports that a vertex shader may write the render target's slice. A Unity game with the OpenVR
    /// provider draws both eyes into one two-slice texture that it owns, and with the report off its
    /// instanced draws reach slice 0 only: the right eye stays black, and `eye_per_pass` cannot help,
    /// because the game's texture is not a swapchain of the runtime. The black-eye answer (black_eye.rs)
    /// turns this on when the picture stays black with `eye_per_pass` on, and turns that off.
    pub eye_one_pass: bool,
    /// Why `eye_one_pass` is on, when cranvr turned it on. A change by the user clears it.
    pub eye_one_pass_reason: String,
    /// The user turned `eye_one_pass` off by hand: cranvr leaves it to the user.
    pub eye_one_pass_declined: bool,
    /// Wine DLL overrides as WINEDLLOVERRIDES takes them, such as "winhttp=n,b" to load a
    /// winhttp.dll from the game's folder before Wine's own
    pub dll_overrides: String,
    /// The Steam app ID that the program's Steam check asks for: cranvr sets it as SteamAppId and
    /// SteamGameId. Empty: a program in a Steam library gets the ID of its install folder's manifest, any
    /// other program none. A copy of a Steam game needs the ID of the original (see [`detect`]).
    pub steam_app_id: String,
    /// Why `steam_app_id` holds what it holds, when cranvr filled it. The Options panel shows it. A
    /// change by the user clears it.
    pub steam_app_id_reason: String,
    /// More environment variables: NAME=value entries separated by spaces
    pub env: String,
    /// The program's arguments, separated by spaces
    pub args: String,
    /// The program's OpenVR calls go to OpenComposite (see openvr.rs): its own choice, or None to
    /// follow [`WineOptions::opencomposite`]
    pub opencomposite: Option<bool>,
}

impl GameOptions {
    pub fn is_default(&self) -> bool {
        // A reason belongs to the option: with the option off it is a leftover
        let mut plain = self.clone();
        if !plain.eye_per_pass {
            plain.eye_per_pass_reason.clear();
        }
        if plain.steam_app_id.is_empty() {
            plain.steam_app_id_reason.clear();
        }
        if !plain.eye_one_pass {
            plain.eye_one_pass_reason.clear();
        }
        plain == Self::default()
    }
}

/// The options of every Windows program that cranvr starts. Steam does not get them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// OpenVR games run through OpenComposite, which turns their OpenVR calls into OpenXR calls to
    /// cranvr's runtime (see openvr.rs). On by default: SteamVR is not installed, so an OpenVR game
    /// finds no runtime without it. An OpenXR game ignores the choice. A program can set its own
    /// ([`GameOptions::opencomposite`]).
    pub opencomposite: bool,
}

impl Default for WineOptions {
    fn default() -> Self {
        Self { mac_window: false, dll_overrides: String::new(), env: String::new(), opencomposite: true }
    }
}

/// Whether the program's OpenVR calls go to OpenComposite: its own choice, else the choice for every
/// program.
pub fn opencomposite(wine: &WineOptions, game: &GameOptions) -> bool {
    game.opencomposite.unwrap_or(wine.opencomposite)
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
    let steam_id = game.steam_app_id.trim();
    if !steam_id.is_empty() {
        vars.push(("SteamAppId".to_owned(), steam_id.to_owned()));
        vars.push(("SteamGameId".to_owned(), steam_id.to_owned()));
    }
    if game.eye_one_pass {
        vars.push(("DXMT_RT_ARRAY_INDEX_FROM_ANY_SHADER".to_owned(), "1".to_owned()));
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

/// The Steam app ID in `steam_appid.txt` next to a program: the file that Steam's own library uses to
/// name the game of a program that Steam did not start.
pub fn steam_appid_file(folder: &Path) -> Option<String> {
    let text = fs::read_to_string(folder.join("steam_appid.txt")).ok()?;
    let id = text.lines().next()?.trim();
    (!id.is_empty() && id.len() <= 10 && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.to_owned())
}

/// The launch options that cranvr fills in for a program that the user adds from a folder, with the
/// reason that the Options panel shows. It reads the program's own folder: `steam_appid.txt` gives the
/// Steam app ID. `copy_of` is the name and the app ID of a Steam game that this program is a copy of (see
/// steam.rs): the ID is then that game's. The user can change the value, and a change clears its reason.
///
/// cranvr sets no Wine DLL override. A DLL of a mod loader in the folder (`winhttp.dll` of a loader that
/// patches the game's own files, such as BSIPA) starts the loader twice when an override names it. A loader
/// that needs the override gets it from the user.
pub fn detect(program: &Path, copy_of: Option<(&str, &str)>) -> GameOptions {
    let mut options = GameOptions::default();
    let Some(folder) = program.parent() else { return options };
    if let Some(id) = steam_appid_file(folder) {
        options.steam_app_id = id;
        options.steam_app_id_reason = "cranvr took this from steam_appid.txt in the program's folder.".to_owned();
    } else if let Some((name, id)) = copy_of {
        options.steam_app_id = id.to_owned();
        options.steam_app_id_reason = format!(
            "cranvr took this from {name} in your Steam library. The program's folder holds the same files. A copy of a Steam game asks Steam for the ID of the original."
        );
    }
    options
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
    fn a_program_follows_the_choice_for_every_program_until_it_has_its_own() {
        let on = WineOptions::default();
        let off = WineOptions { opencomposite: false, ..Default::default() };
        let own = |choice| GameOptions { opencomposite: choice, ..Default::default() };
        assert!(on.opencomposite, "OpenComposite starts on");
        assert!(opencomposite(&on, &own(None)));
        assert!(!opencomposite(&off, &own(None)));
        assert!(!opencomposite(&on, &own(Some(false))));
        assert!(opencomposite(&off, &own(Some(true))));
        assert!(own(None).is_default());
        assert!(!own(Some(true)).is_default());
        // The choice reaches no environment variable: the OpenVR path file carries it
        assert_eq!(environment(&off, &own(Some(true))), environment(&on, &own(Some(false))));
    }

    #[test]
    fn settings_saved_before_the_choice_existed_keep_it_on() {
        let wine: WineOptions = serde_json::from_str(r#"{"mac_window": true}"#).unwrap();
        assert!(wine.opencomposite && wine.mac_window);
        let game: GameOptions = serde_json::from_str(r#"{"args": "-x"}"#).unwrap();
        assert_eq!(game.opencomposite, None);
    }

    #[test]
    fn default_options_only_keep_the_mac_window_empty() {
        assert_eq!(environment(&WineOptions::default(), &GameOptions::default()), [("DXMT_SKIP_PRESENT".to_owned(), "1".to_owned())]);
        let wine = WineOptions { mac_window: true, ..Default::default() };
        assert!(environment(&wine, &GameOptions::default()).is_empty());
        assert_eq!(arguments(&GameOptions::default()).count(), 0);
    }

    #[test]
    fn both_eyes_in_one_pass_reaches_the_environment_and_a_reason_without_it_is_a_leftover() {
        let game = GameOptions { eye_one_pass: true, eye_one_pass_reason: "because".into(), ..Default::default() };
        let vars = environment(&WineOptions::default(), &game);
        assert!(vars.contains(&("DXMT_RT_ARRAY_INDEX_FROM_ANY_SHADER".to_owned(), "1".to_owned())));
        assert!(!vars.iter().any(|(name, _)| name == "BASALT_MAX_ARRAY_SIZE"));
        assert!(!game.is_default());
        let reason = GameOptions { eye_one_pass_reason: "because".into(), ..Default::default() };
        assert!(reason.is_default());
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
        let wine = WineOptions { mac_window: true, dll_overrides: "dxgi=n".into(), env: "LEVEL=1".into(), ..Default::default() };
        let game = GameOptions { dll_overrides: " winhttp=n,b ".into(), env: "LEVEL=2".into(), ..Default::default() };
        let vars = environment(&wine, &game);
        assert_eq!(vars[0], ("WINEDLLOVERRIDES".to_owned(), "dxgi=n;winhttp=n,b".to_owned()));
        assert_eq!(vars.iter().rev().find(|(name, _)| name == "LEVEL").map(|(_, value)| value.as_str()), Some("2"));
        assert_eq!(summary(&wine, &game), "WINEDLLOVERRIDES=dxgi=n;winhttp=n,b LEVEL=1 LEVEL=2");
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cranvr-launch-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_mod_loader_s_dll_in_the_folder_gets_no_override() {
        let dir = scratch("loader");
        fs::write(dir.join("Game.exe"), b"x").unwrap();
        fs::write(dir.join("winhttp.dll"), b"x").unwrap();
        assert_eq!(detect(&dir.join("Game.exe"), None), GameOptions::default());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_steam_app_id_comes_from_the_folder_first_then_from_the_copied_game() {
        let dir = scratch("appid");
        let program = dir.join("Game.exe");
        fs::write(&program, b"x").unwrap();
        let from_copy = detect(&program, Some(("Some Game", "123")));
        assert_eq!(from_copy.steam_app_id, "123");
        assert!(from_copy.steam_app_id_reason.contains("Some Game"));
        fs::write(dir.join("steam_appid.txt"), "480\r\n").unwrap();
        let from_file = detect(&program, Some(("Some Game", "123")));
        assert_eq!(from_file.steam_app_id, "480");
        assert!(from_file.steam_app_id_reason.contains("steam_appid.txt"));
        // A file that holds no number is no ID
        fs::write(dir.join("steam_appid.txt"), "not a number").unwrap();
        assert_eq!(steam_appid_file(&dir), None);
        fs::write(dir.join("steam_appid.txt"), "").unwrap();
        assert_eq!(steam_appid_file(&dir), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_steam_app_id_reaches_the_environment_and_a_reason_without_it_is_a_leftover() {
        let game = GameOptions { steam_app_id: " 620 ".into(), steam_app_id_reason: "because".into(), ..Default::default() };
        let vars = environment(&WineOptions::default(), &game);
        assert!(vars.contains(&("SteamAppId".to_owned(), "620".to_owned())));
        assert!(vars.contains(&("SteamGameId".to_owned(), "620".to_owned())));
        assert!(!game.is_default());
        assert!(GameOptions { steam_app_id_reason: "because".into(), ..Default::default() }.is_default());
        assert!(!GameOptions { dll_overrides: "winhttp=n,b".into(), ..Default::default() }.is_default());
        let old: GameOptions = serde_json::from_str(r#"{"dll_overrides": "winhttp=n,b"}"#).unwrap();
        assert!(old.steam_app_id.is_empty() && old.steam_app_id_reason.is_empty());
    }
}
