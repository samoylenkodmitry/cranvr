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
    /// turns this on at once for a game in OpenComposite or SteamVR. For an OpenXR game it turns this on
    /// when the picture stays black with `eye_per_pass` on, and turns that off.
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
    /// The OpenVR runtime of the program (see openvr.rs): its own choice, or None to follow
    /// [`WineOptions::openvr`]
    pub openvr: Option<OpenVr>,
    /// Why `openvr` holds what it holds, when cranvr chose it: a game that drew nothing through OpenComposite
    /// gets SteamVR (vr_system.rs). The Play tab shows it. A change by the user clears it.
    pub openvr_reason: String,
    /// The graphics API of a game that picks one by an argument: None leaves the game its own choice and
    /// adds no argument
    pub graphics: Option<Graphics>,
    /// The arguments that make the game draw with Vulkan. Empty: the arguments of the game's engine when
    /// cranvr knows the engine (see [`engine`])
    pub vulkan_args: String,
    /// The arguments that make the game draw with Direct3D 11. Empty: the engine's, as for `vulkan_args`
    pub direct3d_args: String,
    /// The game's own window on the Mac: None follows [`WineOptions::mac_window`] (see [`window`])
    pub window: Option<Window>,
}

/// The size of a game's own window on the Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Window {
    /// A small window, by the arguments of the game's engine (see [`Engine::small_window`]). The window
    /// shows no picture while [`WineOptions::mac_window`] is off. A full-screen window would then only cover
    /// the Mac's desktop, which the headset's desktop panel shows.
    Small,
    /// The window that the game picks itself, often one that covers the whole screen
    Own,
}

impl Window {
    pub const ALL: [Window; 2] = [Window::Small, Window::Own];

    /// The word of app.json and of the console in VR
    pub fn key(self) -> &'static str {
        match self {
            Window::Small => "small",
            Window::Own => "own",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }

    /// The name that the Play tab shows
    pub fn label(self) -> &'static str {
        match self {
            Window::Small => "Small",
            Window::Own => "Game's own",
        }
    }
}

/// The graphics API that a game draws with. A game that offers more than one picks it by an argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Graphics {
    /// Direct3D 11, which DXMT runs on Metal
    Direct3D,
    /// Vulkan, which MoltenVK runs on Metal. cranvr's runtime and OpenComposite take the game's Vulkan
    /// images on the GPU.
    Vulkan,
}

impl Graphics {
    pub const ALL: [Graphics; 2] = [Graphics::Direct3D, Graphics::Vulkan];

    /// The word of app.json and of the console in VR
    pub fn key(self) -> &'static str {
        match self {
            Graphics::Direct3D => "direct3d",
            Graphics::Vulkan => "vulkan",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }

    /// The name that the Play tab shows
    pub fn label(self) -> &'static str {
        match self {
            Graphics::Direct3D => "Direct3D 11",
            Graphics::Vulkan => "Vulkan",
        }
    }
}

/// A game engine whose games take one argument for each graphics API. cranvr knows the engine by its
/// files next to the program, never by the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Engine {
    pub name: &'static str,
    /// What showed the engine, for the Play tab
    pub sign: &'static str,
    pub vulkan: &'static str,
    pub direct3d: &'static str,
    /// The arguments that give a game of the engine a small window on the Mac, when cranvr measured them
    pub small_window: Option<&'static str>,
}

/// Unity's player. It keeps the size of its window in the game's settings and takes it from these arguments
/// first. The Thrill of the Fight (Unity 2021.3) opened a window of 3840x2160 that covered a 3840x2160 display
/// without them.
pub const UNITY: Engine = Engine {
    name: "Unity",
    sign: "UnityPlayer.dll or the Data folder",
    vulkan: "-force-vulkan",
    direct3d: "-force-d3d11",
    small_window: Some("-screen-fullscreen 0 -screen-width 640 -screen-height 360"),
};

/// Unreal Engine. Its documented arguments of a window (`-windowed -ResX=W -ResY=H`) are unmeasured under
/// cranvr's Wine, so a game of the engine keeps its own window until the user types them.
pub const UNREAL: Engine = Engine { name: "Unreal Engine", sign: "the folder Engine/Binaries", vulkan: "-vulkan", direct3d: "-d3d11", small_window: None };

/// Croteam's Serious Engine. It takes console variables as arguments.
pub const SERIOUS: Engine = Engine {
    name: "Serious Engine",
    sign: "Content/CachedShaders_VLK.gro",
    vulkan: "+gfx_strAPI VLK",
    direct3d: "+gfx_strAPI D3D11",
    small_window: None,
};

impl Engine {
    pub fn arguments(self, graphics: Graphics) -> &'static str {
        match graphics {
            Graphics::Direct3D => self.direct3d,
            Graphics::Vulkan => self.vulkan,
        }
    }
}

