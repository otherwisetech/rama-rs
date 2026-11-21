//! Disk space a repository's worktrees could give back: merged worktrees, cargo's incremental
//! caches, and the build folders of worktrees nobody touched in a while. [`plan`] is pure and
//! deterministic (the app shows its exact paths and sizes before anything happens);
//! [`execute`] checks every rule again right before each delete.
use crate::git::{self, Cancel};
use crate::worktree::{DiskUsage, WorktreeEntry};
use crate::{Repo, Result};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Worktrees untouched this many days get their build folders offered.
pub const DEFAULT_STALE_DAYS: u32 = 14;
/// A repository with more than this to give back gets a Today entry.
pub const TODAY_THRESHOLD: u64 = 10 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReclaimKind {
    /// The worktree's branch is on the default branch, nothing is uncommitted or unpushed:
    /// `git worktree remove`, then optionally `git branch -d`.
    MergedWorktree,
    /// `target/**/incremental`: cargo's incremental caches, rebuilt on the next build.
    Incremental,
    /// The whole `target/` of a worktree untouched for the configured days.
    StaleTarget,
    /// `node_modules/` and `.next/` of a worktree untouched for the configured days.
    StaleDeps,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub kind: ReclaimKind,
    /// The worktree (or shared cargo target folder) the space is freed in.
    pub owner: PathBuf,
    /// `owner` is a cargo target folder outside the repository, used by `users`.
    pub shared: bool,
    pub users: Vec<PathBuf>,
    /// Merged worktrees: their branch, deleted with `git branch -d` only if the user asks
    /// (never a protected one).
    pub branch: Option<String>,
    /// Exactly what goes, with sizes: the worktree folder, or folders inside `owner`. Sorted.
    pub paths: Vec<(PathBuf, u64)>,
    pub bytes: u64,
    /// Whole days since anything outside build folders changed or was committed.
    pub idle_days: i64,
}

/// A cargo target folder outside the repository that some worktrees build into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedTarget {
    pub path: PathBuf,
    pub usage: DiskUsage,
    /// Worktrees whose builds go there, sorted.
    pub users: Vec<PathBuf>,
}

pub struct PlanInput<'a> {
    pub entries: &'a [WorktreeEntry],
    pub usage: &'a HashMap<PathBuf, DiskUsage>,
    pub shared: &'a [SharedTarget],
    /// The worktree this window has open: never removed.
    pub current: Option<&'a Path>,
    /// Branch patterns never deleted (and their worktrees never removed).
    pub protected: &'a [String],
    pub now: i64,
    pub stale_days: u32,
}

/// No uncommitted changes and no untracked files, and git could read its status.
pub fn is_clean(e: &WorktreeEntry) -> bool {
    e.wt.prunable.is_none() && !e.wt.bare && e.info.status.is_some() && e.info.changed() == 0
}

/// Every commit is on the default branch (its remote-tracking ref when there is one) and on
/// the upstream, if the branch has one that still exists.
pub fn nothing_unpushed(e: &WorktreeEntry) -> bool {
    let on_default = e.info.merged && e.info.vs_default().is_some_and(|(ahead, _)| ahead == 0);
    let pushed = e.info.upstream.is_none() || e.info.gone || e.info.ahead == 0;
    on_default && pushed
}

/// When someone last worked in a worktree: newest file outside build folders, or HEAD's commit.
pub fn last_touched(e: &WorktreeEntry, u: &DiskUsage) -> i64 {
    u.touched.max(e.info.time.unwrap_or(0))
}

fn removable(e: &WorktreeEntry, input: &PlanInput) -> bool {
    !e.wt.main && e.wt.locked.is_none() && input.current != Some(e.wt.path.as_path()) && is_clean(e) && nothing_unpushed(e) && !e.wt.branch.as_deref().is_some_and(|b| crate::is_protected(b, input.protected))
}

