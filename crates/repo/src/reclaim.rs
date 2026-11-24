//! Disk space a repository's worktrees could give back: merged worktrees, cargo's incremental
//! caches, and the build folders of worktrees nobody touched in a while. [`plan`] is
//! deterministic (the app shows its exact paths, sizes, what each one is and what deleting it
//! costs before anything happens, with nothing selected); [`Repo::reclaim`] checks every rule
//! again right before each item and moves it to the Trash through the remover it is given
//! (never a permanent delete). Paths in a tool's folder, a cloud folder or the system are never
//! offered ([`crate::safety`]), and neither are cargo target folders outside the repository:
//! other repositories, and builds running right now, may use them.
use crate::git::{self, Cancel};
use crate::safety;
use crate::worktree::{DiskUsage, WorktreeEntry, is_artifact_dir};
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
    /// the folder goes to the Trash (with any ignored files in it), then git forgets it;
    /// optionally `git branch -d`.
    MergedWorktree,
    /// `target/**/incremental`: cargo's incremental caches, rebuilt on the next build.
    Incremental,
    /// The whole `target/` of a worktree untouched for the configured days.
    StaleTarget,
    /// Other build output (`node_modules/`, `.next/`, `dist/`, `build/`, `.gradle/`,
    /// `DerivedData/`) of a worktree untouched for the configured days.
    StaleDeps,
}

/// What a path offered is, and so what deleting it costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// A whole merged worktree.
    Worktree,
    /// `incremental/` inside a cargo `target/`.
    Incremental,
    /// cargo / maven `target/`.
    Target,
    /// `node_modules/`: reinstalled by the package manager.
    NodeModules,
    /// `.next/`: Next.js' build and cache.
    Next,
    /// `dist/` / `build/`: the project's build output.
    BuildOutput,
    /// `.gradle/`: Gradle's project cache.
    Gradle,
    /// Xcode's `DerivedData/`.
    DerivedData,
}

/// What `path` is, from its name (the plan only holds paths [`is_artifact_dir`] accepted).
pub fn category(kind: ReclaimKind, path: &Path) -> Category {
    if kind == ReclaimKind::MergedWorktree {
        return Category::Worktree;
    }
    match path.file_name().and_then(|n| n.to_str()).unwrap_or_default() {
        "incremental" => Category::Incremental,
        "target" => Category::Target,
        "node_modules" => Category::NodeModules,
        ".next" => Category::Next,
        ".gradle" => Category::Gradle,
        "DerivedData" => Category::DerivedData,
        _ => Category::BuildOutput,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub kind: ReclaimKind,
    /// The worktree the space is freed in.
    pub owner: PathBuf,
    /// Merged worktrees: their branch, deleted with `git branch -d` only if the user asks
    /// (never a protected one).
    pub branch: Option<String>,
    /// Exactly what goes, with sizes: the worktree folder, or folders inside `owner`. Sorted.
    pub paths: Vec<(PathBuf, u64)>,
    pub bytes: u64,
    /// Whole days since anything outside build folders changed or was committed.
    pub idle_days: i64,
}

/// A cargo target folder outside the repository that some worktrees build into. Shown for
/// information only: never in the plan.
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
    /// The worktree this window has open: never removed.
    pub current: Option<&'a Path>,
    /// Branch patterns never deleted (and their worktrees never removed).
    pub protected: &'a [String],
    /// The user's home folder ([`safety::user_home`]), for the protected places.
    pub home: Option<&'a Path>,
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
    !e.wt.main && e.wt.locked.is_none() && input.current != Some(e.wt.path.as_path()) && is_clean(e) && nothing_unpushed(e) && !e.wt.branch.as_deref().is_some_and(|b| crate::is_protected(b, input.protected)) && !safety::removal_protected(&e.wt.path, input.home)
}

/// `path` lies inside `root` (or is it, with `or_equal`), compared without touching the disk:
/// no `..` or `.` components on either side.
pub fn contained(path: &Path, root: &Path, or_equal: bool) -> bool {
    let plain = |p: &Path| p.is_absolute() && p.components().all(|c| !matches!(c, Component::ParentDir | Component::CurDir));
    plain(path) && plain(root) && path.starts_with(root) && (or_equal || path != root)
}

