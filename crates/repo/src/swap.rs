//! Changing the branch of any checkout and swapping the branches of two checkouts (usually the
//! main checkout and a worktree), with uncommitted changes travelling along in tagged stashes.
//!
//! Every write is the `git` CLI (hooks run as in a terminal). Stashes are pushed with a unique
//! message (`rama-swap:<id>:<branch>`), remembered by SHA, applied by SHA and dropped only after
//! a clean apply, after finding their index again by SHA and tag: the stash stack is shared by
//! every worktree and every other tool, so positions can't be trusted. A failed step puts
//! everything back; a stash that doesn't apply cleanly is kept and reported, never dropped.
use crate::git::{self as cmd, Cancel};
use crate::work::{FileState, parse_stash_list, parse_status, split_remote_name};
use crate::worktree::parse_worktree_list;
use crate::{Oid, Result};
use std::path::{Path, PathBuf};

pub const TAG_PREFIX: &str = "rama-swap"; // i18n-ok

/// Why a change of branch or a swap isn't started. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    /// The folder is gone or isn't a working tree.
    Missing(PathBuf),
    /// The checkout is on a detached HEAD, so there is no branch to hand over.
    Detached(PathBuf),
    /// A rebase, merge, cherry-pick, revert or bisect is in progress there.
    InProgress { path: PathBuf, what: &'static str },
    /// Files with unresolved conflicts.
    Conflicts(PathBuf),
    /// A submodule with its own changes or commits: a stash would not carry them.
    Submodule { path: PathBuf, submodule: String },
    /// The repository uses Git LFS but `git lfs` isn't installed: checkouts would leave pointer
    /// files instead of content.
    LfsMissing(PathBuf),
    /// git checks a branch out in one worktree at a time.
    Elsewhere { branch: String, worktree: PathBuf },
    /// One of the two checkouts is swapped with a third one; swap that back first.
    AlreadySwapped { path: PathBuf, with: PathBuf },
    /// Both sides are the same checkout, or the branch is already checked out here.
    Nothing,
}

/// A stash left in place because it couldn't be applied (or put back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptStash {
    pub sha: String,
    pub tag: String,
    /// The checkout the changes were meant for.
    pub path: PathBuf,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapError {
    Blocked(Blocker),
    /// A step failed; every earlier step was undone and the changes are back where they were.
    RolledBack { step: String, output: String },
    /// A step failed and undoing it failed too. `kept` stashes still hold changes.
    RollbackFailed { step: String, output: String, rollback: Vec<String>, kept: Vec<KeptStash> },
    /// The branches moved, but changes didn't apply cleanly; their stashes are kept. `output`
    /// is git's message (conflicted files are left marked in the checkout).
    Conflict { kept: Vec<KeptStash>, output: String },
}

impl std::fmt::Display for SwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwapError::Blocked(b) => write!(f, "{b:?}"), // i18n-ok
            SwapError::RolledBack { step, output } => write!(f, "{step}: {}", output.trim()), // i18n-ok
            SwapError::RollbackFailed { step, output, rollback, .. } => write!(f, "{step}: {} (undo: {})", output.trim(), rollback.join("; ")), // i18n-ok
            SwapError::Conflict { output, .. } => f.write_str(output.trim()),
        }
    }
}

impl std::error::Error for SwapError {}

/// Where a change of branch goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Local(String),
    /// A remote-tracking branch (`origin/dev`), checked out as a new local branch tracking it,
    /// or as the local branch of that name when one exists.
    Remote(String),
}

/// What the confirmation shows before anything runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapPreview {
    pub a: PathBuf,
    pub b: PathBuf,
    pub a_branch: String,
    pub b_branch: String,
    /// Changed files (tracked changes plus untracked entries) on each side.
    pub a_changed: usize,
    pub b_changed: usize,
    /// This swap undoes a recorded one.
    pub back: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangePreview {
    pub path: PathBuf,
    pub from: Option<String>,
    /// The local branch it ends up on.
    pub to: String,
    pub changed: usize,
}

