//! The program that Steam starts for a game: the launch entries of Steam's app cache, `appcache/appinfo.vdf`.
//! The file is binary KeyValues. Steam writes version 29 of it (magic 0x07564429): a header with the offset of a
//! table of strings, then one record for each app, whose keys are numbers into that table. A file of another
//! version or one that does not read gives no entries, and the caller takes the program that it finds in the
//! game's folder.
//!
//! Steam offers a game's entries for the operating system, its bitness, the user's beta branch and DLC. cranvr runs
//! 64-bit Windows programs, so [`choose`] takes, in the order of their numbers:
//! - entries for Windows: their `oslist` is empty or names `windows`,
//! - not for a 32-bit system (`osarch` 32), not for a beta branch (`betakey`), not for an owner of a DLC
//!   (`ownsdlc`) and not for another realm than Steam's global one,
//! - with an executable that ends in `.exe` and exists in the game's folder,
//! - of type `vr` first (a VR streamer starts the game in VR, as Steam does in SteamVR), then the plain types
//!   (`default`, `none` or none at all), then the other choices that Steam offers in its dialog (`option1` to
//!   `option3`, `othervr`). Servers, editors, tools and benchmarks never start.
//!
//! A program starts in the entry's `workingdir` inside the game's folder. An entry without one starts in the
//! program's own folder, as cranvr started every game before. Steam's documentation does not say which folder
//! Steam takes then, and it is not measured here.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

/// The magic number of version 29 of appinfo.vdf
const MAGIC_29: u32 = 0x0756_4429;

/// One launch entry of an app, as Steam's cache holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// The entry's key: Steam numbers the entries from 0
    pub number: u32,
    /// The program, relative to the game's folder, with `/` or `\`
    pub executable: String,
    pub arguments: String,
    pub working_dir: String,
    /// `default`, `vr`, `option1` and the like; empty when the entry has none
    pub kind: String,
    pub oslist: String,
    pub osarch: String,
    pub betakey: String,
    pub ownsdlc: String,
    pub realm: String,
}

/// What Steam starts for a game: the program, its arguments and the folder that it starts in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub working_dir: PathBuf,
}

/// A value of a binary KeyValues record
#[derive(Debug)]
enum Value {
    Map(Vec<(String, Value)>),
    Text(String),
    Number(i64),
}

impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(items) => items.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            _ => None,
        }
    }

    fn text(&self) -> String {
        match self {
            Value::Text(text) => text.clone(),
            Value::Number(n) => n.to_string(),
            Value::Map(_) => String::new(),
        }
    }
}

/// Reads binary KeyValues from `data` at `at`. Each read checks its bounds, so a file that ends early or holds
/// nonsense gives None, never a panic.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
    strings: &'a [String],
}

impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Option<&[u8]> {
        let slice = self.data.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }

    fn cstring(&mut self) -> Option<String> {
        let rest = self.data.get(self.at..)?;
        let end = rest.iter().position(|b| *b == 0)?;
        let text = String::from_utf8_lossy(&rest[..end]).into_owned();
        self.at += end + 1;
        Some(text)
    }

    /// A map up to its end mark. `depth` keeps a file of nested maps from running the stack out.
    fn map(&mut self, depth: u32) -> Option<Value> {
        if depth > 32 {
            return None;
        }
        let mut items = Vec::new();
        loop {
            let kind = self.u8()?;
            if kind == 8 {
                return Some(Value::Map(items));
            }
            let key = self.strings.get(self.u32()? as usize)?.clone();
            let value = match kind {
                0 => self.map(depth + 1)?,
                1 => Value::Text(self.cstring()?),
                2 => Value::Number(i32::from_le_bytes(self.bytes(4)?.try_into().ok()?) as i64),
                3 => Value::Number(f32::from_le_bytes(self.bytes(4)?.try_into().ok()?) as i64),
                7 => Value::Number(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?) as i64),
                _ => return None,
            };
            items.push((key, value));
        }
    }
}

