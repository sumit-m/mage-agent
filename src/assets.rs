//! Buzz's Character and Reference menus, filled from `mage assets --json`.
//!
//! `install.ps1` declares the two menus (kind `assets`, ids `character` and
//! `reference`) with `--refresh-assets` as their refresh: Buzz runs it when a
//! menu opens, and this rewrites the menus' options in the harness file. A
//! pick puts `@handle` in the prompt, which mage resolves itself.

use serde_json::{json, Value};

/// Buzz's limits for an `assets` menu; entries past them are left out.
const MAX_OPTIONS: usize = 200;
const MAX_TEXT: usize = 64;
const MAX_URL: usize = 2048;

/// Panel control id → the `type` of the assets it lists.
const MENUS: [(&str, &str); 2] = [("character", "character"), ("reference", "reference")];

fn usable_handle(handle: &str) -> bool {
    !handle.is_empty()
        && handle.len() <= MAX_TEXT
        && handle
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn option(asset: &Value) -> Option<Value> {
    let handle = asset["handle"].as_str()?.trim_start_matches('@');
    if !usable_handle(handle) {
        return None;
    }
    let name = asset["name"].as_str().map(str::trim).unwrap_or_default();
    let label: String = if name.is_empty() { handle } else { name }
        .chars()
        .take(MAX_TEXT)
        .collect();
    let mut option = json!({ "value": handle, "label": label });
    if let Some(url) = asset["image_url"]
        .as_str()
        .filter(|url| url.len() <= MAX_URL && url.starts_with("https://"))
    {
        option["image"] = url.into();
    }
    if let Some(variant) = asset["variant"]
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.chars().count() <= MAX_TEXT)
    {
        option["hint"] = capitalized(variant).into();
    }
    Some(option)
}

/// The options each asset menu lists, by control id.
pub fn menu_options(assets: &[Value]) -> Vec<(&'static str, Vec<Value>)> {
    MENUS
        .iter()
        .map(|(control, kind)| {
            let options = assets
                .iter()
                .filter(|asset| asset["type"] == *kind)
                .filter_map(option)
                .take(MAX_OPTIONS)
                .collect();
            (*control, options)
        })
        .collect()
}

/// Sets the options of the panel's asset menus. Menus the panel doesn't
/// declare are left out. Returns whether anything changed.
pub fn apply(controls: &mut [Value], assets: &[Value]) -> bool {
    let mut changed = false;
    for (id, options) in menu_options(assets) {
        let Some(control) = controls
            .iter_mut()
            .find(|c| c["id"] == id && c["kind"] == "assets")
        else {
            continue;
        };
        let options = Value::Array(options);
        if control["options"] != options {
            control["options"] = options;
            changed = true;
        }
    }
    changed
}

/// `mage assets --json` output: an array of assets.
pub fn parse(stdout: &str) -> Result<Vec<Value>, String> {
    match serde_json::from_str::<Value>(stdout.trim_start_matches('\u{feff}').trim()) {
        Ok(Value::Array(assets)) => Ok(assets),
        Ok(_) => Err("mage assets --json did not print a list".into()),
        Err(e) => Err(format!("mage assets --json: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSETS: &str = r#"[
      {"type":"character","handle":"ryantennis","name":"ryan tennis","id":"1",
       "image_url":"https://cdn.example/c.jpg","modality":"image","visibility":"private"},
      {"type":"reference","variant":"outfit","handle":"niketrack","name":"niketrack","id":"2",
       "image_url":"https://cdn.example/r.jpg","modality":"image","visibility":"private"},
      {"type":"reference","variant":"pose","handle":"bad handle","name":"x","id":"3"},
      {"type":"character","handle":"@nameless","name":"  ","id":"4","image_url":"http://plain/c.jpg"},
      {"type":"style","handle":"later","name":"From a newer mage","id":"5"}
    ]"#;

    #[test]
    fn options_keep_usable_assets_by_type() {
        let assets = parse(ASSETS).unwrap();
        let menus = menu_options(&assets);
        assert_eq!(
            menus[0],
            (
                "character",
                vec![
                    json!({ "value": "ryantennis", "label": "ryan tennis",
                            "image": "https://cdn.example/c.jpg" }),
                    json!({ "value": "nameless", "label": "nameless" }),
                ]
            )
        );
        assert_eq!(
            menus[1],
            (
                "reference",
                vec![json!({ "value": "niketrack", "label": "niketrack",
                             "image": "https://cdn.example/r.jpg", "hint": "Outfit" })]
            )
        );
    }

    #[test]
    fn apply_fills_declared_menus_only() {
        let assets = parse(ASSETS).unwrap();
        let mut controls = vec![
            json!({ "id": "model", "kind": "list", "options": [] }),
            json!({ "id": "character", "kind": "assets", "label": "Character", "options": [] }),
        ];
        assert!(apply(&mut controls, &assets));
        assert_eq!(controls[1]["options"][0]["value"], "ryantennis");
        assert_eq!(controls.len(), 2);
        assert!(!apply(&mut controls, &assets));
        assert!(apply(&mut controls, &[]));
        assert_eq!(controls[1]["options"], json!([]));
    }

    #[test]
    fn parse_rejects_anything_but_a_list() {
        assert!(parse("\u{feff}[]").unwrap().is_empty());
        assert!(parse("{}").is_err());
        assert!(parse("not signed in").is_err());
    }
}
