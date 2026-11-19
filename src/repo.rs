//! `--repo [<repo>:]<path>` and `--src <repo>:<path>`: Buzz repositories bound
//! to the channel, or named as `<owner-hex>/<repo>` from anywhere (a DM, say)
//! by someone who owns it or belongs to its bound channel. Each is reached
//! through a temporary blobless clone in the job's work
//! folder. It holds the latest commit and its folder listings but no file
//! contents, so saving uploads only the new files and reading fetches only the
//! one file asked for. A relay without partial-clone support sends every file
//! instead; that still works, just slower.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::proc;

const GIT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Deserialize)]
pub struct RepoInfo {
    pub id: String,
    pub name: Option<String>,
    pub clone: Option<String>,
    pub branch: String,
    #[serde(default)]
    pub owner: Option<String>,
    /// The channel the repository is bound to.
    #[serde(default)]
    pub channel: Option<String>,
}

#[derive(Deserialize)]
struct Member {
    pubkey: String,
}

impl RepoInfo {
    fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    fn matches(&self, wanted: &str) -> bool {
        if let Some((owner, id)) = owner_and_id(wanted) {
            return self.id == id
                && self
                    .owner
                    .as_deref()
                    .is_some_and(|o| o.eq_ignore_ascii_case(owner));
        }
        self.id == wanted
            || self
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(wanted))
    }
}

/// `<owner-hex>/<repo>` split into its parts.
fn owner_and_id(wanted: &str) -> Option<(&str, &str)> {
    let (owner, id) = wanted.split_once('/')?;
    (crate::prompt::is_hex64(owner) && !id.is_empty()).then_some((owner, id))
}

#[derive(Debug)]
pub struct Target {
    pub repo: RepoInfo,
    pub path: String,
}

pub struct Saved {
    /// Repo paths written, images first.
    #[cfg_attr(not(test), allow(dead_code))]
    pub files: Vec<String>,
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

fn list_owner_repos(buzz: &Path, owner: &str) -> Result<Vec<RepoInfo>, String> {
    let mut cmd = proc::command(buzz);
    cmd.args(["--format", "compact", "repos", "list", "--owner", owner]);
    let out = proc::run(cmd, None, Duration::from_secs(60)).map_err(|e| e.to_string())?;
    if !out.success {
        return Err(format!("`buzz repos list` failed: {}", out.failure_text()));
    }
    serde_json::from_str(out.stdout.trim()).map_err(|e| format!("unreadable repo list: {e}"))
}

fn channel_members(buzz: &Path, channel: &str) -> Result<Vec<String>, String> {
    let mut cmd = proc::command(buzz);
    cmd.args([
        "--format",
        "compact",
        "channels",
        "members",
        "--channel",
        channel,
    ]);
    let out = proc::run(cmd, None, Duration::from_secs(60)).map_err(|e| e.to_string())?;
    if !out.success {
        return Err(format!(
            "`buzz channels members` failed: {}",
            out.failure_text()
        ));
    }
    let members: Vec<Member> = serde_json::from_str(out.stdout.trim())
        .map_err(|e| format!("unreadable member list: {e}"))?;
    Ok(members.into_iter().map(|m| m.pubkey).collect())
}

/// The repository `spec` names for a request from `sender` in `channel`: one
/// bound to that channel, else one named `<owner-hex>/<repo>` that `sender`
/// owns or whose bound channel `sender` belongs to. The relay still decides
/// whether mage itself may push there.
pub fn resolve_for(buzz: &Path, channel: &str, sender: &str, spec: &str) -> Result<Target, String> {
    let repos = list_channel_repos(buzz, channel)?;
    let not_here = match resolve_target(spec, &repos) {
        Ok(target) => return Ok(target),
        Err(e) => e,
    };
    let Some((owner, id, path)) = spec
        .split_once(':')
        .and_then(|(wanted, path)| owner_and_id(wanted).map(|(o, i)| (o, i, path)))
    else {
        return Err(not_here);
    };
    let repo = list_owner_repos(buzz, owner)?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("no repository `{id}` from that owner"))?;
    let members = match &repo.channel {
        Some(bound) if !sender.eq_ignore_ascii_case(owner) => channel_members(buzz, bound)?,
        _ => Vec::new(),
    };
    authorize_unbound(&repo, owner, sender, &members)?;
    Ok(Target {
        repo,
        path: validate_path(path)?,
    })
}

