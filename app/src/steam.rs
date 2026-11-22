//! Windows Steam in cranvr's Wine prefix: its state, its game library, and starting games.

use crate::{
    backend::{self, config_dir},
    launch, openvr,
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
/// hold this prefix's steam.exe are its Steam. Another prefix's Steam does not count.
pub fn running() -> bool {
    Command::new("lsof").arg("-t").arg(steam_dir().join("steam.exe")).output().is_ok_and(|o| !o.stdout.is_empty())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Game {
    /// The app ID of the game's Steam manifest. A game that the user added from a folder (see
    /// [`added_games`]) has none: it is empty, and the program's launch options may name an ID.
    pub app_id: String,
    pub name: String,
    /// The program to start, found in the install folder
    pub exe: Option<PathBuf>,
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
        .map(|added| Game { app_id: String::new(), name: added.name.clone(), exe: Some(added.program.clone()) })
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
            let exe = find_exe(&steamapps.join("common").join(&installdir), &installdir);
            games.push(Game { app_id: app_id.to_string(), name, exe });
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
/// file now, because the game reads that file when it starts.
fn game_wine(program: &str) -> Command {
    let settings = backend::Settings::load();
    let options = settings.launch.get(program).cloned().unwrap_or_default();
    let choice = launch::openvr(&settings.wine, &options);
    let steamvr = if choice == launch::OpenVr::SteamVr { steamvr() } else { None };
    if let Err(e) = openvr::apply(&prefix(), choice, steamvr.as_ref()) {
        backend::notify(format!("cranvr could not set the OpenVR runtime: {e}"));
    }
    if steamvr.is_some() {
        match openvr::unblock_driver(&steam_dir().join("config/steamvr.vrsettings")) {
            Ok(true) => backend::log("SteamVR had turned cranvr's driver off after a crash. cranvr turned it on again.".into()),
            Ok(false) => {}
            Err(e) => backend::notify(format!("cranvr could not turn its SteamVR driver on again: {e}")),
        }
    }
    let mut command = wine_with(program, settings.game_msaa_limit);
    command.envs(launch::environment(&settings.wine, &options)).args(launch::arguments(&options));
    // cranvr's SteamVR driver finds the game's render size in game-sizes.txt by the game's program. SteamVR,
    // which the game starts, inherits the variable.
    command.env("CRANVR_GAME", program);
    command
}

/// The Steam app ID that a program's Steam check gets: the one in its launch options, else the one of
/// the Steam manifest that names its install folder. None when it has neither.
fn steam_id(program: &str, manifest_id: Option<&str>) -> Option<String> {
    let options = backend::Settings::load().launch.get(program).cloned().unwrap_or_default();
    let own = options.steam_app_id.trim();
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
/// has a Steam check, so Steam starts first. A game with none (a program that the user added from a
/// folder, with no ID in its options) starts without Steam.
pub fn play(game: &Game) -> Result<(), String> {
    require_current_game_support()?;
    let exe = game.exe.clone().ok_or(format!("No program found for {}", game.name))?;
    let program = mac_to_windows(&exe);
    let app_id = steam_id(&program, Some(&game.app_id));
    if app_id.is_some() && !running() {
        start()?;
        // Steam must answer the game's Steam check: give it time to come up
        let since = Instant::now();
        while !running() && since.elapsed() < Duration::from_secs(20) {
            thread::sleep(Duration::from_millis(500));
        }
        thread::sleep(Duration::from_secs(5));
    } else if !wine_ready() {
        return Err("Set up game support first".into());
    }
    let mut command = game_wine(&program);
    if let Some(id) = &app_id {
        command.env("SteamAppId", id).env("SteamGameId", id);
    }
    command
        .current_dir(exe.parent().unwrap_or(Path::new("/")))
        .stdout(log_file("game.log"))
        .stderr(log_file("game.err.log"))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Starts any Windows program (a Mac path) with cranvr's environment.
pub fn run_program(path: &Path) -> Result<(), String> {
    if !wine_ready() {
        return Err("Set up game support first".into());
    }
    require_current_game_support()?;
    let program = mac_to_windows(path);
    let mut command = game_wine(&program);
    if let Some(id) = steam_id(&program, backend::steam_app_id(path.parent().unwrap_or(path)).as_deref()) {
        command.env("SteamAppId", &id).env("SteamGameId", &id);
    }
    command
        .current_dir(path.parent().unwrap_or(Path::new("/")))
        .stdout(log_file("game.log"))
        .stderr(log_file("game.err.log"))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Ends a running program and starts it again with the launch options that are saved now. The
/// program is a Windows path, as the host reports it. Steam keeps running, so the game needs no
/// new start of Steam.
pub fn restart(program: &str) -> Result<(), String> {
    let name = program.rsplit('\\').next().unwrap_or(program);
    end_program(name);
    let path = windows_to_mac(program).ok_or(format!("cranvr cannot start {program} again"))?;
    match games().into_iter().find(|game| self::program(game).is_some_and(|own| own.eq_ignore_ascii_case(program))) {
        Some(game) => play(&game),
        None => run_program(&path),
    }
}

/// Ends the Windows program with this file name and waits until its process is gone.
fn end_program(name: &str) {
    wine("taskkill").args(["/IM", name, "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).status().ok();
    for _ in 0..30 {
        if !program_running(name) {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    // The host sees the end of the old connection first, then the new game connects
    thread::sleep(Duration::from_secs(2));
}

fn program_running(name: &str) -> bool {
    Command::new("pgrep").args(["-if", &regex_escaped(name)]).output().is_ok_and(|o| !o.stdout.is_empty())
}

/// `text` for a regular expression that matches it as it is.
fn regex_escaped(text: &str) -> String {
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
        let steam = [Game { app_id: "620".into(), name: "Some Game".into(), exe: Some(original.join("Some Game.exe")) }];
        assert_eq!(copy_of(&copy.join("Some Game.exe"), &steam), Some(&steam[0]));
        // The same program name alone is no copy: another game may call its program the same
        assert_eq!(copy_of(&other.join("Some Game.exe"), &steam), None);
        assert_eq!(copy_of(&renamed.join("Other.exe"), &steam), None);
        // A game without an app ID (one that the user added) is no original
        let added = [Game { app_id: String::new(), name: "Added".into(), exe: steam[0].exe.clone() }];
        assert_eq!(copy_of(&copy.join("Some Game.exe"), &added), None);
        for dir in [original, copy, other, renamed] {
            fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn adding_a_copy_of_a_modded_game_fills_in_its_steam_app_id_and_no_override() {
        let original = folder("add-original", &["Some Game.exe", "Some Game_Data/", "UnityPlayer.dll", "MonoBleedingEdge/"]);
        let copy = folder("add-copy", &["Some Game.exe", "Some Game_Data/", "UnityPlayer.dll", "MonoBleedingEdge/", "winhttp.dll", "Plugins/"]);
        let steam = [Game { app_id: "620".into(), name: "Some Game".into(), exe: Some(original.join("Some Game.exe")) }];
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
