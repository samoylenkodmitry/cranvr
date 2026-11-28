//! A Steam game's cloud files on this Mac, as Steam's cache for the signed-in account lists them.
//!
//! Steam downloads a game's cloud files at its own start and before it starts a game itself. cranvr starts a game
//! by its program (steam.rs), and Steam syncs nothing then. Right after an install Steam knows the game's cloud
//! files: its cloud log says "Need to download ... but not attempting now", and the game's `remotecache.vdf` gives
//! each file the sync state 2. The files then wait for Steam's next start. A game that starts without them makes new
//! saves and default settings. Steam takes those up to the cloud at the game's end. So the start of such a game
//! starts Steam again and waits until the cache lists every file in sync (state 1). When the files do not come, the
//! start fails with the reason on the game's row.
//!
//! The cache holds what Steam learned at its last look at the cloud: at its own start, at an install and at the end
//! of a game. A save that another computer sends to the cloud while Steam runs here reaches this Mac at Steam's next
//! start.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
    thread,
    time::{Duration, Instant, SystemTime},
};

/// Steam's 64-bit IDs of accounts begin at this number. A folder under `userdata` carries the rest, the account ID.
const STEAM_ID_BASE: u64 = 76_561_197_960_265_728;

/// How long a start waits after a start of Steam for Steam's look at the game's cloud files and their download.
/// Steam looked about 4 s after its start and downloaded 28 files of 60 MB in 3 s.
pub const SYNC_TIME: Duration = Duration::from_secs(120);

/// The account that Steam signs in by itself (`MostRecent` or `AutoLogin` in `loginusers.vdf`), else the only one
/// there. `steam` is Steam's folder.
pub fn account(steam: &Path) -> Option<u64> {
    let text = fs::read_to_string(steam.join("config/loginusers.vdf")).ok()?;
    // (account, signs in by itself)
    let mut users: Vec<(u64, bool)> = Vec::new();
    for line in text.lines() {
        match fields(line).as_slice() {
            [id] => {
                if let Some(account) = id.parse::<u64>().ok().and_then(|id| id.checked_sub(STEAM_ID_BASE)) {
                    users.push((account, false));
                }
            }
            [key, "1"] if key.eq_ignore_ascii_case("MostRecent") || key.eq_ignore_ascii_case("AutoLogin") => {
                if let Some(user) = users.last_mut() {
                    user.1 = true;
                }
            }
            _ => {}
        }
    }
    let only = (users.len() == 1).then(|| users[0]);
    users.iter().copied().find(|user| user.1).or(only).map(|user| user.0)
}

/// The quoted fields of a line of Steam's text files
fn fields(line: &str) -> Vec<&str> {
    line.split('"').filter(|field| !field.trim().is_empty()).collect()
}

/// What Steam's cache says about a game's cloud files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cache {
    /// When Steam wrote the cache last
    pub written: SystemTime,
    /// The files that Steam lists for the game
    pub files: usize,
    /// The files whose sync state is not 1 (in sync). Steam gives 2 to a file in the cloud that it still has to
    /// download.
    pub waiting: usize,
}

/// What a read of a game's cache found.
enum Found {
    /// No cache
    Nothing,
    /// A cache that Steam writes now: its braces do not close yet
    Partial,
    Cache(Cache),
}

impl Cache {
    /// The cache of the game `app_id` for `account` in Steam's folder `steam`. None when Steam keeps no cache for
    /// the game: it knows no cloud file of the game, or it deleted the cache because the cloud holds none.
    pub fn of(steam: &Path, account: u64, app_id: &str) -> Option<Self> {
        match Self::find(steam, account, app_id) {
            Found::Cache(cache) => Some(cache),
            Found::Nothing | Found::Partial => None,
        }
    }

    fn find(steam: &Path, account: u64, app_id: &str) -> Found {
        let path = steam.join("userdata").join(account.to_string()).join(app_id).join("remotecache.vdf");
        let (Ok(written), Ok(text)) = (fs::metadata(&path).and_then(|meta| meta.modified()), fs::read_to_string(&path)) else { return Found::Nothing };
        // A cache read while Steam writes it would count too few files that wait
        if !text.trim_end().ends_with('}') || text.matches('{').count() != text.matches('}').count() {
            return Found::Partial;
        }
        let states = text.lines().filter_map(|line| match fields(line).as_slice() {
            [key, state] if key.eq_ignore_ascii_case("syncstate") => Some(*state == "1"),
            _ => None,
        });
        let (files, waiting) = states.fold((0, 0), |(files, waiting), in_sync| (files + 1, waiting + usize::from(!in_sync)));
        Found::Cache(Self { written, files, waiting })
    }
}

/// "1 cloud file", "28 cloud files"
fn files(count: usize) -> String {
    if count == 1 { "1 cloud file".into() } else { format!("{count} cloud files") }
}

/// Why a game did not start: its cloud files did not come.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Another game runs, and a new start of Steam would end it
    GameRuns { waiting: usize },
    /// Steam did not end, so it could not start again
    SteamStays,
    /// Steam ended during the wait
    SteamEnded { waiting: usize },
    /// Steam's download ended with this result
    Failed { waiting: usize, result: String },
    /// Steam's look at the files or their download took longer than [`SYNC_TIME`]
    Late { waiting: usize },
}

