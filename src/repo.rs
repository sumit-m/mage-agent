//! `--repo [<repo>:]<path>`: commit generated files to a Buzz repository bound
//! to the channel, using a mage-agent–owned clone so no shared checkout is touched.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;

use crate::proc;

static GIT_LOCK: Mutex<()> = Mutex::new(());
const GIT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Deserialize)]
pub struct RepoInfo {
    pub id: String,
    pub name: Option<String>,
    pub owner: String,
    pub clone: Option<String>,
    pub branch: String,
}

impl RepoInfo {
    fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    fn matches(&self, wanted: &str) -> bool {
        self.id == wanted
            || self
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(wanted))
    }
}

#[derive(Debug)]
pub struct Target {
    pub repo: RepoInfo,
    pub path: String,
}

pub struct Saved {
    pub label: String,
    pub files: Vec<String>,
    pub commit: String,
}

pub fn list_channel_repos(buzz: &Path, channel: &str) -> Result<Vec<RepoInfo>, String> {
    let mut cmd = proc::command(buzz);
    cmd.args(["--format", "compact", "repos", "list", "--channel", channel]);
    let out = proc::run(cmd, None, Duration::from_secs(60)).map_err(|e| e.to_string())?;
    if !out.success {
        return Err(format!("`buzz repos list` failed: {}", out.failure_text()));
    }
    serde_json::from_str(out.stdout.trim()).map_err(|e| format!("unreadable repo list: {e}"))
}

