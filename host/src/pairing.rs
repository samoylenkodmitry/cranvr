//! The headsets that the user paired with the native protocol.
//!
//! A headset has a key (see `cranvr_proto::identity`). The host keeps the keys of the paired
//! headsets in a file in its config folder, one line for each: the public key in hex, a space and
//! the headset's name.
//!
//! A headset that the host does not know waits (protocol version 5). The host keeps one [`Request`]
//! at a time: the headset's name and the code that both screens show (see
//! `cranvr_proto::secure::approval_code`). The cranvr app lists the request in the host's status
//! line and sends the user's answer back (`allow KEY` or `deny KEY` on the host's input). An
//! answer for a headset that no request names does nothing. An allowed headset is paired for good.
//!
//! A headset with the older cranvr app (version 4) cannot wait. It pairs with `--pair CODE`: the
//! host then pairs the headset whose key has that code, in a window that stays open for ten
//! minutes or until it paired one headset. The window closes after five tries with a wrong code, so
//! a device on the network cannot try all codes.
//!
//! The host has a key of its own, in `host-identity.txt` beside the file of the paired headsets.
//! The headset remembers it when the host accepted the headset the first time. A host with another
//! key is refused afterwards. Keep the file. A host without it gets a new key. The paired headsets
//! then refuse it until their user forgets the old one.

use log::{info, warn};
use cranvr_proto::{
    discovery::clean_name,
    identity::{format_code, from_hex, pairing_code, to_hex, Identity, KEY_LEN},
};
use std::{
    io::Write,
    net::IpAddr,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

/// How long a window that `--pair` opened stays open
const WINDOW_OPEN: Duration = Duration::from_secs(10 * 60);
/// Tries with a wrong code that close the window
const MAX_FAILURES: u32 = 5;
const FILE: &str = "paired-headsets.txt";
const HOST_KEY_FILE: &str = "host-identity.txt";

/// What the host shows and saves of a headset's name: the headset chose it, so it has no control
/// characters (a name with a line break would break the file's lines) and it is short. A headset
/// without a name is "A headset".
pub fn display_name(name: &str) -> String {
    Some(clean_name(name)).filter(|name| !name.is_empty()).unwrap_or_else(|| "A headset".to_owned())
}

/// The name of this Mac as its user knows it, such as "Dmitry's MacBook Pro". The headset lists
/// the Mac by it.
pub fn mac_name() -> String {
    let ask = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|text| clean_name(&text))
            .filter(|name| !name.is_empty())
    };
    ask("scutil", &["--get", "ComputerName"]).or_else(|| ask("hostname", &[])).unwrap_or_else(|| "This Mac".to_owned())
}

/// The host's key. It comes from `host-identity.txt` in `config_dir`. A host that has none makes a key and
/// saves it there, readable by the user alone. When the file cannot be written the key lasts for
/// this run only. The headsets that paired with the old key then refuse the host.
pub fn host_identity(config_dir: &std::path::Path) -> Identity {
    use std::os::unix::fs::OpenOptionsExt;
    let path = config_dir.join(HOST_KEY_FILE);
    if let Some(seed) = std::fs::read_to_string(&path).ok().and_then(|text| from_hex(&text)) {
        return Identity::from_seed(seed);
    }
    let identity = Identity::generate().expect("random numbers for the host's key");
    let saved = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(format!("{}\n", to_hex(&identity.seed())).as_bytes()));
    match saved {
        Ok(()) => info!("Native protocol: made the host's key in {}", path.display()),
        Err(e) => warn!("Native protocol: the host's key was not saved to {}: {e}. Paired headsets refuse the host at its next start.", path.display()),
    }
    identity
}

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

/// A headset that waits for the user's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub key: [u8; KEY_LEN],
    pub name: String,
    pub ip: IpAddr,
    /// The code that the headset shows too
    pub code: u32,
    /// The user's answer, once given
    answer: Option<bool>,
}