/// A build folder `path` of the worktree `owner` may be offered: strictly inside it, the
/// worktree isn't in a protected place, and nothing between them is a tool's folder or bundle,
/// nor is `path` a project itself.
pub fn may_offer(path: &Path, owner: &Path, home: Option<&Path>) -> bool {
    contained(path, owner, false) && !safety::place_protected(owner, home) && !safety::protected_below(path, owner)
}

fn is_target(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == "target")
}

/// What could be cleaned up, in a stable order (kind, then folder). Worktrees with
/// uncommitted changes, unreadable status or a partial size are never in it; a removed
/// worktree isn't offered anything else, and a `target/` offered whole doesn't also list its
/// incremental caches. Worktrees in a protected place ([`safety::place_protected`]) get
/// nothing, and no path below a worktree that runs through a tool's folder is offered.
pub fn plan(input: &PlanInput) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    let days = |t: i64| ((input.now - t).max(0)) / 86_400;
    let stale_after = input.stale_days as i64;
    for e in input.entries {
        let Some(u) = input.usage.get(&e.wt.path).filter(|u| !u.partial) else { continue };
        if !is_clean(e) || safety::place_protected(&e.wt.path, input.home) {
            continue;
        }
        let owner = e.wt.path.clone();
        let idle_days = days(last_touched(e, u));
        let mk = |kind, paths: Vec<(PathBuf, u64)>, branch: Option<String>| {
            let merged = kind == ReclaimKind::MergedWorktree;
            let mut paths: Vec<(PathBuf, u64)> = paths.into_iter().filter(|(p, b)| *b > 0 && if merged { *p == owner && contained(p, &owner, true) } else { may_offer(p, &owner, input.home) }).collect();
            paths.sort();
            Suggestion { kind, owner: owner.clone(), branch, bytes: paths.iter().map(|(_, b)| b).sum(), paths, idle_days }
        };
        if removable(e, input) {
            let branch = e.wt.branch.clone().filter(|b| !crate::is_protected(b, input.protected));
            out.push(mk(ReclaimKind::MergedWorktree, vec![(owner.clone(), u.total)], branch));
            continue;
        }
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
    /// Its branch has commits its upstream doesn't.
    Unpushed,
    /// The worktree is locked.
    Locked,
    /// Not a real folder inside the worktree, or not a build folder.
    NotArtifact,
    /// A place rama never touches (tool config or install, cloud folder, system…).
    Protected,
    /// git tracks files in it, or sees files there that aren't ignored.
    Tracked,
    /// git couldn't list the files it doesn't track there.
    Unreadable,
    Failed(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Bytes moved to the Trash (given back once it is emptied).
    pub freed: u64,
    pub done: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, Skip)>,
    /// Removed worktrees that held files git doesn't track (ignored `.env`, notes…): they are
    /// in the Trash with the folder.
    pub kept_in_trash: Vec<(PathBuf, Vec<String>)>,
    pub branches_deleted: Vec<String>,
    pub branches_failed: Vec<(String, String)>,
}

/// Moves a path to the Trash. The app passes [`system_trash`]; tests pass their own.
pub type Remover<'a> = &'a mut dyn FnMut(&Path) -> std::result::Result<(), String>;

/// The system Trash.
pub fn system_trash(p: &Path) -> std::result::Result<(), String> {
    trash::delete(p).map_err(|e| e.to_string())
}

