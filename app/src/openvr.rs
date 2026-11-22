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
//! not read the file. The file belongs to the prefix, so cranvr writes or removes it each time that it
//! starts a program (scripts/run-game.sh does the same). It removes only a file that it wrote, so a runtime
//! of the user's own stays.

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

/// Whether a path file is one that cranvr wrote: it names OpenComposite or cranvr's driver.
fn written_by_cranvr(text: &str) -> bool {
    text.contains(&RUNTIME.replace('\\', r"\\")) || text.contains(&DRIVER.replace('\\', r"\\"))
}

/// Makes the prefix's OpenVR path file follow the choice and returns the choice. The game support of this
/// app installs OpenComposite and the driver, so a prefix without them is an error that the user sees.
/// `steamvr` is SteamVR's place when the Steam library has it.
pub fn apply(prefix: &Path, choice: OpenVr, steamvr: Option<&SteamVr>) -> io::Result<OpenVr> {
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
        return if text.is_some() { Err(missing("the prefix has no Windows user folder")) } else { Ok(OpenVr::Off) };
    };
    let Some(text) = text else {
        if fs::read_to_string(&file).is_ok_and(|now| written_by_cranvr(&now)) {
            fs::remove_file(&file)?;
        }
        return Ok(OpenVr::Off);
    };
    if fs::read_to_string(&file).is_ok_and(|now| now == text) {
        return Ok(choice);
    }
    if let Some(folder) = file.parent() {
        fs::create_dir_all(folder)?;
    }
    fs::write(&file, text)?;
    Ok(choice)
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

    #[test]
    fn the_file_names_the_runtime_with_escaped_backslashes() {
        assert_eq!(file_text(), r#"{"jsonid": "vrpathreg", "runtime": ["C:\\opencomposite"], "version": 1}"#);
        assert!(written_by_cranvr(&file_text()));
        assert!(!written_by_cranvr(r#"{"runtime": ["C:\\Program Files\\SteamVR"]}"#));
        let text = steamvr_text(&steamvr());
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["runtime"][0], r"C:\Program Files (x86)\Steam\steamapps\common\SteamVR");
        assert_eq!(parsed["external_drivers"][0], DRIVER);
        assert_eq!(parsed["config"][0], r"C:\Program Files (x86)\Steam\config");
        assert!(written_by_cranvr(&text));
    }

    #[test]
    fn each_choice_writes_its_file_and_off_removes_it() {
        let prefix = prefix("onoff", "someone");
        install(&prefix);
        let file = path_file(&prefix).unwrap();
        assert!(file.ends_with("AppData/Local/openvr/openvrpaths.vrpath"));
        assert_eq!(apply(&prefix, OpenVr::OpenComposite, None).unwrap(), OpenVr::OpenComposite);
        assert_eq!(fs::read_to_string(&file).unwrap(), file_text());
        // A second start finds it done
        assert_eq!(apply(&prefix, OpenVr::OpenComposite, None).unwrap(), OpenVr::OpenComposite);
        assert_eq!(apply(&prefix, OpenVr::SteamVr, Some(&steamvr())).unwrap(), OpenVr::SteamVr);
        assert_eq!(fs::read_to_string(&file).unwrap(), steamvr_text(&steamvr()));
        assert_eq!(apply(&prefix, OpenVr::Off, None).unwrap(), OpenVr::Off);
        assert!(!file.exists());
        assert_eq!(apply(&prefix, OpenVr::Off, None).unwrap(), OpenVr::Off);
        fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn off_leaves_a_runtime_of_the_user_alone() {
        let prefix = prefix("foreign", "someone");
        install(&prefix);
        let file = path_file(&prefix).unwrap();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let own = r#"{"runtime": ["C:\\Program Files (x86)\\Steam\\steamapps\\common\\SteamVR"], "version": 1}"#;
        fs::write(&file, own).unwrap();
        assert_eq!(apply(&prefix, OpenVr::Off, None).unwrap(), OpenVr::Off);
        assert_eq!(fs::read_to_string(&file).unwrap(), own);
        // A choice replaces it: the choice is the user's
        assert_eq!(apply(&prefix, OpenVr::OpenComposite, None).unwrap(), OpenVr::OpenComposite);
        assert_eq!(fs::read_to_string(&file).unwrap(), file_text());
        fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn a_missing_runtime_is_an_error_and_changes_nothing() {
        let prefix = prefix("missing", "someone");
        assert!(apply(&prefix, OpenVr::OpenComposite, None).is_err());
        assert!(apply(&prefix, OpenVr::SteamVr, Some(&steamvr())).is_err(), "no driver");
        install(&prefix);
        let error = apply(&prefix, OpenVr::SteamVr, None).unwrap_err();
        assert!(error.to_string().contains("SteamVR is not installed"), "{error}");
        assert!(!path_file(&prefix).unwrap().exists());
        // Off needs no install
        assert_eq!(apply(&prefix, OpenVr::Off, None).unwrap(), OpenVr::Off);
        fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn a_prefix_without_a_user_folder_has_no_file() {
        let dir = std::env::temp_dir().join(format!("cranvr-openvr-none-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(dir.join("drive_c/users/Public")).unwrap();
        install(&dir);
        assert_eq!(path_file(&dir), None);
        assert!(apply(&dir, OpenVr::OpenComposite, None).is_err());
        assert_eq!(apply(&dir, OpenVr::Off, None).unwrap(), OpenVr::Off);
        fs::remove_dir_all(&dir).ok();
    }
}