pub fn resolve_target(spec: &str, repos: &[RepoInfo]) -> Result<Target, String> {
    let names = || {
        repos
            .iter()
            .map(|r| format!("`{}`", r.label()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let (repo, path) = match spec.split_once(':') {
        Some((wanted, path)) => {
            let repo = repos.iter().find(|r| r.matches(wanted)).ok_or_else(|| {
                if repos.is_empty() {
                    format!(
                        "no repository named `{wanted}`: this channel has no bound repositories"
                    )
                } else {
                    format!(
                        "no repository named `{wanted}` in this channel; bound: {}",
                        names()
                    )
                }
            })?;
            (repo, path)
        }
        None => match repos {
            [only] => (only, spec),
            [] => return Err("this channel has no bound repository".into()),
            _ => return Err(format!(
                "this channel has several repositories; use `--repo <repo>:{spec}` with one of {}",
                names()
            )),
        },
    };
    Ok(Target {
        repo: repo.clone(),
        path: validate_path(path)?,
    })
}

fn validate_path(path: &str) -> Result<String, String> {
    let normalized = path.trim().replace('\\', "/");
    let bad = normalized.starts_with('/')
        || normalized.contains([':', '*', '?', '"', '<', '>', '|'])
        || normalized
            .split('/')
            .any(|part| part == ".." || part.eq_ignore_ascii_case(".git"));
    if bad {
        return Err(format!(
            "`{path}` must be a relative path inside the repository"
        ));
    }
    Ok(normalized.trim_start_matches("./").to_string())
}

/// Where each generated file lands, relative to the repository root.
///
/// A path whose last part has an extension names the image itself (its
/// extension follows the real image; the sidecar shares its stem). Anything
/// else is a directory that keeps mage's file names.
fn destinations(
    path: &str,
    checkout: &Path,
    images: &[PathBuf],
    sidecars: &[PathBuf],
) -> Result<Vec<(PathBuf, String)>, String> {
    let file_name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
    let trimmed = path.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or("");
    let names_file = !path.ends_with('/')
        && Path::new(last).extension().is_some()
        && !checkout.join(trimmed).is_dir();

    if !names_file {
        let dir = if trimmed.is_empty() || trimmed == "." {
            String::new()
        } else {
            format!("{trimmed}/")
        };
        return Ok(images
            .iter()
            .chain(sidecars)
            .map(|src| (src.clone(), format!("{dir}{}", file_name(src))))
            .collect());
    }

    let [image] = images else {
        return Err(format!(
            "`{path}` names one file but mage produced {} images; use a directory path ending in `/`",
            images.len()
        ));
    };
    let stem = trimmed.rsplit_once('.').map_or(trimmed, |(s, _)| s);
    let ext = image
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut placed = vec![(image.clone(), format!("{stem}.{ext}"))];
    let own_sidecar = image.with_extension("json");
    if let Some(sidecar) = sidecars
        .iter()
        .find(|s| **s == own_sidecar)
        .or(sidecars.first())
    {
        placed.push((sidecar.clone(), format!("{stem}.json")));
    }
    Ok(placed)
}

pub fn save(
    cache_root: &Path,
    target: &Target,
    images: &[PathBuf],
    sidecars: &[PathBuf],
    message: &str,
) -> Result<Saved, String> {
    let _guard = GIT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let repo = &target.repo;
    let checkout = synced_checkout(cache_root, repo)?;
    let placed = destinations(&target.path, &checkout, images, sidecars)?;
    for (src, rel) in &placed {
        let dest = checkout.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(src, &dest).map_err(|e| format!("copy to {rel}: {e}"))?;
    }

    let rels: Vec<String> = placed.into_iter().map(|(_, rel)| rel).collect();
    let mut add = vec!["add", "--"];
    add.extend(rels.iter().map(String::as_str));
    git(&checkout, &add)?;
    git(&checkout, &["commit", "--cleanup=verbatim", "-m", message])
        .map_err(|e| format!("commit failed: {e}"))?;

    let refspec = format!("HEAD:refs/heads/{}", repo.branch);
    if let Err(first) = git(&checkout, &["push", "origin", &refspec]) {
        let retried = git(&checkout, &["pull", "--rebase", "origin", &repo.branch])
            .and_then(|_| git(&checkout, &["push", "origin", &refspec]));
        if let Err(second) = retried {
            let _ = git(&checkout, &["rebase", "--abort"]);
            return Err(format!(
                "push rejected: {}",
                if second.is_empty() { first } else { second }
            ));
        }
    }

    Ok(Saved {
        label: repo.label().to_string(),
        files: rels,
        commit: git(&checkout, &["rev-parse", "--short", "HEAD"])?,
    })
}

/// Copies `target.path` from the repository's default branch into `dest_dir`,
/// so a later save to the same clone cannot change it mid-job.
pub fn fetch_file(cache_root: &Path, target: &Target, dest_dir: &Path) -> Result<PathBuf, String> {
    let _guard = GIT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let repo = &target.repo;
    let checkout = synced_checkout(cache_root, repo)?;
    let source = checkout.join(&target.path);
    if !source.is_file() {
        return Err(format!(
            "`{}` is not a file on `{}` in `{}`",
            target.path,
            repo.branch,
            repo.label()
        ));
    }
    let dest = dest_dir.join(source.file_name().ok_or("path names no file")?);
    std::fs::copy(&source, &dest).map_err(|e| format!("copy {}: {e}", target.path))?;
    Ok(dest)
}

fn synced_checkout(cache_root: &Path, repo: &RepoInfo) -> Result<PathBuf, String> {
    let clone_url = repo
        .clone
        .as_deref()
        .ok_or_else(|| format!("repository `{}` publishes no clone URL", repo.label()))?;
    let owner_prefix: String = repo.owner.chars().take(8).collect();
    let checkout = cache_root.join(format!("{owner_prefix}-{}", repo.id));
    sync_checkout(&checkout, clone_url, &repo.branch)?;
    Ok(checkout)
}

fn sync_checkout(checkout: &Path, clone_url: &str, branch: &str) -> Result<(), String> {
    if !checkout.join(".git").is_dir() {
        if let Some(parent) = checkout.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let _ = std::fs::remove_dir_all(checkout);
        git(
            Path::new("."),
            &["clone", clone_url, &checkout.to_string_lossy()],
        )
        .map_err(|e| format!("clone failed: {e}"))?;
    }
    git(checkout, &["fetch", "origin", "--prune"]).map_err(|e| format!("fetch failed: {e}"))?;
    let remote = format!("origin/{branch}");
    if git(checkout, &["rev-parse", "--verify", "--quiet", &remote]).is_ok() {
        git(checkout, &["checkout", "-f", "-B", branch, &remote])?;
        git(checkout, &["reset", "--hard", &remote])?;
    } else {
        git(checkout, &["checkout", "-f", "--orphan", branch])?;
        let _ = git(checkout, &["rm", "-rf", "--cached", "--quiet", "."]);
    }
    git(checkout, &["clean", "-fdq"])?;
    Ok(())
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = proc::command(Path::new("git"));
    cmd.arg("-C").arg(dir).args(args);
    let out = proc::run(cmd, None, GIT_TIMEOUT).map_err(|e| format!("git: {e}"))?;
    if out.success {
        Ok(out.stdout.trim().to_string())
    } else {
        Err(out.failure_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(id: &str, name: &str) -> RepoInfo {
        RepoInfo {
            id: id.into(),
            name: Some(name.into()),
            owner: "ab".repeat(32),
            clone: Some(format!("https://relay/git/x/{id}")),
            branch: "main".into(),
        }
    }

    #[test]
    fn default_repo_needs_exactly_one() {
        let one = [repo("site", "Site")];
        let t = resolve_target("assets/", &one).unwrap();
        assert_eq!((t.repo.id.as_str(), t.path.as_str()), ("site", "assets/"));

        let two = [repo("site", "Site"), repo("art", "Art")];
        let err = resolve_target("assets/", &two).unwrap_err();
        assert!(err.contains("`Site`") && err.contains("`Art`"));
        assert!(resolve_target("assets/", &[]).is_err());
    }

    #[test]
    fn named_repo_matches_id_or_name() {
        let two = [repo("site", "Site"), repo("art-repo", "Art")];
        assert_eq!(
            resolve_target("art:img/", &two).unwrap().repo.id,
            "art-repo"
        );
        assert_eq!(
            resolve_target("art-repo:img/", &two).unwrap().repo.id,
            "art-repo"
        );
        assert!(resolve_target("nope:img/", &two).is_err());
    }

    #[test]
    fn rejects_escaping_paths() {
        let one = [repo("site", "Site")];
        for bad in [
            "../x",
            "/abs",
            "a/../../b",
            ".git/hooks/x",
            "site:C:/x",
            "assets/**",
            "a?.jpg",
        ] {
            assert!(resolve_target(bad, &one).is_err(), "{bad}");
        }
        assert_eq!(
            resolve_target("site:.\\img\\a.jpg", &one).unwrap().path,
            "img/a.jpg"
        );
    }

    #[test]
    fn directory_and_file_destinations() {
        let tmp = std::env::temp_dir().join("mage-agent-dest-test");
        let images = [PathBuf::from("out/h1.jpg")];
        let sidecars = [PathBuf::from("out/h1.json")];

        let d = destinations("art/", &tmp, &images, &sidecars).unwrap();
        assert_eq!(d[0].1, "art/h1.jpg");
        assert_eq!(d[1].1, "art/h1.json");

        let d = destinations("art/fox", &tmp, &images, &sidecars).unwrap();
        assert_eq!(d[0].1, "art/fox/h1.jpg");

        let d = destinations("", &tmp, &images, &sidecars).unwrap();
        assert_eq!(d[0].1, "h1.jpg");

        let d = destinations("art/fox.png", &tmp, &images, &sidecars).unwrap();
        assert_eq!(d[0].1, "art/fox.jpg");
        assert_eq!(d[1].1, "art/fox.json");

        let two = [PathBuf::from("a.jpg"), PathBuf::from("b.jpg")];
        assert!(destinations("fox.jpg", &tmp, &two, &[]).is_err());
    }
}