/// A finished swap or change of branch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Moved {
    /// Changed files carried along, per side (a, b).
    pub carried: (usize, usize),
    /// A hook (post-checkout) that failed after git had switched; the switch itself happened.
    pub hook_warnings: Vec<String>,
}

/// A swap that is in effect: `a` had `a_branch` and `b` had `b_branch` before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapRecord {
    pub id: String,
    pub a: PathBuf,
    pub b: PathBuf,
    pub a_branch: String,
    pub b_branch: String,
    /// Unix seconds.
    pub at: i64,
}

impl SwapRecord {
    pub fn involves(&self, p: &Path) -> bool {
        same(&self.a, p) || same(&self.b, p)
    }

    /// The other checkout of the pair.
    pub fn partner(&self, p: &Path) -> Option<&Path> {
        if same(&self.a, p) {
            Some(&self.b)
        } else if same(&self.b, p) {
            Some(&self.a)
        } else {
            None
        }
    }

    /// The checkouts still hold each other's branch.
    pub fn holds(&self, branch_of: &dyn Fn(&Path) -> Option<String>) -> bool {
        branch_of(&self.a).as_deref() == Some(self.b_branch.as_str()) && branch_of(&self.b).as_deref() == Some(self.a_branch.as_str())
    }
}

/// The swaps in effect, kept in the app's own state file so they survive a restart: one swap
/// per line, `id`, `a`, `b`, `a_branch`, `b_branch`, `at` separated by tabs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    pub swaps: Vec<SwapRecord>,
}

impl Ledger {
    pub fn load(file: &Path) -> Ledger {
        Ledger::parse(&std::fs::read_to_string(file).unwrap_or_default())
    }

    pub fn parse(text: &str) -> Ledger {
        let swaps = text
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                let [id, a, b, ab, bb, at] = f.as_slice() else { return None };
                Some(SwapRecord { id: id.to_string(), a: PathBuf::from(a), b: PathBuf::from(b), a_branch: ab.to_string(), b_branch: bb.to_string(), at: at.parse().unwrap_or(0) })
            })
            .collect();
        Ledger { swaps }
    }

    pub fn to_text(&self) -> String {
        self.swaps.iter().map(|r| format!("{}\t{}\t{}\t{}\t{}\t{}\n", r.id, r.a.display(), r.b.display(), r.a_branch, r.b_branch, r.at)).collect()
    }

    pub fn save(&self, file: &Path) -> Result<()> {
        if let Some(d) = file.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, self.to_text())?;
        std::fs::rename(tmp, file)?;
        Ok(())
    }

    pub fn find(&self, p: &Path) -> Option<&SwapRecord> {
        self.swaps.iter().find(|r| r.involves(p))
    }

    pub fn pair(&self, a: &Path, b: &Path) -> Option<&SwapRecord> {
        self.swaps.iter().find(|r| (same(&r.a, a) && same(&r.b, b)) || (same(&r.a, b) && same(&r.b, a)))
    }

    /// Forget swaps the checkouts no longer reflect (switched by hand, folder removed).
    pub fn retain_live(&mut self, branch_of: &dyn Fn(&Path) -> Option<String>) -> bool {
        let before = self.swaps.len();
        self.swaps.retain(|r| r.holds(branch_of));
        before != self.swaps.len()
    }

    /// Record a finished swap of `a` (was on `a_was`) and `b` (was on `b_was`): a swap back
    /// removes the record, anything else adds one.
    pub fn after_swap(&mut self, id: &str, a: &Path, b: &Path, a_was: &str, b_was: &str, at: i64) {
        let back = |r: &SwapRecord| (same(&r.a, a) && same(&r.b, b) && r.a_branch == b_was && r.b_branch == a_was) || (same(&r.a, b) && same(&r.b, a) && r.a_branch == a_was && r.b_branch == b_was);
        if let Some(i) = self.swaps.iter().position(back) {
            self.swaps.remove(i);
        } else {
            self.swaps.retain(|r| !r.involves(a) && !r.involves(b));
            self.swaps.push(SwapRecord { id: id.to_string(), a: a.to_path_buf(), b: b.to_path_buf(), a_branch: a_was.to_string(), b_branch: b_was.to_string(), at });
        }
    }
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn same(a: &Path, b: &Path) -> bool {
    a == b || canon(a) == canon(b)
}

