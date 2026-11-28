use super::*;
use std::time::{Duration, Instant};

/// The account of the test folders. Its Steam ID is STEAM_ID_BASE plus this number.
const ACCOUNT: u64 = 1234;

/// A Steam folder in which the account signs in by itself, with an empty cloud log.
fn steam_folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cranvr-cloud-{name}-{}", std::process::id()));
    fs::create_dir_all(dir.join("config")).unwrap();
    fs::create_dir_all(dir.join("logs")).unwrap();
    let users = format!(
        "\"users\"\n{{\n\t\"{}\"\n\t{{\n\t\t\"AccountName\"\t\t\"someone\"\n\t\t\"AutoLogin\"\t\t\"1\"\n\t}}\n\t\"{}\"\n\t{{\n\t\t\"AutoLogin\"\t\t\"0\"\n\t}}\n}}\n",
        STEAM_ID_BASE + ACCOUNT,
        STEAM_ID_BASE + 99
    );
    fs::write(dir.join("config/loginusers.vdf"), users).unwrap();
    fs::write(dir.join(CLOUD_LOG), "").unwrap();
    dir
}

/// Writes the game's cache with one entry per sync state, as Steam writes it.
fn write_cache(steam: &Path, app_id: &str, states: &[&str]) {
    let dir = steam.join("userdata").join(ACCOUNT.to_string()).join(app_id);
    fs::create_dir_all(&dir).unwrap();
    let mut text = format!("\"{app_id}\"\n{{\n\t\"ChangeNumber\"\t\t\"0\"\n\t\"OSType\"\t\t\"0\"\n");
    for (i, state) in states.iter().enumerate() {
        text += &format!("\t\"save/slot{i}.sav\"\n\t{{\n\t\t\"root\"\t\t\"1\"\n\t\t\"size\"\t\t\"10\"\n\t\t\"syncstate\"\t\t\"{state}\"\n\t\t\"persiststate\"\t\t\"0\"\n\t}}\n");
    }
    text += "}\n";
    fs::write(dir.join("remotecache.vdf"), text).unwrap();
}

fn log(steam: &Path, line: &str) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(steam.join(CLOUD_LOG)).unwrap();
    writeln!(file, "[2026-11-28 01:11:09] {line}").unwrap();
}

#[test]
fn the_account_that_signs_in_by_itself_holds_the_cache_and_a_file_in_state_2_waits() {
    let steam = steam_folder("count");
    assert_eq!(login(&steam), Some(Login { account: ACCOUNT, offline: false }));
    write_cache(&steam, "100", &["2", "1", "2"]);
    write_cache(&steam, "200", &["1", "1"]);
    let cache = Cache::of(&steam, ACCOUNT, "100").unwrap();
    assert_eq!((cache.files.in_sync, cache.files.waiting()), (1, 2));
    assert_eq!(Cache::of(&steam, ACCOUNT, "300"), None, "a game with no cache has no known cloud file");
    let notes = notes(&steam, ["100", "200", "300"].into_iter());
    assert_eq!(notes.len(), 1, "only the game with files that wait gets a word: {notes:?}");
    assert!(notes["100"].contains("2 cloud files of this game that are not on this Mac yet"), "{}", notes["100"]);
    fs::remove_dir_all(steam).ok();
}

#[test]
fn a_start_waits_for_steam_s_look_and_its_download() {
    let steam = steam_folder("download");
    write_cache(&steam, "100", &["1", "1"]);
    log(&steam, "[AppID 100] Download complete, result I/O Operation Failed");
    thread::sleep(Duration::from_millis(20));
    let look = Look::before_start(&steam, ACCOUNT, "100");
    let writer = {
        let steam = steam.clone();
        thread::spawn(move || {
            // Steam's look finds two files newer in the cloud, then downloads them
            thread::sleep(Duration::from_millis(300));
            log(&steam, "[AppID 100] Eval complete");
            write_cache(&steam, "100", &["2", "2"]);
            log(&steam, "[AppID 200] Download complete, result I/O Operation Failed");
            thread::sleep(Duration::from_millis(300));
            log(&steam, "[AppID 100] Download complete, result OK");
            write_cache(&steam, "100", &["1", "1"]);
        })
    };
    let begun = Instant::now();
    assert_eq!(look.wait(Duration::from_secs(5), || true), Ok(()));
    assert!(begun.elapsed() >= Duration::from_millis(600), "the cache from before Steam's start answers nothing: {:?}", begun.elapsed());
    writer.join().unwrap();
    fs::remove_dir_all(steam).ok();
}

#[test]
fn a_failed_download_refuses_the_start_with_steam_s_result() {
    let steam = steam_folder("failed");
    write_cache(&steam, "100", &["2", "2"]);
    thread::sleep(Duration::from_millis(20));
    let look = Look::before_start(&steam, ACCOUNT, "100");
    write_cache(&steam, "100", &["2", "2"]);
    log(&steam, "[AppID 100] Download complete, result I/O Operation Failed");
    let refusal = look.wait(Duration::from_secs(5), || true).unwrap_err();
    assert_eq!(refusal, Refusal::Failed { waiting: 2, result: "I/O Operation Failed".into() });
    assert!(refusal.words().starts_with("Steam could not download 2 cloud files of this game: I/O Operation Failed."), "{}", refusal.words());
    fs::remove_dir_all(steam).ok();
}

