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
    /// Installed, but the web helper shim is missing (new install or after a Steam update)
    NeedsSetup,
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
    let shim = prefix().join("drive_c/basalt/runtime/steamwebhelper_compat.exe");
    match (fs::read(helper), fs::read(shim)) {
        (Ok(h), Ok(s)) if h == s => SteamState::Ready,
        _ => SteamState::NeedsSetup,
    }
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
    pub app_id: String,
    pub name: String,
    /// The program to start, found in the install folder
    pub exe: Option<PathBuf>,
}

/// Installed games from every Steam library in the prefix.
pub fn games() -> Vec<Game> {
    let mut games = Vec::new();
    for steamapps in libraries() {
        let Ok(entries) = fs::read_dir(&steamapps) else { continue };
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().to_string();
            let Some(app_id) = file.strip_prefix("appmanifest_").and_then(|f| f.strip_suffix(".acf")) else { continue };
            let text = fs::read_to_string(entry.path()).unwrap_or_default();
            let (Some(name), Some(installdir)) = (vdf_value(&text, "name"), vdf_value(&text, "installdir")) else { continue };
            // Steam's own tools (runtimes, redistributables) are not games
            if name.starts_with("Steamworks") || name.contains("Redistributable") || name.starts_with("Proton") {
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
    let on = launch::opencomposite(&settings.wine, &options);
    // An older game support has no OpenComposite: the Play tab then offers its update, so a start says nothing
    if let Err(e) = openvr::apply(&prefix(), on) {
        backend::notify(format!("cranvr could not set the OpenVR runtime: {e}"));
    }
    let mut command = wine_with(program, settings.game_msaa_limit);
    command.envs(launch::environment(&settings.wine, &options)).args(launch::arguments(&options));
    command
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
/// (an update would replace the web helper shim), no file check, no CEF sandbox.
pub fn start() -> Result<(), String> {
    match state() {
        SteamState::Ready => {}
        SteamState::NoWine => return Err("Set up game support first".into()),
        _ => return Err("Finish the Steam setup first".into()),
    }
    if running() {
        return Ok(());
    }
    let dir = steam_dir();
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
/// would get Steam's environment; this one gets the host's control file and its app ID.
pub fn play(game: &Game) -> Result<(), String> {
    let exe = game.exe.clone().ok_or(format!("No program found for {}", game.name))?;
    if !running() {
        start()?;
        // Steam must answer the game's Steam check: give it time to come up
        let since = Instant::now();
        while !running() && since.elapsed() < Duration::from_secs(20) {
            thread::sleep(Duration::from_millis(500));
        }
        thread::sleep(Duration::from_secs(5));
    }
    game_wine(&mac_to_windows(&exe))
        .current_dir(exe.parent().unwrap_or(Path::new("/")))
        .env("SteamAppId", &game.app_id)
        .env("SteamGameId", &game.app_id)
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
    let mut command = game_wine(&mac_to_windows(path));
    if let Some(app_id) = backend::steam_app_id(path.parent().unwrap_or(path)) {
        command.env("SteamAppId", &app_id).env("SteamGameId", &app_id);
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
}
