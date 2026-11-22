//! OpenVR games under cranvr's Wine. Valve's openvr_api.dll, which every OpenVR game carries, reads the
//! file %LOCALAPPDATA%\openvr\openvrpaths.vrpath and loads bin\vrclient_x64.dll from the runtime that it
//! names. The game's launch options choose the runtime (launch::OpenVr):
//! - OpenComposite (GPL 3, see README.md), which cranvr's game support installs as C:\opencomposite. It
//!   takes a game's OpenVR calls and makes OpenXR calls to cranvr's runtime.
//! - SteamVR for Windows from the user's Steam library, with cranvr's driver (steamvr/driver_cranvr.cpp),
//!   which the game support installs as C:\cranvr\steamvr. The file names the driver's folder, so SteamVR
//!   loads it. The game starts SteamVR itself, as on Windows.
//!
//! The choice sits in the file, not in a game's folder: no file of a game changes, and an OpenXR game does
//! not read the file. The file belongs to the prefix, and Steam reads it too. Steam starts OpenVR as its
//! watchdog when SteamVR is installed, and Steam writes its own paths into the file at its start. So the file
//! names OpenComposite only while a game that uses it runs ([`begin`] and [`end`], see game_run.rs). While no
//! game runs it names SteamVR when the Steam library has it, or no runtime ([`settle`]). A game's end puts
//! back what the file said before the game's start. scripts/run-game.sh follows the same rule.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::launch::OpenVr;

/// OpenComposite's folder inside the prefix, as Windows programs see it.
pub const RUNTIME: &str = r"C:\opencomposite";

/// The folder of cranvr's SteamVR driver inside the prefix, as Windows programs see it.
pub const DRIVER: &str = r"C:\cranvr\steamvr";

/// A Windows path as a JSON string: the backslash is an escape.
fn json(path: &str) -> String {
    format!("\"{}\"", path.replace('\\', r"\\"))
}

/// What the file says: OpenVR's runtime is OpenComposite.
fn file_text() -> String {
    format!(r#"{{"jsonid": "vrpathreg", "runtime": [{}], "version": 1}}"#, json(RUNTIME))
}

/// Where SteamVR lives, as Windows programs see it: its own folder, and Steam's folders for SteamVR's
/// settings and logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SteamVr {
    pub runtime: String,
    pub config: String,
    pub log: String,
}

/// What the file says: OpenVR's runtime is SteamVR, with cranvr's driver among its drivers.
fn steamvr_text(steamvr: &SteamVr) -> String {
    format!(
        r#"{{"config": [{}], "external_drivers": [{}], "jsonid": "vrpathreg", "log": [{}], "runtime": [{}], "version": 1}}"#,
        json(&steamvr.config),
        json(DRIVER),
        json(&steamvr.log),
        json(&steamvr.runtime)
    )
}

/// The game support install has OpenComposite: its DLL sits where an OpenVR loader looks for it.
pub fn installed(prefix: &Path) -> bool {
    prefix.join("drive_c/opencomposite/bin/vrclient_x64.dll").is_file()
}

/// The game support install has cranvr's SteamVR driver: its manifest sits where SteamVR looks for it.
pub fn driver_installed(prefix: &Path) -> bool {
    prefix.join("drive_c/cranvr/steamvr/driver.vrdrivermanifest").is_file()
}

/// The Windows user folder of the prefix. Wine names it after the Mac user. A prefix that another
/// account made has a folder of another name, so the first folder that is not Public does too.
fn user_folder(prefix: &Path) -> Option<PathBuf> {
    let users = prefix.join("drive_c/users");
    let own = ["USER", "LOGNAME"].iter().filter_map(|name| std::env::var(name).ok()).map(|name| users.join(name)).find(|dir| dir.is_dir());
    own.or_else(|| {
        let mut folders: Vec<PathBuf> = fs::read_dir(&users).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "Public")).collect();
        folders.sort();
        folders.into_iter().next()
    })
}

/// The OpenVR path file of the prefix, when the prefix has a user folder.
pub fn path_file(prefix: &Path) -> Option<PathBuf> {
    Some(user_folder(prefix)?.join("AppData/Local/openvr/openvrpaths.vrpath"))
}