#[test]
fn a_game_whose_cache_steam_deletes_at_its_look_starts() {
    let steam = steam_folder("deleted");
    write_cache(&steam, "100", &["1"]);
    let look = Look::before_start(&steam, ACCOUNT, "100");
    fs::remove_file(steam.join("userdata").join(ACCOUNT.to_string()).join("100/remotecache.vdf")).unwrap();
    assert_eq!(look.wait(Duration::from_millis(300), || true), Err(Refusal::NoLook), "a cache that is gone before Steam looked answers nothing");
    log(&steam, "[AppID 100] Eval complete");
    assert_eq!(look.wait(Duration::from_secs(5), || true), Ok(()));
    fs::remove_dir_all(steam).ok();
}

#[test]
fn a_start_stops_waiting_when_steam_ends() {
    let steam = steam_folder("ended");
    write_cache(&steam, "100", &["2"]);
    let look = Look::before_start(&steam, ACCOUNT, "100");
    assert_eq!(look.wait(Duration::from_secs(5), || false), Err(Refusal::SteamEnded { waiting: 1 }));
    fs::remove_dir_all(steam).ok();
}

#[test]
fn each_sync_state_of_steam_s_cache_has_its_own_words() {
    let steam = steam_folder("states");
    // The states of a test game's two saves, as Steam's cloud log named them (cranvr issue #217)
    write_cache(&steam, "100", &["1", "3"]);
    write_cache(&steam, "200", &["3", "4"]);
    write_cache(&steam, "300", &["1", "7"]);
    let files = Cache::of(&steam, ACCOUNT, "200").unwrap().files;
    assert_eq!(files, Files { upload: 1, conflict: 1, ..Files::default() });
    assert_eq!(files.waiting(), 0, "a file that waits for its upload or a conflict needs no download");
    assert_eq!(Cache::of(&steam, ACCOUNT, "300").unwrap().files.waiting(), 1, "a state that cranvr does not know waits for Steam's start");
    let notes = notes(&steam, ["100", "200", "300"].into_iter());
    assert!(notes["100"].starts_with("Steam has 1 cloud file of this game that changed on this Mac and is not in the cloud yet."), "{}", notes["100"]);
    assert!(notes["200"].starts_with("1 cloud file of this game changed on this Mac and in the cloud. Steam asks which copy to keep"), "{}", notes["200"]);
    assert!(notes["300"].contains("1 cloud file of this game that is not on this Mac yet"), "{}", notes["300"]);
    fs::remove_dir_all(steam).ok();
}

#[test]
fn a_conflict_that_steam_s_look_finds_refuses_the_start() {
    let steam = steam_folder("conflict");
    write_cache(&steam, "100", &["2", "1"]);
    thread::sleep(Duration::from_millis(20));
    let look = Look::before_start(&steam, ACCOUNT, "100");
    log(&steam, "[AppID 100] Sync conflict for file save/slot0.sav");
    log(&steam, "[AppID 100] Eval complete");
    write_cache(&steam, "100", &["4", "1"]);
    let refusal = look.wait(Duration::from_secs(5), || true).unwrap_err();
    assert_eq!(refusal, Refusal::Conflict { files: 1 });
    assert!(refusal.words().contains("Start the game once from the Steam window and choose the copy to keep. Quit the game after that."), "{}", refusal.words());
    fs::remove_dir_all(steam).ok();
}

#[test]
fn a_steam_that_writes_no_line_about_the_game_did_not_look() {
    let steam = steam_folder("nolook");
    write_cache(&steam, "100", &["1", "1"]);
    thread::sleep(Duration::from_millis(20));
    let look = Look::before_start(&steam, ACCOUNT, "100");
    // An offline Steam or one that waits for a sign-in writes lines of other apps only
    log(&steam, "[AppID 7] Starting sync (init,)");
    let refusal = look.wait(Duration::from_millis(300), || true).unwrap_err();
    assert_eq!(refusal, Refusal::NoLook);
    assert!(refusal.words().starts_with("Steam did not look at this game's cloud files in 120 s after its start."), "{}", refusal.words());
    log(&steam, "[AppID 100] Starting sync (eval,)");
    assert_eq!(look.wait(Duration::from_millis(300), || true), Err(Refusal::Late { waiting: 0 }), "a look that leaves the cache as it was is late");
    fs::remove_dir_all(steam).ok();
}

#[test]
fn steam_in_offline_mode_holds_a_game_whose_files_wait() {
    let steam = steam_folder("offline");
    let users = fs::read_to_string(steam.join("config/loginusers.vdf")).unwrap();
    // The account that signs in by itself goes offline, the other one stays as it was
    let users = users.replacen("\"AutoLogin\"\t\t\"1\"", "\"AutoLogin\"\t\t\"1\"\n\t\t\"WantsOfflineMode\"\t\t\"1\"", 1);
    fs::write(steam.join("config/loginusers.vdf"), users).unwrap();
    assert_eq!(login(&steam), Some(Login { account: ACCOUNT, offline: true }));
    write_cache(&steam, "100", &["2", "1"]);
    let notes = notes(&steam, ["100"].into_iter());
    assert!(notes["100"].contains("Steam downloads no file in offline mode. Go online in Steam's menu."), "{}", notes["100"]);
    assert!(Refusal::Offline { waiting: 1 }.words().contains("cranvr did not start the game."));
    fs::remove_dir_all(steam).ok();
}
