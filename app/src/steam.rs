//! Windows Steam in cranvr's Wine prefix: its state, its game library, and starting games.

use crate::{
    appinfo,
    backend::{self, config_dir},
    game_run, launch, openvr, steam_cloud,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn prefix() -> PathBuf {
    config_dir().join("wine/prefix")
}

/// The Wine that the game support step installs, with DXMT and cranvr's OpenXR runtime
pub fn engine() -> PathBuf {
    config_dir().join("wine/engine")
}

/// Steam's start flags under Wine on macOS, as BasaltVR found them: no update at start (an update
/// would replace the web helper shim), no file check, no CEF sandbox
pub const STEAM_FLAGS: [&str; 3] = ["-inhibitbootstrap", "-noverifyfiles", "-no-cef-sandbox"];

pub fn wine_ready() -> bool {
    config_dir().join("wine/engine/bin/wine").exists()
}

/// The game support version that this build of the app expects: the date of the release with the
/// newest runtime and DXMT patches. scripts/game-support-version.txt names it, and
/// scripts/package-game-support.sh puts the same name into the archive.
pub const GAME_SUPPORT_VERSION: &str = include_str!("../../scripts/game-support-version.txt");

/// The version that setup-wine.sh stamped into the Wine folder with the last install, if any.
pub fn game_support_installed() -> Option<String> {
    let text = fs::read_to_string(config_dir().join("wine/game-support-version.txt")).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The version that this build of the app expects, as plain text.
pub fn game_support_expected() -> &'static str {
    GAME_SUPPORT_VERSION.trim()
}

/// Whether the installed game support is older than this app expects. A stack without a stamp
/// came from a release before stamps existed, so it is older. No Wine stack at all is not
/// outdated: it is missing, and the setup installs it.
pub fn game_support_outdated() -> bool {
    wine_ready() && is_older(game_support_installed().as_deref(), game_support_expected())
}

/// An error that tells the user to update the game support, while the installed one is older than
/// this app expects. Nothing starts with an older game support: its runtime and the host speak
/// different versions of the control protocol.
pub fn require_current_game_support() -> Result<(), String> {
    if game_support_outdated() {
        return Err("The game support is older than this app expects. Press Update game support in the Play tab.".into());
    }
    Ok(())
}

/// Whether `installed` is older than `expected`. Versions are dates, "2026.11.19", with an optional
/// fourth number for a second release on one day. A missing or unreadable version is older.
fn is_older(installed: Option<&str>, expected: &str) -> bool {
    let numbers = |text: &str| text.split('.').map(|part| part.parse::<u64>().ok()).collect::<Option<Vec<u64>>>();
    match installed.and_then(numbers) {
        Some(installed) => numbers(expected).is_some_and(|expected| installed < expected),
        None => true,
    }
}

pub fn steam_dir() -> PathBuf {
    prefix().join("drive_c/Program Files (x86)/Steam")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SteamState {
    /// No Wine stack
    #[default]
    NoWine,
    NotInstalled,
    /// Installed, but the web helper is missing, or it is neither Valve's helper nor a shim with a
    /// backup of Valve's helper beside it. Only the setup can repair it.
    NeedsSetup,
    /// Installed with the web helper shim, or with a helper that [`start`] replaces by the shim
    Ready,
}

pub fn state() -> SteamState {
    if !wine_ready() {
        return SteamState::NoWine;
    }
    let dir = steam_dir();
    if !dir.join("steam.exe").exists() {
        return SteamState::NotInstalled;
    }
    let Some(helper) = web_helper(&dir) else {
        return SteamState::NeedsSetup;
    };
    match helper_state(&helper, &shim()) {
        Helper::Broken => SteamState::NeedsSetup,
        Helper::Shim | Helper::Valve | Helper::OldShim => SteamState::Ready,
    }
}

/// BasaltVR's web helper shim, which the game support installs into the prefix
fn shim() -> PathBuf {
    prefix().join("drive_c/basalt/runtime/steamwebhelper_compat.exe")
}

/// What stands at the place of Steam's web helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Helper {
    /// The current shim
    Shim,
    /// Valve's helper. Steam's own update or file check put it back.
    Valve,
    /// An older shim, with a backup of Valve's helper beside it. An update of the game support
    /// brought a new shim.
    OldShim,
    /// No shim in the prefix, or a helper that is neither Valve's nor a shim with a backup
    Broken,
}

/// Where setup-steam.sh keeps Valve's helper. The shim starts it.
fn helper_backup(helper: &Path) -> PathBuf {
    helper.with_file_name("steamwebhelper.basalt-original.exe")
}

/// Valve signs its helper, so the file holds the name of Valve's company. BasaltVR's shim does not.
fn is_valve_helper(bytes: &[u8]) -> bool {
    const MARK: &[u8] = b"Valve Corp";
    bytes.windows(MARK.len()).any(|window| window == MARK)
}

fn helper_state(helper: &Path, shim: &Path) -> Helper {
    let (Ok(helper_bytes), Ok(shim_bytes)) = (fs::read(helper), fs::read(shim)) else {
        return Helper::Broken;
    };
    if helper_bytes == shim_bytes {
        Helper::Shim
    } else if is_valve_helper(&helper_bytes) {
        Helper::Valve
    } else if helper_backup(helper).is_file() {
        Helper::OldShim
    } else {
        Helper::Broken
    }
}

/// Puts the shim in place of Steam's web helper, as step 3 of setup-steam.sh does. Valve's helper
/// goes to the backup, which the shim starts. An older shim gets replaced, and the backup stays.
/// Steam must not run. The shim goes to a new file first and replaces the helper with a rename, so
/// a failed copy leaves the helper as it was. Returns a line for the log when it changed a file.
fn repair_helper(helper: &Path, shim: &Path) -> Result<Option<String>, String> {
    let found = helper_state(helper, shim);
    if found == Helper::Shim {
        return Ok(None);
    }
    if found == Helper::Broken {
        return Err("Steam's web helper needs a repair. Press Set up games in the Play tab.".into());
    }
    let fresh = helper.with_file_name("steamwebhelper.cranvr-new.exe");
    let failed = |e: std::io::Error| format!("cranvr could not put the web helper shim into Steam: {e}");
    fs::copy(shim, &fresh).map_err(failed)?;
    if found == Helper::Valve {
        fs::rename(helper, helper_backup(helper)).map_err(failed)?;
    }
    fs::rename(&fresh, helper).map_err(failed)?;
    Ok(Some(match found {
        Helper::Valve => "Steam had put Valve's web helper back. cranvr kept it as the backup and put the shim in its place.".into(),
        _ => "The game support brought a new web helper shim. cranvr put it into Steam.".into(),
    }))
}

/// Steam's 64-bit web helper: in cef.win64 in newer clients, in cef.win7x64 in older ones.
fn web_helper(steam: &Path) -> Option<PathBuf> {
    ["cef.win64", "cef.win7x64"]
        .iter()
        .map(|d| steam.join("bin/cef").join(d).join("steamwebhelper.exe"))
        .find(|p| p.exists())
}

/// Steam signed in: its config holds a remembered account after the first login.
pub fn signed_in() -> bool {
    fs::read_to_string(steam_dir().join("config/loginusers.vdf")).is_ok_and(|t| t.contains("AccountName"))
}

/// Whether Rosetta 2 is installed: Wine's macOS build is Intel code.
pub fn rosetta_ready() -> bool {
    Path::new("/Library/Apple/usr/libexec/oah/libRosettaRuntime").exists()
}

/// Installs Rosetta 2. macOS asks for an administrator's password in its own dialog.
pub fn install_rosetta() -> Result<(), String> {
    let script = "do shell script \"/usr/sbin/softwareupdate --install-rosetta --agree-to-license\" with administrator privileges";
    let output = Command::new("/usr/bin/osascript").args(["-e", script]).output().map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // osascript reports the Cancel button of the password dialog as error -128
    if stderr.contains("(-128)") {
        return Err("You cancelled the password request. Retry when you are ready to type your Mac password.".into());
    }
    Err(format!("Rosetta did not install: {}", stderr.trim()))
}

/// Whether Steam of this prefix runs. A Wine process maps its program's file, so the processes that
/// hold this prefix's steam.exe are its Steam. Another prefix's Steam does not count. lsof looks only
/// at the processes whose command line names a steam.exe: over every process of the Mac it took 0.2 s
/// of CPU time, and the window asks every 2 s.
pub fn running() -> bool {
    let Ok(named) = Command::new("pgrep").args(["-i", "-d", ",", "-f", r"steam\.exe"]).output() else { return false };
    let pids = String::from_utf8_lossy(&named.stdout).trim().to_owned();
    !pids.is_empty()
        && Command::new("lsof").args(["-a", "-t", "-p", &pids]).arg(steam_dir().join("steam.exe")).output().is_ok_and(|o| !o.stdout.is_empty())
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Game {
    /// The app ID of the game's Steam manifest. A game that the user added from a folder (see
    /// [`added_games`]) has none: it is empty, and the program's launch options may name an ID.
    pub app_id: String,
    pub name: String,
    /// The program to start: the one of the game's launch entry in Steam's app cache (appinfo.rs), else the one
    /// that [`find_exe`] finds in the install folder
    pub exe: Option<PathBuf>,
    /// The arguments of the launch entry. The program's launch options add their own after them.
    pub args: Vec<String>,
    /// The folder that the launch entry starts the program in: its `workingdir`, else the program's own folder.
    /// None for a game that Steam's cache does not know: the program's own folder too.
    pub working_dir: Option<PathBuf>,
    /// The engine that the program's files show (see [`launch::engine`]). It gives the arguments of the
    /// graphics choice in the game's options.
    pub engine: Option<launch::Engine>,
}

/// Installed games from every Steam library in the prefix, then the games that the user added from a
/// folder. A name is unique in the list, because the console in VR plays a game by its name.
pub fn games() -> Vec<Game> {
    let mut all = steam_games();
    for added in added_games(&backend::Settings::load()) {
        let taken = |name: &str, all: &[Game]| all.iter().any(|game| game.name.eq_ignore_ascii_case(name));
        let mut game = added;
        let base = game.name.clone();
        for n in 2.. {
            if !taken(&game.name, &all) {
                break;
            }
            game.name = format!("{base} ({n})");
        }
        all.push(game);
    }
    all
}

/// The games that the user added from a folder, in the order of adding. Each is a program that cranvr
/// starts like a Steam game. Its name is its folder's name unless the user changed it.
pub fn added_games(settings: &backend::Settings) -> Vec<Game> {
    settings
        .added_games
        .iter()
        .map(|added| Game { name: added.name.clone(), exe: Some(added.program.clone()), engine: launch::engine(&added.program), ..Default::default() })
        .collect()
}

/// The games of the Steam libraries, with no game that the user added.
pub fn steam_games() -> Vec<Game> {
    let mut games = Vec::new();
    for steamapps in libraries() {
        let Ok(entries) = fs::read_dir(&steamapps) else { continue };
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().to_string();
            let Some(app_id) = file.strip_prefix("appmanifest_").and_then(|f| f.strip_suffix(".acf")) else { continue };
            let text = fs::read_to_string(entry.path()).unwrap_or_default();
            let (Some(name), Some(installdir)) = (vdf_value(&text, "name"), vdf_value(&text, "installdir")) else { continue };
            // Steam's own tools (runtimes, redistributables) are not games. SteamVR is the OpenVR runtime
            // that a game's launch options choose (launch::OpenVr), so it is no game either.
            if name.starts_with("Steamworks") || name.contains("Redistributable") || name.starts_with("Proton") || app_id == STEAMVR_APP_ID {
                continue;
            }
            let folder = steamapps.join("common").join(&installdir);
            // The program that Steam starts, with its arguments and folder. A game that Steam's cache does not know
            // gets the program that the folder shows.
            let launch = app_id.parse().ok().and_then(|app| appinfo::entries(&steam_dir().join("appcache/appinfo.vdf"), app)).and_then(|entries| appinfo::choose(&entries, &folder));
            let mut game = match launch {
                Some(launch) => Game { app_id: app_id.to_string(), name, exe: Some(launch.program), args: launch.arguments, working_dir: Some(launch.working_dir), engine: None },
                None => Game { app_id: app_id.to_string(), name, exe: find_exe(&folder, &installdir), ..Default::default() },
            };
            game.engine = game.exe.as_deref().and_then(launch::engine);
            games.push(game);
        }
    }
    games.sort_by_key(|a| a.name.to_lowercase());
    games.dedup_by(|a, b| a.app_id == b.app_id);
    games
}

/// SteamVR's app ID in the Steam store
const STEAMVR_APP_ID: &str = "250820";

/// Where SteamVR for Windows lives when a Steam library of the prefix has it, as Windows programs see it.
pub fn steamvr() -> Option<openvr::SteamVr> {
    let folder = libraries().into_iter().find_map(|steamapps| {
        let text = fs::read_to_string(steamapps.join(format!("appmanifest_{STEAMVR_APP_ID}.acf"))).ok()?;
        let folder = steamapps.join("common").join(vdf_value(&text, "installdir")?);
        folder.join("bin/win64/vrserver.exe").is_file().then_some(folder)
    })?;
    let steam = steam_dir();
    Some(openvr::SteamVr {
        runtime: mac_to_windows(&folder),
        config: mac_to_windows(&steam.join("config")),
        log: mac_to_windows(&steam.join("logs")),
    })
}

/// The `steamapps` folders: the one in Steam's folder and those in libraryfolders.vdf.
fn libraries() -> Vec<PathBuf> {
    let main = steam_dir().join("steamapps");
    let mut all = vec![main.clone()];
    let text = fs::read_to_string(main.join("libraryfolders.vdf")).unwrap_or_default();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('"').filter(|f| !f.trim().is_empty()).collect();
        if fields.len() == 2 && fields[0] == "path" {
            if let Some(p) = windows_to_mac(&fields[1].replace("\\\\", "\\")) {
                let lib = p.join("steamapps");
                if !all.contains(&lib) {
                    all.push(lib);
                }
            }
        }
    }
    all
}

