//! Buzz's Preset menu for mage, kept in step with mage's `presets.ini`.
//!
//! `install.ps1` writes the menu into the harness file (`mage.json`, named by
//! `MAGE_HARNESS_FILE`, or found as the harness whose command is this exe).
//! mage-agent rebuilds it from `presets.ini` each time it starts, so an edited
//! preset reaches the panel without a reinstall (from the next request on: the
//! panel reads the file before the agent starts).

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Buzz rejects a whole panel over one bad option, so these are left out.
const MAX_OPTIONS: usize = 50;
const MAX_TEXT: usize = 64;

/// `presets.ini` field → the panel control it sets.
const SETS: [(&str, &str); 3] = [
    ("model_id", "model"),
    ("aspect_ratio", "aspect"),
    ("resolution", "res"),
];

/// `[name]` sections with their `key = value` lines, in file order. Lines of
/// any other shape (comments, blanks) are skipped.
pub fn parse(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut presets: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            presets.push((name.trim().to_string(), Vec::new()));
        } else if let (Some((_, fields)), Some((key, value))) =
            (presets.last_mut(), line.split_once('='))
        {
            let key = key.trim();
            if !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                fields.push((key.to_string(), value.trim().to_string()));
            }
        }
    }
    presets
}

fn usable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TEXT
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'')
}

/// The Preset control for these presets, or `None` when there are none.
pub fn preset_control(presets: &[(String, Vec<(String, String)>)]) -> Option<Value> {
    let options: Vec<Value> = presets
        .iter()
        .filter(|(name, _)| {
            let ok = usable_name(name);
            if !ok {
                eprintln!("mage-agent: preset {name:?} left out of the panel (spaces or quotes)");
            }
            ok
        })
        .take(MAX_OPTIONS)
        .map(|(name, fields)| {
            let mut sets = Map::new();
            for (field, control) in SETS {
                if let Some((_, value)) = fields.iter().find(|(k, v)| k == field && !v.is_empty()) {
                    sets.insert(control.to_string(), Value::String(value.clone()));
                }
            }
            json!({ "value": name, "label": name, "sets": sets })
        })
        .collect();
    (!options.is_empty()).then(|| {
        json!({ "id": "preset", "kind": "preset", "label": "Preset", "icon": "wand", "options": options })
    })
}

/// Puts `control` in place of the panel's Preset control (before the asset
/// menus and repo picker when there was none), or removes it when `control` is `None`.
/// Returns whether anything changed.
fn replace_preset(controls: &mut Vec<Value>, control: Option<Value>) -> bool {
    let existing = controls.iter().position(|c| c["kind"] == "preset");
    match (existing, control) {
        (Some(at), Some(control)) if controls[at] == control => false,
        (Some(at), Some(control)) => {
            controls[at] = control;
            true
        }
        (Some(at), None) => {
            controls.remove(at);
            true
        }
        (None, Some(control)) => {
            let at = controls
                .iter()
                .position(|c| c["kind"] == "assets" || c["kind"] == "repoFolder")
                .unwrap_or(controls.len());
            controls.insert(at, control);
            true
        }
        (None, None) => false,
    }
}

/// The harness file in `dirs` whose `command` is `exe`.
pub fn harness_file_for(dirs: &[PathBuf], exe: &Path) -> Option<PathBuf> {
    let normalize = |path: &str| {
        let path = path.trim().replace('/', "\\").to_lowercase();
        path.strip_prefix(r"\\?\")
            .map_or(path.clone(), str::to_string)
    };
    let wanted = normalize(&exe.to_string_lossy());
    dirs.iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .find(|path| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|raw| {
                    serde_json::from_str::<Value>(raw.trim_start_matches('\u{feff}')).ok()
                })
                .and_then(|harness| harness["command"].as_str().map(normalize))
                .is_some_and(|command| command == wanted)
        })
}

