//! Finding local branches that are safe to clean up: merged into the default branch, upstream
//! gone, or squash- / rebase-merged (every patch, or the branch's combined patch, already on
//! the default branch, by `git patch-id`). Read-only; deleting goes through `work.rs`.
use crate::git::{self, Cancel};
use crate::worktree::{BaseRef, parallel};
use crate::{Oid, Repo, Result};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// Reachable from this ref of the default branch (`origin/main` or `main`).
    Merged(String),
    /// Its commits' changes are on the default branch under other hashes. `confident` when
    /// the patches match exactly; otherwise only the files it touched are identical there.
    Squashed { confident: bool },
    /// The upstream branch was deleted on the remote.
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub oid: Oid,
    pub reason: Reason,
    /// Also gone upstream (whatever the main reason).
    pub gone: bool,
    pub upstream: Option<String>,
    pub time: i64,
    pub subject: String,
    /// Checked out in this worktree (never the one rama has open as current).
    pub worktree: Option<PathBuf>,
    /// `git branch -d` would refuse (not merged into its upstream / HEAD): needs `-D`.
    pub needs_force: bool,
}

impl Candidate {
    /// Pre-selected in the dialog: everything except a squash match that isn't exact.
    pub fn suggested(&self) -> bool {
        !matches!(self.reason, Reason::Squashed { confident: false }) || self.gone
    }
}

/// Default protected branch patterns (`*` matches anything).
pub const PROTECTED: &[&str] = &["main", "master", "dev", "develop", "release*", "trunk"];

/// `release*` / `*-stable` / exact names; `*` matches any run of characters.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for p in &parts[1..parts.len() - 1] {
        match rest.find(p) {
            Some(at) => rest = &rest[at + p.len()..],
            None => return false,
        }
    }
    true
}

pub fn is_protected(name: &str, patterns: &[String]) -> bool {
    patterns.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).any(|p| glob_match(p, name))
}

/// Commits on the default branch searched for squash merges, at most.
const SQUASH_SCAN: usize = 3000;
/// Branches with more commits of their own than this are not checked for squash merges.
const SQUASH_MAX_COMMITS: usize = 300;

