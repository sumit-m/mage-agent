//! One `@mage` request: run the CLI, post its output and images, save to a repo.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::prompt::Event;
use crate::{command, composer, models};
use crate::{proc, repo, strip, terminal};

/// Includes time spent waiting in mage's queue; stays under buzz-acp's 2-hour turn cap.
const MAGE_TIMEOUT: Duration = Duration::from_secs(100 * 60);
const BUZZ_TIMEOUT: Duration = Duration::from_secs(180);
const IMAGE_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "webp", "gif"];
/// Far longer than any job (see `MAGE_TIMEOUT`), so a folder this old was
/// left by an agent stopped mid-job, not one still running elsewhere.
const STALE_WORK_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Removes job folders under `work_root` last changed more than `max_age`
/// ago. Returns how many were removed.
pub fn sweep_stale_work(work_root: &Path, max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(work_root) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > max_age)
        })
        .filter(|entry| std::fs::remove_dir_all(entry.path()).is_ok())
        .count()
}

/// [`sweep_stale_work`] with the standard age, logging what it removed.
pub fn sweep_stale(work_root: &Path) {
    let removed = sweep_stale_work(work_root, STALE_WORK_AGE);
    if removed > 0 {
        eprintln!(
            "mage-agent: removed {removed} leftover job folder(s) from {}",
            work_root.display()
        );
    }
}

pub struct Config {
    pub mage: PathBuf,
    pub buzz: PathBuf,
    pub display_name: Option<String>,
    pub work_root: PathBuf,
}

impl Config {
    /// mage's install folder, where its ini files live; `None` for a mage
    /// found only on PATH.
    pub fn mage_dir(&self) -> Option<&Path> {
        self.mage
            .parent()
            .filter(|dir| dir.is_absolute() && dir.is_dir())
    }
}

struct Attachment {
    url: String,
    mime: String,
    /// The imeta fields Buzz needs to attach the same upload again.
    imeta: Value,
}

pub fn run(config: &Config, event: &Event, reply_to: Option<&str>) {
    let work = config.work_root.join(&event.id[..16.min(event.id.len())]);
    let _ = std::fs::remove_dir_all(&work);
    let message = match std::fs::create_dir_all(work.join("out")) {
        Ok(()) => handle(config, event, &work),
        Err(e) => Reply::text(format!(
            "mage-agent could not create {}: {e}",
            work.display()
        )),
    };
    post(config, &event.channel, reply_to, message);
    let _ = std::fs::remove_dir_all(&work);
}

struct Reply {
    content: String,
    files: Vec<PathBuf>,
}

impl Reply {
    fn text(content: String) -> Self {
        Reply {
            content,
            files: Vec::new(),
        }
    }
}

