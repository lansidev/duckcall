//! The settings duckcall needs in other apps. A store may only be written while its app is not running.

use crate::{Result, platform};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq)]
pub enum App {
    Handy,
    Fluxer,
    Discord,
}

pub enum Setup {
    /// Not installed, or never started: there are no settings to change yet.
    Absent,
    Done,
    Pending,
}

impl App {
    pub const ALL: [App; 3] = [App::Handy, App::Fluxer, App::Discord];

    /// Also the name the system starts the app by.
    pub fn name(self) -> &'static str {
        match self {
            App::Handy => "Handy",
            App::Fluxer => "Fluxer",
            App::Discord => "Discord",
        }
    }

    pub fn setup(self) -> Result<Setup> {
        let store = platform::store(self);
        if !store.exists() {
            return Ok(Setup::Absent);
        }
        let done = match self {
            App::Handy => Some(handy_done(&handy_read(&store)?)),
            App::Fluxer => fluxer_read(&store)?.map(|state| fluxer_done(&state)),
            // macOS can protect another app's data: the store is visible but unreadable. Treat it as absent.
            App::Discord => match discord_read(&store) {
                Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied) => None,
                state => state?.map(|state| discord_done(&state)),
            },
        };
        Ok(match done {
            None => Setup::Absent,
            Some(true) => Setup::Done,
            Some(false) => Setup::Pending,
        })
    }

    /// Writes what `setup` reported as pending. The app must not be running.
    pub fn configure(self) -> Result<()> {
        let store = platform::store(self);
        match self {
            App::Handy => handy_set(&store, &[("keyboard_implementation", "tauri"), ("log_level", "debug")]),
            App::Fluxer => fluxer_configure(&store),
            App::Discord => discord_configure(&store),
        }
    }
}

// --- Handy: a JSON file that Handy rewrites whenever a setting changes ---------------------------

fn handy_read(store: &Path) -> Result<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(store)?)?)
}

/// "tauri": Handy registers a system hotkey instead of swallowing the key event, so a call app
/// listening for the same key still sees it. "debug": the log lines `run` follows are DEBUG.
fn handy_done(settings: &Value) -> bool {
    settings["settings"]["keyboard_implementation"] == "tauri" && settings["settings"]["log_level"] == "debug"
}

pub fn handy_set(store: &Path, values: &[(&str, &str)]) -> Result<()> {
    let mut settings = handy_read(store)?;
    let entries = settings.get_mut("settings").and_then(Value::as_object_mut).ok_or("unexpected Handy settings")?;
    for &(key, value) in values {
        entries.insert(key.to_string(), json!(value));
    }
    fs::write(store, serde_json::to_string_pretty(&settings)?)?;
    Ok(())
}

/// A string from Handy's settings by JSON pointer, e.g. "/settings/keyboard_implementation".
pub fn handy_get(pointer: &str) -> Result<String> {
    let store = platform::store(App::Handy);
    if !store.exists() {
        return Err("Handy settings not found; start Handy once and run this again".into());
    }
    Ok(handy_read(&store)?.pointer(pointer).and_then(Value::as_str).unwrap_or_default().to_string())
}

// --- Fluxer: a SQLite key-value store; this row is the persisted keybind state -------------------

const FLUXER_ROW: &str = "store = 'app' AND scope = 'global' AND entry_key = 'Keybind'";

/// Marks the keybind as written by duckcall.
const FLUXER_KEYBIND_ID: &str = "d0cca110-d0cc-4a11-8a11-d0cca11d0cca";