/// A fresh id for stash tags and records.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}{:x}{:x}", nanos & 0xffff_ffff_ffff, std::process::id() & 0xffff, SEQ.fetch_add(1, Ordering::Relaxed))
}

pub fn tag(id: &str, branch: &str) -> String {
    format!("{TAG_PREFIX}:{id}:{branch}") // i18n-ok
}

fn text(v: Vec<u8>) -> String {
    String::from_utf8_lossy(&v).trim().to_string()
}

/// The checked-out branch, `None` when detached.
pub fn branch_at(dir: &Path) -> Option<String> {
    cmd::output(dir, &["symbolic-ref", "-q", "--short", "HEAD"]).ok().map(text).filter(|s| !s.is_empty())
}

fn head_at(dir: &Path) -> Option<String> {
    cmd::output(dir, &["rev-parse", "-q", "--verify", "HEAD"]).ok().map(text).filter(|s| !s.is_empty())
}

fn worktree_paths(dir: &Path) -> Vec<PathBuf> {
    cmd::output(dir, &["worktree", "list", "--porcelain", "-z"]).map(|raw| parse_worktree_list(&raw).into_iter().map(|w| w.path).collect()).unwrap_or_default()
}

/// Changed files in a checkout: tracked changes plus untracked entries, leaving out the
/// worktrees nested inside it (git lists them as untracked folders; a stash leaves them alone).
pub fn changed_at(dir: &Path) -> Result<usize> {
    let raw = cmd::output(dir, &["--no-optional-locks", "status", "--porcelain=v2", "-z", "--untracked-files=normal"])?;
    let st = parse_status(&raw);
    let others: Vec<PathBuf> = worktree_paths(dir).into_iter().map(|p| canon(&p)).filter(|p| *p != canon(dir)).collect();
    let root = canon(dir);
    let mut paths: Vec<&str> = st
        .staged
        .iter()
        .chain(st.unstaged.iter())
        .filter(|f| f.state != FileState::Untracked || !others.iter().any(|o| root.join(f.path.trim_end_matches('/')).starts_with(o) || o.starts_with(root.join(f.path.trim_end_matches('/')))))
        .map(|f| f.path.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    Ok(paths.len())
}

/// Refuse checkouts where a stash or a switch could lose work or leave a mess.
pub fn preflight(dir: &Path) -> std::result::Result<(), Blocker> {
    if !dir.is_dir() || cmd::output(dir, &["rev-parse", "--is-inside-work-tree"]).map(text).ok().as_deref() != Some("true") {
        return Err(Blocker::Missing(dir.to_path_buf()));
    }
    let git_path = |name: &str| cmd::output(dir, &["rev-parse", "--git-path", name]).ok().map(text).map(|p| dir.join(p));
    for (name, what) in [("rebase-merge", "rebase"), ("rebase-apply", "rebase"), ("MERGE_HEAD", "merge"), ("CHERRY_PICK_HEAD", "cherry-pick"), ("REVERT_HEAD", "revert"), ("BISECT_LOG", "bisect")] {
        if git_path(name).is_some_and(|p| p.exists()) {
            return Err(Blocker::InProgress { path: dir.to_path_buf(), what });
        }
    }
    let raw = cmd::output(dir, &["--no-optional-locks", "status", "--porcelain=v2", "-z", "--ignore-submodules=none", "--untracked-files=no"]).map_err(|_| Blocker::Missing(dir.to_path_buf()))?;
    let s = String::from_utf8_lossy(&raw);
    let mut recs = s.split('\0');
    while let Some(rec) = recs.next() {
        let mut f = rec.split(' ');
        let kind = f.next().unwrap_or_default();
        if !matches!(kind, "1" | "2" | "u") {
            continue;
        }
        let _xy = f.next();
        let sub = f.next().unwrap_or_default();
        let fields = match kind {
            "1" => 9,
            "2" => 10,
            _ => 11,
        };
        let path = rec.splitn(fields, ' ').nth(fields - 1).unwrap_or_default().to_string();
        if kind == "2" {
            recs.next(); // the rename's original path
        }
        if kind == "u" {
            return Err(Blocker::Conflicts(dir.to_path_buf()));
        }
        if sub.starts_with('S') && sub != "S..." {
            return Err(Blocker::Submodule { path: dir.to_path_buf(), submodule: path });
        }
    }
    let uses_lfs = std::fs::read_to_string(dir.join(".gitattributes")).is_ok_and(|a| a.contains("filter=lfs"));
    if uses_lfs && cmd::output(dir, &["lfs", "version"]).is_err() {
        return Err(Blocker::LfsMissing(dir.to_path_buf()));
    }
    Ok(())
}

/// The output of the steps, one line each (`$ git -C <dir> …` then git's own lines).
type Log<'a> = &'a mut dyn FnMut(&str);

fn run(dir: &Path, args: &[&str], log: Log) -> (bool, String, Option<String>) {
    log(&format!("$ git -C {} {}", dir.display(), args.join(" ")));
    match cmd::run(dir, args, None, &Cancel::default(), &mut |l, _| log(l)) {
        Ok(r) => (r.success, r.output, r.failed_hook),
        Err(e) => (false, e.to_string(), None),
    }
}

/// `git switch …`, judged by where HEAD ended up: a failing post-checkout hook makes git exit
/// non-zero after the switch already happened. `Ok(Some(hook))` reports that hook.
fn switch(dir: &Path, args: &[&str], expect: Option<&str>, log: Log) -> std::result::Result<Option<String>, String> {
    let (ok, out, hook) = run(dir, args, log);
    let now = branch_at(dir);
    let arrived = match expect {
        Some(b) => now.as_deref() == Some(b),
        None => now.is_none(),
    };
    match (ok, arrived) {
        (true, true) => Ok(None),
        (false, true) => Ok(Some(hook.unwrap_or_else(|| String::from("post-checkout")))),
        _ => Err(if out.trim().is_empty() { String::from("git switch failed") } else { out }), // i18n-ok
    }
}

/// Stash everything (untracked files too, ignored ones stay) under `tag`; its SHA, found by tag.
fn stash_push(dir: &Path, tag: &str, log: Log) -> std::result::Result<String, String> {
    let (ok, out, _) = run(dir, &["stash", "push", "--include-untracked", "-m", tag], log);
    if !ok {
        return Err(out);
    }
    find_stash(dir, None, tag).map(|(_, sha)| sha).ok_or_else(|| format!("stash {tag} not found after git stash push: {out}")) // i18n-ok
}

/// (index, sha) of the stash with this tag (and SHA, when given).
fn find_stash(dir: &Path, sha: Option<&str>, tag: &str) -> Option<(usize, String)> {
    let want = match sha {
        Some(h) => Some(parse_oid(h)?),
        None => None,
    };
    let raw = cmd::output(dir, &["stash", "list", "-z", "--format=%H%x1f%gs"]).ok()?;
    parse_stash_list(&raw).into_iter().find(|s| want.is_none_or(|w| s.id == w) && (s.message == tag || s.message.ends_with(&format!(": {tag}")))).map(|s| (s.index, s.id.to_string()))
}

fn parse_oid(hex: &str) -> Option<Oid> {
    Oid::from_hex(hex.as_bytes()).ok()
}

/// Apply by SHA. With `index` the staged part is restored as staged; when git can't do that
/// but left the tree untouched, the plain apply is tried.
fn stash_apply(dir: &Path, sha: &str, index: bool, log: Log) -> std::result::Result<(), String> {
    if index {
        let (ok, out, _) = run(dir, &["stash", "apply", "--index", sha], log);
        if ok {
            return Ok(());
        }
        if changed_at(dir).unwrap_or(1) != 0 {
            return Err(out);
        }
    }
    let (ok, out, _) = run(dir, &["stash", "apply", sha], log);
    if ok { Ok(()) } else { Err(out) }
}

/// Drop the stash once applied: its index is looked up again by SHA and tag right before.
fn stash_drop(dir: &Path, sha: &str, tag: &str, log: Log) -> std::result::Result<(), String> {
    let Some((i, _)) = find_stash(dir, Some(sha), tag) else { return Ok(()) };
    let (ok, out, _) = run(dir, &["stash", "drop", "-q", &format!("stash@{{{i}}}")], log);
    if ok { Ok(()) } else { Err(out) }
}

/// Apply then drop; on a failed apply the stash stays and is returned.
fn carry(dir: &Path, st: &Stashed, index: bool, log: Log) -> std::result::Result<(), (KeptStash, String)> {
    match stash_apply(dir, &st.sha, index, log) {
        Ok(()) => {
            // applied: a failed drop only leaves a duplicate behind, never loses anything
            if let Err(e) = stash_drop(dir, &st.sha, &st.tag, log) {
                log(&format!("(the stash {} was applied but not dropped: {})", st.tag, e.trim())); // i18n-ok
            }
            Ok(())
        }
        Err(out) => Err((st.kept(dir), out)),
    }
}

struct Stashed {
    sha: String,
    tag: String,
    branch: String,
}

impl Stashed {
    fn kept(&self, path: &Path) -> KeptStash {
        KeptStash { sha: self.sha.clone(), tag: self.tag.clone(), path: path.to_path_buf(), branch: self.branch.clone() }
    }
}

fn stash_if_dirty(dir: &Path, changed: usize, id: &str, branch: &str, log: Log) -> std::result::Result<Option<Stashed>, String> {
    if changed == 0 {
        return Ok(None);
    }
    let tag = tag(id, branch);
    stash_push(dir, &tag, log).map(|sha| Some(Stashed { sha, tag, branch: branch.to_string() }))
}

fn local_name(dir: &Path, target: &Target) -> String {
    match target {
        Target::Local(b) => b.clone(),
        Target::Remote(r) => {
            let remotes: Vec<String> = cmd::output(dir, &["remote"]).map(|v| String::from_utf8_lossy(&v).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default();
            split_remote_name(&remotes, r).map(|(_, b)| b).unwrap_or_else(|| r.split_once('/').map(|(_, b)| b.to_string()).unwrap_or_else(|| r.clone()))
        }
    }
}

fn checked_out_elsewhere(dir: &Path, branch: &str) -> Option<PathBuf> {
    let raw = cmd::output(dir, &["worktree", "list", "--porcelain", "-z"]).ok()?;
    parse_worktree_list(&raw).into_iter().find(|w| w.branch.as_deref() == Some(branch) && !same(&w.path, dir)).map(|w| w.path)
}

/// What changing `dir` to `target` would do, or why it can't.
pub fn preview_change(dir: &Path, target: &Target) -> std::result::Result<ChangePreview, Blocker> {
    preflight(dir)?;
    let to = local_name(dir, target);
    let from = branch_at(dir);
    if from.as_deref() == Some(to.as_str()) {
        return Err(Blocker::Nothing);
    }
    if let Some(w) = checked_out_elsewhere(dir, &to) {
        return Err(Blocker::Elsewhere { branch: to, worktree: w });
    }
    let changed = changed_at(dir).map_err(|_| Blocker::Missing(dir.to_path_buf()))?;
    Ok(ChangePreview { path: dir.to_path_buf(), from, to, changed })
}

/// Check out another branch in `dir`, carrying its uncommitted changes along: stash (tagged),
/// switch, apply by SHA, drop. A failed switch puts the changes back on the old branch.
pub fn change_branch(dir: &Path, target: &Target, id: &str, log: Log) -> std::result::Result<Moved, SwapError> {
    let p = preview_change(dir, target).map_err(SwapError::Blocked)?;
    let from = p.from.clone().or_else(|| head_at(dir)).unwrap_or_default();
    let stash = stash_if_dirty(dir, p.changed, id, &from, log).map_err(|output| SwapError::RolledBack { step: String::from("git stash push"), output })?;
    let local_exists = cmd::output(dir, &["rev-parse", "-q", "--verify", &format!("refs/heads/{}", p.to)]).is_ok();
    let args: Vec<&str> = match target {
        Target::Remote(r) if !local_exists => vec!["switch", "--track", r.as_str()],
        _ => vec!["switch", "--no-guess", p.to.as_str()],
    };
    let mut moved = Moved::default();
    match switch(dir, &args, Some(&p.to), log) {
        Ok(hook) => moved.hook_warnings.extend(hook),
        Err(output) => {
            let step = format!("git {}", args.join(" "));
            return Err(match stash {
                None => SwapError::RolledBack { step, output },
                Some(st) => match carry(dir, &st, true, log) {
                    Ok(()) => SwapError::RolledBack { step, output },
                    Err((kept, e)) => SwapError::RollbackFailed { step, output, rollback: vec![e], kept: vec![kept] },
                },
            });
        }
    }
    if let Some(st) = stash {
        if let Err((kept, output)) = carry(dir, &st, true, log) {
            return Err(SwapError::Conflict { kept: vec![kept], output });
        }
        moved.carried.0 = p.changed;
    }
    Ok(moved)
}

/// What swapping `a` and `b` would do, or why it can't. `ledger` refuses a checkout that is
/// swapped with a third one.
pub fn preview_swap(a: &Path, b: &Path, ledger: &Ledger) -> std::result::Result<SwapPreview, Blocker> {
    if same(a, b) {
        return Err(Blocker::Nothing);
    }
    preflight(a)?;
    preflight(b)?;
    let a_branch = branch_at(a).ok_or_else(|| Blocker::Detached(a.to_path_buf()))?;
    let b_branch = branch_at(b).ok_or_else(|| Blocker::Detached(b.to_path_buf()))?;
    if !worktree_paths(a).iter().any(|w| same(w, b)) {
        return Err(Blocker::Missing(b.to_path_buf()));
    }
    for (p, other) in [(a, b), (b, a)] {
        if let Some(r) = ledger.find(p)
            && let Some(with) = r.partner(p)
            && !same(with, other)
        {
            return Err(Blocker::AlreadySwapped { path: p.to_path_buf(), with: with.to_path_buf() });
        }
    }
    let back = ledger.pair(a, b).is_some_and(|r| r.holds(&|p| if same(p, a) { Some(a_branch.clone()) } else if same(p, b) { Some(b_branch.clone()) } else { None }));
    let a_changed = changed_at(a).map_err(|_| Blocker::Missing(a.to_path_buf()))?;
    let b_changed = changed_at(b).map_err(|_| Blocker::Missing(b.to_path_buf()))?;
    Ok(SwapPreview { a: a.to_path_buf(), b: b.to_path_buf(), a_branch, b_branch, a_changed, b_changed, back })
}

/// Swap the branches of `a` and `b`, each branch taking its uncommitted changes along:
/// stash both (tagged), detach `b`, switch `a` to `b`'s branch, switch `b` to `a`'s old one,
/// apply each stash where its branch now is (by SHA), drop it. Any failed step before the
/// applies undoes the earlier ones in reverse and puts the changes back.
pub fn swap(a: &Path, b: &Path, id: &str, ledger: &Ledger, log: Log) -> std::result::Result<(SwapPreview, Moved), SwapError> {
    let p = preview_swap(a, b, ledger).map_err(SwapError::Blocked)?;
    let (ab, bb) = (p.a_branch.as_str(), p.b_branch.as_str());
    let sa = stash_if_dirty(a, p.a_changed, id, ab, log).map_err(|output| SwapError::RolledBack { step: String::from("git stash push"), output })?;
    let sb = match stash_if_dirty(b, p.b_changed, id, bb, log) {
        Ok(s) => s,
        Err(output) => return Err(undo(Progress::default(), a, b, ab, bb, sa.as_ref(), None, String::from("git stash push"), output, log)),
    };
    let mut moved = Moved::default();
    let mut progress = Progress::default();
    let steps: [(&Path, Vec<&str>, Option<&str>); 3] = [(b, vec!["switch", "--detach"], None), (a, vec!["switch", "--no-guess", bb], Some(bb)), (b, vec!["switch", "--no-guess", ab], Some(ab))];
    for (i, (dir, args, expect)) in steps.iter().enumerate() {
        match switch(dir, args, *expect, log) {
            Ok(hook) => moved.hook_warnings.extend(hook),
            Err(output) => return Err(undo(progress, a, b, ab, bb, sa.as_ref(), sb.as_ref(), format!("git -C {} {}", dir.display(), args.join(" ")), output, log)),
        }
        match i {
            0 => progress.b_detached = true,
            1 => progress.a_switched = true,
            _ => {}
        }
    }
    // each branch's changes go where the branch went, onto the same commit they came from
    let mut kept = Vec::new();
    let mut outputs = Vec::new();
    for (st, dest, n) in [(sa.as_ref(), b, p.a_changed), (sb.as_ref(), a, p.b_changed)] {
        let Some(st) = st else { continue };
        if !kept.is_empty() {
            kept.push(st.kept(dest));
            continue;
        }
        match carry(dest, st, true, log) {
            Ok(()) => {
                if same(dest, b) {
                    moved.carried.0 = n;
                } else {
                    moved.carried.1 = n;
                }
            }
            Err((k, out)) => {
                kept.push(k);
                outputs.push(out);
            }
        }
    }
    if !kept.is_empty() {
        return Err(SwapError::Conflict { kept, output: outputs.join("\n") });
    }
    Ok((p, moved))
}

#[derive(Default, Clone, Copy)]
struct Progress {
    b_detached: bool,
    a_switched: bool,
}

#[allow(clippy::too_many_arguments)]
fn undo(progress: Progress, a: &Path, b: &Path, ab: &str, bb: &str, sa: Option<&Stashed>, sb: Option<&Stashed>, step: String, output: String, log: Log) -> SwapError {
    log("(undoing the earlier steps)"); // i18n-ok
    let mut errors = Vec::new();
    if progress.a_switched
        && let Err(e) = switch(a, &["switch", "--no-guess", ab], Some(ab), log)
    {
        errors.push(e);
    }
    if progress.b_detached
        && errors.is_empty()
        && let Err(e) = switch(b, &["switch", "--no-guess", bb], Some(bb), log)
    {
        errors.push(e);
    }
    let mut kept = Vec::new();
    for (st, dir) in [(sa, a), (sb, b)] {
        let Some(st) = st else { continue };
        if !errors.is_empty() {
            kept.push(st.kept(dir));
            continue;
        }
        if let Err((k, e)) = carry(dir, st, true, log) {
            kept.push(k);
            errors.push(e);
        }
    }
    if errors.is_empty() { SwapError::RolledBack { step, output } } else { SwapError::RollbackFailed { step, output, rollback: errors, kept } }
}