/// The Windows programs that a user may add as games: each program in the top folder of a game's
/// folder in a Steam library or in `C:\Games`, which no game of `games` starts. Helpers such as
/// crash reporters, uninstallers and setups stay out. At most `limit` programs, sorted by path. The
/// headset lists them in place of a file chooser, which it does not have.
pub fn found_programs(games: &[Game], limit: usize) -> Vec<PathBuf> {
    let skip = ["crash", "unins", "setup", "redist", "vcredist", "dxsetup", "launcherhelper", "report", "helper", "installer"];
    let mut roots: Vec<PathBuf> = libraries().into_iter().map(|library| library.join("common")).collect();
    roots.push(prefix().join("drive_c/Games"));
    let mut found = Vec::new();
    for root in roots {
        let Ok(folders) = fs::read_dir(&root) else { continue };
        for folder in folders.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()) {
            let Ok(files) = fs::read_dir(&folder) else { continue };
            for path in files.flatten().map(|entry| entry.path()) {
                let name = path.file_name().map(|name| name.to_string_lossy().to_lowercase()).unwrap_or_default();
                let known = games.iter().any(|game| game.exe.as_deref() == Some(path.as_path()));
                if name.ends_with(".exe") && path.is_file() && !known && !skip.iter().any(|word| name.contains(word)) {
                    found.push(path);
                }
            }
        }
    }
    found.sort();
    found.truncate(limit);
    found
}

/// C:\... is inside the prefix; Z:\ is the Mac's root.
fn windows_to_mac(path: &str) -> Option<PathBuf> {
    let (drive, rest) = path.split_once(":\\").or_else(|| path.split_once(':'))?;
    let rest = rest.replace('\\', "/");
    match drive.to_ascii_uppercase().as_str() {
        "C" => Some(prefix().join("drive_c").join(rest)),
        "Z" => Some(PathBuf::from("/").join(rest)),
        _ => None,
    }
}

fn vdf_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let fields: Vec<&str> = l.split('"').filter(|f| !f.trim().is_empty()).collect();
        (fields.len() == 2 && fields[0].eq_ignore_ascii_case(key)).then(|| fields[1].to_string())
    })
}

/// The launcher of Easy Anti-Cheat that a protected game ships. It starts the game with the protection on,
/// as the developer set it up.
const EAC_LAUNCHER: &str = "start_protected_game.exe";

/// The game's program: the launcher of Easy Anti-Cheat if the game ships it, else `<installdir>.exe` if
/// present, else the largest .exe that is not a crash handler, installer or redistributable. The search goes
/// three folders deep, as Serious Sam VR keeps its program in Bin/x64. A program in a 64-bit folder (x64,
/// Win64) goes first. A protected game starts through its launcher: the game's own program refuses to run
/// without the anti-cheat, and cranvr never starts a protected game without it.
fn find_exe(dir: &Path, installdir: &str) -> Option<PathBuf> {
    let launcher = dir.join(EAC_LAUNCHER);
    if launcher.exists() {
        return Some(launcher);
    }
    let named = dir.join(format!("{installdir}.exe"));
    if named.exists() {
        return Some(named);
    }
    let skip = ["crash", "unins", "setup", "redist", "vcredist", "dxsetup", "launcherhelper", "report"];
    let mut found = Vec::new();
    let mut folders = vec![(dir.to_path_buf(), 0)];
    while let Some((folder, depth)) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let path = entry.path();
            if path.is_dir() {
                if depth < 3 && !skip.iter().any(|s| name.contains(s)) {
                    folders.push((path, depth + 1));
                }
            } else if name.ends_with(".exe") && !skip.iter().any(|s| name.contains(s)) {
                let wide = path.components().any(|c| {
                    let c = c.as_os_str().to_string_lossy().to_lowercase();
                    c == "x64" || c == "win64"
                });
                // the top folder first, then a 64-bit folder, then the largest
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                found.push(((depth == 0, wide, size), path));
            }
        }
    }
    found.into_iter().max_by_key(|(key, _)| *key).map(|(_, path)| path)
}

/// A Wine command with cranvr's environment: the prefix, the runtime's control file and the
/// DXMT IOSurface gate. Games and Steam both get it, so a game Steam starts still finds the host.
pub fn wine(program: &str) -> Command {
    wine_with(program, backend::Settings::load().game_msaa_limit)
}