impl Refusal {
    /// The words on the game's row and in the window's message
    pub fn words(&self) -> String {
        let not_started = "cranvr did not start the game. A start without these files would make new saves and settings, which Steam would put into the cloud over yours.";
        match self {
            Self::GameRuns { waiting } => format!(
                "Steam has {} of this game that are not on this Mac yet. Steam downloads them only at its start. A start of Steam now would end the game that runs. Stop that game, then press Play.",
                files(*waiting)
            ),
            Self::SteamStays => format!("Steam did not end in time for a new start. Steam downloads this game's cloud files only at its start. {not_started} Press Play to try again."),
            Self::SteamEnded { waiting } => format!("Steam ended before it downloaded {} of this game. {not_started} Press Play to try again.", files(*waiting)),
            Self::Failed { waiting, result } => format!("Steam could not download {} of this game: {result}. {not_started} Press Play to try again.", files(*waiting)),
            Self::Late { waiting } => format!("Steam did not download {} of this game in {} s. {not_started} Press Play to try again.", files(*waiting), SYNC_TIME.as_secs()),
        }
    }
}

/// A start of Steam that a game's start waits for. Steam looks at the game's cloud files at its start and downloads
/// the files that the cloud holds newer.
pub struct Look {
    steam: PathBuf,
    account: u64,
    app_id: String,
    /// When Steam started
    since: SystemTime,
    /// The length of Steam's cloud log at Steam's start: the lines after it are of this start
    log_from: u64,
    /// The game had a cache before Steam's start
    had_cache: bool,
}

impl Look {
    /// Notes the moment right before a start of Steam, for the game `app_id`.
    pub fn before_start(steam: &Path, account: u64, app_id: &str) -> Self {
        Self {
            steam: steam.to_owned(),
            account,
            app_id: app_id.to_owned(),
            since: SystemTime::now(),
            log_from: fs::metadata(steam.join(CLOUD_LOG)).map_or(0, |meta| meta.len()),
            had_cache: Cache::of(steam, account, app_id).is_some(),
        }
    }

    /// Waits until Steam has looked at the game's cloud files and downloaded those that the cloud holds newer, for
    /// up to `time`. `steam_runs` tells whether Steam still runs.
    pub fn wait(&self, time: Duration, steam_runs: impl Fn() -> bool) -> Result<(), Refusal> {
        let begun = Instant::now();
        let mut waiting = 0;
        loop {
            let lines = self.log_lines();
            match Cache::find(&self.steam, self.account, &self.app_id) {
                // Steam deletes the cache of a game whose cloud holds no file when it looks
                Found::Nothing if !self.had_cache || lines.iter().any(|line| line == "Eval complete") => return Ok(()),
                Found::Cache(cache) if cache.written >= self.since && cache.waiting == 0 => return Ok(()),
                Found::Cache(cache) => waiting = cache.waiting,
                Found::Nothing | Found::Partial => {}
            }
            let result = lines.iter().rev().find_map(|line| line.strip_prefix("Download complete, result "));
            if let Some(result) = result.filter(|result| *result != "OK") {
                return Err(Refusal::Failed { waiting, result: result.to_owned() });
            }
            if lines.iter().any(|line| line == "Download failed due to conflicts in build list") {
                return Err(Refusal::Failed { waiting, result: "the files on this Mac and in the cloud differ, and Steam asks which to keep".into() });
            }
            if !steam_runs() {
                return Err(Refusal::SteamEnded { waiting });
            }
            if begun.elapsed() > time {
                return Err(Refusal::Late { waiting });
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    /// The lines of Steam's cloud log for the game since Steam's start, without their time and app ID.
    fn log_lines(&self) -> Vec<String> {
        let Ok(mut file) = fs::File::open(self.steam.join(CLOUD_LOG)) else { return Vec::new() };
        // Steam starts a new log when the old one grew large: then every line is new
        let from = if file.metadata().is_ok_and(|meta| meta.len() < self.log_from) { 0 } else { self.log_from };
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(from)).is_err() || file.read_to_end(&mut bytes).is_err() {
            return Vec::new();
        }
        let tag = format!("[AppID {}] ", self.app_id);
        String::from_utf8_lossy(&bytes).lines().filter_map(|line| line.split_once(&tag).map(|(_, rest)| rest.trim().to_owned())).collect()
    }
}

/// Steam's cloud log, in Steam's folder
const CLOUD_LOG: &str = "logs/cloud_log.txt";

/// The note of a game's start about its cloud files, by app ID: Steam downloads them now, or the start failed.
static NOTES: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Sets the note of the start of the game `app_id`, or clears it.
pub fn set_note(app_id: &str, note: Option<String>) {
    let mut notes = NOTES.lock().unwrap_or_else(|e| e.into_inner());
    match note {
        Some(note) => notes.insert(app_id.to_owned(), note),
        None => notes.remove(app_id),
    };
}

/// The note of a game whose files wait while Steam runs
pub fn waiting_note(waiting: usize) -> String {
    format!("Steam downloads {} of this game. The game starts after them.", files(waiting))
}

/// The words on the row of each game in `app_ids` whose cloud files need a word: the note of its start, else a
/// count of the files that are not on this Mac yet.
pub fn notes<'a>(steam: &Path, app_ids: impl Iterator<Item = &'a str>) -> HashMap<String, String> {
    let account = account(steam);
    let mut notes = HashMap::new();
    for app_id in app_ids {
        let started = NOTES.lock().unwrap_or_else(|e| e.into_inner()).get(app_id).cloned();
        if let Some(note) = started {
            notes.insert(app_id.to_owned(), note);
            continue;
        }
        let waiting = account.and_then(|account| Cache::of(steam, account, app_id)).map_or(0, |cache| cache.waiting);
        if waiting > 0 {
            notes.insert(app_id.to_owned(), format!("Steam has {} of this game that are not on this Mac yet. Play starts Steam again first. Steam downloads the files at its start.", files(waiting)));
        }
    }
    notes
}

#[cfg(test)]
#[path = "tests/steam_cloud.rs"]
mod tests;