/// A repository from outside the request's channel is open to its owner and
/// to members of the channel it's bound to.
fn authorize_unbound(
    repo: &RepoInfo,
    owner: &str,
    sender: &str,
    members: &[String],
) -> Result<(), String> {
    if sender.eq_ignore_ascii_case(owner) {
        return Ok(());
    }
    if repo.channel.is_none() {
        return Err(format!("`{}` isn't bound to a channel", repo.label()));
    }
    if members.iter().any(|m| m.eq_ignore_ascii_case(sender)) {
        Ok(())
    } else {
        Err(format!(
            "only members of `{}`'s channel can save to it",
            repo.label()
        ))
    }
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
            _ => {
                return Err(format!(
                "this channel has several repositories; use `--repo <repo>:{spec}` with one of {}",
                names()
            ))
            }
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
    is_existing_folder: impl Fn(&str) -> bool,
    images: &[PathBuf],
    sidecars: &[PathBuf],
) -> Result<Vec<(PathBuf, String)>, String> {
    let file_name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
    let trimmed = path.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or("");
    let names_file = !path.ends_with('/')
        && Path::new(last).extension().is_some()
        && !is_existing_folder(trimmed);

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

/// Commits the files on top of the default branch's latest commit without
/// checking anything out: the new files are hashed into the clone and added to
/// an index read from the latest folder listings.
pub fn save(
    work: &Path,
    target: &Target,
    images: &[PathBuf],
    sidecars: &[PathBuf],
    message: &str,
) -> Result<Saved, String> {
    let repo = &target.repo;
    let clone = work.join("repo-save");
    let mut base = blobless_clone(&clone, repo)?;
    let placed = destinations(
        &target.path,
        |folder| {
            base.as_deref().is_some_and(|commit| {
                git(&clone, &["cat-file", "-t", &format!("{commit}:{folder}")])
                    .is_ok_and(|kind| kind == "tree")
            })
        },
        images,
        sidecars,
    )?;
    let mut entries = Vec::new();
    for (src, rel) in &placed {
        let oid = git(
            &clone,
            &[
                "hash-object",
                "-w",
                "--no-filters",
                "--",
                &src.to_string_lossy(),
            ],
        )
        .map_err(|e| format!("could not add {rel}: {e}"))?;
        entries.push((oid, rel.as_str()));
    }

    let refspec = |commit: &str| format!("{commit}:refs/heads/{}", repo.branch);
    let mut commit = commit_on(&clone, base.as_deref(), &entries, message)?;
    if let Err(first) = git(&clone, &["push", "-q", "origin", &refspec(&commit)]) {
        // The branch moved on: rebuild the same commit on its new tip.
        base =
            Some(fetch_tip(&clone, &repo.branch).map_err(|_| format!("push rejected: {first}"))?);
        commit = commit_on(&clone, base.as_deref(), &entries, message)?;
        git(&clone, &["push", "-q", "origin", &refspec(&commit)])
            .map_err(|second| format!("push rejected: {second}"))?;
    }

    Ok(Saved {
        files: placed.into_iter().map(|(_, rel)| rel).collect(),
    })
}

/// Writes `target.path` from the default branch into `dest_dir`, fetching only
/// that file.
pub fn fetch_file(work: &Path, target: &Target, dest_dir: &Path) -> Result<PathBuf, String> {
    let repo = &target.repo;
    let clone = work.join("repo-src");
    let not_a_file = || {
        format!(
            "`{}` is not a file on `{}` in `{}`",
            target.path,
            repo.branch,
            repo.label()
        )
    };
    let base = blobless_clone(&clone, repo)?.ok_or_else(not_a_file)?;
    let spec = format!("{base}:{}", target.path);
    if git(&clone, &["cat-file", "-t", &spec]).ok().as_deref() != Some("blob") {
        return Err(not_a_file());
    }
    git(&clone, &["checkout", &base, "--", &target.path])
        .map_err(|e| format!("could not fetch {}: {e}", target.path))?;
    let source = clone.join(&target.path);
    let dest = dest_dir.join(source.file_name().ok_or("path names no file")?);
    std::fs::copy(&source, &dest).map_err(|e| format!("copy {}: {e}", target.path))?;
    Ok(dest)
}

/// Clones only the default branch's latest commit and folder listings into
/// `dir`, returning that commit, or `None` when the branch has no commits yet.
fn blobless_clone(dir: &Path, repo: &RepoInfo) -> Result<Option<String>, String> {
    let url = repo
        .clone
        .as_deref()
        .ok_or_else(|| format!("repository `{}` publishes no clone URL", repo.label()))?;
    let _ = std::fs::remove_dir_all(dir);
    let parent = dir.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let branch_ref = format!("refs/heads/{}", repo.branch);
    let heads = git(parent, &["ls-remote", url, &branch_ref])
        .map_err(|e| format!("could not reach `{}`: {e}", repo.label()))?;
    let dir_arg = dir.to_string_lossy();
    if heads.is_empty() {
        git(parent, &["init", "-q", &dir_arg])?;
        git(dir, &["remote", "add", "origin", url])?;
        return Ok(None);
    }
    git(
        parent,
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            "--filter=blob:none",
            "--no-checkout",
            "--branch",
            &repo.branch,
            url,
            &dir_arg,
        ],
    )
    .map_err(|e| format!("clone failed: {e}"))?;
    git(dir, &["rev-parse", "HEAD"]).map(Some)
}