/// Hand a build folder to `remove` (the Trash) after checking it again: a real directory, not a
/// symlink and nothing linked on the way, strictly inside `owner` after resolving links, of a
/// build-folder kind ([`is_artifact_dir`], or `incremental/` inside such a `target/`), not in
/// a protected place, and (in a repository) entirely ignored by git.
pub fn delete_artifact(path: &Path, owner: &Path, home: Option<&Path>, remove: Remover) -> std::result::Result<(), Skip> {
    let md = std::fs::symlink_metadata(path).map_err(|e| Skip::Failed(e.to_string()))?;
    if md.file_type().is_symlink() || !md.is_dir() {
        return Err(Skip::NotArtifact);
    }
    let real = safety::canonical(path).map_err(|e| Skip::Failed(e.to_string()))?;
    let root = safety::canonical(owner).map_err(|e| Skip::Failed(e.to_string()))?;
    // nothing between the worktree and the folder may be a link
    let same_rel = matches!((real.strip_prefix(&root), path.strip_prefix(owner)), (Ok(a), Ok(b)) if a == b);
    if !same_rel || real == root {
        return Err(Skip::NotArtifact);
    }
    // the path as planned and as it resolves must both pass
    if !may_offer(path, owner, home) || !may_offer(&real, &root, home) {
        return Err(Skip::Protected);
    }
    let incremental = real.file_name().is_some_and(|n| n == "incremental") && real.ancestors().skip(1).take_while(|a| *a != root).any(|a| is_target(a) && is_artifact_dir(a));
    if !(incremental || is_artifact_dir(&real)) {
        return Err(Skip::NotArtifact);
    }
    if git_ignores_all(owner, path) != Some(true) {
        return Err(Skip::Tracked);
    }
    remove(&real).map_err(Skip::Failed)
}

/// git's view of `path` in the worktree at `worktree`: `true` when nothing under it is tracked
/// and nothing there is untracked-but-not-ignored. `None` when git can't tell.
pub fn git_ignores_all(worktree: &Path, path: &Path) -> Option<bool> {
    let rel = path.strip_prefix(worktree).ok()?.to_string_lossy().replace('\\', "/");
    let tracked = git::output(worktree, &["ls-files", "-z", "--", &rel]).ok()?;
    let status = git::output(worktree, &["--no-optional-locks", "status", "--porcelain", "-z", "--untracked-files=all", "--", &rel]).ok()?;
    Some(tracked.is_empty() && status.is_empty())
}

