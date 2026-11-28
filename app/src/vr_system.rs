//! A game that drew nothing through OpenComposite (game_run.rs gives the verdict). Some OpenVR games need SteamVR
//! itself: The Lab stops in OpenComposite before its first frame. The app answers the verdict as black_eye.rs
//! answers a black picture: it sets the game's VR system to SteamVR for its next start, records why and shows a
//! notice. That needs SteamVR in the Steam library. A game that OpenComposite stopped hangs in OpenComposite's
//! message box, so the app ends it and starts it again through SteamVR at once. A game that closed its session or
//! ended by itself waits for the user's next start. A game whose VR system the user chose
//! keeps it: the notice only says what to try. The user can set the VR system back in the Play tab or in the
//! console in VR, and that choice is then the user's.
//!
//! The choice is data of the program's launch options. Nothing here depends on which game it is.

use crate::{
    black_eye::Notice,
    game_run::Nothing,
    launch::{GameOptions, OpenVr},
    steam::Game,
};
use std::collections::BTreeMap;

/// The reason that the Play tab shows beside the choice.
pub const REASON: &str = "cranvr picked SteamVR because the game drew nothing through OpenComposite.";

/// What a verdict changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The game followed the choice for every program. It runs through SteamVR now and starts again.
    Switched { program: String },
    /// SteamVR is not installed: nothing changes.
    NoSteamVr { program: String },
    /// The user chose OpenComposite for this game: nothing changes.
    Kept { program: String },
}

/// Applies a verdict to the launch options. `known` holds the programs that the Play tab lists, whose spelling
/// a program takes (see [`crate::launch::options_key`]).
pub fn apply(launch: &mut BTreeMap<String, GameOptions>, known: &[String], program: &str, steamvr: bool) -> Outcome {
    let key = crate::launch::options_key(launch, known, program);
    let own = launch.get(&key).and_then(|options| options.openvr);
    if own.is_some() {
        return Outcome::Kept { program: key };
    }
    if !steamvr {
        return Outcome::NoSteamVr { program: key };
    }
    let options = launch.entry(key.clone()).or_default();
    options.openvr = Some(OpenVr::SteamVr);
    options.openvr_reason = REASON.to_owned();
    Outcome::Switched { program: key }
}

/// The two texts of a notice: the window's and a line for the console in VR.
pub fn notice(outcome: &Outcome, name: &str, why: Nothing) -> Notice {
    let what = match why {
        Nothing::Aborted => format!("OpenComposite stopped {name} before its first picture"),
        Nothing::Closed => format!("{name} closed its VR session without a picture through OpenComposite"),
        Nothing::Ended => format!("{name} ended without a picture through OpenComposite"),
    };
    match outcome {
        Outcome::Switched { .. } if why == Nothing::Aborted => Notice {
            window: format!("{what}. cranvr set its VR system to SteamVR. The game starts again."),
            headset: "No picture. The game restarts in SteamVR.".to_owned(),
        },
        Outcome::Switched { .. } => Notice {
            window: format!("{what}. cranvr set its VR system to SteamVR for its next start."),
            headset: "No picture. Its next start uses SteamVR.".to_owned(),
        },
        Outcome::NoSteamVr { .. } => Notice {
            window: format!("{what}. It may need SteamVR: install SteamVR from your Steam library."),
            headset: "No picture. It may need SteamVR.".to_owned(),
        },
        Outcome::Kept { .. } => Notice {
            window: format!("{what}. You chose its VR system: try SteamVR in its options in the Play tab."),
            headset: "No picture. Try SteamVR for it.".to_owned(),
        },
    }
}

/// What the app does with one verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub notice: Notice,
    /// The program to start again: only when the verdict switched it to SteamVR and OpenComposite stopped it
    pub restart: Option<String>,
}

/// Answers one verdict: changes the launch options as [`apply`] says and words the notice. The caller saves the
/// options and starts the program again.
pub fn respond(launch: &mut BTreeMap<String, GameOptions>, games: &[Game], program: &str, why: Nothing, steamvr: bool) -> Response {
    let known: Vec<String> = games.iter().filter_map(crate::steam::program).collect();
    let outcome = apply(launch, &known, program, steamvr);
    let notice = notice(&outcome, &crate::black_eye::name(games, program), why);
    let restart = match outcome {
        Outcome::Switched { program } if why == Nothing::Aborted => Some(program),
        _ => None,
    };
    Response { notice, restart }
}

#[cfg(test)]
#[path = "tests/vr_system.rs"]
mod tests;
