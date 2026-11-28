//! A game whose picture stays black: the right eye alone, or both eyes. The host looks at the game's
//! pictures and sends a `black_eye` status line once for each game run, with the eye "right" or
//! "both" (host/src/eye_check.rs). The app answers in the same way for both. Its answer depends on
//! where the game draws its eyes ([`EyeImages`]).
//!
//! An OpenXR game draws into the runtime's swapchains. The app answers with the launch option "Each
//! eye in its own pass": it turns the option on for the program, records why, shows a notice and
//! starts the game again. When cranvr turned that option on and the picture is still black with it,
//! the game draws both eyes into a texture of its own, which the option cannot reach. The app then
//! turns the option off and turns on "Both eyes in one pass" (DXMT reports that a vertex shader may
//! write the slice), and starts the game again.
//!
//! An OpenVR game hands OpenComposite or SteamVR a texture of its own (a Unity game with the OpenVR
//! provider draws both eyes into one). "Each eye in its own pass" only changes the runtime's
//! swapchains, so it cannot help such a game. The app turns on "Both eyes in one pass" at once.
//!
//! When the picture is still black with "Both eyes in one pass", the app says so once and changes
//! nothing, so no run ends in a loop. When the user turned an option off by hand, or turned
//! "Each eye in its own pass" on by hand, the choice stays the user's: the app only says how to fix
//! the picture.
//!
//! The option is data of the program's launch options, as the Play tab shows it. Nothing here
//! depends on which game it is.

use crate::{launch::GameOptions, steam::Game};
use std::{collections::BTreeMap, time::Duration};

/// The reason that the Options panel shows beside "Each eye in its own pass".
pub const REASON: &str = "cranvr turned this on because the picture stayed black.";

/// The reason that the Options panel shows beside "Both eyes in one pass" after "Each eye in its own
/// pass" did not help.
pub const REASON_ONE_PASS: &str = "cranvr turned this on because the picture stayed black with \"Each eye in its own pass\".";

/// The reason that the Options panel shows beside "Both eyes in one pass" for an OpenVR game.
pub const REASON_OPENVR: &str = "cranvr turned this on because the picture stayed black. The game hands its eyes to OpenVR in a texture of its own.";

/// How long the notice stays in the window and in the console in VR.
pub const NOTICE_TIME: Duration = Duration::from_secs(120);

/// Where a game draws its eyes in the run that the report is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EyeImages {
    /// The runtime's swapchains: an OpenXR game. "Each eye in its own pass" makes the runtime refuse
    /// swapchains of two slices.
    Swapchains,
    /// A texture of the game's own, which the game hands to an OpenVR runtime: OpenComposite or
    /// SteamVR. Only "Both eyes in one pass" reaches the game's draws.
    GameTexture,
}

impl EyeImages {
    /// `name` is the game's name on the host's line: cranvr's SteamVR driver names itself there.
    /// `opencomposite` says whether OpenComposite took the game's OpenVR calls in this run
    /// (game_run.rs). Any other game is an OpenXR game, whatever OpenVR runtime its options choose.
    pub fn of(name: Option<&str>, opencomposite: bool) -> Self {
        if opencomposite || name == Some(crate::openvr::STEAMVR_DRIVER) {
            EyeImages::GameTexture
        } else {
            EyeImages::Swapchains
        }
    }
}

/// One of the two eye options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// "Each eye in its own pass" (`GameOptions::eye_per_pass`)
    Each,
    /// "Both eyes in one pass" (`GameOptions::eye_one_pass`)
    Both,
}

impl Pass {
    /// The option's name in the Options panel
    pub fn label(self) -> &'static str {
        match self {
            Pass::Each => "Each eye in its own pass",
            Pass::Both => "Both eyes in one pass",
        }
    }

    /// The option that a black picture asks for first
    fn first(images: EyeImages) -> Self {
        match images {
            EyeImages::Swapchains => Pass::Each,
            EyeImages::GameTexture => Pass::Both,
        }
    }
}

/// What a report changed, and what the app does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The option `pass` was off. It is on now, with its reason. The game starts again.
    Restart { program: String, pass: Pass },
    /// "Each eye in its own pass" was on (cranvr's choice) and the picture stays black. It is off now
    /// and "Both eyes in one pass" is on. The game starts again.
    Switch { program: String },
    /// The option `pass` was on already: the picture stays black with it. Nothing changes.
    Leave { program: String, pass: Pass },
    /// The user turned the option `pass` off by hand. Nothing changes.
    Declined { program: String, pass: Pass },
    /// The host named no program, so no option can change. `pass` is the option that would help.
    Unknown { pass: Pass },
}