/// The launch entries of every app in a version 29 appinfo.vdf, by app ID. None for another version or a file
/// that does not read.
pub fn parse(data: &[u8]) -> Option<HashMap<u32, Vec<Entry>>> {
    let mut header = Reader { data, at: 0, strings: &[] };
    if header.u32()? != MAGIC_29 {
        return None;
    }
    header.u32()?; // the universe
    let table = usize::try_from(u64::from_le_bytes(header.bytes(8)?.try_into().ok()?)).ok()?;
    let mut table_reader = Reader { data, at: table, strings: &[] };
    let count = table_reader.u32()? as usize;
    let mut strings = Vec::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        strings.push(table_reader.cstring()?);
    }
    let mut apps = HashMap::new();
    let mut at = header.at;
    while at < table {
        let mut record = Reader { data, at, strings: &strings };
        let app = record.u32()?;
        if app == 0 {
            break;
        }
        let size = record.u32()? as usize;
        let next = record.at.checked_add(size).filter(|next| *next <= table)?;
        // info state 4, last change 4, access token 8, SHA-1 of the text 20, change number 4, SHA-1 of the binary 20.
        // A record that does not read costs only its own app: its size says where the next one starts.
        let mut record = Reader { data: &data[..next], at: record.at, strings: &strings };
        let values = record.bytes(60).is_some().then(|| record.map(0)).flatten();
        if let Some(values) = values {
            let launch = values.get("appinfo").and_then(|v| v.get("config")).and_then(|v| v.get("launch"));
            if let Some(Value::Map(items)) = launch {
                let entries = items.iter().filter_map(|(key, entry)| entry_of(key, entry)).collect();
                apps.insert(app, entries);
            }
        }
        at = next;
    }
    Some(apps)
}

fn entry_of(key: &str, value: &Value) -> Option<Entry> {
    let text = |name: &str| value.get(name).map(Value::text).unwrap_or_default();
    let config = |name: &str| value.get("config").and_then(|c| c.get(name)).map(Value::text).unwrap_or_default();
    Some(Entry {
        number: key.parse().ok()?,
        executable: text("executable"),
        arguments: text("arguments"),
        working_dir: text("workingdir"),
        kind: text("type"),
        oslist: config("oslist"),
        osarch: config("osarch"),
        betakey: config("betakey"),
        ownsdlc: config("ownsdlc"),
        realm: config("realm"),
    })
}

/// A path of Steam's, with `/` or `\` and maybe a leading `.\`, inside `folder`
fn inside(folder: &Path, relative: &str) -> PathBuf {
    let mut path = folder.to_owned();
    for part in relative.split(['/', '\\']).filter(|part| !part.is_empty() && *part != ".") {
        path.push(part);
    }
    path
}

/// How Steam's dialog ranks a type: 0 is the first choice, None is never
fn rank(kind: &str) -> Option<u8> {
    match kind.to_ascii_lowercase().as_str() {
        "vr" => Some(0),
        "" | "default" | "none" => Some(1),
        "option1" | "option2" | "option3" | "othervr" => Some(2),
        _ => None,
    }
}

/// The entry that cranvr starts for a game in `folder`, by the rules at the top of this file.
pub fn choose(entries: &[Entry], folder: &Path) -> Option<Launch> {
    let mut usable: Vec<(u8, u32, &Entry)> = entries
        .iter()
        .filter(|e| e.oslist.is_empty() || e.oslist.split(',').any(|os| os.trim().eq_ignore_ascii_case("windows")))
        .filter(|e| e.osarch.trim() != "32")
        .filter(|e| e.betakey.is_empty() && e.ownsdlc.is_empty())
        .filter(|e| e.realm.is_empty() || e.realm.eq_ignore_ascii_case("steamglobal"))
        .filter(|e| e.executable.to_ascii_lowercase().ends_with(".exe") && inside(folder, &e.executable).is_file())
        .filter_map(|e| Some((rank(&e.kind)?, e.number, e)))
        .collect();
    usable.sort_by_key(|(rank, number, _)| (*rank, *number));
    let (_, _, entry) = usable.first()?;
    let program = inside(folder, &entry.executable);
    let working_dir = if entry.working_dir.trim().is_empty() { program.parent()?.to_owned() } else { inside(folder, entry.working_dir.trim()) };
    Some(Launch { program, arguments: split_arguments(&entry.arguments), working_dir })
}

/// A command line's arguments, as Windows splits them for the common cases: spaces part them, and double quotes
/// hold an argument with spaces together.
pub fn split_arguments(text: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut started) = (false, false);
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        arguments.push(current);
    }
    arguments
}

