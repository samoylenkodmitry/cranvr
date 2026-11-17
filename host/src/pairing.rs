//! The headsets that the user paired with the native protocol.
//!
//! A headset has a key (see `cranvr_proto::identity`). The host keeps the keys of the paired
//! headsets in a file in its config folder, one line for each: the public key in hex, a space and
//! the headset's name. A headset that the host does not know shows a six-digit code on its panel.
//! The user starts the host with `--pair CODE`. The host then pairs the headset whose code matches
//! and remembers its key.
//!
//! The pairing window stays open for ten minutes or until it paired one headset. It closes after
//! five tries with a wrong code, so a device on the network cannot try all codes.

use alvr_common::{info, warn};
use cranvr_proto::identity::{format_code, from_hex, pairing_code, to_hex, KEY_LEN};
use std::{
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

/// How long a window that `--pair` opened stays open
const WINDOW_OPEN: Duration = Duration::from_secs(10 * 60);
/// Tries with a wrong code that close the window
const MAX_FAILURES: u32 = 5;
const FILE: &str = "paired-headsets.txt";

struct Window {
    code: u32,
    opened: Instant,
    failures: u32,
}

/// The result of an attempt to pair a headset whose key the host does not know
#[derive(Debug, PartialEq, Eq)]
pub enum Attempt {
    Paired,
    /// A window is open and the code did not match
    WrongCode,
    /// No window is open, or it expired, or too many tries closed it
    NoWindow,
}

pub struct Pairings {
    path: PathBuf,
    keys: Mutex<Vec<([u8; KEY_LEN], String)>>,
    window: Mutex<Option<Window>>,
}

impl Pairings {
    /// Reads the file in `config_dir`. `code` opens a pairing window.
    pub fn load(config_dir: &std::path::Path, code: Option<u32>) -> Self {
        let path = config_dir.join(FILE);
        let keys: Vec<_> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (key, name) = line.split_once(' ').unwrap_or((line, ""));
                Some((from_hex(key)?, name.trim().to_owned()))
            })
            .collect();
        if !keys.is_empty() {
            info!("Native protocol: {} paired headsets in {}", keys.len(), path.display());
        }
        if let Some(code) = code {
            info!("Native protocol: pairing is open for the headset that shows the code {}", format_code(code));
        }
        Self {
            path,
            keys: Mutex::new(keys),
            window: Mutex::new(code.map(|code| Window { code, opened: Instant::now(), failures: 0 })),
        }
    }

    pub fn contains(&self, key: &[u8; KEY_LEN]) -> bool {
        self.keys.lock().unwrap().iter().any(|(known, _)| known == key)
    }

    /// Whether a headset that is not on this Mac can pair or connect: a key is paired or a window
    /// is open.
    pub fn reachable(&self) -> bool {
        !self.keys.lock().unwrap().is_empty() || self.window.lock().unwrap().is_some()
    }

    /// Pairs the headset if a window is open and `key` has its code. The caller has checked that
    /// the headset holds the key.
    pub fn try_pair(&self, key: &[u8; KEY_LEN], name: &str) -> Attempt {
        let mut window = self.window.lock().unwrap();
        let Some(open) = window.as_mut() else { return Attempt::NoWindow };
        if open.opened.elapsed() > WINDOW_OPEN {
            *window = None;
            return Attempt::NoWindow;
        }
        if pairing_code(key) != open.code {
            open.failures += 1;
            if open.failures >= MAX_FAILURES {
                warn!("Native protocol: {MAX_FAILURES} headsets showed a wrong code. Pairing is closed. Start the host again to open it.");
                *window = None;
            }
            return Attempt::WrongCode;
        }
        *window = None;
        drop(window);
        let mut keys = self.keys.lock().unwrap();
        if !keys.iter().any(|(known, _)| known == key) {
            keys.push((*key, name.to_owned()));
            // A name has no line break, so the file keeps one headset to a line
            let name = name.replace(['\n', '\r'], " ");
            let line = format!("{} {name}\n", to_hex(key));
            if let Err(e) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).and_then(|mut f| f.write_all(line.as_bytes())) {
                warn!("Native protocol: the pairing of {name} was not saved to {}: {e}", self.path.display());
            }
        }
        Attempt::Paired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranvr_proto::identity::Identity;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cranvr-pairing-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::remove_file(dir.join(FILE)).ok();
        dir
    }

    fn key(seed: u8) -> ([u8; 32], u32) {
        let identity = Identity::from_seed([seed; 32]);
        (identity.public_key(), identity.code())
    }

    #[test]
    fn a_matching_code_pairs_and_the_file_remembers_it() {
        let dir = dir("match");
        let (headset, code) = key(1);
        let pairings = Pairings::load(&dir, Some(code));
        assert!(pairings.reachable() && !pairings.contains(&headset));
        assert_eq!(pairings.try_pair(&headset, "Meta Quest 3"), Attempt::Paired);
        assert!(pairings.contains(&headset));
        // The window closed with the first pairing
        assert_eq!(pairings.try_pair(&key(2).0, "Another"), Attempt::NoWindow);
        // A new host process reads the key from the file and needs no window
        let again = Pairings::load(&dir, None);
        assert!(again.contains(&headset) && !again.contains(&key(2).0) && again.reachable());
    }

    #[test]
    fn a_wrong_code_does_not_pair_and_too_many_close_the_window() {
        let dir = dir("wrong");
        let (_, code) = key(1);
        let pairings = Pairings::load(&dir, Some(code));
        // Other keys have other codes. A few seeds lack the code by chance only with a very small probability.
        for seed in 2..2 + MAX_FAILURES as u8 {
            let (other, other_code) = key(seed);
            assert_ne!(other_code, code);
            assert_eq!(pairings.try_pair(&other, "Intruder"), Attempt::WrongCode);
            assert!(!pairings.contains(&other));
        }
        // Five wrong tries closed the window, so even the right headset cannot pair now
        assert_eq!(pairings.try_pair(&key(1).0, "Headset"), Attempt::NoWindow);
        assert!(!pairings.reachable());
    }

    #[test]
    fn without_a_window_or_a_key_nothing_is_reachable() {
        let pairings = Pairings::load(&dir("none"), None);
        assert!(!pairings.reachable());
        assert_eq!(pairings.try_pair(&key(1).0, "x"), Attempt::NoWindow);
    }

    #[test]
    fn a_name_with_a_line_break_keeps_the_file_to_one_line_for_each_headset() {
        let dir = dir("name");
        let (headset, code) = key(3);
        Pairings::load(&dir, Some(code)).try_pair(&headset, "a\nb");
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(Pairings::load(&dir, None).contains(&headset));
    }
}