/// The engine of the program at `program` (a Mac path), when its files show one. A Unity player has
/// UnityPlayer.dll or a folder `<program>_Data` next to it. An Unreal game has the folder Engine/Binaries
/// beside its program or beside the project folder of `<project>/Binaries/Win64/<program>`. A game of
/// Croteam's Serious Engine keeps its programs in `Bin` or `Bin/x64` and its Vulkan shaders in
/// `Content/CachedShaders_VLK.gro`; it takes the console variable gfx_strAPI as an argument.
pub fn engine(program: &Path) -> Option<Engine> {
    let folder = program.parent()?;
    let stem = program.file_stem()?.to_string_lossy();
    if folder.join("UnityPlayer.dll").is_file() || folder.join(format!("{stem}_Data")).is_dir() {
        return Some(UNITY);
    }
    let unreal = [Some(folder), folder.ancestors().nth(3)].into_iter().flatten().any(|root| root.join("Engine/Binaries").is_dir());
    if unreal {
        return Some(UNREAL);
    }
    let serious = [folder.ancestors().nth(1), folder.ancestors().nth(2)].into_iter().flatten().any(|root| root.join("Content/CachedShaders_VLK.gro").is_file());
    serious.then_some(SERIOUS)
}

/// The runtime that answers a game's OpenVR calls. An OpenXR game ignores the choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenVr {
    /// OpenComposite turns the game's OpenVR calls into OpenXR calls to cranvr's runtime
    #[default]
    OpenComposite,
    /// SteamVR for Windows runs in the prefix with cranvr's driver, for a game that needs SteamVR
    /// itself: its own menus, its input bindings or its overlays
    SteamVr,
    /// No OpenVR runtime: a game with an OpenXR mode takes that
    Off,
}

impl OpenVr {
    pub const ALL: [OpenVr; 3] = [OpenVr::OpenComposite, OpenVr::SteamVr, OpenVr::Off];

    /// The word of app.json and of the console in VR
    pub fn key(self) -> &'static str {
        match self {
            OpenVr::OpenComposite => "opencomposite",
            OpenVr::SteamVr => "steamvr",
            OpenVr::Off => "off",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }

    /// The name that the Play tab shows
    pub fn label(self) -> &'static str {
        match self {
            OpenVr::OpenComposite => "OpenComposite",
            OpenVr::SteamVr => "SteamVR",
            OpenVr::Off => "Off",
        }
    }
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
        if plain.openvr.is_none() {
            plain.openvr_reason.clear();
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
    /// The OpenVR runtime of every program (see openvr.rs). OpenComposite by default: it needs no
    /// SteamVR, and an OpenVR game finds no runtime without one. An OpenXR game ignores the choice. A
    /// program can set its own ([`GameOptions::openvr`]).
    pub openvr: OpenVr,
}

impl Default for WineOptions {
    fn default() -> Self {
        Self { mac_window: false, dll_overrides: String::new(), env: String::new(), openvr: OpenVr::OpenComposite }
    }
}

/// The program's OpenVR runtime: its own choice, else the choice for every program.
pub fn openvr(wine: &WineOptions, game: &GameOptions) -> OpenVr {
    game.openvr.unwrap_or(wine.openvr)
}

/// The program's window on the Mac: its own choice, else a small window while the window shows no picture.
/// A user who wants the game's picture on the Mac keeps the game's own window.
pub fn window(wine: &WineOptions, game: &GameOptions) -> Window {
    game.window.unwrap_or(if wine.mac_window { Window::Own } else { Window::Small })
}

/// The arguments of the program's window choice: the small window of its engine, else nothing.
pub fn window_arguments(wine: &WineOptions, game: &GameOptions, engine: Option<Engine>) -> &'static str {
    match window(wine, game) {
        Window::Small => engine.and_then(|engine| engine.small_window).unwrap_or_default(),
        Window::Own => "",
    }
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

/// The arguments of the game's graphics choice: the game's own text for the choice, else the arguments of
/// its engine. None of both: nothing, and the game keeps its own choice.
pub fn graphics_arguments(game: &GameOptions, engine: Option<Engine>) -> &str {
    let Some(graphics) = game.graphics else { return "" };
    let own = match graphics {
        Graphics::Direct3D => game.direct3d_args.trim(),
        Graphics::Vulkan => game.vulkan_args.trim(),
    };
    if own.is_empty() { engine.map_or("", |engine| engine.arguments(graphics)) } else { own }
}

/// The program's arguments: its own, then the ones of its graphics choice and of its window. `engine` is the
/// program's engine (see [`engine`]).
pub fn arguments<'a>(wine: &WineOptions, game: &'a GameOptions, engine: Option<Engine>) -> impl Iterator<Item = &'a str> {
    game.args
        .split_whitespace()
        .chain(graphics_arguments(game, engine).split_whitespace())
        .chain(window_arguments(wine, game, engine).split_whitespace())
}

/// The options as one line that the UI shows: the environment, then the arguments.
pub fn summary(wine: &WineOptions, game: &GameOptions, engine: Option<Engine>) -> String {
    let mut parts: Vec<String> = environment(wine, game).into_iter().map(|(name, value)| format!("{name}={value}")).collect();
    parts.extend(arguments(wine, game, engine).map(str::to_owned));
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
#[path = "tests/launch.rs"]
mod tests;
