//! OpenVR games under cranvr's Wine. SteamVR is not installed, so an OpenVR game finds no runtime unless
//! the prefix names one. Valve's openvr_api.dll, which every OpenVR game carries, reads the file
//! %LOCALAPPDATA%\openvr\openvrpaths.vrpath and loads bin\vrclient_x64.dll from the runtime that it names.
//! cranvr's game support installs OpenComposite (GPL 3, see README.md) as C:\opencomposite. It is such a
//! runtime: it takes a game's OpenVR calls and makes OpenXR calls to cranvr's runtime.
//!
//! The choice sits in the file, not in a game's folder: no file of a game changes, and an OpenXR game does
//! not read the file. The file belongs to the prefix, so cranvr writes or removes it each time that it
//! starts a program (scripts/run-game.sh does the same). It removes only a file that names
//! OpenComposite, so a runtime of the user's own stays.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// OpenComposite's folder inside the prefix, as Windows programs see it.
pub const RUNTIME: &str = r"C:\opencomposite";

/// What the file says: OpenVR's runtime is OpenComposite. The backslash is a JSON escape.
fn file_text() -> String {
    format!(r#"{{"jsonid": "vrpathreg", "runtime": ["{}"], "version": 1}}"#, RUNTIME.replace('\\', r"\\"))
}

/// The game support install has OpenComposite: its DLL sits where an OpenVR loader looks for it.
pub fn installed(prefix: &Path) -> bool {
    prefix.join("drive_c/opencomposite/bin/vrclient_x64.dll").is_file()
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

/// Whether a path file names OpenComposite.
fn names_opencomposite(text: &str) -> bool {
    text.contains(&RUNTIME.replace('\\', r"\\"))
}

/// What [`apply`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// OpenVR games find OpenComposite
    On,
    /// The file that named OpenComposite is gone, or there was none
    Off,
}

/// Makes the prefix's OpenVR path file follow the choice. The game support of this app installs
/// OpenComposite, so a prefix without it is an error that the user sees.
pub fn apply(prefix: &Path, on: bool) -> io::Result<Applied> {
    let missing = || io::Error::new(io::ErrorKind::NotFound, "the game support has no OpenComposite");
    let Some(file) = path_file(prefix) else {
        return if on { Err(missing()) } else { Ok(Applied::Off) };
    };
    if on {
        if !installed(prefix) {
            return Err(missing());
        }
        let text = file_text();
        if fs::read_to_string(&file).is_ok_and(|now| now == text) {
            return Ok(Applied::On);
        }
        if let Some(folder) = file.parent() {
            fs::create_dir_all(folder)?;
        }
        fs::write(&file, text)?;
        return Ok(Applied::On);
    }
    if fs::read_to_string(&file).is_ok_and(|now| names_opencomposite(&now)) {
        fs::remove_file(&file)?;
    }
    Ok(Applied::Off)
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
    }

    #[test]
    fn the_file_names_the_runtime_with_escaped_backslashes() {
        assert_eq!(file_text(), r#"{"jsonid": "vrpathreg", "runtime": ["C:\\opencomposite"], "version": 1}"#);
        assert!(names_opencomposite(&file_text()));
        assert!(!names_opencomposite(r#"{"runtime": ["C:\\Program Files\\SteamVR"]}"#));
    }

    #[test]
    fn on_writes_the_file_and_off_removes_it() {
        let prefix = prefix("onoff", "someone");
        install(&prefix);
        let file = path_file(&prefix).unwrap();
        assert!(file.ends_with("AppData/Local/openvr/openvrpaths.vrpath"));
        assert_eq!(apply(&prefix, true).unwrap(), Applied::On);
        assert_eq!(fs::read_to_string(&file).unwrap(), file_text());
        // A second start finds it done
        assert_eq!(apply(&prefix, true).unwrap(), Applied::On);
        assert_eq!(apply(&prefix, false).unwrap(), Applied::Off);
        assert!(!file.exists());
        assert_eq!(apply(&prefix, false).unwrap(), Applied::Off);
        fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn off_leaves_a_runtime_of_the_user_alone() {
        let prefix = prefix("foreign", "someone");
        install(&prefix);
        let file = path_file(&prefix).unwrap();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let steamvr = r#"{"runtime": ["C:\\Program Files (x86)\\Steam\\steamapps\\common\\SteamVR"], "version": 1}"#;
        fs::write(&file, steamvr).unwrap();
        assert_eq!(apply(&prefix, false).unwrap(), Applied::Off);
        assert_eq!(fs::read_to_string(&file).unwrap(), steamvr);
        // On replaces it: the choice is the user's
        assert_eq!(apply(&prefix, true).unwrap(), Applied::On);
        assert_eq!(fs::read_to_string(&file).unwrap(), file_text());
        fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn without_the_install_on_is_an_error_and_changes_nothing() {
        let prefix = prefix("missing", "someone");
        assert!(apply(&prefix, true).is_err());
        assert!(!path_file(&prefix).unwrap().exists());
        // Off needs no install
        assert_eq!(apply(&prefix, false).unwrap(), Applied::Off);
        fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn a_prefix_without_a_user_folder_has_no_file() {
        let dir = std::env::temp_dir().join(format!("cranvr-openvr-none-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(dir.join("drive_c/users/Public")).unwrap();
        assert_eq!(path_file(&dir), None);
        assert!(apply(&dir, true).is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