fn fetch_tip(clone: &Path, branch: &str) -> Result<String, String> {
    git(clone, &["fetch", "-q", "--depth", "1", "origin", branch])?;
    git(clone, &["rev-parse", "FETCH_HEAD"])
}

/// A commit whose tree is `base`'s plus `entries` (blob id, repository path).
fn commit_on(
    clone: &Path,
    base: Option<&str>,
    entries: &[(String, &str)],
    message: &str,
) -> Result<String, String> {
    match base {
        Some(commit) => git(clone, &["read-tree", commit])?,
        None => git(clone, &["read-tree", "--empty"])?,
    };
    for (oid, rel) in entries {
        let info = format!("100644,{oid},{rel}");
        git(clone, &["update-index", "--add", "--cacheinfo", &info])?;
    }
    // Without --missing-ok, write-tree checks every file exists and so
    // downloads all of the existing ones.
    let tree = git(clone, &["write-tree", "--missing-ok"])?;
    let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
    if let Some(commit) = base {
        args.extend(["-p", commit]);
    }
    git(clone, &args).map_err(|e| format!("commit failed: {e}"))
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
            clone: Some(format!("https://relay/git/x/{id}")),
            branch: "main".into(),
            owner: Some(OWNER.into()),
            channel: Some("ch-1".into()),
        }
    }

    const OWNER: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn owner_qualified_name_matches_id_and_owner() {
        let two = [repo("site", "Site"), repo("art", "Art")];
        let spec = format!("{OWNER}/art:img/");
        assert_eq!(resolve_target(&spec, &two).unwrap().repo.id, "art");
        assert!(resolve_target(&format!("{OTHER}/art:img/"), &two).is_err());
        assert!(resolve_target(&format!("{OWNER}/Art:img/"), &two).is_err());
    }

    #[test]
    fn unbound_repo_is_for_its_owner_and_its_channel_members() {
        let art = repo("art", "Art");
        assert!(authorize_unbound(&art, OWNER, OWNER, &[]).is_ok());
        assert!(authorize_unbound(&art, OWNER, OTHER, &[OTHER.to_uppercase()]).is_ok());
        let err = authorize_unbound(&art, OWNER, OTHER, &[OWNER.into()]).unwrap_err();
        assert!(err.contains("only members of `Art`"));
        let unbound = RepoInfo {
            channel: None,
            ..repo("art", "Art")
        };
        assert!(authorize_unbound(&unbound, OWNER, OTHER, &[OTHER.into()]).is_err());
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
        let no_folders = |_: &str| false;
        let images = [PathBuf::from("out/h1.jpg")];
        let sidecars = [PathBuf::from("out/h1.json")];

        let d = destinations("art/", no_folders, &images, &sidecars).unwrap();
        assert_eq!(d[0].1, "art/h1.jpg");
        assert_eq!(d[1].1, "art/h1.json");

        let d = destinations("art/fox", no_folders, &images, &sidecars).unwrap();
        assert_eq!(d[0].1, "art/fox/h1.jpg");

        let d = destinations("", no_folders, &images, &sidecars).unwrap();
        assert_eq!(d[0].1, "h1.jpg");

        let d = destinations("art/fox.png", no_folders, &images, &sidecars).unwrap();
        assert_eq!(d[0].1, "art/fox.jpg");
        assert_eq!(d[1].1, "art/fox.json");

        let two = [PathBuf::from("a.jpg"), PathBuf::from("b.jpg")];
        assert!(destinations("fox.jpg", no_folders, &two, &[]).is_err());
    }

    /// A bare repository served over `file://` with the relay's partial-clone
    /// settings, optionally holding `assets/old.bin`.
    fn served_repo(root: &Path, seeded: bool) -> RepoInfo {
        let bare = root.join("remote.git");
        let bare_arg = bare.to_string_lossy();
        git(root, &["init", "-q", "--bare", "-b", "main", &bare_arg]).unwrap();
        git(&bare, &["config", "uploadpack.allowFilter", "true"]).unwrap();
        git(
            &bare,
            &["config", "uploadpack.allowReachableSHA1InWant", "true"],
        )
        .unwrap();
        let url = format!(
            "file:///{}",
            bare_arg.replace('\\', "/").trim_start_matches('/')
        );
        if seeded {
            let seed = root.join("seed");
            git(root, &["init", "-q", "-b", "main", &seed.to_string_lossy()]).unwrap();
            std::fs::create_dir_all(seed.join("assets")).unwrap();
            std::fs::write(seed.join("assets/old.bin"), vec![7u8; 300_000]).unwrap();
            git(&seed, &["add", "."]).unwrap();
            git(&seed, &["commit", "-q", "-m", "seed"]).unwrap();
            git(&seed, &["push", "-q", &url, "main"]).unwrap();
        }
        RepoInfo {
            id: "art".into(),
            name: Some("Art".into()),
            clone: Some(url),
            branch: "main".into(),
            owner: None,
            channel: None,
        }
    }

    fn generated(root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("h1.jpg"), b"jpeg bytes").unwrap();
        std::fs::write(out.join("h1.json"), b"{\"prompt\":\"fox\"}").unwrap();
        (vec![out.join("h1.jpg")], vec![out.join("h1.json")])
    }

    /// Buzz supplies the commit identity through the environment.
    fn set_identity() {
        for (key, value) in [
            ("GIT_AUTHOR_NAME", "mage"),
            ("GIT_AUTHOR_EMAIL", "mage@test"),
            ("GIT_COMMITTER_NAME", "mage"),
            ("GIT_COMMITTER_EMAIL", "mage@test"),
        ] {
            std::env::set_var(key, value);
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mage-agent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_adds_files_without_downloading_existing_ones_and_src_fetches_one() {
        set_identity();
        let root = scratch("save");
        let repo = served_repo(&root, true);
        let (images, sidecars) = generated(&root);
        let work = root.join("work");
        let target = Target {
            repo: repo.clone(),
            path: "assets".into(),
        };
        let message = "@Mage gen a fox --repo assets\n\n# kept verbatim";
        let saved = save(&work, &target, &images, &sidecars, message).unwrap();
        assert_eq!(saved.files, ["assets/h1.jpg", "assets/h1.json"]);

        let listing = git(
            &work.join("repo-save"),
            &["rev-list", "--objects", "--missing=print", "HEAD"],
        )
        .unwrap();
        assert!(
            listing.lines().any(|l| l.starts_with('?')),
            "old.bin was downloaded: {listing}"
        );

        let bare = root.join("remote.git");
        let tree = git(&bare, &["ls-tree", "-r", "--name-only", "main"]).unwrap();
        assert_eq!(tree, "assets/h1.jpg\nassets/h1.json\nassets/old.bin");
        let body = git(&bare, &["log", "-1", "--format=%B", "main"]).unwrap();
        assert_eq!(body, message);

        let dest = root.join("src");
        std::fs::create_dir_all(&dest).unwrap();
        let src = Target {
            repo,
            path: "assets/old.bin".into(),
        };
        let file = fetch_file(&root.join("work2"), &src, &dest).unwrap();
        assert_eq!(std::fs::read(file).unwrap().len(), 300_000);

        let absent = Target {
            path: "assets/nope.jpg".into(),
            ..src
        };
        let err = fetch_file(&root.join("work3"), &absent, &dest).unwrap_err();
        assert!(err.contains("is not a file"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_starts_the_default_branch_of_an_empty_repository() {
        set_identity();
        let root = scratch("empty");
        let repo = served_repo(&root, false);
        let (images, sidecars) = generated(&root);
        let target = Target {
            repo,
            path: "img/fox.png".into(),
        };
        let saved = save(&root.join("work"), &target, &images, &sidecars, "first").unwrap();
        assert_eq!(saved.files, ["img/fox.jpg", "img/fox.json"]);
        let bare = root.join("remote.git");
        let tree = git(&bare, &["ls-tree", "-r", "--name-only", "main"]).unwrap();
        assert_eq!(tree, "img/fox.jpg\nimg/fox.json");
        let _ = std::fs::remove_dir_all(&root);
    }
}