/// `presets.ini` as text: UTF-8 with or without a byte-order mark, UTF-16
/// (Notepad's "Unicode"), or anything else with bad bytes replaced.
fn decode(bytes: &[u8]) -> String {
    let utf16 = |bytes: &[u8], from: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| from([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Rewrites the harness file's Preset control from `presets_ini`. A missing
/// or empty `presets.ini` means no presets (mage doesn't need one), so the
/// menu goes away. Returns whether the file changed.
pub fn refresh(harness_file: &Path, presets_ini: &Path) -> Result<bool, String> {
    let text = match std::fs::read(presets_ini) {
        Ok(bytes) => decode(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", presets_ini.display())),
    };
    update_controls(harness_file, |controls| {
        replace_preset(controls, preset_control(&parse(&text)))
    })
}

/// Applies `change` to the harness file's panel controls and writes the file
/// back when it reports a change. Returns whether the file changed.
pub fn update_controls(
    harness_file: &Path,
    change: impl FnOnce(&mut Vec<Value>) -> bool,
) -> Result<bool, String> {
    let raw = std::fs::read_to_string(harness_file)
        .map_err(|e| format!("{}: {e}", harness_file.display()))?;
    let mut harness: Value = serde_json::from_str(raw.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("{}: {e}", harness_file.display()))?;
    let Some(controls) = harness
        .pointer_mut("/composer/controls")
        .and_then(Value::as_array_mut)
    else {
        return Ok(false);
    };
    if !change(controls) {
        return Ok(false);
    }
    let out = serde_json::to_string_pretty(&harness).map_err(|e| e.to_string())?;
    // Buzz's refresh and the agent's start-up can overlap: each writes its own
    // temp file, and the last rename wins with a complete file either way.
    let temp = harness_file.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temp, out).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, harness_file).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("{}: {e}", harness_file.display())
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "; mage presets\n\
                       [logo]\n\
                       model_id = mango-v4\n\
                       aspect_ratio = square\n\
                       style = flat\n\
                       \n\
                       [wide shot]\n\
                       aspect_ratio = cinema\n\
                       [night]\n\
                       resolution = 2K\n";

    #[test]
    fn parse_keeps_sections_in_order() {
        let presets = parse(INI);
        let names: Vec<&str> = presets.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["logo", "wide shot", "night"]);
        assert_eq!(presets[0].1.len(), 3);
        assert!(parse("").is_empty());
        assert!(parse("\r\n  \r\n; only a comment\r\nstray = line\r\n").is_empty());
    }

    #[test]
    fn decode_reads_each_notepad_encoding() {
        let ini = "[logo]\r\nmodel_id = mango-v4\r\n";
        let utf16le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(ini.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let utf16be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain(ini.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        let bom: Vec<u8> = [0xEF, 0xBB, 0xBF].into_iter().chain(ini.bytes()).collect();
        for bytes in [ini.as_bytes().to_vec(), bom, utf16le, utf16be] {
            assert_eq!(parse(&decode(&bytes))[0].0, "logo");
        }
        assert_eq!(parse(&decode(b"[caf\xE9]\n"))[0].0, "caf\u{FFFD}");
    }

    #[test]
    fn control_maps_fields_and_skips_unusable_names() {
        let control = preset_control(&parse(INI)).unwrap();
        let options = control["options"].as_array().unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(
            options[0],
            json!({ "value": "logo", "label": "logo",
                    "sets": { "model": "mango-v4", "aspect": "square" } })
        );
        assert_eq!(options[1]["sets"], json!({ "res": "2K" }));
        assert!(preset_control(&parse("; none\n")).is_none());
    }

    #[test]
    fn harness_file_is_the_one_naming_this_exe() {
        let dir = std::env::temp_dir().join(format!("mage-harnesses-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("other.json"), r#"{"command":"C:\\x\\other.exe"}"#).unwrap();
        std::fs::write(
            dir.join("mage.json"),
            r#"{"command":"C:\\Apps\\Mage\\mage-agent.exe"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "C:\\Apps\\Mage\\mage-agent.exe").unwrap();
        let dirs = [dir.join("missing"), dir.clone()];
        assert_eq!(
            harness_file_for(&dirs, Path::new("c:/apps/mage/MAGE-AGENT.exe")),
            Some(dir.join("mage.json"))
        );
        assert_eq!(
            harness_file_for(&dirs, Path::new("C:\\y\\mage-agent.exe")),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn harness(controls: Value) -> Value {
        json!({ "id": "mage", "composer": { "modes": [], "controls": controls } })
    }

    #[test]
    fn refresh_replaces_inserts_and_removes_the_preset_control() {
        let dir = std::env::temp_dir().join(format!("mage-presets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mage.json");
        let ini = dir.join("presets.ini");
        let repo = json!({ "id": "repo", "kind": "repoFolder", "label": "Save" });
        let read =
            || serde_json::from_str::<Value>(&std::fs::read_to_string(&file).unwrap()).unwrap();

        std::fs::write(&file, harness(json!([{ "id": "model" }, repo])).to_string()).unwrap();
        std::fs::write(&ini, "[logo]\nmodel_id = mango-v2\n").unwrap();
        assert!(refresh(&file, &ini).unwrap());
        let controls = read()["composer"]["controls"].clone();
        assert_eq!(controls[1]["kind"], "preset");
        assert_eq!(controls[2]["kind"], "repoFolder");
        assert!(!refresh(&file, &ini).unwrap());

        std::fs::write(&ini, "[logo]\nmodel_id = mango-v4\n").unwrap();
        assert!(refresh(&file, &ini).unwrap());
        assert_eq!(
            read()["composer"]["controls"][1]["options"][0]["sets"]["model"],
            "mango-v4"
        );

        std::fs::write(&ini, "").unwrap();
        assert!(refresh(&file, &ini).unwrap());
        assert_eq!(read()["composer"]["controls"].as_array().unwrap().len(), 2);

        std::fs::remove_file(&ini).unwrap();
        assert!(!refresh(&file, &ini).unwrap());
        assert_eq!(read()["composer"]["controls"].as_array().unwrap().len(), 2);

        std::fs::write(&ini, "[logo]\n").unwrap();
        std::fs::write(&file, "{ not json").unwrap();
        assert!(refresh(&file, &ini).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ not json");
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