impl Repo {
    /// Local branches that look merged into `default`, excluding protected ones, the checked-out
    /// branch, and the default branch itself. Branches checked out in another worktree are
    /// listed with that worktree so the user can choose to remove both. `progress(done, total)`
    /// reports the squash-merge checks (only with `squash`; they take seconds on big repos).
    pub fn cleanup_candidates(&self, default: &BaseRef, protected: &[String], squash: bool, cancel: &Cancel, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<Vec<Candidate>> {
        let facts = self.branch_facts(&[])?;
        let wts = self.worktrees().unwrap_or_default();
        let current = self.out(&["symbolic-ref", "-q", "--short", "HEAD"]).ok().map(|v| String::from_utf8_lossy(&v).trim().to_string());
        let here = std::fs::canonicalize(self.root()).unwrap_or_else(|_| self.root());
        let worktree_of = |b: &str| wts.iter().find(|w| w.branch.as_deref() == Some(b)).map(|w| w.path.clone());
        let merged_remote = self.merged_into(&[default.rev.as_str()]);
        let merged_local = default.local.as_deref().filter(|l| *l != default.rev).map(|l| self.merged_into(&[l])).unwrap_or_default();

        let mut out: Vec<Candidate> = Vec::new();
        let mut squash_check = Vec::new();
        for f in facts {
            let Some(oid) = f.oid else { continue };
            if f.name == default.name || current.as_deref() == Some(f.name.as_str()) || is_protected(&f.name, protected) {
                continue;
            }
            let worktree = worktree_of(&f.name);
            if worktree.as_ref().is_some_and(|w| std::fs::canonicalize(w).unwrap_or_else(|_| w.clone()) == here) {
                continue;
            }
            let reason = if merged_remote.contains(&f.name) {
                Some(Reason::Merged(default.short_rev().to_string()))
            } else if merged_local.contains(&f.name) {
                Some(Reason::Merged(default.name.clone()))
            } else {
                None
            };
            let c = Candidate { name: f.name.clone(), oid, reason: reason.clone().unwrap_or(Reason::Gone), gone: f.gone, upstream: f.upstream.clone(), time: f.time, subject: f.subject.clone(), worktree, needs_force: false };
            if reason.is_some() {
                out.push(c);
            } else {
                squash_check.push(c);
            }
        }

        if !squash {
            out.extend(squash_check.drain(..).filter(|c| c.gone));
        }
        if !squash_check.is_empty() && !cancel.is_cancelled() {
            let since = squash_check.iter().map(|c| c.time).min().unwrap_or(0) - 86_400;
            let done = std::sync::atomic::AtomicUsize::new(0);
            let total = squash_check.len();
            progress(0, total);
            // the default branch's patch ids are computed while the branches' own are
            let (on_default, own) = std::thread::scope(|s| {
                let d = s.spawn(|| self.default_patch_ids(&default.rev, since));
                let own = parallel(&squash_check, 6, |c| {
                    let r = if cancel.is_cancelled() { None } else { self.own_patches(&default.rev, c.oid) };
                    progress(done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1, total);
                    r
                });
                (d.join().unwrap_or_default(), own)
            });
            let pairs: Vec<(Candidate, Option<Own>)> = squash_check.into_iter().zip(own).collect();
            let found = parallel(&pairs, 6, |(c, own)| own.as_ref().and_then(|o| self.squash_verdict(&default.rev, c.oid, o, &on_default)));
            for ((mut c, _), verdict) in pairs.into_iter().zip(found) {
                match verdict {
                    Some(confident) => c.reason = Reason::Squashed { confident },
                    None if c.gone => c.reason = Reason::Gone,
                    None => continue,
                }
                out.push(c);
            }
        }
        let force = parallel(&out, 6, |c| !self.branch_merged(&c.name));
        for (c, f) in out.iter_mut().zip(force) {
            c.needs_force = f;
        }
        out.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    /// Stable patch ids of the non-merge commits on `rev` committed after `since`.
    fn default_patch_ids(&self, rev: &str, since: i64) -> HashSet<String> {
        let n = SQUASH_SCAN.to_string();
        let since = format!("--since={since}");
        let Ok(log) = self.out(&["log", "-p", "--no-merges", "--no-color", "--no-ext-diff", "--format=commit %H", &since, "-n", &n, rev]) else { return HashSet::new() };
        patch_ids(&self.cwd(), &log).into_iter().collect()
    }

    /// The branch's own commits as patch ids, the patch id of its whole diff from the merge
    /// base, and the files that diff touches.
    fn own_patches(&self, rev: &str, oid: Oid) -> Option<Own> {
        let tip = oid.to_string();
        let log = self.out(&["log", "-p", "--no-merges", "--no-color", "--no-ext-diff", "--format=commit %H", &format!("{rev}..{tip}")]).ok()?;
        let commits = patch_ids(&self.cwd(), &log);
        if commits.len() > SQUASH_MAX_COMMITS {
            return None;
        }
        let diff = self.out(&["diff", "--no-color", "--no-ext-diff", &format!("{rev}...{tip}")]).ok()?;
        if diff.is_empty() {
            return None;
        }
        let combined = patch_ids(&self.cwd(), &diff).into_iter().next();
        Some(Own { commits, combined, files: diff_paths(&diff) })
    }

    /// `Some(true)` when each of the branch's commits, or its whole diff, has a patch id found
    /// on the default branch; `Some(false)` when only the files it changed are identical on the
    /// default branch now; `None` otherwise.
    fn squash_verdict(&self, rev: &str, oid: Oid, own: &Own, on_default: &HashSet<String>) -> Option<bool> {
        if (!own.commits.is_empty() && own.commits.iter().all(|p| on_default.contains(p))) || own.combined.as_ref().is_some_and(|p| on_default.contains(p)) {
            return Some(true);
        }
        if own.files.is_empty() {
            return None;
        }
        let tip = oid.to_string();
        let mut args = vec!["diff", "--quiet", rev, tip.as_str(), "--"];
        args.extend(own.files.iter().map(String::as_str));
        self.out(&args).is_ok().then_some(false)
    }

    /// Recreate a deleted branch at its old commit (`git branch <name> <sha>`); git refuses if
    /// the name is taken again.
    pub fn restore_branch(&self, name: &str, oid: Oid) -> Result<crate::Run> {
        let out = git::output(&self.cwd(), &["branch", "--", name, &oid.to_string()])?;
        Ok(crate::Run { success: true, output: String::from_utf8_lossy(&out).into_owned(), ..Default::default() })
    }
}

struct Own {
    commits: Vec<String>,
    combined: Option<String>,
    files: Vec<String>,
}

/// `git patch-id --stable` over `input`: one id per commit (or one for a bare diff), in order.
fn patch_ids(dir: &std::path::Path, input: &[u8]) -> Vec<String> {
    if input.is_empty() {
        return Vec::new();
    }
    let Ok(out) = git::output_with_stdin(dir, &["patch-id", "--stable"], input) else { return Vec::new() };
    String::from_utf8_lossy(&out).lines().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect()
}

/// Paths touched by a unified diff (both sides of renames).
fn diff_paths(diff: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in String::from_utf8_lossy(diff).lines() {
        let p = l.strip_prefix("--- a/").or_else(|| l.strip_prefix("+++ b/"));
        if let Some(p) = p
            && !out.iter().any(|x| x == p)
        {
            out.push(p.to_string());
        }
    }
    out
}