/// Applies a report to the launch options. `known` holds the programs that the Play tab lists, whose
/// spelling a program takes (see [`crate::launch::options_key`]).
pub fn apply(launch: &mut BTreeMap<String, GameOptions>, known: &[String], program: Option<&str>, images: EyeImages) -> Outcome {
    let pass = Pass::first(images);
    let Some(program) = program.filter(|program| !program.is_empty()) else { return Outcome::Unknown { pass } };
    let key = crate::launch::options_key(launch, known, program);
    let options = launch.entry(key.clone()).or_default();
    if options.eye_one_pass {
        return Outcome::Leave { program: key, pass: Pass::Both };
    }
    if options.eye_per_pass {
        // Only cranvr's own choice gives way to the other option: a user's choice stays
        if !options.eye_per_pass_reason.is_empty() && !options.eye_one_pass_declined {
            options.eye_per_pass = false;
            options.eye_per_pass_reason.clear();
            options.eye_one_pass = true;
            options.eye_one_pass_reason = REASON_ONE_PASS.to_owned();
            return Outcome::Switch { program: key };
        }
        return Outcome::Leave { program: key, pass: Pass::Each };
    }
    match pass {
        Pass::Each if options.eye_per_pass_declined => return Outcome::Declined { program: key, pass },
        Pass::Both if options.eye_one_pass_declined => return Outcome::Declined { program: key, pass },
        Pass::Each => {
            options.eye_per_pass = true;
            options.eye_per_pass_reason = REASON.to_owned();
        }
        Pass::Both => {
            options.eye_one_pass = true;
            options.eye_one_pass_reason = REASON_OPENVR.to_owned();
        }
    }
    Outcome::Restart { program: key, pass }
}

/// The two texts of a notice: one for the window and a short one for the console in VR, which
/// has one line of text for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub window: String,
    pub headset: String,
}

/// `eye` is the report's word: "both", else the right eye alone.
pub fn notice(outcome: &Outcome, name: &str, eye: &str) -> Notice {
    let name = if name.is_empty() { "the game" } else { name };
    let (what, short) = if eye == "both" {
        (format!("Both eyes of {name}"), "Both eyes")
    } else {
        (format!("The right eye of {name}"), "The right eye")
    };
    let (window, headset) = match outcome {
        Outcome::Restart { pass: Pass::Each, .. } => (
            format!("{what} stayed black. cranvr turned on \"Each eye in its own pass\". The game starts again."),
            format!("{short} stayed black. The game restarts."),
        ),
        Outcome::Restart { pass: Pass::Both, .. } => (
            format!(
                "{what} stayed black. The game hands its eyes to OpenVR in a texture of its own. cranvr turned on \"Both eyes in one pass\". The game starts again."
            ),
            format!("{short} stayed black. The game restarts."),
        ),
        Outcome::Switch { .. } => (
            format!(
                "{what} stayed black with \"Each eye in its own pass\". cranvr turned that off and turned on \"Both eyes in one pass\". The game starts again."
            ),
            "Still black. Another try. The game restarts.".to_owned(),
        ),
        Outcome::Leave { pass, .. } => (
            format!("{what} stayed black again with \"{}\". cranvr leaves the game as it is.", pass.label()),
            format!("{short} stayed black. No restart."),
        ),
        Outcome::Declined { pass, .. } => (
            format!("{what} stayed black. Turn on \"{}\" in its options in the Play tab to fix it.", pass.label()),
            format!("{short} stayed black. See the cranvr window."),
        ),
        Outcome::Unknown { pass } => (
            format!("{what} stayed black. Open the options of the game in the Play tab and turn on \"{}\".", pass.label()),
            format!("{short} stayed black. See the cranvr window."),
        ),
    };
    Notice { window, headset }
}

/// What the app does with one report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub notice: Notice,
    /// The program to start again: only when this report turned an option on
    pub restart: Option<String>,
}

/// Answers one report (`eye` is its word, "right" or "both"): changes the launch options as `apply`
/// says and words the notice. The caller saves the options and starts the program again.
pub fn respond(launch: &mut BTreeMap<String, GameOptions>, games: &[Game], program: Option<&str>, eye: &str, images: EyeImages) -> Response {
    let known: Vec<String> = games.iter().filter_map(crate::steam::program).collect();
    let outcome = apply(launch, &known, program, images);
    let (Outcome::Restart { program, .. } | Outcome::Switch { program } | Outcome::Leave { program, .. } | Outcome::Declined { program, .. }) = &outcome else {
        return Response { notice: notice(&outcome, "", eye), restart: None };
    };
    let notice = notice(&outcome, &name(games, program), eye);
    let restart = matches!(outcome, Outcome::Restart { .. } | Outcome::Switch { .. }).then(|| program.clone());
    Response { notice, restart }
}

/// The name that a notice shows for a program: the game's name in Steam, else the program's file.
pub fn name(games: &[Game], program: &str) -> String {
    let listed = games.iter().find(|game| crate::steam::program(game).is_some_and(|own| own.eq_ignore_ascii_case(program)));
    match listed {
        Some(game) => game.name.clone(),
        None => program.rsplit('\\').next().unwrap_or(program).to_owned(),
    }
}

#[cfg(test)]
#[path = "tests/black_eye.rs"]
mod tests;