/// `path` lies inside `root` (or is it, with `or_equal`), compared without touching the disk:
/// no `..` or `.` components on either side.
pub fn contained(path: &Path, root: &Path, or_equal: bool) -> bool {
    let plain = |p: &Path| p.is_absolute() && p.components().all(|c| !matches!(c, Component::ParentDir | Component::CurDir));
    plain(path) && plain(root) && path.starts_with(root) && (or_equal || path != root)
}

fn is_target(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == "target")
}

/// What could be cleaned up, in a stable order (kind, then folder). Worktrees with
/// uncommitted changes, unreadable status or a partial size are never in it; a removed
/// worktree isn't offered anything else, and a `target/` offered whole doesn't also list its
/// incremental caches.
pub fn plan(input: &PlanInput) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    let days = |t: i64| ((input.now - t).max(0)) / 86_400;
    let stale_after = input.stale_days as i64;
    let mut idle: HashMap<&Path, Option<i64>> = HashMap::new();
    for e in input.entries {
        let Some(u) = input.usage.get(&e.wt.path).filter(|u| !u.partial) else {
            idle.insert(&e.wt.path, None);
            continue;
        };
        if !is_clean(e) {
            idle.insert(&e.wt.path, None);
            continue;
        }
        let owner = e.wt.path.clone();
        let idle_days = days(last_touched(e, u));
        let mk = |kind, paths: Vec<(PathBuf, u64)>, branch: Option<String>| {
            let mut paths: Vec<(PathBuf, u64)> = paths.into_iter().filter(|(p, b)| *b > 0 && contained(p, &owner, kind == ReclaimKind::MergedWorktree)).collect();
            paths.sort();
            Suggestion { kind, owner: owner.clone(), shared: false, users: Vec::new(), branch, bytes: paths.iter().map(|(_, b)| b).sum(), paths, idle_days }
        };
        if removable(e, input) {
            // a worktree removed counts as idle for the shared targets it used
            idle.insert(&e.wt.path, Some(i64::MAX));
            let branch = e.wt.branch.clone().filter(|b| !crate::is_protected(b, input.protected));
            out.push(mk(ReclaimKind::MergedWorktree, vec![(owner.clone(), u.total)], branch));
            continue;
        }
        idle.insert(&e.wt.path, Some(idle_days));
        let stale = idle_days >= stale_after;
        let targets: Vec<(PathBuf, u64)> = u.artifacts.iter().filter(|(p, _)| is_target(p)).cloned().collect();
        let deps: Vec<(PathBuf, u64)> = u.artifacts.iter().filter(|(p, _)| !is_target(p)).cloned().collect();
        let incremental: Vec<(PathBuf, u64)> = u.incremental.iter().filter(|(p, _)| !(stale && targets.iter().any(|(t, _)| p.starts_with(t)))).cloned().collect();
        out.push(mk(ReclaimKind::Incremental, incremental, None));
        if stale {
            out.push(mk(ReclaimKind::StaleTarget, targets, None));
            out.push(mk(ReclaimKind::StaleDeps, deps, None));
        }
    }
    for s in input.shared {
        if s.usage.partial || s.users.is_empty() {
            continue;
        }
        // every user must be known and clean; the folder is idle only when all of them are
        let users: Option<Vec<i64>> = s.users.iter().map(|u| idle.get(u.as_path()).copied().flatten()).collect();
        let Some(users) = users else { continue };
        // other repositories may build there too: the folder's own last change counts
        let idle_days = users.iter().copied().min().unwrap_or(0).min(days(s.usage.touched));
        let whole = idle_days >= stale_after;
        let (kind, paths) = if whole { (ReclaimKind::StaleTarget, vec![(s.path.clone(), s.usage.total)]) } else { (ReclaimKind::Incremental, s.usage.incremental.clone()) };
        let mut paths: Vec<(PathBuf, u64)> = paths.into_iter().filter(|(p, b)| *b > 0 && contained(p, &s.path, whole)).collect();
        paths.sort();
        out.push(Suggestion { kind, owner: s.path.clone(), shared: true, users: s.users.clone(), branch: None, bytes: paths.iter().map(|(_, b)| b).sum(), paths, idle_days });
    }
    out.retain(|s| !s.paths.is_empty() && s.bytes > 0);
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.owner.cmp(&b.owner)));
    out
}