/// OpenComposite's log in the prefix. Each process that loads OpenComposite starts it anew, and its lines name
/// the program's folder (OpenComposite looks there for its settings file).
pub fn opencomposite_log(prefix: &Path) -> Option<PathBuf> {
    Some(user_folder(prefix)?.join("AppData/Local/OpenComposite/logs/opencomposite.log"))
}

/// Whether a path file names OpenComposite as the runtime. Steam rewrites the file in its own layout, so the
/// test looks for the folder, not for cranvr's text.
fn names_opencomposite(text: &str) -> bool {
    text.contains(&RUNTIME.replace('\\', r"\\"))
}

/// What the file says while no game runs: SteamVR with cranvr's driver when the Steam library has SteamVR and
/// the game support has the driver, else no runtime. OpenComposite runs scene applications only, and Steam
/// loads the runtime of the file as its watchdog.
fn idle_text(prefix: &Path, steamvr: Option<&SteamVr>) -> Option<String> {
    steamvr.filter(|_| driver_installed(prefix)).map(steamvr_text)
}

/// The file's text, or None when there is no file.
fn read(file: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Makes the file say `text`, or removes it for None. A file that says it already stays untouched.
fn write(file: &Path, text: Option<&str>) -> io::Result<()> {
    let now = read(file)?;
    if now.as_deref() == text {
        return Ok(());
    }
    match text {
        Some(text) => {
            if let Some(folder) = file.parent() {
                fs::create_dir_all(folder)?;
            }
            fs::write(file, text)
        }
        None => fs::remove_file(file),
    }
}

/// What the path file said before a game's start, to put back when the game ends. None: no file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved(Option<String>);

/// Makes the prefix's OpenVR path file follow a game's choice for the game's run and returns what to put back
/// at its end. The game support of this app installs OpenComposite and the driver, so a prefix without them
/// is an error that the user sees. `steamvr` is SteamVR's place when the Steam library has it. Off leaves the
/// game no runtime, so a game with an OpenXR mode takes that. A file that names OpenComposite before the start
/// is left over from a run that did not end through cranvr, such as a crash of the app: the end puts the idle
/// file in its place.
pub fn begin(prefix: &Path, choice: OpenVr, steamvr: Option<&SteamVr>) -> io::Result<Saved> {
    let missing = |what: &str| io::Error::new(io::ErrorKind::NotFound, what.to_owned());
    let text = match choice {
        OpenVr::OpenComposite if !installed(prefix) => return Err(missing("the game support has no OpenComposite")),
        OpenVr::OpenComposite => Some(file_text()),
        OpenVr::SteamVr if !driver_installed(prefix) => return Err(missing("the game support has no SteamVR driver")),
        OpenVr::SteamVr => match steamvr {
            Some(steamvr) => Some(steamvr_text(steamvr)),
            None => return Err(missing("SteamVR is not installed: install SteamVR from your Steam library")),
        },
        OpenVr::Off => None,
    };
    let Some(file) = path_file(prefix) else {
        return if text.is_some() { Err(missing("the prefix has no Windows user folder")) } else { Ok(Saved(None)) };
    };
    let before = read(&file)?;
    let saved = Saved(if before.as_deref().is_some_and(names_opencomposite) { idle_text(prefix, steamvr) } else { before });
    write(&file, text.as_deref())?;
    Ok(saved)
}

/// Puts back what the file said before the game's start.
pub fn end(prefix: &Path, saved: &Saved) -> io::Result<()> {
    match path_file(prefix) {
        Some(file) => write(&file, saved.0.as_deref()),
        None => Ok(()),
    }
}

/// Replaces a file that names OpenComposite while no game runs with the idle file: SteamVR or no runtime.
/// Steam's start calls it, because Steam loads that runtime as its watchdog. Says whether the file changed.
pub fn settle(prefix: &Path, steamvr: Option<&SteamVr>) -> io::Result<bool> {
    let Some(file) = path_file(prefix) else { return Ok(false) };
    if !read(&file)?.as_deref().is_some_and(names_opencomposite) {
        return Ok(false);
    }
    write(&file, idle_text(prefix, steamvr).as_deref())?;
    Ok(true)
}

/// Turns cranvr's driver on again in SteamVR's settings file (steamvr.vrsettings in Steam's config folder)
/// and says whether it was off. After a crash of its server SteamVR marks the drivers there with
/// `blocked_by_safe_mode` and loads them no more. Without cranvr's driver SteamVR has no headset. A file
/// that is missing or holds no such mark stays as it is.
pub fn unblock_driver(settings: &Path) -> io::Result<bool> {
    let Ok(text) = fs::read_to_string(settings) else {
        return Ok(false);
    };
    let Ok(mut values) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(false);
    };
    let Some(driver) = values.get_mut("driver_cranvr").and_then(|d| d.as_object_mut()) else {
        return Ok(false);
    };
    if driver.remove("blocked_by_safe_mode").is_none_or(|v| v != serde_json::Value::Bool(true)) {
        return Ok(false);
    }
    if driver.is_empty() {
        values.as_object_mut().map(|all| all.remove("driver_cranvr"));
    }
    fs::write(settings, serde_json::to_string_pretty(&values).map_err(io::Error::other)?)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix(name: &str, user: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cranvr-openvr-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(dir.join("drive_c/users/Public")).unwrap();
        fs::create_dir_all(dir.join("drive_c/users").join(user)).unwrap();
        dir
    }

    fn install(prefix: &Path) {
        fs::create_dir_all(prefix.join("drive_c/opencomposite/bin")).unwrap();
        fs::write(prefix.join("drive_c/opencomposite/bin/vrclient_x64.dll"), b"x").unwrap();
        fs::create_dir_all(prefix.join("drive_c/cranvr/steamvr")).unwrap();
        fs::write(prefix.join("drive_c/cranvr/steamvr/driver.vrdrivermanifest"), b"{}").unwrap();
    }

    fn steamvr() -> SteamVr {
        SteamVr {
            runtime: r"C:\Program Files (x86)\Steam\steamapps\common\SteamVR".into(),
            config: r"C:\Program Files (x86)\Steam\config".into(),
            log: r"C:\Program Files (x86)\Steam\logs".into(),
        }
    }

    #[test]
    fn a_driver_that_safe_mode_blocked_is_turned_on_again() {
        let dir = prefix("unblock", "someone");
        let settings = dir.join("steamvr.vrsettings");
        assert!(!unblock_driver(&settings).unwrap(), "a missing file stays missing");
        assert!(!settings.exists());
        fs::write(&settings, r#"{"driver_cranvr": {"blocked_by_safe_mode": true}, "steamvr": {"enableHomeApp": false}}"#).unwrap();
        assert!(unblock_driver(&settings).unwrap());
        let values: serde_json::Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(values.get("driver_cranvr").is_none());
        assert_eq!(values["steamvr"]["enableHomeApp"], false);
        assert!(!unblock_driver(&settings).unwrap(), "a second call finds nothing to do");
        fs::write(&settings, r#"{"driver_cranvr": {"blocked_by_safe_mode": false, "enable": true}}"#).unwrap();
        assert!(!unblock_driver(&settings).unwrap());
        fs::write(&settings, r#"{"driver_cranvr": {"blocked_by_safe_mode": true, "enable": true}}"#).unwrap();
        assert!(unblock_driver(&settings).unwrap());
        let values: serde_json::Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(values["driver_cranvr"]["enable"], true);
        assert!(values["driver_cranvr"].get("blocked_by_safe_mode").is_none());
        fs::remove_dir_all(&dir).ok();
    }

    /// What Steam writes when it finds no file at its start, in its own layout
    const STEAMS_OWN: &str = "{\n\t\"config\" : \n\t[\n\t\t\"C:\\\\Program Files (x86)\\\\Steam\\\\config\"\n\t],\n\t\"external_drivers\" : null,\n\t\"runtime\" : \n\t[\n\t\t\"C:\\\\Program Files (x86)\\\\Steam\\\\steamapps\\\\common\\\\SteamVR\"\n\t],\n\t\"version\" : 1\n}";

    #[test]
    fn the_file_names_the_runtime_with_escaped_backslashes() {
        assert_eq!(file_text(), r#"{"jsonid": "vrpathreg", "runtime": ["C:\\opencomposite"], "version": 1}"#);
        assert!(names_opencomposite(&file_text()));
        assert!(!names_opencomposite(r#"{"runtime": ["C:\\Program Files\\SteamVR"]}"#));
        let text = steamvr_text(&steamvr());
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["runtime"][0], r"C:\Program Files (x86)\Steam\steamapps\common\SteamVR");
        assert_eq!(parsed["external_drivers"][0], DRIVER);
        assert_eq!(parsed["config"][0], r"C:\Program Files (x86)\Steam\config");
        assert!(!names_opencomposite(&text));
        let steams: serde_json::Value = serde_json::from_str(STEAMS_OWN).unwrap();
        assert_eq!(steams["runtime"][0], r"C:\Program Files (x86)\Steam\steamapps\common\SteamVR");
        // Steam keeps a runtime that the file names, and adds its paths in its own layout
        let rewritten = "{\n\t\"config\" : \n\t[\n\t\t\"C:\\\\Program Files (x86)\\\\Steam\\\\config\"\n\t],\n\t\"runtime\" : \n\t[\n\t\t\"C:\\\\opencomposite\"\n\t]\n}";
        assert!(names_opencomposite(rewritten));
    }

    #[test]
    fn a_game_s_run_names_its_runtime_and_its_end_puts_back_the_file_before_it() {
        let prefix = prefix("run", "someone");
        install(&prefix);
        let file = path_file(&prefix).unwrap();
        assert!(file.ends_with("AppData/Local/openvr/openvrpaths.vrpath"));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, STEAMS_OWN).unwrap();
        for (choice, during) in [
            (OpenVr::OpenComposite, Some(file_text())),
            (OpenVr::SteamVr, Some(steamvr_text(&steamvr()))),
            (OpenVr::Off, None),
        ] {
            let saved = begin(&prefix, choice, Some(&steamvr())).unwrap();
            assert_eq!(fs::read_to_string(&file).ok(), during, "{choice:?} during the run");
            end(&prefix, &saved).unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), STEAMS_OWN, "{choice:?} after the run");
        }
        // No file before the run: none after it
        fs::remove_file(&file).unwrap();
        let saved = begin(&prefix, OpenVr::OpenComposite, None).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), file_text());
        end(&prefix, &saved).unwrap();
        assert!(!file.exists());
        fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn opencomposite_left_over_from_a_run_gives_way_to_steamvr_or_to_no_runtime() {
        let prefix = prefix("leftover", "someone");
        install(&prefix);
        let file = path_file(&prefix).unwrap();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        // Steam's start: OpenComposite goes, SteamVR comes when the library has it
        fs::write(&file, file_text()).unwrap();
        assert!(settle(&prefix, Some(&steamvr())).unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), steamvr_text(&steamvr()));
        assert!(!settle(&prefix, Some(&steamvr())).unwrap(), "SteamVR stays");
        fs::write(&file, file_text()).unwrap();
        assert!(settle(&prefix, None).unwrap());
        assert!(!file.exists(), "no SteamVR: no runtime");
        assert!(!settle(&prefix, None).unwrap());
        // A file of Steam's own stays as it is
        fs::write(&file, STEAMS_OWN).unwrap();
        assert!(!settle(&prefix, None).unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), STEAMS_OWN);
        // A run that starts on a leftover puts the idle file back at its end, not OpenComposite
        fs::write(&file, file_text()).unwrap();
        let saved = begin(&prefix, OpenVr::OpenComposite, Some(&steamvr())).unwrap();
        end(&prefix, &saved).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), steamvr_text(&steamvr()));
        fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn a_missing_runtime_is_an_error_and_changes_nothing() {
        let prefix = prefix("missing", "someone");
        assert!(begin(&prefix, OpenVr::OpenComposite, None).is_err());
        assert!(begin(&prefix, OpenVr::SteamVr, Some(&steamvr())).is_err(), "no driver");
        install(&prefix);
        let error = begin(&prefix, OpenVr::SteamVr, None).unwrap_err();
        assert!(error.to_string().contains("SteamVR is not installed"), "{error}");
        assert!(!path_file(&prefix).unwrap().exists());
        // Off needs no install
        assert_eq!(begin(&prefix, OpenVr::Off, None).unwrap(), Saved(None));
        fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn a_prefix_without_a_user_folder_has_no_file() {
        let dir = std::env::temp_dir().join(format!("cranvr-openvr-none-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(dir.join("drive_c/users/Public")).unwrap();
        install(&dir);
        assert_eq!(path_file(&dir), None);
        assert!(begin(&dir, OpenVr::OpenComposite, None).is_err());
        assert_eq!(begin(&dir, OpenVr::Off, None).unwrap(), Saved(None));
        assert!(!settle(&dir, None).unwrap());
        fs::remove_dir_all(&dir).ok();
    }
}
