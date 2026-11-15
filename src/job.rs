//! One `@mage` request: run the CLI, post its output and images, save to a repo.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::command;
use crate::prompt::Event;
use crate::{proc, repo, strip, terminal};

/// Includes time spent waiting in mage's queue; stays under buzz-acp's 2-hour turn cap.
const MAGE_TIMEOUT: Duration = Duration::from_secs(100 * 60);
const BUZZ_TIMEOUT: Duration = Duration::from_secs(180);
const IMAGE_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "webp", "gif"];

pub struct Config {
    pub mage: PathBuf,
    pub buzz: PathBuf,
    pub display_name: Option<String>,
    pub work_root: PathBuf,
    pub cache_root: PathBuf,
}

struct Attachment {
    url: String,
    mime: String,
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
    let mut request =
        match command::parse_request(&event.content, &urls, config.display_name.as_deref()) {
            Ok(request) => request,
            Err(message) => return Reply::text(message),
        };

    let mut args = request.args.clone();
    if request.subcommand() == "edit" && !request.has_flag("--src") {
        if let Some(image) = attachments.iter().find(|a| a.mime.starts_with("image/")) {
            match download(config, image, work) {
                Ok(path) => {
                    args.push("--src".into());
                    args.push(path.to_string_lossy().into_owned());
                }
                Err(e) => {
                    return Reply::text(format!("Could not download the attached image: {e}"))
                }
            }
        }
    }
    if let Err(e) = localize_repo_src(config, event, &mut args, work) {
        return Reply::text(format!("Could not get the `--src` image: {e}"));
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

    let mut content = without_lines_mentioning(&terminal::clean(&output.combined()), work);
    if output.timed_out {
        content.push_str(&format!(
            "\nStopped waiting after {} minutes; mage may still finish the job.",
            MAGE_TIMEOUT.as_secs() / 60
        ));
    } else if !output.success {
        content.push_str(&format!(
            "\nmage exited with code {}.",
            output.code.unwrap_or(-1)
        ));
    }

    let (images, sidecars) = produced_files(&out_dir);
    let mut files = attachable(&images, work);
    files.extend(sidecars.iter().cloned());

    if images.is_empty() {
        if output.success && request.produces_images() {
            content.push_str("\nmage reported success but wrote no image.");
        }
    } else if let Some(spec) = request.repo.take() {
        content.push('\n');
        content.push_str(&save_to_repo(config, event, &spec, &images, &sidecars));
    }
    let mut content = content.trim().to_string();
    if content.is_empty() {
        content = "mage finished without output.".into();
    }
    Reply { content, files }
}

fn save_to_repo(
    config: &Config,
    event: &Event,
    spec: &str,
    images: &[PathBuf],
    sidecars: &[PathBuf],
) -> String {
    let result = repo::list_channel_repos(&config.buzz, &event.channel)
        .and_then(|repos| repo::resolve_target(spec, &repos))
        .and_then(|target| {
            repo::save(
                &config.cache_root,
                &target,
                images,
                sidecars,
                &command::strip_formatting(&event.content),
            )
        });
    match result {
        Ok(saved) => format!(
            "Saved to `{}` @ `{}`: {}",
            saved.label,
            saved.commit,
            saved
                .files
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Err(e) => format!("Repo save failed: {e}"),
    }
}

/// Replaces a `--src <repo>:<path>` value with a local copy of that file.
fn localize_repo_src(
    config: &Config,
    event: &Event,
    args: &mut [String],
    work: &Path,
) -> Result<(), String> {
    let Some((index, prefix, value)) = src_value(args) else {
        return Ok(());
    };
    if !is_repo_ref(&value) {
        return Ok(());
    }
    let repos = repo::list_channel_repos(&config.buzz, &event.channel)?;
    let target = repo::resolve_target(&value, &repos)?;
    let dest_dir = work.join("src");
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    let local = repo::fetch_file(&config.cache_root, &target, &dest_dir)?;
    args[index] = format!("{prefix}{}", local.to_string_lossy());
    Ok(())
}

/// Index of the `--src` value, the text before it in that argument, and the value.
fn src_value(args: &[String]) -> Option<(usize, &'static str, String)> {
    args.iter().enumerate().find_map(|(i, arg)| {
        if arg == "--src" {
            args.get(i + 1).map(|v| (i + 1, "", v.clone()))
        } else {
            arg.strip_prefix("--src=")
                .map(|v| (i, "--src=", v.to_string()))
        }
    })
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
            Some(Attachment {
                url: field("url ")?,
                mime: field("m ").unwrap_or_default(),
            })
        })
        .collect()
}

fn download(config: &Config, image: &Attachment, work: &Path) -> Result<PathBuf, String> {
    let ext = match image.mime.as_str() {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "jpg",
    };
    let path = work.join(format!("source.{ext}"));
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

fn post(config: &Config, channel: &str, reply_to: Option<&str>, reply: Reply) {
    let send = |content: &str, files: &[PathBuf]| {
        let mut cmd = proc::command(&config.buzz);
        cmd.args(["messages", "send", "--channel", channel, "--content", "-"]);
        if let Some(id) = reply_to {
            cmd.args(["--reply-to", id]);
        }
        for file in files {
            cmd.arg("--file").arg(file);
        }
        proc::run(cmd, Some(content), BUZZ_TIMEOUT)
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
            ],
        ];
        let found = attachments(&tags);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url, "https://r/media/a.png");
        assert_eq!(found[0].mime, "image/png");
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
    fn src_value_forms() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            src_value(&args(&["edit", "--src", "site:a.jpg", "x"])),
            Some((2, "", "site:a.jpg".to_string()))
        );
        assert_eq!(
            src_value(&args(&["edit", "--src=site:a.jpg"])),
            Some((1, "--src=", "site:a.jpg".to_string()))
        );
        assert_eq!(src_value(&args(&["edit", "x"])), None);
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