pub struct Pairings {
    path: PathBuf,
    keys: Mutex<Vec<([u8; KEY_LEN], String)>>,
    window: Mutex<Option<Window>>,
    request: Mutex<Option<Request>>,
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
            request: Mutex::new(None),
        }
    }

    /// What the `native` status line says about pairing: the names of the paired headsets (the
    /// start of the key for a headset without a name) and the seconds that the window stays open,
    /// or `None` while no window is open.
    pub fn summary(&self) -> (Vec<String>, Option<u64>) {
        let names = self
            .keys
            .lock()
            .unwrap()
            .iter()
            .map(|(key, name)| if name.is_empty() { to_hex(&key[..4]) } else { name.clone() })
            .collect();
        let seconds = self.window.lock().unwrap().as_ref().and_then(|w| WINDOW_OPEN.checked_sub(w.opened.elapsed())).map(|left| left.as_secs());
        (names, seconds)
    }

    pub fn contains(&self, key: &[u8; KEY_LEN]) -> bool {
        self.keys.lock().unwrap().iter().any(|(known, _)| known == key)
    }

    /// Starts a request for the headset with `key`. It fails while another headset waits: the
    /// user sees one request at a time.
    pub fn ask(&self, key: [u8; KEY_LEN], name: &str, ip: IpAddr, code: u32) -> bool {
        let mut request = self.request.lock().unwrap();
        if request.is_some() {
            return false;
        }
        *request = Some(Request { key, name: display_name(name), ip, code, answer: None });
        true
    }

    /// The headsets that wait for the user's answer
    pub fn waiting(&self) -> Vec<Request> {
        self.request.lock().unwrap().iter().cloned().collect()
    }

    /// Records the user's answer for the headset with `key`. It returns whether a request for that
    /// key waited. The key names the request, so a late click on an old card cannot allow the
    /// headset of a newer request.
    pub fn answer(&self, key: &[u8; KEY_LEN], allow: bool) -> bool {
        match self.request.lock().unwrap().as_mut().filter(|request| request.key == *key) {
            Some(request) => {
                request.answer = Some(allow);
                true
            }
            None => false,
        }
    }

    /// The user's answer for the request of `key`, if it came
    pub fn answered(&self, key: &[u8; KEY_LEN]) -> Option<bool> {
        self.request.lock().unwrap().as_ref().filter(|request| request.key == *key).and_then(|request| request.answer)
    }

    /// Ends the request of `key`, whatever became of it
    pub fn withdraw(&self, key: &[u8; KEY_LEN]) {
        let mut request = self.request.lock().unwrap();
        if request.as_ref().is_some_and(|request| request.key == *key) {
            *request = None;
        }
    }

    /// Remembers a headset for good: in memory and in the file.
    pub fn add(&self, key: &[u8; KEY_LEN], name: &str) {
        let mut keys = self.keys.lock().unwrap();
        if keys.iter().any(|(known, _)| known == key) {
            return;
        }
        let name = display_name(name);
        keys.push((*key, name.clone()));
        let line = format!("{} {name}\n", to_hex(key));
        if let Err(e) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).and_then(|mut f| f.write_all(line.as_bytes())) {
            warn!("Native protocol: the pairing of {name} was not saved to {}: {e}", self.path.display());
        }
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
        self.add(key, name);
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
        assert!(!pairings.contains(&headset));
        assert_eq!(pairings.try_pair(&headset, "Meta Quest 3"), Attempt::Paired);
        assert!(pairings.contains(&headset));
        // The window closed with the first pairing
        assert_eq!(pairings.try_pair(&key(2).0, "Another"), Attempt::NoWindow);
        // A new host process reads the key from the file and needs no window
        let again = Pairings::load(&dir, None);
        assert!(again.contains(&headset) && !again.contains(&key(2).0));
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
    }

    #[test]
    fn the_host_makes_its_key_once_and_keeps_it_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = dir("host-key");
        std::fs::remove_file(dir.join(HOST_KEY_FILE)).ok();
        let first = host_identity(&dir);
        let again = host_identity(&dir);
        assert_eq!(first.public_key(), again.public_key());
        let mode = std::fs::metadata(dir.join(HOST_KEY_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "only the user reads the host's seed");
        // A damaged file gives a new key. The host does not crash.
        std::fs::write(dir.join(HOST_KEY_FILE), "not a key").unwrap();
        assert_ne!(host_identity(&dir).public_key(), first.public_key());
    }

    #[test]
    fn the_summary_names_the_paired_headsets_and_the_open_window() {
        let dir = dir("summary");
        let (headset, code) = key(4);
        let pairings = Pairings::load(&dir, Some(code));
        let (names, open) = pairings.summary();
        assert!(names.is_empty());
        assert!(open.is_some_and(|s| s > WINDOW_OPEN.as_secs() - 5 && s <= WINDOW_OPEN.as_secs()));
        pairings.try_pair(&headset, "Meta Quest 3");
        // The pairing closed the window
        assert_eq!(pairings.summary(), (vec!["Meta Quest 3".to_string()], None));
        // A line without a name shows the start of the key
        std::fs::write(dir.join(FILE), format!("{} \n", to_hex(&headset))).unwrap();
        assert_eq!(Pairings::load(&dir, None).summary().0, [to_hex(&headset[..4])]);
    }

    #[test]
    fn without_a_window_nothing_pairs() {
        let pairings = Pairings::load(&dir("none"), None);
        assert_eq!(pairings.try_pair(&key(1).0, "x"), Attempt::NoWindow);
    }

    fn ip() -> IpAddr {
        [192, 168, 1, 30].into()
    }

    #[test]
    fn a_request_takes_the_answer_of_its_own_key_only() {
        let pairings = Pairings::load(&dir("request"), None);
        let (headset, other) = (key(1).0, key(2).0);
        assert!(pairings.ask(headset, "Meta Quest 3", ip(), 123_456));
        let waiting = pairings.waiting();
        assert_eq!(waiting.len(), 1);
        assert_eq!((waiting[0].name.as_str(), waiting[0].code, waiting[0].ip), ("Meta Quest 3", 123_456, ip()));
        // An answer that names another key does nothing
        assert!(!pairings.answer(&other, true));
        assert_eq!(pairings.answered(&headset), None);
        assert!(pairings.answer(&headset, false));
        assert_eq!(pairings.answered(&headset), Some(false));
        assert_eq!(pairings.answered(&other), None);
        // Withdrawing another key's request does nothing, and withdrawing its own ends it
        pairings.withdraw(&other);
        assert_eq!(pairings.waiting().len(), 1);
        pairings.withdraw(&headset);
        assert!(pairings.waiting().is_empty());
        // A late answer for a request that ended does nothing
        assert!(!pairings.answer(&headset, true));
        assert_eq!(pairings.answered(&headset), None);
    }

    #[test]
    fn one_request_waits_at_a_time() {
        let pairings = Pairings::load(&dir("one"), None);
        assert!(pairings.ask(key(1).0, "First", ip(), 1));
        assert!(!pairings.ask(key(2).0, "Second", ip(), 2));
        assert_eq!(pairings.waiting()[0].name, "First");
        pairings.withdraw(&key(1).0);
        assert!(pairings.ask(key(2).0, "Second", ip(), 2));
    }

    #[test]
    fn an_allowed_headset_stays_paired_after_a_restart() {
        let dir = dir("allowed");
        let pairings = Pairings::load(&dir, None);
        let headset = key(5).0;
        pairings.add(&headset, "Meta Quest 3");
        // A second add of the same key adds no second line
        pairings.add(&headset, "Meta Quest 3");
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap().lines().count(), 1);
        assert!(Pairings::load(&dir, None).contains(&headset));
    }

    #[test]
    fn the_name_of_a_headset_is_short_and_has_no_control_characters() {
        assert_eq!(display_name("Meta Quest 3"), "Meta Quest 3");
        assert_eq!(display_name("\n\u{7}"), "A headset");
        assert_eq!(display_name(""), "A headset");
        assert!(display_name(&"x".repeat(5000)).len() <= 40);
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