fn fluxer_read(store: &Path) -> Result<Option<Value>> {
    let db = Connection::open_with_flags(store, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let row: Option<String> = db
        .query_row(&format!("SELECT value FROM scoped_storage WHERE {FLUXER_ROW}"), [], |row| row.get(0))
        .optional()?;
    Ok(row.map(|state| serde_json::from_str(&state)).transpose()?)
}

/// Is there a working global push-to-mute on Option+Space?
fn fluxer_done(state: &Value) -> bool {
    state["customKeybinds"].as_array().is_some_and(|keybinds| {
        keybinds.iter().any(|keybind| {
            let combo = &keybind["combo"];
            keybind["action"] == "voice_push_to_mute"
                && keybind["enabled"] != false
                && combo["code"] == "Space"
                && combo["alt"] == true
                && combo["global"] == true
                && combo["enabled"] != false
                && ["ctrl", "shift", "meta", "ctrlOrMeta"].iter().all(|&modifier| combo[modifier] != true)
        })
    })
}

fn fluxer_configure(store: &Path) -> Result<()> {
    let mut state = fluxer_read(store)?.ok_or("Fluxer has no keybind settings yet")?;
    let keybinds = state
        .get_mut("customKeybinds")
        .and_then(Value::as_array_mut)
        .ok_or("unexpected Fluxer keybind settings")?;
    keybinds.push(json!({
        "id": FLUXER_KEYBIND_ID,
        "action": "voice_push_to_mute",
        "combo": { "key": " ", "code": "Space", "alt": true, "global": true, "enabled": true },
        "enabled": true,
    }));
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as f64;
    Connection::open(store)?.execute(
        &format!("UPDATE scoped_storage SET value = ?1, updated_at = ?2 WHERE {FLUXER_ROW}"),
        rusqlite::params![state.to_string(), now],
    )?;
    Ok(())
}

// --- Discord: Chromium localStorage, a LevelDB; "keybinds" is the persisted KeybindsStore --------

const DISCORD_KEYBINDS: &[u8] = b"_https://discord.com\x00\x01keybinds";

fn discord_open(dir: &Path) -> Result<rusty_leveldb::DB> {
    let options = rusty_leveldb::Options { create_if_missing: false, ..Default::default() };
    Ok(rusty_leveldb::DB::open(dir, options)?)
}

fn discord_parse(raw: &[u8]) -> Result<Value> {
    Ok(serde_json::from_str(&local_storage_decode(raw).ok_or("unreadable Discord keybind settings")?)?)
}

/// Opening a LevelDB replays its log and rewrites its manifest, and Discord may be running:
/// look at a copy.
fn discord_read(dir: &Path) -> Result<Option<Value>> {
    // A directory of its own for every read, also for two reads in the same process.
    static COPIES: AtomicU32 = AtomicU32::new(0);
    let copy = format!("duckcall-discord-{}-{}", std::process::id(), COPIES.fetch_add(1, Ordering::Relaxed));
    let copy = std::env::temp_dir().join(copy);
    let state = copy_dir(dir, &copy).and_then(|()| {
        let mut db = discord_open(&copy)?;
        db.get(DISCORD_KEYBINDS).map(|raw| discord_parse(&raw)).transpose()
    });
    fs::remove_dir_all(&copy).ok();
    state
}

/// Is there a push-to-mute on Option+Space?
fn discord_done(state: &Value) -> bool {
    state["_state"].as_object().is_some_and(|keybinds| {
        keybinds.values().any(|keybind| {
            keybind["action"] == "PUSH_TO_MUTE"
                && keybind["enabled"] != false
                && keybind["shortcut"] == json!(platform::DISCORD_OPTION_SPACE)
        })
    })
}

fn discord_configure(dir: &Path) -> Result<()> {
    // The same LevelDB holds the Discord login: keep the state from before the write next to it.
    let backup = dir.with_file_name("leveldb.duckcall-backup");
    fs::remove_dir_all(&backup).ok();
    copy_dir(dir, &backup)?;

    let mut db = discord_open(dir)?;
    let mut state = discord_parse(&db.get(DISCORD_KEYBINDS).ok_or("Discord has no keybind settings yet")?)?;
    let keybinds = state.get_mut("_state").and_then(Value::as_object_mut).ok_or("unexpected Discord keybind settings")?;
    // Discord numbers a new keybind as the highest id + 1, and drops ids that are not numbers.
    let id = keybinds.keys().filter_map(|id| id.parse::<u64>().ok()).max().map_or(0, |highest| highest + 1).to_string();
    keybinds.insert(
        id.clone(),
        json!({
            "id": id,
            "enabled": true,
            "action": "PUSH_TO_MUTE",
            "shortcut": platform::DISCORD_OPTION_SPACE,
            "managed": false,
            "params": {},
        }),
    );
    db.put(DISCORD_KEYBINDS, &local_storage_encode(&state.to_string()))?;
    db.flush()?;
    db.close()?;
    Ok(())
}

/// Chromium stores a localStorage value as a marker byte plus UTF-16LE (0) or Latin-1 (1) text.
fn local_storage_decode(raw: &[u8]) -> Option<String> {
    match raw.split_first()? {
        (0, text) => {
            let units: Vec<u16> = text.as_chunks::<2>().0.iter().map(|&pair| u16::from_le_bytes(pair)).collect();
            String::from_utf16(&units).ok()
        }
        (1, text) => Some(text.iter().map(|&byte| byte as char).collect()),
        _ => None,
    }
}

fn local_storage_encode(text: &str) -> Vec<u8> {
    if text.chars().all(|c| c as u32 <= 0xFF) {
        std::iter::once(1).chain(text.chars().map(|c| c as u8)).collect()
    } else {
        std::iter::once(0).chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect()
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("duckcall-test-{name}"));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // --- Handy -----------------------------------------------------------------------------------

    #[test]
    fn handy_gets_the_settings_and_keeps_the_rest() {
        let store = scratch("handy").join("settings_store.json");
        fs::write(&store, r#"{"settings":{"keyboard_implementation":"handy_keys","log_level":"info","theme":"system"}}"#).unwrap();
        assert!(!handy_done(&handy_read(&store).unwrap()));

        handy_set(&store, &[("keyboard_implementation", "tauri"), ("log_level", "debug")]).unwrap();

        let settings = handy_read(&store).unwrap();
        assert!(handy_done(&settings));
        assert_eq!(settings["settings"]["theme"], "system");
    }

    #[test]
    fn handy_settings_in_an_unknown_shape_are_refused_and_kept() {
        let store = scratch("handy-odd").join("settings_store.json");
        let odd = r#"{"settings":["not","an","object"]}"#;
        fs::write(&store, odd).unwrap();

        assert!(handy_set(&store, &[("log_level", "debug")]).is_err());

        assert_eq!(fs::read_to_string(&store).unwrap(), odd);
    }

    // --- Fluxer ----------------------------------------------------------------------------------

    /// A Fluxer store with one unrelated row and, if given, the keybind row.
    fn fluxer_store(name: &str, keybind_state: Option<&str>) -> PathBuf {
        let store = scratch(name).join("desktop-app-store.sqlite3");
        let db = Connection::open(&store).unwrap();
        db.execute_batch(
            "CREATE TABLE scoped_storage (store TEXT NOT NULL, scope TEXT NOT NULL, entry_key TEXT NOT NULL,
                value TEXT NOT NULL, updated_at REAL NOT NULL, PRIMARY KEY(store, scope, entry_key));
             INSERT INTO scoped_storage VALUES ('app', 'global', 'VoiceSettings', '{\"keep\":true}', 1.0);",
        )
        .unwrap();
        if let Some(state) = keybind_state {
            db.execute("INSERT INTO scoped_storage VALUES ('app', 'global', 'Keybind', ?1, 1.0)", [state]).unwrap();
        }
        store
    }

    /// The combo Fluxer's own settings page stores for Option+Space as a global shortcut.
    fn option_space() -> Value {
        json!({ "key": " ", "code": "Space", "alt": true, "global": true, "enabled": true })
    }

    #[test]
    fn fluxer_gets_push_to_mute_next_to_existing_keybinds() {
        let toggle = r#"{"id":"a","action":"voice_toggle_mute","combo":{"key":"m","code":"KeyM","alt":true,"global":true,"enabled":true},"enabled":true}"#;
        let state = format!(r#"{{"customKeybinds":[{toggle}],"transmitMode":"voice_activity","__mps__":{{"version":5}}}}"#);
        let store = fluxer_store("fluxer", Some(&state));
        assert!(!fluxer_done(&fluxer_read(&store).unwrap().unwrap()));

        fluxer_configure(&store).unwrap();

        let state = fluxer_read(&store).unwrap().unwrap();
        assert!(fluxer_done(&state));
        let keybinds = state["customKeybinds"].as_array().unwrap();
        assert_eq!(keybinds[0], serde_json::from_str::<Value>(toggle).unwrap());
        assert_eq!(
            keybinds[1],
            json!({ "id": FLUXER_KEYBIND_ID, "action": "voice_push_to_mute", "combo": option_space(), "enabled": true })
        );
        assert_eq!(keybinds.len(), 2);
        assert_eq!(state["transmitMode"], "voice_activity");
        assert_eq!(state["__mps__"]["version"], 5); // Fluxer discards the row when this changes
        let other: String = Connection::open(&store)
            .unwrap()
            .query_row("SELECT value FROM scoped_storage WHERE entry_key = 'VoiceSettings'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(other, r#"{"keep":true}"#);
    }

    #[test]
    fn fluxer_counts_only_a_working_push_to_mute_on_option_space() {
        let done = |action: &str, combo: Value, enabled: bool| {
            fluxer_done(&json!({ "customKeybinds": [{ "id": "manual", "action": action, "combo": combo, "enabled": enabled }] }))
        };
        let with = |key: &str, value: Value| {
            let mut combo = option_space();
            combo[key] = value;
            combo
        };
        assert!(done("voice_push_to_mute", option_space(), true)); // set by hand in Fluxer: nothing to add
        assert!(!done("voice_toggle_mute", option_space(), true));
        assert!(!done("voice_push_to_mute", option_space(), false));
        assert!(!done("voice_push_to_mute", with("enabled", json!(false)), true));
        assert!(!done("voice_push_to_mute", with("global", json!(false)), true));
        assert!(!done("voice_push_to_mute", with("shift", json!(true)), true));
        assert!(!done("voice_push_to_mute", with("code", json!("KeyM")), true));
        assert!(!fluxer_done(&json!({ "customKeybinds": [] })));
    }

    #[test]
    fn fluxer_without_keybind_settings_gets_no_row() {
        let store = fluxer_store("fluxer-fresh", None);
        assert!(fluxer_read(&store).unwrap().is_none());

        assert!(fluxer_configure(&store).is_err());

        assert!(fluxer_read(&store).unwrap().is_none()); // a row with a guessed version would be discarded
    }

    #[test]
    fn fluxer_keybind_settings_in_an_unknown_shape_are_refused_and_kept() {
        let odd = r#"{"customKeybinds":"none","__mps__":{"version":5}}"#;
        let store = fluxer_store("fluxer-odd", Some(odd));

        assert!(fluxer_configure(&store).is_err());

        assert_eq!(fluxer_read(&store).unwrap().unwrap(), serde_json::from_str::<Value>(odd).unwrap());
    }

    // --- Discord ---------------------------------------------------------------------------------

    const DISCORD_DEFAULTS: &str = r#"{"_state":{"1":{"id":"1","enabled":true,"action":"SAVE_SCREENSHOT","shortcut":[[0,69,2]],"managed":true,"params":{}},"1004":{"id":"1004","enabled":true,"action":"SOUNDBOARD_HOLD","shortcut":[[0,224,2],[0,53,2]],"managed":true,"params":{}}},"_version":5}"#;
    const DISCORD_TOKEN: &[u8] = b"_https://discord.com\x00\x01token";

    /// A Discord localStorage with the login and, if given, the keybind settings.
    fn discord_store(name: &str, keybind_state: Option<&str>) -> PathBuf {
        let dir = scratch(name).join("leveldb");
        let mut db = rusty_leveldb::DB::open(&dir, rusty_leveldb::Options::default()).unwrap();
        db.put(DISCORD_TOKEN, &local_storage_encode("\"login\"")).unwrap();
        if let Some(state) = keybind_state {
            db.put(DISCORD_KEYBINDS, &local_storage_encode(state)).unwrap();
        }
        db.close().unwrap();
        dir
    }

    /// One value straight from a store, the way Discord reads it on its next start.
    fn discord_value(dir: &Path, key: &[u8]) -> Option<String> {
        discord_open(dir).unwrap().get(key).map(|raw| local_storage_decode(&raw).unwrap())
    }

    #[test]
    fn discord_gets_push_to_mute_with_the_next_id() {
        let dir = discord_store("discord", Some(DISCORD_DEFAULTS));
        assert!(!discord_done(&discord_read(&dir).unwrap().unwrap()));

        discord_configure(&dir).unwrap();

        let state = discord_read(&dir).unwrap().unwrap();
        assert!(discord_done(&state));
        assert_eq!(
            state["_state"]["1005"],
            json!({ "id": "1005", "enabled": true, "action": "PUSH_TO_MUTE", "shortcut": [[0, 226, 2], [0, 44, 2]], "managed": false, "params": {} })
        );
        let defaults: Value = serde_json::from_str(DISCORD_DEFAULTS).unwrap();
        assert_eq!(state["_state"]["1"], defaults["_state"]["1"]);
        assert_eq!(state["_state"]["1004"], defaults["_state"]["1004"]);
        assert_eq!(state["_version"], 5);
        assert_eq!(discord_value(&dir, DISCORD_TOKEN).as_deref(), Some("\"login\"")); // the login survives
    }

    #[test]
    fn discord_backup_holds_the_state_from_before_the_write() {
        let dir = discord_store("discord-backup", Some(DISCORD_DEFAULTS));

        discord_configure(&dir).unwrap();

        let backup = dir.with_file_name("leveldb.duckcall-backup");
        let before: Value = serde_json::from_str(&discord_value(&backup, DISCORD_KEYBINDS).unwrap()).unwrap();
        assert_eq!(before, serde_json::from_str::<Value>(DISCORD_DEFAULTS).unwrap());
        assert_eq!(discord_value(&backup, DISCORD_TOKEN).as_deref(), Some("\"login\""));
    }

    #[test]
    fn discord_read_leaves_the_store_untouched() {
        let dir = discord_store("discord-read", Some(DISCORD_DEFAULTS));
        let files = |dir: &Path| {
            let mut files: Vec<_> = fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap())
                .map(|entry| (entry.file_name(), fs::read(entry.path()).unwrap()))
                .collect();
            files.sort();
            files
        };
        let before = files(&dir);

        assert!(discord_read(&dir).unwrap().is_some());

        assert!(files(&dir) == before, "reading changed Discord's LevelDB");
    }

    #[test]
    fn discord_counts_only_an_enabled_push_to_mute_on_option_space() {
        let done = |action: &str, shortcut: Value, enabled: bool| {
            let keybind = json!({ "id": "7", "enabled": enabled, "action": action, "shortcut": shortcut, "managed": false, "params": {} });
            discord_done(&json!({ "_state": { "7": keybind }, "_version": 5 }))
        };
        let option_space = json!([[0, 226, 2], [0, 44, 2]]);
        assert!(done("PUSH_TO_MUTE", option_space.clone(), true)); // set by hand in Discord: nothing to add
        assert!(!done("PUSH_TO_MUTE", option_space.clone(), false));
        assert!(!done("PUSH_TO_TALK", option_space, true));
        assert!(!done("PUSH_TO_MUTE", json!([[0, 230, 2], [0, 44, 2]]), true)); // right Option
        assert!(!done("PUSH_TO_MUTE", json!([[0, 44, 2]]), true));
        assert!(!discord_done(&json!({ "_state": {}, "_version": 5 })));
    }

    #[test]
    fn discord_numbers_the_first_keybind_0() {
        let dir = discord_store("discord-empty", Some(r#"{"_state":{},"_version":5}"#));

        discord_configure(&dir).unwrap();

        assert_eq!(discord_read(&dir).unwrap().unwrap()["_state"]["0"]["id"], "0");
    }

    #[test]
    fn discord_keeps_text_outside_latin_1() {
        let state = r#"{"_state":{"3":{"id":"3","enabled":true,"action":"SWITCH_TO_VOICE_CHANNEL","shortcut":[[0,58,2]],"managed":false,"params":{"channel":"Büro ✓"}}},"_version":5}"#;
        let dir = discord_store("discord-utf16", Some(state));

        discord_configure(&dir).unwrap();

        let state = discord_read(&dir).unwrap().unwrap();
        assert_eq!(state["_state"]["3"]["params"]["channel"], "Büro ✓");
        assert!(discord_done(&state));
    }

    #[test]
    fn discord_without_keybind_settings_gets_no_entry() {
        let dir = discord_store("discord-fresh", None);
        assert!(discord_read(&dir).unwrap().is_none());

        assert!(discord_configure(&dir).is_err());

        assert_eq!(discord_value(&dir, DISCORD_KEYBINDS), None);
    }

    #[test]
    fn discord_keybind_settings_in_an_unknown_shape_are_refused_and_kept() {
        let odd = r#"{"keybinds":[]}"#;
        let dir = discord_store("discord-odd", Some(odd));

        assert!(discord_configure(&dir).is_err());

        assert_eq!(discord_value(&dir, DISCORD_KEYBINDS).as_deref(), Some(odd));
    }

    #[test]
    fn local_storage_text_survives_both_encodings() {
        for text in ["plain", "latin-1 é", "utf-16 ✓"] {
            assert_eq!(local_storage_decode(&local_storage_encode(text)).as_deref(), Some(text));
        }
        assert_eq!(local_storage_encode("a"), [1, b'a']);
        assert_eq!(local_storage_encode("✓")[0], 0);
        assert_eq!(local_storage_decode(&[]), None);
        assert_eq!(local_storage_decode(&[7, b'a']), None); // a marker Chromium does not write
    }
}