/// The launch entries from the appinfo.vdf at `file`, read again only when its time or size changed: the Play tab
/// lists the games every 2 seconds. None when the file is missing or does not read.
pub fn entries(file: &Path, app: u32) -> Option<Vec<Entry>> {
    type Cache = Option<(PathBuf, SystemTime, u64, Option<HashMap<u32, Vec<Entry>>>)>;
    static CACHE: Mutex<Cache> = Mutex::new(None);
    let meta = fs::metadata(file).ok()?;
    let (modified, len) = (meta.modified().ok()?, meta.len());
    let mut cache = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fresh = cache.as_ref().is_some_and(|(path, time, size, _)| path == file && *time == modified && *size == len);
    if !fresh {
        let parsed = fs::read(file).ok().and_then(|data| parse(&data));
        if parsed.is_none() {
            crate::backend::log(format!("cranvr cannot read Steam's app cache {}: it looks in each game's folder for its program.", file.display()));
        }
        *cache = Some((file.to_owned(), modified, len, parsed));
    }
    cache.as_ref()?.3.as_ref()?.get(&app).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binary KeyValues of version 29 for tests: keys go into the table of strings.
    #[derive(Default)]
    struct Writer {
        strings: Vec<String>,
    }

    enum Kv<'a> {
        Map(Vec<(&'a str, Kv<'a>)>),
        Text(&'a str),
        Int(i32),
    }

    impl Writer {
        fn key(&mut self, key: &str) -> u32 {
            match self.strings.iter().position(|s| s == key) {
                Some(at) => at as u32,
                None => {
                    self.strings.push(key.to_owned());
                    self.strings.len() as u32 - 1
                }
            }
        }

        fn map(&mut self, out: &mut Vec<u8>, items: &[(&str, Kv)]) {
            for (key, value) in items {
                let kind = match value {
                    Kv::Map(_) => 0u8,
                    Kv::Text(_) => 1,
                    Kv::Int(_) => 2,
                };
                out.push(kind);
                let index = self.key(key);
                out.extend(index.to_le_bytes());
                match value {
                    Kv::Map(inner) => self.map(out, inner),
                    Kv::Text(text) => {
                        out.extend(text.as_bytes());
                        out.push(0);
                    }
                    Kv::Int(n) => out.extend(n.to_le_bytes()),
                }
            }
            out.push(8);
        }

        fn file(mut self, apps: &[(u32, Kv)]) -> Vec<u8> {
            let mut records = Vec::new();
            for (app, kv) in apps {
                let mut body = vec![0u8; 60];
                let Kv::Map(items) = kv else { panic!() };
                self.map(&mut body, items);
                records.extend(app.to_le_bytes());
                records.extend((body.len() as u32).to_le_bytes());
                records.extend(body);
            }
            records.extend(0u32.to_le_bytes());
            let mut out = Vec::new();
            out.extend(MAGIC_29.to_le_bytes());
            out.extend(1u32.to_le_bytes());
            out.extend(((16 + records.len()) as u64).to_le_bytes());
            out.extend(records);
            out.extend((self.strings.len() as u32).to_le_bytes());
            for s in &self.strings {
                out.extend(s.as_bytes());
                out.push(0);
            }
            out
        }
    }

    fn app<'a>(launch: Vec<(&'a str, Kv<'a>)>) -> Kv<'a> {
        Kv::Map(vec![("appinfo", Kv::Map(vec![("appid", Kv::Int(1)), ("config", Kv::Map(vec![("installdir", Kv::Text("Game")), ("launch", Kv::Map(launch))]))]))])
    }

    fn entry<'a>(exe: &'a str, kind: &'a str, config: Vec<(&'a str, Kv<'a>)>) -> Kv<'a> {
        Kv::Map(vec![("executable", Kv::Text(exe)), ("type", Kv::Text(kind)), ("arguments", Kv::Text("-popupwindow")), ("config", Kv::Map(config))])
    }

    fn folder(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cranvr-appinfo-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        for file in files {
            let path = inside(&dir, file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"MZ").unwrap();
        }
        dir
    }

    #[test]
    fn the_lab_starts_its_vr_entry_with_its_arguments_not_the_largest_program() {
        // The Lab's entry in Steam's cache, and a side experience with a larger program
        let data = Writer::default().file(&[
            (228980, app(vec![("0", Kv::Map(vec![("executable", Kv::Text("steam://open/tools")), ("type", Kv::Text("default"))]))])),
            (450390, app(vec![("0", entry("TheLab/win64/TheLab.exe", "vr", vec![("oslist", Kv::Text("windows"))]))])),
        ]);
        let apps = parse(&data).unwrap();
        assert_eq!(apps.len(), 2);
        let dir = folder("lab", &["TheLab/win64/TheLab.exe", "SecretShop/win64/SecretShop.exe"]);
        let launch = choose(&apps[&450390], &dir).unwrap();
        assert_eq!(launch.program, dir.join("TheLab/win64/TheLab.exe"));
        assert_eq!(launch.arguments, ["-popupwindow"]);
        assert_eq!(launch.working_dir, dir.join("TheLab/win64"), "no working folder: the program's folder");
        // A URL is no program
        assert_eq!(choose(&apps[&228980], &dir), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_32_bit_entry_a_beta_a_dlc_and_another_system_give_way_to_the_64_bit_windows_entry() {
        let data = Writer::default().file(&[(
            465240,
            app(vec![
                ("1", entry("Bin/beta.exe", "default", vec![("betakey", Kv::Text("public_beta"))])),
                ("2", entry("Bin/dlc.exe", "default", vec![("ownsdlc", Kv::Text("123"))])),
                ("3", entry("Bin/SamTLH.exe", "vr", vec![("oslist", Kv::Text("windows")), ("osarch", Kv::Text("32"))])),
                ("4", entry("Bin/x64/SamTLH.exe", "vr", vec![("oslist", Kv::Text("windows")), ("osarch", Kv::Int(64))])),
                ("5", entry("Bin/x64/SamTLH", "vr", vec![("oslist", Kv::Text("linux"))])),
                ("6", entry("Bin/x64/Editor.exe", "editor", vec![])),
            ]),
        )]);
        let apps = parse(&data).unwrap();
        let dir = folder("sam", &["Bin/beta.exe", "Bin/dlc.exe", "Bin/SamTLH.exe", "Bin/x64/SamTLH.exe", "Bin/x64/Editor.exe"]);
        assert_eq!(choose(&apps[&465240], &dir).unwrap().program, dir.join("Bin/x64/SamTLH.exe"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_vr_entry_comes_before_the_desktop_entry_and_a_missing_program_does_not_count() {
        let data = Writer::default().file(&[(
            7,
            app(vec![
                ("0", entry("Game.exe", "default", vec![])),
                ("1", entry("Missing\\VR.exe", "vr", vec![])),
                ("2", Kv::Map(vec![("executable", Kv::Text(".\\bin\\GameVR.exe")), ("type", Kv::Text("vr")), ("workingdir", Kv::Text("bin"))])),
            ]),
        )]);
        let apps = parse(&data).unwrap();
        let dir = folder("vr", &["Game.exe", "bin/GameVR.exe"]);
        let launch = choose(&apps[&7], &dir).unwrap();
        assert_eq!(launch.program, dir.join("bin/GameVR.exe"));
        assert_eq!(launch.working_dir, dir.join("bin"));
        assert!(launch.arguments.is_empty());
        // Without the VR program the desktop entry is the one
        fs::remove_file(dir.join("bin/GameVR.exe")).unwrap();
        assert_eq!(choose(&apps[&7], &dir).unwrap().program, dir.join("Game.exe"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_record_that_does_not_read_costs_only_its_own_app() {
        let mut data = Writer::default().file(&[
            (1, app(vec![("0", entry("One.exe", "default", vec![]))])),
            (2, app(vec![("0", entry("Two.exe", "default", vec![]))])),
        ]);
        // The first record's first value gets a type that KeyValues does not have
        let first_value = 16 + 8 + 60;
        assert_eq!(data[first_value], 0);
        data[first_value] = 9;
        let apps = parse(&data).unwrap();
        assert!(!apps.contains_key(&1));
        assert_eq!(apps[&2][0].executable, "Two.exe");
    }

    #[test]
    fn another_version_or_a_cut_file_reads_as_nothing() {
        let data = Writer::default().file(&[(450390, app(vec![("0", entry("TheLab/win64/TheLab.exe", "vr", vec![]))]))]);
        let mut v28 = data.clone();
        v28[0] = 0x28;
        assert!(parse(&v28).is_none());
        for cut in [3, 15, 40, data.len() / 2, data.len() - 3] {
            let _ = parse(&data[..cut]);
        }
        assert!(parse(&data[..data.len() - 3]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn arguments_split_at_spaces_and_quotes_hold_them_together() {
        assert_eq!(split_arguments("-popupwindow"), ["-popupwindow"]);
        assert_eq!(split_arguments("  -a  -b "), ["-a", "-b"]);
        assert_eq!(split_arguments("-config \"My Games\\x.cfg\" -vr"), ["-config", "My Games\\x.cfg", "-vr"]);
        assert_eq!(split_arguments("\"\""), [""]);
        assert!(split_arguments("").is_empty());
    }
}