/// Files in `worktree` git doesn't track, other than regenerable build output: ignored ones
/// (`.env`, local configs, notes) and untracked ones, as git lists them (relative, `/`, a
/// folder whose contents are all ignored ends in `/`). Removing a worktree with plain
/// `git worktree remove` would delete these for good; rama moves the folder to the Trash
/// instead and shows them first. `None` when git can't tell.
pub fn untracked_keepers(worktree: &Path) -> Option<Vec<String>> {
    let raw = git::output(worktree, &["--no-optional-locks", "status", "--porcelain", "-z", "--ignored=traditional", "--untracked-files=normal"]).ok()?;
    let mut out = Vec::new();
    let mut fields = raw.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(f) = fields.next() {
        let line = String::from_utf8_lossy(f);
        let (xy, rel) = (line.get(..2).unwrap_or_default(), line.get(3..).unwrap_or_default());
        if xy.starts_with('R') || xy.starts_with('C') {
            fields.next(); // the rename's source
        }
        if xy != "!!" && xy != "??" {
            continue;
        }
        let abs = worktree.join(rel.trim_end_matches('/'));
        let name = abs.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name == ".DS_Store" || abs.is_dir() && !abs.is_symlink() && is_artifact_dir(&abs) {
            continue;
        }
        out.push(rel.to_string());
    }
    out.sort();
    Some(out)
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

/// The branch has commits its (existing) upstream lacks.
fn unpushed_now(worktree: &Path) -> bool {
    if git::output(worktree, &["rev-parse", "--verify", "-q", "@{upstream}"]).is_err() {
        return false; // no upstream, or it is gone: merged into the default branch is enough
    }
    match git::output(worktree, &["rev-list", "--count", "@{upstream}..HEAD"]) {
        Ok(n) => String::from_utf8_lossy(&n).trim() != "0",
        Err(_) => true,
    }
}

fn git_path(dir: &Path, args: &[&str]) -> Option<PathBuf> {
    let o = git::output(dir, args).ok()?;
    let s = String::from_utf8_lossy(&o).trim().to_string();
    let p = PathBuf::from(&s);
    let p = if p.is_absolute() { p } else { dir.join(p) };
    safety::canonical(&p).ok()
}

impl Repo {
    /// Carry out the chosen suggestions, one failure not stopping the rest. Before each one
    /// the worktree must still be clean (and, to be removed: still merged into `default_rev`,
    /// pushed, unlocked, a linked worktree of this repository). Everything goes through
    /// `remove` (the Trash): build folders, and merged worktrees as whole folders, with any
    /// ignored files in them ([`untracked_keepers`], listed in the report); git then forgets
    /// the worktree (`git worktree remove` of the now missing folder). Branches go through
    /// `git branch -d` when `delete_branches`.
    #[allow(clippy::too_many_arguments)]
    pub fn reclaim(&self, items: &[Suggestion], default_rev: &str, delete_branches: bool, home: Option<&Path>, remove: Remover, cancel: &Cancel, on_line: &mut dyn FnMut(&str, bool)) -> Result<Report> {
        let mut r = Report::default();
        let common = git_path(&self.cwd(), &["rev-parse", "--git-common-dir"]);
        for s in items {
            if cancel.is_cancelled() {
                break;
            }
            if s.owner.is_dir() && !clean_now(&s.owner) {
                r.skipped.push((s.owner.clone(), Skip::Changed));
                continue;
            }
            if s.kind != ReclaimKind::MergedWorktree {
                for (p, bytes) in &s.paths {
                    match delete_artifact(p, &s.owner, home, &mut *remove) {
                        Ok(()) => {
                            r.freed += bytes;
                            r.done.push(p.clone());
                        }
                        Err(e) => r.skipped.push((p.clone(), e)),
                    }
                }
                continue;
            }
            match self.remove_merged(s, default_rev, home, &common, &mut *remove, cancel, on_line) {
                Err(skip) => r.skipped.push((s.owner.clone(), skip)),
                Ok(keepers) => {
                    r.freed += s.bytes;
                    r.done.push(s.owner.clone());
                    if !keepers.is_empty() {
                        r.kept_in_trash.push((s.owner.clone(), keepers));
                    }
                    if let Some(b) = s.branch.as_ref().filter(|_| delete_branches) {
                        match self.delete_branch(b) {
                            Ok(_) => r.branches_deleted.push(b.clone()),
                            Err(e) => r.branches_failed.push((b.clone(), e.to_string())),
                        }
                    }
                }
            }
        }
        Ok(r)
    }

    #[allow(clippy::too_many_arguments)]
    fn remove_merged(&self, s: &Suggestion, default_rev: &str, home: Option<&Path>, common: &Option<PathBuf>, remove: Remover, cancel: &Cancel, on_line: &mut dyn FnMut(&str, bool)) -> std::result::Result<Vec<String>, Skip> {
        let wt = &s.owner;
        let md = std::fs::symlink_metadata(wt).map_err(|e| Skip::Failed(e.to_string()))?;
        if md.file_type().is_symlink() || !md.is_dir() || s.paths.iter().any(|(p, _)| p != wt) {
            return Err(Skip::NotArtifact);
        }
        if safety::removal_protected(wt, home) || !safety::canonical(wt).is_ok_and(|c| !safety::removal_protected(&c, home)) {
            return Err(Skip::Protected);
        }
        // a linked worktree of this repository: its own git dir under the shared one
        let gitdir = git_path(wt, &["rev-parse", "--absolute-git-dir"]).ok_or(Skip::NotArtifact)?;
        let own = git_path(wt, &["rev-parse", "--git-common-dir"]);
        if own.is_none() || own != *common || Some(&gitdir) == own.as_ref() {
            return Err(Skip::NotArtifact);
        }
        if gitdir.join("locked").exists() {
            return Err(Skip::Locked);
        }
        if !head_merged(wt, default_rev) {
            return Err(Skip::NotMerged);
        }
        if unpushed_now(wt) {
            return Err(Skip::Unpushed);
        }
        let keepers = untracked_keepers(wt).ok_or(Skip::Unreadable)?;
        remove(wt).map_err(Skip::Failed)?;
        // the folder is in the Trash: git forgets just this worktree (a plain prune would also
        // forget others whose disk is merely unplugged)
        let args = Repo::worktree_remove_args(wt, false);
        let forgot = self.run_args(&args, cancel, on_line).map(|r| r.success).unwrap_or(false);
        if !forgot && self.worktree_prune_preview().is_ok_and(|l| l.iter().all(|l| gitdir.file_name().is_some_and(|n| l.contains(&*n.to_string_lossy())))) {
            let _ = self.run_args(&["worktree".to_string(), "prune".to_string()], cancel, on_line);
        }
        Ok(keepers)
    }
}