fn handle(config: &Config, event: &Event, work: &Path) -> Reply {
    let attachments = attachments(&event.tags);
    let urls: Vec<String> = attachments.iter().map(|a| a.url.clone()).collect();
    let display_name = config.display_name.as_deref();
    let parsed = match composer::payload(&event.tags) {
        Some(json) => composer::request(&event.content, &urls, display_name, json),
        None => command::parse_request(&event.content, &urls, display_name),
    };
    let mut request = match parsed {
        Ok(request) => request,
        Err(message) => return Reply::text(message),
    };

    let mut args = request.args.clone();
    if request.produces_images() && !request.has_flag("--arch") {
        // mage defaults `--arch` to mango whatever the model.
        let arch = flag_value(&args, "--model")
            .and_then(|model| models::architecture(&config.mage_dir()?.join("models.ini"), model));
        if let Some(arch) = arch {
            args.extend(["--arch".into(), arch]);
        }
    }
    // Every attached image goes to mage, in order: the first is the primary,
    // the rest are `@image2`… A request naming its own `--image` keeps it.
    let mut sources = Vec::new();
    if request.produces_images() && !request.has_flag("--image") {
        let images = attachments.iter().filter(|a| a.mime.starts_with("image/"));
        for (index, image) in images.enumerate() {
            match download(config, image, work, index + 1) {
                Ok(path) => {
                    sources.push(image.imeta.clone());
                    args.push("--image".into());
                    args.push(path.to_string_lossy().into_owned());
                }
                Err(e) => {
                    return Reply::text(format!(
                        "Could not download attached image {}: {e}",
                        index + 1
                    ))
                }
            }
        }
    }
    let mut origin = json!({ "mode": if sources.is_empty() { "generate" } else { "edit" } });
    if let Some(first) = sources.first() {
        // Desktops before multi-image sources read only `source`.
        origin["source"] = first.clone();
        origin["sources"] = sources.into();
    }
    if let Err(e) = localize_repo_images(config, event, &mut args, work) {
        return Reply::text(format!("Could not get the `--image` image: {e}"));
    }
    let out_dir = work.join("out");
    if request.produces_images() {
        args.push("--output".into());
        args.push(out_dir.to_string_lossy().into_owned());
    }

    let mut cmd = proc::command(&config.mage);
    cmd.args(&args)
        .env_remove("BUZZ_PRIVATE_KEY")
        .env_remove("BUZZ_AUTH_TAG");
    if let Some(dir) = config.mage.parent().filter(|d| !d.as_os_str().is_empty()) {
        cmd.current_dir(dir);
    }
    let output = match proc::run(cmd, None, MAGE_TIMEOUT) {
        Ok(output) => output,
        Err(e) => return Reply::text(format!("Could not start `{}`: {e}", config.mage.display())),
    };

    let printed = without_lines_mentioning(&terminal::clean(&output.combined()), work);
    let mut notes = Vec::new();
    if output.timed_out {
        notes.push(format!(
            "Stopped waiting after {} minutes; mage may still finish the job.",
            MAGE_TIMEOUT.as_secs() / 60
        ));
    } else if !output.success {
        notes.push(format!(
            "mage exited with code {}.",
            output.code.unwrap_or(-1)
        ));
    }

    let (images, sidecars) = produced_files(&out_dir);
    annotate_sidecars(&sidecars, &origin);
    let mut files = attachable(&images, work);
    files.extend(sidecars.iter().cloned());

    if images.is_empty() {
        if output.success && request.produces_images() {
            notes.push("mage reported success but wrote no image.".into());
        }
        let content = std::iter::once(printed.trim().to_string())
            .chain(notes)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let content = if content.is_empty() {
            "mage finished without output.".to_string()
        } else {
            content
        };
        return Reply { content, files };
    }

    // With an image, Buzz Desktop shows its prompt, settings, source and repo
    // location in a sidebar built from the sidecar, so the reply carries only
    // notes (a failed save among them). The commit message, which has no
    // sidebar, is still built from mage's output.
    if let Some(spec) = request.repo.take() {
        let message = commit_message(&printed, &event.content);
        let save = Save {
            images: &images,
            sidecars: &sidecars,
            message: &message,
            origin: &origin,
        };
        if let Err(e) = save_to_repo(config, event, work, &spec, &save) {
            notes.insert(0, format!("Repo save failed: {e}"));
        }
    }
    Reply {
        content: notes.join("\n"),
        files,
    }
}

/// The commit message is the channel reply without its `url:` line (the CDN
/// link expires after 30 days, while the commit lasts), as one line with its
/// lines separated by commas. Falls back to the request text when mage printed
/// nothing else.
fn commit_message(reply: &str, request: &str) -> String {
    let kept: Vec<&str> = reply
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.to_ascii_lowercase().starts_with("url:"))
        .collect();
    let message = kept.join(", ");
    if message.is_empty() {
        command::strip_formatting(request).trim().to_string()
    } else {
        message
    }
}

struct Save<'a> {
    images: &'a [PathBuf],
    sidecars: &'a [PathBuf],
    message: &'a str,
    origin: &'a Value,
}

/// Commits the job's files, with the sidecars naming the repo they were saved
/// to (`<owner>/<repo>`; Buzz finds the image's folder in it by file name, so
/// a later move is still found). A failed save takes that back out again.
fn save_to_repo(
    config: &Config,
    event: &Event,
    work: &Path,
    spec: &str,
    save: &Save,
) -> Result<(), String> {
    let target = repo::resolve_for(&config.buzz, &event.channel, &event.sender, spec)?;
    let mut saved_origin = save.origin.clone();
    saved_origin["repo"] = repo_address(&target.repo).into();
    annotate_sidecars(save.sidecars, &saved_origin);
    repo::save(work, &target, save.images, save.sidecars, save.message)
        .map(|_| ())
        .inspect_err(|_| annotate_sidecars(save.sidecars, save.origin))
}