/// Bytes the whole plan would free.
pub fn reclaimable(plan: &[Suggestion]) -> u64 {
    plan.iter().map(|s| s.bytes).sum()
}

/// Where cargo puts this worktree's builds when that's outside it: `CARGO_TARGET_DIR`
/// (`env`, absolute only), else `build.target-dir` of the nearest `.cargo/config.toml` (or
/// `.cargo/config`) in the worktree or a folder above it, else `cargo_home`'s. `None` when the
/// worktree has no `Cargo.toml`, the folder doesn't exist, or it lies inside the worktree
/// (the walk finds those).
pub fn cargo_target_dir(worktree: &Path, env: Option<&str>, cargo_home: Option<&Path>) -> Option<PathBuf> {
    if !worktree.join("Cargo.toml").is_file() {
        return None;
    }
    let from_env = env.map(str::trim).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute());
    let from_config = || {
        let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
        for d in worktree.ancestors() {
            for f in [".cargo/config.toml", ".cargo/config"] {
                files.push((d.join(f), d.to_path_buf()));
            }
        }
        if let Some(h) = cargo_home {
            // relative paths in $CARGO_HOME/config.toml resolve against $CARGO_HOME's parent
            let base = h.parent().unwrap_or(h).to_path_buf();
            files.extend(["config.toml", "config"].map(|f| (h.join(f), base.clone())));
        }
        files.into_iter().find_map(|(f, base)| {
            let v = config_target_dir(&std::fs::read_to_string(f).ok()?)?;
            let p = PathBuf::from(v);
            Some(if p.is_absolute() { p } else { base.join(p) })
        })
    };
    let dir = from_env.or_else(from_config)?;
    let dir = normalize(&dir);
    (dir.is_dir() && !dir.starts_with(worktree)).then_some(dir)
}

/// `a/./b/../c` → `a/c`, lexically.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// `build.target-dir` from a cargo config file (`[build]` + `target-dir = "…"`, or a dotted
/// `build.target-dir = "…"` at the top). Enough TOML for that one key.
pub fn config_target_dir(text: &str) -> Option<String> {
    let mut section = String::new();
    for line in text.lines() {
        let line = line.split_once(" #").map(|(a, _)| a).unwrap_or(line).trim();
        if line.starts_with('[') {
            section = line.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let key = k.trim().trim_matches('"');
        let hit = (section == "build" && key == "target-dir") || (section.is_empty() && key == "build.target-dir");
        if hit {
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))?;
            return Some(v.to_string()).filter(|v| !v.is_empty());
        }
    }
    None
}

/// Why an item of the plan was left alone when it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    /// The worktree has changes now.
    Changed,
    /// Its HEAD is no longer on the default branch (new commits since the plan).
    NotMerged,
    /// Not a real folder inside the worktree / target folder, or not a build folder.
    NotArtifact,
    /// git tracks files in it.
    Tracked,
    Failed(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub freed: u64,
    pub done: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, Skip)>,
    pub branches_deleted: Vec<String>,
    pub branches_failed: Vec<(String, String)>,
}

/// Remove a build folder for good (no Trash: it's regenerated by building, and the Trash would
/// keep the space). Refuses a symlink, anything that isn't a directory, and anything that,
/// after resolving links, isn't strictly inside `owner` with a build-folder name — or, for a
/// shared cargo folder, `owner` itself carrying cargo's `CACHEDIR.TAG`.
pub fn delete_artifact(path: &Path, owner: &Path, shared: bool) -> std::result::Result<(), Skip> {
    let md = std::fs::symlink_metadata(path).map_err(|e| Skip::Failed(e.to_string()))?;
    if md.file_type().is_symlink() || !md.is_dir() {
        return Err(Skip::NotArtifact);
    }
    let real = std::fs::canonicalize(path).map_err(|e| Skip::Failed(e.to_string()))?;
    let root = std::fs::canonicalize(owner).map_err(|e| Skip::Failed(e.to_string()))?;
    let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ok = if real == root {
        shared && real.join("CACHEDIR.TAG").is_file()
    } else {
        let named = name == "incremental" || crate::worktree::is_artifact_dir(&real);
        let incremental_ok = name != "incremental" || real.ancestors().skip(1).any(|a| a == root || is_target(a));
        real.starts_with(&root) && named && incremental_ok
    };
    if !ok {
        return Err(Skip::NotArtifact);
    }
    std::fs::remove_dir_all(&real).map_err(|e| Skip::Failed(e.to_string()))
}