/// A game's Wine command: `wine` with the options for every program and the program's own, which
/// the Play tab shows. Steam does not get them. The OpenVR choice goes into the prefix's OpenVR path
/// file now, because the game reads that file when it starts: the game's run begins (game_run.rs), and
/// its end puts the file back. `steam_args` are the arguments of the game's launch entry in Steam, which go
/// before the program's own. `engine` gives the arguments of the graphics choice. Returns the command and the
/// run's number, which [`spawn_game`] takes.
fn game_wine(program: &str, steam_args: &[String], engine: Option<launch::Engine>) -> (Command, Option<u64>) {
    let settings = backend::Settings::load();
    let options = settings.launch.get(program).cloned().unwrap_or_default();
    let choice = launch::openvr(&settings.wine, &options);
    let steamvr = steamvr();
    let run = game_run::begin(&prefix(), program, choice, steamvr.as_ref()).map_err(backend::notify).ok();
    if choice == launch::OpenVr::SteamVr && steamvr.is_some() {
        match openvr::unblock_driver(&steam_dir().join("config/steamvr.vrsettings")) {
            Ok(true) => backend::log("SteamVR had turned cranvr's driver off after a crash. cranvr turned it on again.".into()),
            Ok(false) => {}
            Err(e) => backend::notify(format!("cranvr could not turn its SteamVR driver on again: {e}")),
        }
    }
    let mut command = wine_with(program, settings.game_msaa_limit);
    command.envs(launch::environment(&settings.wine, &options)).args(steam_args).args(launch::arguments(&settings.wine, &options, engine));
    // cranvr's SteamVR driver finds the game's render size in game-sizes.txt by the game's program. SteamVR,
    // which the game starts, inherits the variable.
    command.env("CRANVR_GAME", program);
    (command, run)
}

/// Starts a game's command and watches its run until the game ends (game_run.rs). A start that fails ends
/// the run at once, so the path file goes back.
fn spawn_game(mut command: Command, run: Option<u64>) -> Result<(), String> {
    let child = command.stdout(log_file("game.log")).stderr(log_file("game.err.log")).spawn();
    match (child, run) {
        (Ok(child), Some(run)) => {
            game_run::watch(&prefix(), run, child);
            Ok(())
        }
        (Ok(_), None) => Ok(()),
        (Err(e), run) => {
            if let Some(run) = run {
                game_run::end(&prefix(), run).ok();
            }
            Err(e.to_string())
        }
    }
}

/// The Steam app ID that a program's Steam check gets: the one in its launch options, else the one of
/// the Steam manifest that names its install folder. None when it has neither.
fn steam_id(program: &str, manifest_id: Option<&str>) -> Option<String> {
    app_id(backend::Settings::load().launch.get(program), manifest_id)
}

/// The Steam app ID of a game with these launch options: the one in the options, else `manifest_id`, the one of
/// the game's Steam manifest. None when it has neither.
pub fn app_id(options: Option<&launch::GameOptions>, manifest_id: Option<&str>) -> Option<String> {
    let own = options.map_or("", |options| options.steam_app_id.trim());
    let id = if own.is_empty() { manifest_id.unwrap_or_default().trim() } else { own };
    (!id.is_empty()).then(|| id.to_owned())
}

/// `wine` with an MSAA limit for the game: 0 leaves it to the game.
fn wine_with(program: &str, msaa_limit: u32) -> Command {
    let home = config_dir();
    let control = home.join("runtime-control.txt");
    let mut command = Command::new(home.join("wine/engine/bin/wine"));
    command
        .arg(program)
        .env("WINEPREFIX", prefix())
        .env("WINEARCH", "win64")
        .env("WINEDEBUG", "-all")
        .env("MVK_CONFIG_LOG_LEVEL", "0")
        .env("DXMT_BASALT_IOSURFACE", "1")
        .env("BASALT_CONTROL", format!("Z:{}", control.display().to_string().replace('/', "\\")));
    // DXMT reports sample counts above the limit as unsupported (cranvr's DXMT patch 0006)
    if msaa_limit > 0 {
        command.env("DXMT_MAX_SAMPLE_COUNT", msaa_limit.to_string());
    }
    command
}

fn log_file(name: &str) -> Stdio {
    fs::File::create(config_dir().join(name)).map(Stdio::from).unwrap_or(Stdio::null())
}