fn repo_address(repo: &repo::RepoInfo) -> String {
    match repo.owner.as_deref().filter(|owner| !owner.is_empty()) {
        Some(owner) => format!("{owner}/{}", repo.id),
        None => repo.id.clone(),
    }
}

/// Adds `origin` to each sidecar as `buzz`: how the image was made (`mode`,
/// the `sources` uploads in order) and where it was saved (`repo`). A sidecar
/// mage wrote in another shape is left as it is.
fn annotate_sidecars(sidecars: &[PathBuf], origin: &Value) {
    for path in sidecars {
        let Some(mut sidecar) = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(raw.trim_start_matches('\u{feff}')).ok())
            .filter(Value::is_object)
        else {
            continue;
        };
        sidecar["buzz"] = origin.clone();
        if let Ok(out) = serde_json::to_string_pretty(&sidecar) {
            let _ = std::fs::write(path, out);
        }
    }
}

/// Replaces each `--image <repo>:<path>` value with a local copy of that file.
fn localize_repo_images(
    config: &Config,
    event: &Event,
    args: &mut [String],
    work: &Path,
) -> Result<(), String> {
    for (count, (index, prefix, value)) in image_values(args).into_iter().enumerate() {
        if !is_repo_ref(&value) {
            continue;
        }
        let target = repo::resolve_for(&config.buzz, &event.channel, &event.sender, &value)?;
        let dest_dir = work.join(format!("src{}", count + 1));
        std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
        let local = repo::fetch_file(work, &target, &dest_dir)?;
        args[index] = format!("{prefix}{}", local.to_string_lossy());
    }
    Ok(())
}

/// The value of `--flag value` or `--flag=value`.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let with_value = format!("{flag}=");
    args.iter().enumerate().find_map(|(i, arg)| {
        if arg == flag {
            args.get(i + 1).map(String::as_str)
        } else {
            arg.strip_prefix(&with_value)
        }
    })
}

/// Each `--image` value: its argument's index, the text before it in that
/// argument, and the value.
fn image_values(args: &[String]) -> Vec<(usize, &'static str, String)> {
    args.iter()
        .enumerate()
        .filter_map(|(i, arg)| {
            if arg == "--image" {
                args.get(i + 1).map(|v| (i + 1, "", v.clone()))
            } else {
                arg.strip_prefix("--image=")
                    .map(|v| (i, "--image=", v.to_string()))
            }
        })
        .collect()
}

/// `<repo>:<path>`, as opposed to a local path such as `C:\pics\a.png`.
fn is_repo_ref(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive_path = bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes.get(2), None | Some(b'\\') | Some(b'/'));
    value.contains(':') && !drive_path && !value.contains("://")
}

/// Drops lines that only point into mage-agent's temp folder, which is
/// deleted once the reply is posted.
fn without_lines_mentioning(text: &str, dir: &Path) -> String {
    let needle = dir.to_string_lossy().replace('\\', "/").to_lowercase();
    text.lines()
        .filter(|line| !line.replace('\\', "/").to_lowercase().contains(&needle))
        .collect::<Vec<_>>()
        .join("\n")
}

fn attachments(tags: &[Vec<String>]) -> Vec<Attachment> {
    tags.iter()
        .filter(|tag| tag.first().map(String::as_str) == Some("imeta"))
        .filter_map(|tag| {
            let field = |name: &str| {
                tag.iter()
                    .find_map(|f| f.strip_prefix(name).map(str::to_string))
            };
            let url = field("url ")?;
            let mut imeta = json!({ "url": url });
            for name in ["m", "x", "size", "dim"] {
                if let Some(value) = field(&format!("{name} ")) {
                    imeta[name] = value.into();
                }
            }
            Some(Attachment {
                mime: field("m ").unwrap_or_default(),
                url,
                imeta,
            })
        })
        .collect()
}

fn download(
    config: &Config,
    image: &Attachment,
    work: &Path,
    number: usize,
) -> Result<PathBuf, String> {
    let ext = match image.mime.as_str() {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "jpg",
    };
    let path = work.join(format!("image{number}.{ext}"));
    let mut cmd = proc::command(&config.buzz);
    cmd.args(["media", "get", &image.url, "--output"])
        .arg(&path);
    let out = proc::run(cmd, None, BUZZ_TIMEOUT).map_err(|e| e.to_string())?;
    if out.success {
        Ok(path)
    } else {
        Err(out.failure_text())
    }
}