/// git tracks something under `path` in `worktree` (a committed `node_modules`, say).
fn tracked_under(worktree: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(worktree) else { return false };
    let rel = rel.to_string_lossy().replace('\\', "/");
    git::output(worktree, &["ls-files", "-z", "--", &rel]).map(|o| !o.is_empty()).unwrap_or(true)
}

fn clean_now(worktree: &Path) -> bool {
    Repo::status_at(worktree).map(|s| s.staged.is_empty() && s.unstaged.is_empty()).unwrap_or(false)
}

/// HEAD of `worktree` is reachable from `rev`.
fn head_merged(worktree: &Path, rev: &str) -> bool {
    let Ok(h) = git::output(worktree, &["rev-parse", "--verify", "-q", "HEAD"]) else { return false };
    let h = String::from_utf8_lossy(&h).trim().to_string();
    !h.is_empty() && git::output(worktree, &["merge-base", "--is-ancestor", &h, rev]).is_ok()
}

impl Repo {
    /// Carry out the chosen suggestions, one failure not stopping the rest. Before each one
    /// the worktree must still be clean (and, to be removed, still merged into `default_rev`);
    /// worktrees go through plain `git worktree remove` (git refuses changes itself), branches
    /// through `git branch -d` when `delete_branches`.
    pub fn reclaim(&self, items: &[Suggestion], default_rev: &str, delete_branches: bool, cancel: &Cancel, on_line: &mut dyn FnMut(&str, bool)) -> Result<Report> {
        let mut r = Report::default();
        for s in items {
            if cancel.is_cancelled() {
                break;
            }
            let users: Vec<&Path> = if s.shared { s.users.iter().map(PathBuf::as_path).collect() } else { vec![s.owner.as_path()] };
            if let Some(dirty) = users.iter().find(|u| u.is_dir() && !clean_now(u)) {
                r.skipped.push((dirty.to_path_buf(), Skip::Changed));
                continue;
            }
            if s.kind == ReclaimKind::MergedWorktree {
                if !head_merged(&s.owner, default_rev) {
                    r.skipped.push((s.owner.clone(), Skip::NotMerged));
                    continue;
                }
                let args = Repo::worktree_remove_args(&s.owner, false);
                match self.run_args(&args, cancel, on_line) {
                    Ok(run) if run.success => {
                        r.freed += s.bytes;
                        r.done.push(s.owner.clone());
                        if let Some(b) = s.branch.as_ref().filter(|_| delete_branches) {
                            match self.delete_branch(b) {
                                Ok(_) => r.branches_deleted.push(b.clone()),
                                Err(e) => r.branches_failed.push((b.clone(), e.to_string())),
                            }
                        }
                    }
                    Ok(run) => r.skipped.push((s.owner.clone(), Skip::Failed(run.output))),
                    Err(e) => r.skipped.push((s.owner.clone(), Skip::Failed(e.to_string()))),
                }
                continue;
            }
            for (p, bytes) in &s.paths {
                if !s.shared && tracked_under(&s.owner, p) {
                    r.skipped.push((p.clone(), Skip::Tracked));
                    continue;
                }
                match delete_artifact(p, &s.owner, s.shared) {
                    Ok(()) => {
                        r.freed += bytes;
                        r.done.push(p.clone());
                    }
                    Err(e) => r.skipped.push((p.clone(), e)),
                }
            }
        }
        Ok(r)
    }
}