/// Starts Steam with the flags BasaltVR found to work under Wine on macOS: no update at start
/// (an update would replace the web helper shim), no file check, no CEF sandbox. Before the start
/// the shim takes the place of the web helper again, if Valve's helper or an older shim stands
/// there. Steam's window stays black without the shim.
pub fn start() -> Result<(), String> {
    require_current_game_support()?;
    match state() {
        SteamState::Ready => {}
        SteamState::NoWine => return Err("Set up game support first".into()),
        _ => return Err("Finish the Steam setup first".into()),
    }
    if running() {
        return Ok(());
    }
    // Steam starts OpenVR as its watchdog when SteamVR is installed, from the runtime that the path file names.
    // OpenComposite runs games only: a file that names it while no game runs gives way to SteamVR or to none.
    if !game_run::active() {
        match openvr::settle(&prefix(), steamvr().as_ref()) {
            Ok(true) => backend::log("The OpenVR path file named OpenComposite with no game running. cranvr put SteamVR or no runtime in its place for Steam.".into()),
            Ok(false) => {}
            Err(e) => backend::notify(format!("cranvr could not check the OpenVR path file: {e}")),
        }
    }
    let dir = steam_dir();
    if let Some(helper) = web_helper(&dir) {
        if let Some(line) = repair_helper(&helper, &shim())? {
            backend::log(line);
        }
    }
    wine("steam.exe")
        .args(STEAM_FLAGS)
        .current_dir(&dir)
        .stdout(log_file("steam.log"))
        .stderr(log_file("steam.err.log"))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Starts a game by its program while Steam runs. A game started by `steam.exe -applaunch`
/// would get Steam's environment; this one gets the host's control file and its app ID. The ID
/// comes from the program's launch options, else from the game's Steam manifest. A game with an ID
/// has a Steam check, so Steam starts first, with the game's cloud files ([`steam_for`]). A game with
/// none (a program that the user added from a folder, with no ID in its options) starts without Steam.
pub fn play(game: &Game) -> Result<(), String> {
    require_current_game_support()?;
    let exe = game.exe.clone().ok_or(format!("No program found for {}", game.name))?;
    let program = mac_to_windows(&exe);
    let app_id = steam_id(&program, Some(&game.app_id));
    match &app_id {
        Some(id) => steam_for(id)?,
        None if !wine_ready() => return Err("Set up game support first".into()),
        None => {}
    }
    let (mut command, run) = game_wine(&program, &game.args, game.engine);
    if let Some(id) = &app_id {
        command.env("SteamAppId", id).env("SteamGameId", id);
    }
    // The folder of Steam's launch entry, else the program's own
    command.current_dir(game.working_dir.as_deref().or(exe.parent()).unwrap_or(Path::new("/")));
    spawn_game(command, run)
}

/// Starts any Windows program (a Mac path) with cranvr's environment.
pub fn run_program(path: &Path) -> Result<(), String> {
    if !wine_ready() {
        return Err("Set up game support first".into());
    }
    require_current_game_support()?;
    let program = mac_to_windows(path);
    let app_id = steam_id(&program, backend::steam_app_id(path.parent().unwrap_or(path)).as_deref());
    if let Some(id) = &app_id {
        steam_for(id)?;
    }
    let (mut command, run) = game_wine(&program, &[], launch::engine(path));
    if let Some(id) = &app_id {
        command.env("SteamAppId", id).env("SteamGameId", id);
    }
    command.current_dir(path.parent().unwrap_or(Path::new("/")));
    spawn_game(command, run)
}

/// Makes Steam run with the cloud files of the game `app_id` on this Mac, before the game starts. Steam downloads
/// a game's cloud files only at its own start (steam_cloud.rs). So a Steam that runs starts again when the game's
/// cache lists files that are not in sync. After a start of Steam the game waits for Steam's look at its files.
/// A game whose files do not come does not start: the error and the game's row say why. A conflict needs the
/// user's choice in Steam, and a Steam in offline mode looks at no file, so neither waits for a new start of Steam.
fn steam_for(app_id: &str) -> Result<(), String> {
    let steam = steam_dir();
    let login = steam_cloud::login(&steam);
    let cache = login.and_then(|login| steam_cloud::Cache::of(&steam, login.account, app_id));
    let refuse = |refusal: steam_cloud::Refusal| {
        let words = refusal.words();
        steam_cloud::set_note(app_id, Some(words.clone()));
        words
    };
    steam_cloud::set_note(app_id, None);
    let files = cache.map(|cache| cache.files).unwrap_or_default();
    if files.conflict > 0 {
        return Err(refuse(steam_cloud::Refusal::Conflict { files: files.conflict }));
    }
    let waiting = files.waiting();
    let offline = login.is_some_and(|login| login.offline);
    if offline && waiting > 0 {
        return Err(refuse(steam_cloud::Refusal::Offline { waiting }));
    }
    if running() {
        if waiting == 0 {
            return Ok(());
        }
        if game_run::active() {
            return Err(refuse(steam_cloud::Refusal::GameRuns { waiting }));
        }
        backend::log(format!("Steam has {waiting} cloud files of app {app_id} that are not on this Mac. cranvr starts Steam again, which downloads them at its start."));
        if !stop_steam() {
            return Err(refuse(steam_cloud::Refusal::SteamStays));
        }
    }
    // Steam in offline mode writes no line about a game's files: the game starts with the files of Steam's last look
    let look = login.filter(|login| !login.offline).map(|login| steam_cloud::Look::before_start(&steam, login.account, app_id));
    if waiting > 0 {
        steam_cloud::set_note(app_id, Some(steam_cloud::waiting_note(waiting)));
    }
    start()?;
    // Steam must answer the game's Steam check: give it time to come up
    let since = Instant::now();
    while !running() && since.elapsed() < Duration::from_secs(20) {
        thread::sleep(Duration::from_millis(500));
    }
    thread::sleep(Duration::from_secs(5));
    if let Some(look) = look {
        look.wait(steam_cloud::SYNC_TIME, running).map_err(refuse)?;
    }
    if offline && cache.is_some() {
        backend::log(format!("Steam runs in offline mode: app {app_id} starts with the cloud files of Steam's last look."));
    } else if cache.is_some() {
        backend::log(format!("The cloud files of app {app_id} are on this Mac, {:.1} s after cranvr started Steam.", since.elapsed().as_secs_f32()));
    }
    steam_cloud::set_note(app_id, None);
    Ok(())
}

/// Ends Steam as the Exit of its menu does, so Steam finishes its own work, and waits until it is gone. Returns
/// whether it ended within 30 s.
fn stop_steam() -> bool {
    wine("steam.exe").arg("-shutdown").current_dir(steam_dir()).stdout(Stdio::null()).stderr(Stdio::null()).status().ok();
    let since = Instant::now();
    while running() {
        if since.elapsed() > Duration::from_secs(30) {
            return false;
        }
        thread::sleep(Duration::from_millis(250));
    }
    true
}

/// Ends a running program and starts it again with the launch options that are saved now. The
/// program is a Windows path, as the host reports it. Steam keeps running, so the game needs no
/// new start of Steam.
pub fn restart(program: &str) -> Result<(), String> {
    let name = program.rsplit('\\').next().unwrap_or(program);
    end_program(name);
    let settings = backend::Settings::load();
    let options = settings.launch.get(program).cloned().unwrap_or_default();
    if launch::openvr(&settings.wine, &options) == launch::OpenVr::SteamVr {
        end_steamvr();
    }
    let path = windows_to_mac(program).ok_or(format!("cranvr cannot start {program} again"))?;
    match games().into_iter().find(|game| self::program(game).is_some_and(|own| own.eq_ignore_ascii_case(program))) {
        Some(game) => play(&game),
        None => run_program(&path),
    }
}

/// Ends the Windows program with this file name and waits until its process is gone. The end is cranvr's, so
/// the game's run gives no verdict for it (game_run.rs).
fn end_program(name: &str) {
    game_run::stopping();
    taskkill(name);
    wait_until_gone(name, Duration::from_secs(15));
    // The host sees the end of the old connection first, then the new game connects
    thread::sleep(Duration::from_secs(2));
}

/// SteamVR's programs. A game in SteamVR starts them, and cranvr's driver asks SteamVR to quit when the game ends.
const STEAMVR_PROGRAMS: [&str; 6] = ["vrserver.exe", "vrcompositor.exe", "vrmonitor.exe", "vrdashboard.exe", "vrwebhelper.exe", "vrstartup.exe"];

/// How long SteamVR may take to quit by itself after its game ended. It took about 3 s with The Thrill of the Fight.
const STEAMVR_QUIT: Duration = Duration::from_secs(8);

/// Waits until SteamVR has quit after its game ended, and ends its programs when it still runs after
/// [`STEAMVR_QUIT`]. A game that starts while SteamVR quits gets "Init_ShuttingDown" from SteamVR, and a Unity
/// player crashed on it. Under Wine SteamVR's quit can stall for minutes on a web helper that it fails to end.
fn end_steamvr() {
    if wait_until_gone("vrserver.exe", STEAMVR_QUIT) {
        return;
    }
    for name in STEAMVR_PROGRAMS {
        taskkill(name);
    }
    let gone = wait_until_gone("vrserver.exe", Duration::from_secs(15));
    backend::log(if gone {
        "SteamVR did not quit after its game ended. cranvr ended SteamVR before the game's new start.".into()
    } else {
        "SteamVR did not quit after its game ended, and cranvr could not end it. The game's new start may find no headset.".into()
    });
}

/// Ends every process of the Windows program with this file name.
fn taskkill(name: &str) {
    wine("taskkill").args(["/IM", name, "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).status().ok();
}

/// Waits up to `time` until no process of the Windows program `name` runs. Returns whether it is gone.
fn wait_until_gone(name: &str, time: Duration) -> bool {
    let since = Instant::now();
    while program_running(name) {
        if since.elapsed() >= time {
            return false;
        }
        thread::sleep(Duration::from_millis(250));
    }
    true
}

fn program_running(name: &str) -> bool {
    Command::new("pgrep").args(["-if", &regex_escaped(name)]).output().is_ok_and(|o| !o.stdout.is_empty())
}

/// `text` for a regular expression that matches it as it is.
pub fn regex_escaped(text: &str) -> String {
    text.chars().flat_map(|c| if "\\.+*?()[]{}|^$".contains(c) { vec!['\\', c] } else { vec![c] }).collect()
}

/// A game's program as Wine sees it, the key of its launch options.
pub fn program(game: &Game) -> Option<String> {
    game.exe.as_deref().map(mac_to_windows)
}

/// A Mac path as Wine sees it: C:\ inside the prefix, Z:\ elsewhere.
pub fn mac_to_windows(path: &Path) -> String {
    let drive_c = prefix().join("drive_c");
    match path.strip_prefix(&drive_c) {
        Ok(rest) => format!("C:\\{}", rest.display().to_string().replace('/', "\\")),
        Err(_) => format!("Z:{}", path.display().to_string().replace('/', "\\")),
    }
}

/// Adds the program at `program` (a Mac path) to the games that the user added from a folder, and fills in
/// its launch options as far as its folder shows what it needs (see [`launch::detect`]): the Steam app ID
/// of the game that it is a copy of. `steam` is the list of
/// Steam games. A value that the program's options hold already stays. The list grows by one game, named
/// after the program's folder, and the options keep the reasons for the user to read.
pub fn add_game(settings: &mut backend::Settings, program: &Path, steam: &[Game]) -> Result<(), String> {
    if !program.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("exe")) || !program.is_file() {
        return Err("Choose a Windows program: a file that ends in .exe.".into());
    }
    if settings.added_games.iter().any(|added| added.program == program) {
        return Err("This program is in the list already.".into());
    }
    if steam.iter().any(|game| game.exe.as_deref() == Some(program)) {
        return Err("Steam lists this program already. Start it from the Steam list.".into());
    }
    let name = program
        .parent()
        .and_then(Path::file_name)
        .or_else(|| program.file_stem())
        .map_or_else(|| "Windows program".to_owned(), |name| name.to_string_lossy().into_owned());
    let original = copy_of(program, steam);
    let detected = launch::detect(program, original.map(|game| (game.name.as_str(), game.app_id.as_str())));
    if !detected.is_default() {
        let options = settings.launch.entry(mac_to_windows(program)).or_default();
        if options.steam_app_id.trim().is_empty() {
            options.steam_app_id = detected.steam_app_id;
            options.steam_app_id_reason = detected.steam_app_id_reason;
        }
    }
    settings.added_games.push(backend::AddedGame { name, program: program.to_owned() });
    Ok(())
}

/// Takes the game at `program` out of the list of added games, with its launch options. No file of
/// the game changes.
pub fn remove_game(settings: &mut backend::Settings, program: &Path) {
    settings.added_games.retain(|added| added.program != program);
    settings.launch.remove(&mac_to_windows(program));
}

/// The Steam game that the program at `program` is a copy of: the same program name, and a folder that
/// holds at least four fifths of the files and folders that the game's own folder holds (and at least
/// three). A copy of a game with a mod loader holds more than the original and so passes. A Steam
/// check asks for the ID of the original, which the copy has no manifest for.
fn copy_of<'a>(program: &Path, games: &'a [Game]) -> Option<&'a Game> {
    let name = program.file_name()?.to_string_lossy().to_lowercase();
    let entries = |folder: &Path| -> Vec<String> {
        fs::read_dir(folder).into_iter().flatten().flatten().map(|entry| entry.file_name().to_string_lossy().to_lowercase()).collect()
    };
    let mine = entries(program.parent()?);
    games.iter().filter(|game| !game.app_id.is_empty()).find(|game| {
        let Some(exe) = &game.exe else { return false };
        if exe.file_name().map(|file| file.to_string_lossy().to_lowercase()).as_deref() != Some(name.as_str()) {
            return false;
        }
        let Some(original) = exe.parent().map(entries) else { return false };
        let shared = original.iter().filter(|entry| mine.contains(entry)).count();
        original.len() >= 3 && shared * 5 >= original.len() * 4
    })
}