fn produced_files(out_dir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut images = Vec::new();
    let mut sidecars = Vec::new();
    let Ok(entries) = std::fs::read_dir(out_dir) else {
        return (images, sidecars);
    };
    for path in entries.flatten().map(|e| e.path()).filter(|p| p.is_file()) {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "json" {
            sidecars.push(path);
        } else if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            images.push(path);
        }
    }
    images.sort();
    sidecars.sort();
    (images, sidecars)
}

/// Metadata-free copies for posting; the originals go to the repo untouched.
fn attachable(images: &[PathBuf], work: &Path) -> Vec<PathBuf> {
    let dir = work.join("attach");
    let _ = std::fs::create_dir_all(&dir);
    images
        .iter()
        .filter_map(|image| {
            let bytes = std::fs::read(image).ok()?;
            let dest = dir.join(image.file_name()?);
            match strip::strip_metadata(&bytes) {
                Some(clean) => std::fs::write(&dest, clean).ok().map(|_| dest),
                None => {
                    eprintln!(
                        "mage-agent: cannot strip {}, attaching as is",
                        image.display()
                    );
                    Some(image.clone())
                }
            }
        })
        .collect()
}

/// `messages send --file` appends `\n![name](url)` per file, so a trailing
/// newline puts the attachments in their own paragraph, a paragraph gap below
/// the text. With no text, the attachments start the message.
fn with_attachment_gap(content: &str, has_files: bool) -> String {
    if !has_files {
        return content.to_string();
    }
    let text = content.trim_end();
    if text.is_empty() {
        String::new()
    } else {
        format!("{text}\n")
    }
}

