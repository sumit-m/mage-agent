//! Requests from Buzz's message-box panel. The panel sends the prompt as the
//! message text and its settings in a hidden `["buzz-composer", "mage", json]`
//! tag; this module turns them into a mage command. Flags are never shown in
//! Buzz, so every setting is passed explicitly rather than left to mage's
//! defaults. The one omission is `auto`: mage has no auto value, and leaving
//! `--aspect` or `--res` out is how `edit` matches the source image.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::command::{self, Request};

pub const TAG: &str = "buzz-composer";
pub const HARNESS_ID: &str = "mage";

#[derive(Deserialize)]
struct Payload {
    v: u32,
    mode: String,
    #[serde(default)]
    controls: BTreeMap<String, String>,
}

/// The panel payload carried by this event, if any.
pub fn payload(tags: &[Vec<String>]) -> Option<&str> {
    tags.iter().find_map(|tag| match tag.as_slice() {
        [name, harness, json, ..] if name == TAG && harness == HARNESS_ID => Some(json.as_str()),
        _ => None,
    })
}

pub fn request(
    content: &str,
    attachment_urls: &[String],
    display_name: Option<&str>,
    json: &str,
) -> Result<Request, String> {
    let payload: Payload =
        serde_json::from_str(json).map_err(|e| format!("Unreadable panel settings: {e}"))?;
    if payload.v != 1 {
        return Err(format!(
            "Panel settings version {} is newer than this mage-agent; update it.",
            payload.v
        ));
    }
    let (subcommand, aspect_default, res_default) = match payload.mode.as_str() {
        "generate" => ("gen", "portrait", "1K"),
        "edit" => ("edit", "auto", "auto"),
        other => return Err(format!("Unknown panel mode `{other}`.")),
    };
    let prompt = prompt_text(content, attachment_urls, display_name);
    if prompt.is_empty() {
        return Err("Write a prompt first.".into());
    }

    let setting = |id: &str, default: &'static str| -> Result<String, String> {
        let value = payload.controls.get(id).map_or(default, String::as_str);
        if value.is_empty() || value.starts_with('-') || value.contains(char::is_whitespace) {
            return Err(format!("Invalid panel value for {id}: `{value}`"));
        }
        Ok(value.to_string())
    };

    let mut args = vec![subcommand.to_string(), "--prompt".into(), prompt];
    args.extend(["--model".into(), setting("model", "mango-v4")?]);
    for (id, flag, default) in [
        ("aspect", "--aspect", aspect_default),
        ("res", "--res", res_default),
    ] {
        let value = setting(id, default)?;
        if value != "auto" {
            args.extend([flag.to_string(), value]);
        }
    }
    if payload.controls.contains_key("preset") {
        args.extend(["--preset".into(), setting("preset", "")?]);
    }
    let repo = payload
        .controls
        .get("repo")
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());
    Ok(Request { args, repo })
}

/// The message text minus attachment lines, markdown markers and Mage's own
/// leading mention, as one line. Other `@name` words are kept: models such as
/// Mango 4 use them for characters and references.
fn prompt_text(content: &str, attachment_urls: &[String], display_name: Option<&str>) -> String {
    let text =
        command::strip_formatting(&command::without_attachment_lines(content, attachment_urls));
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = command::strip_display_name(&text, display_name);
    let text = match text.split_once(' ') {
        Some((first, rest)) if first.starts_with("nostr:") => rest,
        None if text.starts_with("nostr:") => "",
        _ => text,
    };
    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(content: &str, json: &str) -> Result<Request, String> {
        request(content, &[], Some("Mage"), json)
    }

    #[test]
    fn finds_only_this_harness_payload() {
        let tags = vec![
            vec!["h".to_string(), "c".to_string()],
            vec![TAG.to_string(), "other".to_string(), "{}".to_string()],
            vec![
                TAG.to_string(),
                HARNESS_ID.to_string(),
                "{\"v\":1}".to_string(),
            ],
        ];
        assert_eq!(payload(&tags), Some("{\"v\":1}"));
        assert_eq!(payload(&tags[..2]), None);
    }

    #[test]
    fn generate_passes_every_setting_explicitly() {
        let r = run(
            "@Mage a red fox --aspect cinema",
            r#"{"v":1,"mode":"generate","controls":{"model":"mango-v2","aspect":"square","res":"2K","repo":"art:img/"}}"#,
        )
        .unwrap();
        assert_eq!(
            r.args,
            [
                "gen",
                "--prompt",
                "a red fox --aspect cinema",
                "--model",
                "mango-v2",
                "--aspect",
                "square",
                "--res",
                "2K"
            ]
        );
        assert_eq!(r.repo.as_deref(), Some("art:img/"));
    }

    #[test]
    fn missing_settings_use_explicit_defaults() {
        let r = run("@Mage a cat", r#"{"v":1,"mode":"generate"}"#).unwrap();
        assert_eq!(
            r.args,
            [
                "gen", "--prompt", "a cat", "--model", "mango-v4", "--aspect", "portrait", "--res",
                "1K"
            ]
        );
        assert_eq!(r.repo, None);
    }

    #[test]
    fn edit_auto_leaves_aspect_and_res_out() {
        let r = run(
            "@Mage make it night",
            r#"{"v":1,"mode":"edit","controls":{"model":"mango-v4","aspect":"auto","res":"2K","preset":"logo"}}"#,
        )
        .unwrap();
        assert_eq!(
            r.args,
            [
                "edit",
                "--prompt",
                "make it night",
                "--model",
                "mango-v4",
                "--res",
                "2K",
                "--preset",
                "logo"
            ]
        );
    }

    #[test]
    fn rejects_bad_payloads() {
        assert!(run("@Mage x", "not json").is_err());
        assert!(run("@Mage x", r#"{"v":2,"mode":"generate"}"#).is_err());
        assert!(run("@Mage x", r#"{"v":1,"mode":"remix"}"#).is_err());
        assert!(run("@Mage", r#"{"v":1,"mode":"generate"}"#).is_err());
        assert!(run(
            "@Mage x",
            r#"{"v":1,"mode":"generate","controls":{"model":"--output"}}"#
        )
        .is_err());
    }

    #[test]
    fn prompt_drops_mention_formatting_and_attachment_lines() {
        let url = "https://relay/media/a.png".to_string();
        let r = request(
            "@Mage make **the sky** purple\n![a.png](https://relay/media/a.png)",
            &[url],
            Some("Mage"),
            r#"{"v":1,"mode":"edit"}"#,
        )
        .unwrap();
        assert_eq!(r.args[2], "make the sky purple");
    }

    #[test]
    fn prompt_keeps_at_references() {
        let r = run(
            "@Mage @hero meets @villain in the rain",
            r#"{"v":1,"mode":"generate"}"#,
        )
        .unwrap();
        assert_eq!(r.args[2], "@hero meets @villain in the rain");
    }
}