/// Ends Steam and every game in the prefix.
pub fn stop_all() {
    game_run::stopping();
    Command::new(config_dir().join("wine/engine/bin/wineserver"))
        .arg("-k")
        .env("WINEPREFIX", prefix())
        .status()
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::GameOptions;

    #[test]
    fn games_get_the_msaa_limit_unless_it_is_off() {
        let env = |command: &Command| {
            command.get_envs().find(|(k, _)| *k == "DXMT_MAX_SAMPLE_COUNT").and_then(|(_, v)| v).map(|v| v.to_owned())
        };
        assert_eq!(env(&wine_with("game.exe", 2)).as_deref(), Some(std::ffi::OsStr::new("2")));
        assert_eq!(env(&wine_with("game.exe", 0)), None);
    }

    #[test]
    fn reads_vdf_values_and_finds_exe() {
        let text = "\"AppState\"\n{\n\t\"appid\"\t\t\"620980\"\n\t\"name\"\t\t\"Beat Saber\"\n\t\"installdir\"\t\t\"Beat Saber\"\n}\n";
        assert_eq!(vdf_value(text, "name").as_deref(), Some("Beat Saber"));
        assert_eq!(vdf_value(text, "installdir").as_deref(), Some("Beat Saber"));
        let dir = std::env::temp_dir().join(format!("cranvr-exe-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("UnityCrashHandler64.exe"), vec![0u8; 5000]).unwrap();
        fs::write(dir.join("Game.exe"), vec![0u8; 1000]).unwrap();
        assert_eq!(find_exe(&dir, "Some Game"), Some(dir.join("Game.exe")));
        fs::write(dir.join("Some Game.exe"), b"x").unwrap();
        assert_eq!(find_exe(&dir, "Some Game"), Some(dir.join("Some Game.exe")));
        fs::remove_dir_all(&dir).ok();
    }


    #[test]
    fn a_protected_game_starts_through_the_launcher_of_its_anti_cheat() {
        let dir = std::env::temp_dir().join(format!("cranvr-exe-eac-{}", std::process::id()));
        fs::create_dir_all(dir.join("EasyAntiCheat")).unwrap();
        fs::write(dir.join("Some Game.exe"), vec![0u8; 5000]).unwrap();
        fs::write(dir.join("launch.exe"), vec![0u8; 100]).unwrap();
        fs::write(dir.join("EasyAntiCheat/EasyAntiCheat_EOS_Setup.exe"), vec![0u8; 900]).unwrap();
        assert_eq!(find_exe(&dir, "Some Game"), Some(dir.join("Some Game.exe")));
        fs::write(dir.join("start_protected_game.exe"), vec![0u8; 400]).unwrap();
        assert_eq!(find_exe(&dir, "Some Game"), Some(dir.join("start_protected_game.exe")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finds_a_program_in_a_64_bit_subfolder() {
        // Serious Sam VR: Bin/SamTLH.exe (32-bit) and Bin/x64/SamTLH.exe
        let dir = std::env::temp_dir().join(format!("cranvr-exe-deep-{}", std::process::id()));
        fs::create_dir_all(dir.join("Bin/x64")).unwrap();
        fs::create_dir_all(dir.join("Redist")).unwrap();
        fs::write(dir.join("Bin/SamTLH.exe"), vec![0u8; 3000]).unwrap();
        fs::write(dir.join("Bin/x64/SamTLH.exe"), vec![0u8; 2000]).unwrap();
        fs::write(dir.join("Redist/big.exe"), vec![0u8; 9000]).unwrap();
        assert_eq!(find_exe(&dir, "Serious Sam VR The Last Hope"), Some(dir.join("Bin/x64/SamTLH.exe")));
        fs::remove_dir_all(&dir).ok();
    }

    /// A folder with the given files (folders end in a slash) and its program.
    fn folder(name: &str, entries: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cranvr-steam-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        for entry in entries {
            match entry.strip_suffix('/') {
                Some(folder) => fs::create_dir_all(dir.join(folder)).unwrap(),
                None => fs::write(dir.join(entry), b"x").unwrap(),
            }
        }
        dir
    }

    #[test]
    fn a_folder_with_the_same_files_and_more_is_a_copy_of_a_steam_game() {
        let original = folder("original", &["Some Game.exe", "Some Game_Data/", "UnityPlayer.dll", "MonoBleedingEdge/", "Crash.exe"]);
        let copy = folder("copy", &["Some Game.exe", "Some Game_Data/", "UnityPlayer.dll", "MonoBleedingEdge/", "Crash.exe", "Plugins/", "winhttp.dll"]);
        let other = folder("other", &["Some Game.exe", "readme.txt", "data.pak"]);
        let renamed = folder("renamed", &["Other.exe", "Some Game_Data/", "UnityPlayer.dll", "MonoBleedingEdge/", "Crash.exe"]);
        let steam = [Game { app_id: "620".into(), name: "Some Game".into(), exe: Some(original.join("Some Game.exe")), ..Default::default() }];
        assert_eq!(copy_of(&copy.join("Some Game.exe"), &steam), Some(&steam[0]));
        // The same program name alone is no copy: another game may call its program the same
        assert_eq!(copy_of(&other.join("Some Game.exe"), &steam), None);
        assert_eq!(copy_of(&renamed.join("Other.exe"), &steam), None);
        // A game without an app ID (one that the user added) is no original
        let added = [Game { name: "Added".into(), exe: steam[0].exe.clone(), ..Default::default() }];
        assert_eq!(copy_of(&copy.join("Some Game.exe"), &added), None);
        for dir in [original, copy, other, renamed] {
            fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn adding_a_copy_of_a_modded_game_fills_in_its_steam_app_id_and_no_override() {
        let original = folder("add-original", &["Some Game.exe", "Some Game_Data/", "UnityPlayer.dll", "MonoBleedingEdge/"]);
        let copy = folder("add-copy", &["Some Game.exe", "Some Game_Data/", "UnityPlayer.dll", "MonoBleedingEdge/", "winhttp.dll", "Plugins/"]);
        let steam = [Game { app_id: "620".into(), name: "Some Game".into(), exe: Some(original.join("Some Game.exe")), ..Default::default() }];
        let mut settings = backend::Settings::default();
        let program = copy.join("Some Game.exe");
        assert_eq!(add_game(&mut settings, &program, &steam), Ok(()));
        // The name is the folder's name
        let name = copy.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(settings.added_games, [backend::AddedGame { name: name.clone(), program: program.clone() }]);
        let options = &settings.launch[&mac_to_windows(&program)];
        assert_eq!(options.steam_app_id, "620");
        assert!(options.steam_app_id_reason.contains("Some Game"));
        // The mod loader's DLL in the folder is no reason for an override: BSIPA-like loaders start twice with one
        assert!(options.dll_overrides.is_empty());
        // The game's environment holds what its Steam check needs
        let vars = launch::environment(&settings.wine, options);
        assert!(vars.contains(&("SteamAppId".to_owned(), "620".to_owned())));
        assert!(!vars.iter().any(|(name, _)| name == "WINEDLLOVERRIDES"));
        // The second add, the original's own program and a file that is no program are refused
        assert!(add_game(&mut settings, &program, &steam).is_err());
        assert!(add_game(&mut settings, &original.join("Some Game.exe"), &steam).is_err());
        assert!(add_game(&mut settings, &copy.join("UnityPlayer.dll"), &steam).is_err());
        assert_eq!(settings.added_games.len(), 1);
        // The added game is a game like the others, with no manifest
        let games = added_games(&settings);
        assert_eq!((games[0].name.as_str(), games[0].app_id.as_str(), games[0].exe.as_deref()), (name.as_str(), "", Some(program.as_path())));
        // Removing it takes its options too
        remove_game(&mut settings, &program);
        assert!(settings.added_games.is_empty() && settings.launch.is_empty());
        for dir in [original, copy] {
            fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn a_program_with_nothing_to_fill_in_leaves_no_options_and_keeps_the_ones_it_has() {
        let dir = folder("plain", &["Plain Game.exe", "data.pak"]);
        let program = dir.join("Plain Game.exe");
        let mut settings = backend::Settings::default();
        settings.launch.insert(mac_to_windows(&program), GameOptions { args: "-x".into(), ..Default::default() });
        assert_eq!(add_game(&mut settings, &program, &[]), Ok(()));
        assert_eq!(settings.launch[&mac_to_windows(&program)], GameOptions { args: "-x".into(), ..Default::default() });
        fs::remove_dir_all(&dir).ok();
        let again = folder("plain-two", &["Plain Game.exe"]);
        let mut settings = backend::Settings::default();
        assert_eq!(add_game(&mut settings, &again.join("Plain Game.exe"), &[]), Ok(()));
        assert!(settings.launch.is_empty());
        fs::remove_dir_all(&again).ok();
    }

    #[test]
    fn game_support_is_outdated_by_its_date_and_by_a_missing_stamp() {
        assert!(is_older(None, "2026.11.19"));
        assert!(is_older(Some(""), "2026.11.19"));
        assert!(is_older(Some("garbage"), "2026.11.19"));
        assert!(is_older(Some("2026.11.16"), "2026.11.19"));
        assert!(is_older(Some("2025.12.31"), "2026.01.01"));
        assert!(is_older(Some("2026.11.19"), "2026.11.19.2"));
        assert!(!is_older(Some("2026.11.19"), "2026.11.19"));
        assert!(!is_older(Some("2026.11.19.2"), "2026.11.19"));
        assert!(!is_older(Some("2027.01.01"), "2026.11.19"));
        // The version file of this build is a date that the comparison reads
        assert!(is_older(Some("2026.11.16"), game_support_expected()));
        assert!(!is_older(Some(game_support_expected()), game_support_expected()));
    }

    #[test]
    fn a_file_name_matches_itself_in_a_regular_expression() {
        assert_eq!(regex_escaped("Beat Saber.exe"), "Beat Saber\\.exe");
        assert_eq!(regex_escaped("a(1)+b[x]"), "a\\(1\\)\\+b\\[x\\]");
    }

    #[test]
    fn converts_paths() {
        let p = prefix().join("drive_c/Program Files (x86)/Steam/steam.exe");
        assert_eq!(mac_to_windows(&p), "C:\\Program Files (x86)\\Steam\\steam.exe");
        assert_eq!(mac_to_windows(Path::new("/Users/x/a.exe")), "Z:\\Users\\x\\a.exe");
        assert_eq!(windows_to_mac("C:\\Games"), Some(prefix().join("drive_c/Games")));
        assert_eq!(windows_to_mac("Z:\\Volumes\\SSD"), Some(PathBuf::from("/Volumes/SSD")));
    }

    /// A cef folder with a shim and a helper, in a folder of the test's own
    fn helper_folder(name: &str, helper: &[u8], backup: Option<&[u8]>) -> (PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("cranvr-helper-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("steamwebhelper_compat.exe");
        let path = dir.join("steamwebhelper.exe");
        fs::write(&shim, b"MZ new shim").unwrap();
        fs::write(&path, helper).unwrap();
        if let Some(backup) = backup {
            fs::write(helper_backup(&path), backup).unwrap();
        }
        (dir, path, shim)
    }

    #[test]
    fn valves_helper_goes_to_the_backup_and_the_shim_takes_its_place() {
        let valve = b"MZ helper of Valve Corporation, new build";
        let (dir, helper, shim) = helper_folder("valve", valve, Some(b"MZ helper of Valve Corporation, old build"));
        assert_eq!(helper_state(&helper, &shim), Helper::Valve);
        let line = repair_helper(&helper, &shim).unwrap();
        assert!(line.is_some_and(|line| line.contains("Valve's web helper")));
        assert_eq!(fs::read(&helper).unwrap(), b"MZ new shim");
        assert_eq!(fs::read(helper_backup(&helper)).unwrap(), valve);
        assert!(!helper.with_file_name("steamwebhelper.cranvr-new.exe").exists());
        assert_eq!(helper_state(&helper, &shim), Helper::Shim);
        assert_eq!(repair_helper(&helper, &shim).unwrap(), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_older_shim_gets_the_new_one_and_the_backup_stays() {
        let valve = b"MZ helper of Valve Corporation";
        let (dir, helper, shim) = helper_folder("old-shim", b"MZ old shim", Some(valve));
        assert_eq!(helper_state(&helper, &shim), Helper::OldShim);
        assert!(repair_helper(&helper, &shim).unwrap().is_some_and(|line| line.contains("new web helper shim")));
        assert_eq!(fs::read(&helper).unwrap(), b"MZ new shim");
        assert_eq!(fs::read(helper_backup(&helper)).unwrap(), valve);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_helper_without_a_backup_or_a_missing_shim_needs_the_setup() {
        let (dir, helper, shim) = helper_folder("unknown", b"MZ some other program", None);
        assert_eq!(helper_state(&helper, &shim), Helper::Broken);
        assert!(repair_helper(&helper, &shim).is_err());
        assert_eq!(fs::read(&helper).unwrap(), b"MZ some other program");
        fs::remove_file(&shim).unwrap();
        fs::write(helper_backup(&helper), b"MZ helper of Valve Corporation").unwrap();
        assert_eq!(helper_state(&helper, &shim), Helper::Broken);
        assert_eq!(helper_state(&dir.join("missing.exe"), &shim), Helper::Broken);
        fs::remove_dir_all(&dir).ok();
    }

    /// The start path of the app with the user's own prefix: Steam must not run, and the test leaves
    /// it running. CRANVR_TEST_HELPER names the file that the test puts at the helper's place first,
    /// such as the backup of Valve's helper. [`start`] must put the shim back, log one line and
    /// start Steam. Run with `cargo test -p cranvr-app -- --ignored the_start_puts_the_shim_back`.
    #[test]
    #[ignore]
    fn the_start_puts_the_shim_back() {
        use crate::backend::{self, Model};
        let source = PathBuf::from(std::env::var("CRANVR_TEST_HELPER").expect("CRANVR_TEST_HELPER names the helper to put in"));
        assert!(!running(), "Steam must not run");
        let helper = web_helper(&steam_dir()).expect("Steam has a web helper");
        let backup = fs::read(helper_backup(&helper)).ok();
        fs::copy(&source, &helper).unwrap();
        let put_in = helper_state(&helper, &shim());
        assert!(matches!(put_in, Helper::Valve | Helper::OldShim), "the test helper is Valve's or an older shim: {put_in:?}");
        assert_eq!(state(), SteamState::Ready, "the Play tab shows no repair button");
        start().expect("Steam starts");
        assert_eq!(helper_state(&helper, &shim()), Helper::Shim);
        let now = fs::read(helper_backup(&helper)).ok();
        match put_in {
            Helper::Valve => assert_eq!(now, fs::read(&source).ok(), "Valve's helper is the backup"),
            _ => assert_eq!(now, backup, "the backup stays"),
        }
        let since = Instant::now();
        while !running() && since.elapsed() < Duration::from_secs(20) {
            thread::sleep(Duration::from_millis(500));
        }
        assert!(running(), "Steam runs");
        let mut model = Model::default();
        backend::pump(&mut model);
        let lines: Vec<&String> = model.log.iter().filter(|line| line.contains("web helper")).collect();
        println!("log: {lines:?}");
        assert_eq!(lines.len(), 1, "one line in the log");
    }

    /// A test host of the app on port 9962 with the probe as the headset at 72 Hz, as the Play tab's host. The
    /// user's host keeps port 9950, and the probe's pointer script moves no real mouse. Returns the probe.
    fn test_host(seconds: u32) -> std::process::Child {
        use crate::backend::{self, Settings};
        let release = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/release");
        std::env::set_var("CRANVR_HOST_BIN", release.join("cranvr-host"));
        std::env::set_var("CRANVR_TEST_TRUST_ALL", "1");
        std::env::set_var("CRANVR_NATIVE_PORT", "9962");
        std::env::set_var("CRANVR_POINTER_DRY_RUN", "1");
        let mut settings = Settings::load();
        settings.fps = 72;
        settings.desktop = false;
        settings.virtual_display = None;
        settings.unscale_displays = false;
        settings.save();
        backend::start_host(&settings).expect("host starts");
        thread::sleep(Duration::from_secs(2));
        Command::new(release.join("cranvr-probe"))
            .args(["--seconds", &seconds.to_string(), "--tcp", "--connect", "127.0.0.1:9962", "--quest-view", "--any-picture"])
            .stdout(log_file("probe.log"))
            .spawn()
            .expect("probe runs")
    }

    /// Pumps the host's lines into `model` until `done` says yes or `limit` passes. Returns the most game frames
    /// of a stats window.
    fn pump_until(model: &mut crate::backend::Model, limit: Duration, mut done: impl FnMut(&crate::backend::Model, u64) -> bool) -> u64 {
        let since = Instant::now();
        let mut most = 0;
        while since.elapsed() < limit {
            thread::sleep(Duration::from_millis(200));
            crate::backend::pump(model);
            most = most.max(model.stats.as_ref().map_or(0, |s| s.game_frames));
            if done(model, most) {
                break;
            }
        }
        most
    }

    /// Waits up to `limit` for the path file to say `text`.
    fn path_file_becomes(text: &Option<String>, limit: Duration) -> bool {
        let file = openvr::path_file(&prefix()).unwrap();
        let since = Instant::now();
        while since.elapsed() < limit {
            if &fs::read_to_string(&file).ok() == text {
                return true;
            }
            thread::sleep(Duration::from_millis(250));
        }
        false
    }

    /// The Play button's path while Steam does not run (#115): [`play`] starts Steam with the app's flags, then the
    /// game with the path file at OpenComposite, the default. Steam starts OpenVR as its watchdog about 8 s after
    /// its start and finds OpenComposite. OpenComposite must refuse it with an error and no message box, so Steam
    /// keeps running past its 15-second stall limit. The game's frames reach the host, and the game's end puts the
    /// path file back. Then an OpenVR program through OpenComposite ([`run_program`] with openvr_smoke.exe) must
    /// draw, and its end must put the file back too. CRANVR_HOME names a scratch folder whose `wine` is the user's
    /// Wine folder (a link), so the user's app.json and host stay untouched. CRANVR_TEST_GAME names the Steam
    /// game. Needs `cargo build --release`. Run with `cargo test --release -p cranvr-app -- --ignored
    /// play_starts_steam`. Steam keeps running after the test, with the test's environment.
    #[test]
    #[ignore]
    fn play_starts_steam_and_the_game_while_the_path_file_names_opencomposite() {
        use crate::backend::{self, Model};
        let home = PathBuf::from(std::env::var("CRANVR_HOME").expect("CRANVR_HOME names a scratch folder"));
        assert!(home.join("wine").is_symlink(), "the scratch folder's wine is a link to the user's");
        let name = std::env::var("CRANVR_TEST_GAME").expect("CRANVR_TEST_GAME names the Steam game");
        assert!(!running(), "Steam must not run: the test starts it as the Play button does");
        let file = openvr::path_file(&prefix()).unwrap();
        let before = fs::read_to_string(&file).ok();
        assert!(!before.as_deref().is_some_and(|text| text.contains("opencomposite")), "the file names no OpenComposite before");
        let oc_log = file.parent().unwrap().parent().unwrap().join("OpenComposite/logs/opencomposite.log");
        let steam_console = steam_dir().join("logs/console_log.txt");
        let console_before = fs::metadata(&steam_console).map_or(0, |m| m.len());
        let mut probe = test_host(150);
        let game = games().into_iter().find(|game| game.name == name).expect("the game is installed");
        let program = self::program(&game).unwrap();
        let started = Instant::now();
        play(&game).expect("Steam and the game start");
        println!("play returned after {:.1} s", started.elapsed().as_secs_f64());
        let during = fs::read_to_string(&file).ok();
        let mut model = Model::default();
        // Steam's stall limit is 15 s: it must run 25 s after its start with a game frame
        let frames = pump_until(&mut model, Duration::from_secs(90), |_, most| most > 100 && started.elapsed() > Duration::from_secs(25));
        let steam_alive = running();
        let refused = fs::read_to_string(&oc_log).unwrap_or_default();
        let console = fs::read(&steam_console).map(|bytes| String::from_utf8_lossy(&bytes[console_before as usize..]).into_owned()).unwrap_or_default();
        end_program(program.rsplit('\\').next().unwrap());
        let game_back = path_file_becomes(&before, Duration::from_secs(10));
        println!("game frames in a stats window: {frames}; Steam runs {:.0} s after its start: {steam_alive}", started.elapsed().as_secs_f64());
        println!("path file during the game: {during:?}");
        println!("OpenComposite's log: {:?}", refused.lines().filter(|line| line.contains("refused") || line.contains("Steam")).collect::<Vec<_>>());
        println!("Steam's console log: {:?}", console.lines().filter(|line| line.contains("VR") || line.contains("assert") || line.contains("stall")).collect::<Vec<_>>());
        // An OpenVR program through OpenComposite, from the app's start path. The game's last stats window does
        // not count for it.
        let smoke = prefix().join("drive_c/opencomposite/openvr_smoke.exe");
        run_program(&smoke).expect("the program starts");
        backend::pump(&mut model);
        model.stats = None;
        let smoke_during = fs::read_to_string(&file).ok();
        let smoke_frames = pump_until(&mut model, Duration::from_secs(30), |_, most| most > 100);
        let smoke_log = fs::read_to_string(&oc_log).unwrap_or_default();
        end_program("openvr_smoke.exe");
        let smoke_back = path_file_becomes(&before, Duration::from_secs(10));
        println!("openvr_smoke: {smoke_frames} game frames in a stats window; path file during it: {smoke_during:?}");
        probe.kill().ok();
        probe.wait().ok();
        backend::stop_host();
        assert!(during.as_deref().is_some_and(|text| text.contains("opencomposite")), "the game's run names OpenComposite");
        assert!(frames > 100, "the game's frames reach the host: {frames}");
        assert!(steam_alive, "Steam runs past its stall limit");
        assert!(!console.contains("Fatal assert"), "Steam reports no fatal assert");
        assert!(game_back, "the game's end puts the path file back");
        assert!(smoke_during.as_deref().is_some_and(|text| text.contains("opencomposite")));
        assert!(smoke_frames > 100, "the OpenVR program draws through OpenComposite: {smoke_frames}");
        assert!(smoke_log.contains("C:\\opencomposite\\opencomposite.ini"), "OpenComposite ran the program");
        assert!(smoke_back, "the program's end puts the path file back");
    }

    /// The lit samples of the host's last eye check line in the window's log, as (left, right)
    fn last_eye_check(model: &crate::backend::Model) -> Option<(u32, u32)> {
        model.log.iter().rev().find_map(|line| eye_check_of(line))
    }

    /// "Eye check: 2397 of 4096 samples lit in the left eye, 2392 in the right eye", after the time of day
    fn eye_check_of(line: &str) -> Option<(u32, u32)> {
        let (_, rest) = line.split_once("Eye check: ")?;
        let numbers: Vec<u32> = rest.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect();
        rest.contains("samples lit").then_some(())?;
        Some((*numbers.first()?, *numbers.get(2)?))
    }

    #[test]
    fn the_host_s_eye_check_line_gives_both_eyes() {
        assert_eq!(eye_check_of("14:43:16.650 [INFO] Eye check: 2397 of 4096 samples lit in the left eye, 2392 in the right eye"), Some((2397, 2392)));
        assert_eq!(eye_check_of("14:43:16.650 [WARN] Eye check: cannot write the left eye to /tmp: denied"), None);
    }

    /// Starts the probe again for the next part of a test: the host takes one headset at a time.
    fn next_probe(old: &mut std::process::Child, args: &[&str]) -> std::process::Child {
        old.kill().ok();
        old.wait().ok();
        thread::sleep(Duration::from_secs(1));
        let release = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/release");
        Command::new(release.join("cranvr-probe"))
            .args(["--tcp", "--connect", "127.0.0.1:9962", "--quest-view", "--any-picture"])
            .args(args)
            .stdout(log_file("probe.log"))
            .spawn()
            .expect("probe runs")
    }

    /// Waits until no process of the game's folder runs and the path file says `text` again.
    fn run_ends(folder: &str, text: &Option<String>) -> bool {
        let since = Instant::now();
        while since.elapsed() < Duration::from_secs(30) && crate::game_run::active() {
            thread::sleep(Duration::from_millis(250));
        }
        !crate::game_run::active() && path_file_becomes(text, Duration::from_secs(5)) && !program_running(folder)
    }

    /// What a game drew in a phase of a test: the most game frames of a stats window and the lit samples of
    /// both eyes, counted after the game connected under its own program, and that program.
    #[derive(Debug, Default)]
    struct Drawn {
        frames: u64,
        eyes: Option<(u32, u32)>,
        program: Option<String>,
    }

    impl Drawn {
        fn lit(&self) -> bool {
            self.frames > 100 && self.eyes.is_some_and(|(l, r)| l >= 8 && r >= 8)
        }
    }

    /// Waits until the game whose program ends in `suffix` draws: it connects under that program (cranvr's
    /// SteamVR driver names the game after SteamVR's own room drew), and then a stats window counts over 100 of
    /// its frames and the eye check lights both eyes. The stats and the log of the phase before do not count. The
    /// wait goes on to 25 s after the connection, for a game that starts in the dark, and keeps the brightest
    /// eyes. With CRANVR_EYE_DUMP the eyes of that check are kept as `<name>-left.png` and `<name>-right.png`.
    fn draws(model: &mut crate::backend::Model, suffix: &str, limit: Duration, name: &str) -> Drawn {
        let suffix = suffix.to_ascii_lowercase();
        let since = Instant::now();
        let mut connected: Option<Instant> = None;
        let mut drawn = Drawn::default();
        while since.elapsed() < limit {
            thread::sleep(Duration::from_millis(200));
            crate::backend::pump(model);
            let ours = model.game_program.as_ref().filter(|program| program.to_ascii_lowercase().ends_with(&suffix)).cloned();
            if ours.is_none() {
                model.stats = None;
                model.log.clear();
                connected = None;
                continue;
            }
            let at = *connected.get_or_insert_with(Instant::now);
            drawn.program = ours;
            drawn.frames = drawn.frames.max(model.stats.as_ref().map_or(0, |s| s.game_frames));
            let now = last_eye_check(model);
            let brighter = now.is_some_and(|(l, r)| drawn.eyes.is_none_or(|(bl, br)| l.min(r) > bl.min(br)));
            if brighter {
                drawn.eyes = now;
                if let Some(dump) = std::env::var_os("CRANVR_EYE_DUMP").map(PathBuf::from) {
                    for eye in ["left", "right"] {
                        fs::copy(dump.join(format!("eye-{eye}.png")), dump.join(format!("{name}-{eye}.png"))).ok();
                    }
                }
            }
            if drawn.lit() && at.elapsed() >= Duration::from_secs(25) {
                break;
            }
        }
        drawn
    }

    /// #113 from the app's own paths. A scratch `CRANVR_HOME` whose `wine` links to the user's, with no launch
    /// options for the game, the user's Steam running, a test host on port 9962 and the probe as the headset:
    /// 1. The game (CRANVR_TEST_GAME, The Lab) is listed with the program, the arguments and the folder of its
    ///    launch entry in Steam's app cache.
    /// 2. [`play`], as the Play tab does: the game follows the setting, OpenComposite. It must draw nothing
    ///    through OpenComposite, and the verdict (game_run.rs) must set SteamVR with its reason and start the
    ///    game again, as the window does. The game must then draw through SteamVR, both eyes lit.
    /// 3. A request of the console from the probe (`--console`), as the headset's console sends it: the game
    ///    starts through SteamVR at once and draws.
    /// 4. A game added from a folder (CRANVR_TEST_ADDED, a path of its program through the scratch folder's
    ///    `wine` link) starts and draws as before. CRANVR_TEST_ADDED_EYE_PER_PASS=1 gives it "Each eye in its own
    ///    pass", as the user's options have it.
    ///
    /// A host started with CRANVR_EYE_DUMP=FOLDER keeps the eyes of the game's newest frame there. Needs `cargo
    /// build --release`. Run with `cargo test --release -p cranvr-app -- --ignored a_game_that_drew_nothing`.
    #[test]
    #[ignore]
    fn a_game_that_drew_nothing_through_opencomposite_starts_through_steamvr() {
        use crate::{
            backend::{self, Model, Settings},
            game_run, pages, vr_system,
        };
        let home = PathBuf::from(std::env::var("CRANVR_HOME").expect("CRANVR_HOME names a scratch folder"));
        assert!(home.join("wine").is_symlink(), "the scratch folder's wine is a link to the user's");
        let name = std::env::var("CRANVR_TEST_GAME").expect("CRANVR_TEST_GAME names the Steam game");
        let added = PathBuf::from(std::env::var("CRANVR_TEST_ADDED").expect("CRANVR_TEST_ADDED names a program to add"));
        assert!(added.starts_with(prefix()), "the added program's path goes through the scratch folder");
        assert!(running(), "the user's Steam runs: the game's Steam check needs it");
        assert!(steamvr().is_some(), "the Steam library has SteamVR");
        let mut settings = Settings::load();
        settings.launch.clear();
        settings.added_games.clear();
        settings.wine = Default::default();
        settings.save();
        let file = openvr::path_file(&prefix()).unwrap();
        let before = fs::read_to_string(&file).ok();
        let mut probe = test_host(400);
        let mut model = Model::default();

        // 1. Steam's launch entry
        let game = games().into_iter().find(|game| game.name == name).expect("the game is installed");
        let program = self::program(&game).unwrap();
        let exe_name = program.rsplit('\\').next().unwrap().to_owned();
        let folder = crate::game_run::folder_of(&program);
        println!("{name}: {program} {:?} in {:?}", game.args, game.working_dir.as_deref().map(mac_to_windows));

        // 2. The Play tab's start through OpenComposite, the verdict and the start through SteamVR
        let started = Instant::now();
        play(&game).expect("the game starts");
        let mut verdict = None;
        while verdict.is_none() && started.elapsed() < Duration::from_secs(120) {
            thread::sleep(Duration::from_millis(200));
            backend::pump(&mut model);
            verdict = game_run::take_verdicts().into_iter().next();
        }
        let verdict = verdict.expect("a verdict that the game drew nothing through OpenComposite");
        println!("verdict {:.1} s after the start: {verdict:?}", started.elapsed().as_secs_f64());
        let mut settings = Settings::load();
        let response = vr_system::respond(&mut settings.launch, &games(), &verdict.program, verdict.why, true);
        settings.save();
        println!("notice: {}", response.notice.window);
        let restart_program = response.restart.clone().expect("the game starts again");
        let restarted = Instant::now();
        restart(&restart_program).expect("the game starts through SteamVR");
        let first = draws(&mut model, &exe_name, Duration::from_secs(120), "steamvr-play");
        println!("through SteamVR: {first:?}, {:.0} s after the restart", restarted.elapsed().as_secs_f64());
        end_program(&exe_name);
        let first_back = run_ends(&folder, &before);

        // 3. The console's request
        let request = format!("3:{}", cranvr_common::menu::Request::run("game.play").of(program.clone()).to_json());
        probe = next_probe(&mut probe, &["--seconds", "200", "--console", &request]);
        let asked = Instant::now();
        let mut console_started = false;
        while !console_started && asked.elapsed() < Duration::from_secs(20) {
            thread::sleep(Duration::from_millis(200));
            backend::pump(&mut model);
            for request in std::mem::take(&mut model.vr_requests) {
                let Some(request) = cranvr_common::menu::Request::from_json(&request) else { continue };
                let state = Model { games: games(), ..Default::default() };
                for effect in pages::apply(&mut Settings::load(), &state, &request).effects {
                    if let pages::Effect::Play(game) = effect {
                        play(&game).expect("the console's game starts");
                        console_started = true;
                    }
                }
            }
        }
        let second = draws(&mut model, &exe_name, Duration::from_secs(120), "steamvr-console");
        println!("from the console: {second:?}, {:.0} s after the request", asked.elapsed().as_secs_f64());
        end_program(&exe_name);
        let second_back = run_ends(&folder, &before);

        // 4. A game added from a folder
        let mut settings = Settings::load();
        add_game(&mut settings, &added, &steam_games()).expect("the program is added");
        if std::env::var_os("CRANVR_TEST_ADDED_EYE_PER_PASS").is_some() {
            settings.launch.entry(mac_to_windows(&added)).or_default().eye_per_pass = true;
        }
        settings.save();
        let added_game = added_games(&settings).into_iter().next().unwrap();
        let added_name = added.file_name().unwrap().to_string_lossy().into_owned();
        probe = next_probe(&mut probe, &["--seconds", "120"]);
        play(&added_game).expect("the added game starts");
        let third = draws(&mut model, &added_name, Duration::from_secs(90), "added");
        println!("added game {}: {third:?}", mac_to_windows(&added));
        end_program(&added_name);
        let third_back = run_ends(&game_run::folder_of(&mac_to_windows(&added)), &before);
        probe.kill().ok();
        probe.wait().ok();
        backend::stop_host();
        let options = Settings::load().launch.get(&program).cloned().unwrap_or_default();

        assert!(program.to_ascii_lowercase().ends_with(r"\thelab\win64\thelab.exe"), "Steam's program: {program}");
        assert_eq!(game.args, ["-popupwindow"]);
        assert_eq!(verdict.program.to_ascii_lowercase(), program.to_ascii_lowercase());
        assert_eq!(options.openvr, Some(launch::OpenVr::SteamVr));
        assert_eq!(options.openvr_reason, vr_system::REASON);
        assert!(first.lit(), "the game draws through SteamVR: {first:?}");
        assert!(first_back, "the run's end puts the path file back");
        assert!(console_started, "the console's request starts the game");
        assert!(second.lit(), "the console's start draws: {second:?}");
        assert!(second_back);
        assert!(third.lit(), "the added game draws: {third:?}");
        assert!(third_back);
    }

    /// A start from the app with the launch option "OpenVR: SteamVR", as the Play tab does it: the app's
    /// host on port 9962, the probe as the headset, then [`run_program`] with the game's options. The
    /// game's frames must reach the host through SteamVR and cranvr's driver. Needs `cargo build
    /// --release`, CRANVR_HOME set to a test copy of the game support with SteamVR and the game, and
    /// CRANVR_TEST_PROGRAM set to the game's program (a Mac path). The probe asks for the desktop too,
    /// so its log shows the peak level of the sound that reached it. Run with `cargo test --release -p
    /// cranvr-app -- --ignored a_game_starts_through_steamvr`.
    #[test]
    #[ignore]
    fn a_game_starts_through_steamvr_from_the_app() {
        use crate::backend::{self, Model, Settings};
        let program = PathBuf::from(std::env::var("CRANVR_TEST_PROGRAM").expect("CRANVR_TEST_PROGRAM names the game"));
        assert!(std::env::var_os("CRANVR_HOME").is_some(), "CRANVR_HOME names a test copy, not the user's own");
        let release = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/release");
        std::env::set_var("CRANVR_HOST_BIN", release.join("cranvr-host"));
        std::env::set_var("CRANVR_TEST_TRUST_ALL", "1");
        // The user's host keeps port 9950, and the probe's pointer script moves no real mouse
        std::env::set_var("CRANVR_NATIVE_PORT", "9962");
        std::env::set_var("CRANVR_POINTER_DRY_RUN", "1");
        let name = program.file_name().unwrap().to_string_lossy().into_owned();
        let windows = mac_to_windows(&program);
        let mut settings = Settings::load();
        let before = settings.clone();
        settings.fps = 72;
        settings.desktop = true;
        settings.virtual_display = None;
        settings.unscale_displays = false;
        settings.game_audio = true;
        settings.launch.entry(windows.clone()).or_default().openvr = Some(launch::OpenVr::SteamVr);
        settings.save();
        backend::start_host(&settings).expect("host starts");
        thread::sleep(Duration::from_secs(2));
        let mut probe = Command::new(release.join("cranvr-probe"))
            .args(["--seconds", "100", "--tcp", "--connect", "127.0.0.1:9962", "--quest-view", "--any-picture", "--desktop"])
            .stdout(log_file("probe.log"))
            .spawn()
            .expect("probe runs");
        run_program(&program).expect("the game starts");
        let mut model = Model::default();
        let (mut game_frames, mut game) = (0u64, None);
        let since = Instant::now();
        let probe_status = loop {
            thread::sleep(Duration::from_millis(200));
            backend::pump(&mut model);
            game_frames = game_frames.max(model.stats.as_ref().map_or(0, |s| s.game_frames));
            game = game.or(model.game.clone());
            if let Some(status) = probe.try_wait().unwrap() {
                break status;
            }
            assert!(since.elapsed() < Duration::from_secs(130), "the probe did not end");
        };
        let audio = model.audio.clone();
        end_program(&name);
        backend::stop_host();
        before.save();
        assert!(probe_status.success(), "the probe saw a good stream");
        assert!(game_frames > 100, "the game's frames reached the host: {game_frames} in a stats window");
        assert!(game.is_some(), "the host named the game");
        assert!(matches!(audio, Some((true, _))), "the host captures the sound: {audio:?}");
        let settings = fs::read_to_string(steam_dir().join("config/steamvr.vrsettings")).unwrap_or_default();
        let settings: serde_json::Value = serde_json::from_str(&settings).unwrap_or_default();
        assert_ne!(settings["driver_cranvr"]["blocked_by_safe_mode"], true, "SteamVR loads the driver");
    }
}