fn post(config: &Config, channel: &str, reply_to: Option<&str>, reply: Reply) {
    let send = |content: &str, files: &[PathBuf]| {
        let content = with_attachment_gap(content, !files.is_empty());
        let mut cmd = proc::command(&config.buzz);
        cmd.args(["messages", "send", "--channel", channel, "--content", "-"]);
        if let Some(id) = reply_to {
            cmd.args(["--reply-to", id]);
        }
        for file in files {
            cmd.arg("--file").arg(file);
        }
        proc::run(cmd, Some(&content), BUZZ_TIMEOUT)
            .map_err(|e| e.to_string())
            .and_then(|out| {
                if out.success {
                    Ok(())
                } else {
                    Err(out.failure_text())
                }
            })
    };

    let Err(error) = send(&reply.content, &reply.files) else {
        return;
    };
    eprintln!("mage-agent: send failed: {error}");
    if reply.files.is_empty() {
        return;
    }
    let fallback = format!("{}\nCould not attach the image: {error}", reply.content);
    if let Err(e) = send(&fallback, &[]) {
        eprintln!("mage-agent: fallback send failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imeta_fields() {
        let tags = vec![
            vec!["h".to_string(), "x".to_string()],
            vec![
                "imeta".to_string(),
                "url https://r/media/a.png".to_string(),
                "m image/png".to_string(),
                "x abc123".to_string(),
                "dim 896x1152".to_string(),
                "blurhash LKO2".to_string(),
            ],
        ];
        let found = attachments(&tags);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url, "https://r/media/a.png");
        assert_eq!(found[0].mime, "image/png");
        assert_eq!(
            found[0].imeta,
            json!({ "url": "https://r/media/a.png", "m": "image/png",
                    "x": "abc123", "dim": "896x1152" })
        );
    }

    #[test]
    fn sidecars_get_the_origin_and_others_are_left_alone() {
        let dir = std::env::temp_dir().join(format!("mage-annotate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sidecar = dir.join("h1.json");
        let other = dir.join("h2.json");
        std::fs::write(&sidecar, "\u{feff}{\"prompt\":\"a fox\"}").unwrap();
        std::fs::write(&other, "[1, 2]").unwrap();
        let origin = json!({ "mode": "edit", "repo": "owner/site" });
        annotate_sidecars(&[sidecar.clone(), other.clone()], &origin);
        let read = |path: &Path| std::fs::read_to_string(path).unwrap();
        let written: Value = serde_json::from_str(&read(&sidecar)).unwrap();
        assert_eq!(written["prompt"], "a fox");
        assert_eq!(written["buzz"], origin);
        assert_eq!(read(&other), "[1, 2]");

        annotate_sidecars(std::slice::from_ref(&sidecar), &json!({ "mode": "edit" }));
        let written: Value = serde_json::from_str(&read(&sidecar)).unwrap();
        assert_eq!(written["buzz"], json!({ "mode": "edit" }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repo_address_is_owner_and_id() {
        let repo = |owner: Option<&str>| repo::RepoInfo {
            id: "site".into(),
            name: None,
            clone: None,
            branch: "main".into(),
            owner: owner.map(str::to_string),
            channel: None,
        };
        assert_eq!(repo_address(&repo(Some("ab12"))), "ab12/site");
        assert_eq!(repo_address(&repo(Some(""))), "site");
        assert_eq!(repo_address(&repo(None)), "site");
    }

    #[test]
    fn repo_refs_versus_local_paths() {
        assert!(is_repo_ref("site:img/fox.jpg"));
        assert!(is_repo_ref("art-repo:a.png"));
        assert!(!is_repo_ref(r"C:\pics\a.png"));
        assert!(!is_repo_ref("d:/pics/a.png"));
        assert!(!is_repo_ref("pics/a.png"));
        assert!(!is_repo_ref("https://cdn/x.png"));
    }

    #[test]
    fn flag_value_forms() {
        let args: Vec<String> = ["gen", "--model", "flux2-dev", "--res=2K", "--aspect"]
            .map(String::from)
            .to_vec();
        assert_eq!(flag_value(&args, "--model"), Some("flux2-dev"));
        assert_eq!(flag_value(&args, "--res"), Some("2K"));
        assert_eq!(flag_value(&args, "--aspect"), None);
        assert_eq!(flag_value(&args, "--arch"), None);
    }

    #[test]
    fn image_value_forms() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            image_values(&args(&[
                "generate",
                "--image",
                "site:a.jpg",
                "x",
                "--image=site:b.jpg"
            ])),
            [
                (2, "", "site:a.jpg".to_string()),
                (4, "--image=", "site:b.jpg".to_string())
            ]
        );
        assert_eq!(image_values(&args(&["generate", "x"])), []);
    }

    #[test]
    fn commit_message_is_the_reply_without_the_url_line() {
        let reply = "model: mango-v4, portrait, 1K (896x1152), 21s\n\
                     history_id: f2eb17ef\n\
                     url: https://cdn3.mage.space/x.jpg (expires Dec 15, 2026)\n\
                     Edited the first attached image; 1 other image ignored.";
        assert_eq!(
            commit_message(reply, "@Mage a dragon"),
            "model: mango-v4, portrait, 1K (896x1152), 21s, history_id: f2eb17ef, \
             Edited the first attached image; 1 other image ignored."
        );
        assert_eq!(
            commit_message("url: https://x\n", "@Mage **a** dragon"),
            "@Mage a dragon"
        );
    }

    #[test]
    fn sweep_removes_only_job_folders_past_the_age() {
        let root = std::env::temp_dir().join(format!("mage-sweep-{}", std::process::id()));
        std::fs::create_dir_all(root.join("aaaa/out")).unwrap();
        std::fs::create_dir_all(root.join("bbbb")).unwrap();
        std::fs::write(root.join("note.txt"), "x").unwrap();
        assert_eq!(sweep_stale_work(&root, Duration::from_secs(3600)), 0);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(sweep_stale_work(&root, Duration::ZERO), 2);
        assert!(root.join("note.txt").is_file());
        assert!(!root.join("aaaa").exists());
        assert_eq!(sweep_stale_work(&root.join("missing"), Duration::ZERO), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn attachments_get_their_own_paragraph() {
        assert_eq!(with_attachment_gap("Saved to x\n\n", true), "Saved to x\n");
        assert_eq!(with_attachment_gap(" \n", true), "");
        assert_eq!(with_attachment_gap("no image", false), "no image");
    }

    #[test]
    fn temp_path_lines_are_hidden() {
        let work = Path::new(r"C:\Users\U\AppData\Local\Temp\mage-agent\299ff6337edd6ec1");
        let printed =
            "C:\\Users\\U\\AppData\\Local\\Temp\\mage-agent\\299ff6337edd6ec1\\out\\x.jpg\n\
                       model: mango-v4\nurl: https://cdn/x.jpg";
        assert_eq!(
            without_lines_mentioning(printed, work),
            "model: mango-v4\nurl: https://cdn/x.jpg"
        );
    }
}
