//! Windows Steam in cranvr's Wine prefix: its state, its game library, and starting games.

use crate::backend::{self, config_dir};
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

pub fn wine_ready() -> bool {
    config_dir().join("wine/engine/bin/wine").exists()
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

pub fn running() -> bool {
    Command::new("pgrep").args(["-if", "steam\\\\steam\\.exe"]).output().is_ok_and(|o| !o.stdout.is_empty())
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

/// The game's program: `<installdir>.exe` if present, else the largest .exe that is not a crash
/// handler, installer or redistributable. The search goes three folders deep, as Serious Sam VR
/// keeps its program in Bin/x64. A program in a 64-bit folder (x64, Win64) goes first.
fn find_exe(dir: &Path, installdir: &str) -> Option<PathBuf> {
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

/// `wine` with an MSAA limit for the game: 0 leaves it to the game.
/// A game's Wine command. A VR game draws into the headset: its mirror window on the Mac stays
/// empty and costs no GPU time (cranvr's DXMT patch 0008). Steam does not get this: its own
/// window draws with DXMT too.
fn game_wine(program: &str) -> Command {
    let mut command = wine(program);
    command.env("DXMT_SKIP_PRESENT", "1");
    command
}

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
        .args(["-inhibitbootstrap", "-noverifyfiles", "-no-cef-sandbox"])
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
    let windows = mac_to_windows(&exe);
    game_wine(&windows)
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

/// A Mac path as Wine sees it: C:\ inside the prefix, Z:\ elsewhere.
fn mac_to_windows(path: &Path) -> String {
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
    fn converts_paths() {
        let p = prefix().join("drive_c/Program Files (x86)/Steam/steam.exe");
        assert_eq!(mac_to_windows(&p), "C:\\Program Files (x86)\\Steam\\steam.exe");
        assert_eq!(mac_to_windows(Path::new("/Users/x/a.exe")), "Z:\\Users\\x\\a.exe");
        assert_eq!(windows_to_mac("C:\\Games"), Some(prefix().join("drive_c/Games")));
        assert_eq!(windows_to_mac("Z:\\Volumes\\SSD"), Some(PathBuf::from("/Volumes/SSD")));
    }
}
