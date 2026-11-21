//! Worktrees (`git worktree list --porcelain`), the default / base branches, per-worktree
//! status and how far each one is from its upstream and the default branch, disk usage, and
//! the worktree writes (add, remove, lock, prune). Reads use `--no-optional-locks`.
use crate::git::{self, Cancel, Run};
use crate::work::{WorkStatus, parse_status};
use crate::{Oid, Repo, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// `None` in a bare repository or before the first commit.
    pub head: Option<Oid>,
    /// Checked-out local branch (short name); `None` when detached or bare.
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    /// Locked, with the reason given (possibly empty).
    pub locked: Option<String>,
    /// Its directory is gone; `git worktree prune` would remove it.
    pub prunable: Option<String>,
    /// The main worktree (always listed first by git).
    pub main: bool,
}

/// Parse `git worktree list --porcelain [-z]`.
pub fn parse_worktree_list(raw: &[u8]) -> Vec<Worktree> {
    let s = String::from_utf8_lossy(raw);
    let sep = if s.contains('\0') { '\0' } else { '\n' };
    let mut out: Vec<Worktree> = Vec::new();
    let mut cur: Option<Worktree> = None;
    for line in s.split(sep) {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            out.extend(cur.take());
            continue;
        }
        let (k, v) = line.split_once(' ').unwrap_or((line, ""));
        if k == "worktree" {
            out.extend(cur.take());
            cur = Some(Worktree { path: PathBuf::from(v), head: None, branch: None, bare: false, detached: false, locked: None, prunable: None, main: false });
            continue;
        }
        let Some(w) = cur.as_mut() else { continue };
        match k {
            "HEAD" => w.head = Oid::from_hex(v.as_bytes()).ok().filter(|o| !o.is_null()),
            "branch" => w.branch = Some(v.strip_prefix("refs/heads/").unwrap_or(v).to_string()),
            "bare" => w.bare = true,
            "detached" => w.detached = true,
            "locked" => w.locked = Some(v.to_string()),
            "prunable" => w.prunable = Some(v.to_string()),
            _ => {}
        }
    }
    out.extend(cur);
    if let Some(first) = out.first_mut() {
        first.main = true;
    }
    out
}

/// A branch to compare against: its short name and the ref used (`refs/remotes/origin/main`
/// when the remote-tracking branch exists, since it includes merged pull requests, else
/// `refs/heads/main`). `local` is the local branch's ref when it exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseRef {
    pub name: String,
    pub rev: String,
    pub local: Option<String>,
}

impl BaseRef {
    /// Every ref of this branch that exists (remote-tracking first).
    pub fn revs(&self) -> Vec<&str> {
        let mut v = vec![self.rev.as_str()];
        if let Some(l) = self.local.as_deref().filter(|l| *l != self.rev) {
            v.push(l);
        }
        v
    }

    /// Short display name of `rev` (`origin/main`).
    pub fn short_rev(&self) -> &str {
        self.rev.strip_prefix("refs/remotes/").or_else(|| self.rev.strip_prefix("refs/heads/")).unwrap_or(&self.rev)
    }
}

/// Usual long-lived branch names, in the order tried for the default.
pub const BASE_NAMES: &[&str] = &["main", "master", "dev", "develop", "trunk"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// `None` when the directory is missing.
    pub status: Option<WorkStatus>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// The upstream is configured but no longer exists (deleted on the remote, then pruned).
    pub gone: bool,
    /// (base name, ahead, behind) for each base branch; the default comes first.
    pub vs: Vec<(String, u32, u32)>,
    /// HEAD is reachable from the default branch.
    pub merged: bool,
    /// Committer time and subject of HEAD.
    pub time: Option<i64>,
    pub subject: Option<String>,
}

impl WorktreeInfo {
    pub fn vs_default(&self) -> Option<(u32, u32)> {
        self.vs.first().map(|(_, a, b)| (*a, *b))
    }

    /// The base branch this worktree is closest to: fewest commits of its own on top of it.
    pub fn base(&self) -> Option<&str> {
        self.vs.iter().enumerate().min_by_key(|(i, (_, a, _))| (*a, *i)).map(|(_, (n, _, _))| n.as_str())
    }

    /// Tracked files changed plus untracked entries.
    pub fn changed(&self) -> usize {
        self.status.as_ref().map(|s| {
            let mut paths: HashSet<&str> = s.staged.iter().map(|f| f.path.as_str()).collect();
            paths.extend(s.unstaged.iter().map(|f| f.path.as_str()));
            paths.len()
        }).unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub wt: Worktree,
    pub info: WorktreeInfo,
}

/// What a local branch's `for-each-ref` line says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchFacts {
    pub name: String,
    pub oid: Option<Oid>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub gone: bool,
    pub time: i64,
    pub subject: String,
    /// (ahead, behind) against each requested base, in order; empty if git is too old.
    pub vs: Vec<(u32, u32)>,
}

/// `ahead 1, behind 2` / `gone` / `` (from `%(upstream:track,nobracket)`).
pub fn parse_track(s: &str) -> (u32, u32, bool) {
    let (mut a, mut b) = (0, 0);
    for part in s.split(',').map(str::trim) {
        if part == "gone" {
            return (0, 0, true);
        }
        if let Some(n) = part.strip_prefix("ahead ") {
            a = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            b = n.trim().parse().unwrap_or(0);
        }
    }
    (a, b, false)
}

/// Disk space a directory tree takes, with the usual heavy folders broken out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskUsage {
    pub total: u64,
    pub files: u64,
    /// (folder name, bytes) for `node_modules`, `.git`, `target`… summed wherever they occur
    /// (nested ones count once, in the outermost), largest first.
    pub parts: Vec<(String, u64)>,
    /// Stopped early (cancelled).
    pub partial: bool,
    /// Newest modification time (seconds since the epoch) of anything outside the heavy
    /// folders: when someone last worked here ([`target_usage`]: of anything in it, i.e. the
    /// last build). 0 when nothing was found.
    pub touched: i64,
    /// Regenerable build folders (`target/` of cargo or maven, `node_modules/`, `.next/`), the
    /// outermost of each, with their size; sorted by path.
    pub artifacts: Vec<(PathBuf, u64)>,
    /// `incremental/` caches inside those `target/` folders (`target/debug/incremental`,
    /// `target/<triple>/release/incremental`…), sorted by path.
    pub incremental: Vec<(PathBuf, u64)>,
}

/// Folder names reported separately in [`DiskUsage::parts`].
pub const HEAVY_DIRS: &[&str] = &["node_modules", ".git", "target", ".next", ".turbo", ".venv", ".gradle", "Pods", "vendor"];

/// Is `dir` (a folder named like a build artifact) really one? `target/` only next to a
/// `Cargo.toml` / `pom.xml` or tagged by cargo (`CACHEDIR.TAG`), `.next/` only next to a
/// `package.json`; `node_modules/` always.
pub fn is_artifact_dir(dir: &Path) -> bool {
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = dir.parent().unwrap_or(dir);
    match name.as_str() {
        "node_modules" => true,
        "target" => dir.join("CACHEDIR.TAG").is_file() || parent.join("Cargo.toml").is_file() || parent.join("pom.xml").is_file(),
        ".next" => parent.join("package.json").is_file(),
        _ => false,
    }
}

/// Size on disk of everything under `root` (symlinks are not followed; hard-linked files count
/// once, like `du`). Directories in `skip` (other worktrees nested inside this one) are left
/// out, so each byte is counted on one node only.
pub fn disk_usage(root: &Path, skip: &[PathBuf], cancel: &Cancel) -> DiskUsage {
    walk(root, skip, cancel, false)
}

/// [`disk_usage`] of a cargo target directory kept outside the repository
/// (`CARGO_TARGET_DIR`, `build.target-dir`): the whole folder is one artifact.
pub fn target_usage(root: &Path, cancel: &Cancel) -> DiskUsage {
    walk(root, &[], cancel, true)
}

struct Walk {
    dir: PathBuf,
    heavy: Option<String>,
    artifact: Option<usize>,
    in_target: bool,
    incremental: Option<usize>,
}

fn walk(root: &Path, skip: &[PathBuf], cancel: &Cancel, root_is_target: bool) -> DiskUsage {
    let mut out = DiskUsage::default();
    let mut parts: HashMap<String, u64> = HashMap::new();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    if root_is_target {
        out.artifacts.push((root.to_path_buf(), 0));
    }
    let mut stack = vec![Walk { dir: root.to_path_buf(), heavy: root_is_target.then(|| "target".to_string()), artifact: root_is_target.then_some(0), in_target: root_is_target, incremental: None }];
    while let Some(w) = stack.pop() {
        if cancel.is_cancelled() {
            out.partial = true;
            break;
        }
        let Ok(rd) = std::fs::read_dir(&w.dir) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            let p = e.path();
            if md.is_dir() && skip.contains(&p) {
                continue;
            }
            let size = on_disk(&md, &mut seen);
            out.total += size;
            if let Some(h) = &w.heavy {
                *parts.entry(h.clone()).or_default() += size;
            }
            // a shared target folder's own age tells whether any build (of any repo) used it lately
            if w.heavy.is_none() || root_is_target {
                out.touched = out.touched.max(mtime(&md));
            }
            if let Some(a) = w.artifact {
                out.artifacts[a].1 += size;
            }
            if let Some(i) = w.incremental {
                out.incremental[i].1 += size;
            }
            if !md.is_dir() {
                out.files += 1;
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            let mut next = Walk { dir: p, heavy: w.heavy.clone(), artifact: w.artifact, in_target: w.in_target, incremental: w.incremental };
            if w.heavy.is_none() {
                if HEAVY_DIRS.contains(&name.as_str()) {
                    next.heavy = Some(name.clone());
                }
                if is_artifact_dir(&next.dir) {
                    out.artifacts.push((next.dir.clone(), size));
                    next.artifact = Some(out.artifacts.len() - 1);
                    next.in_target = name == "target";
                }
            } else if w.in_target && w.incremental.is_none() && name == "incremental" {
                out.incremental.push((next.dir.clone(), size));
                next.incremental = Some(out.incremental.len() - 1);
            }
            stack.push(next);
        }
    }
    let mut parts: Vec<(String, u64)> = parts.into_iter().filter(|(_, v)| *v > 0).collect();
    parts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.parts = parts;
    out.artifacts.sort();
    out.incremental.sort();
    out
}

fn mtime(md: &std::fs::Metadata) -> i64 {
    md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(unix)]
fn on_disk(md: &std::fs::Metadata, seen: &mut HashSet<(u64, u64)>) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if md.nlink() > 1 && !md.is_dir() && !seen.insert((md.dev(), md.ino())) {
        return 0;
    }
    md.blocks() * 512
}

#[cfg(not(unix))]
fn on_disk(md: &std::fs::Metadata, _seen: &mut HashSet<(u64, u64)>) -> u64 {
    md.len()
}

/// How a new worktree gets its checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewWorktree {
    /// An existing local branch (git refuses if it is checked out elsewhere).
    Branch(String),
    /// A new branch `name` starting at `start`.
    NewBranch { name: String, start: String },
    /// A detached HEAD at a commit.
    Commit(String),
}

/// `git worktree add …` arguments.
pub fn worktree_add_args(path: &Path, how: &NewWorktree) -> Vec<String> {
    let p = path.display().to_string();
    match how {
        NewWorktree::Branch(b) => vec!["worktree".into(), "add".into(), p, b.clone()],
        NewWorktree::NewBranch { name, start } => vec!["worktree".into(), "add".into(), "-b".into(), name.clone(), p, start.clone()],
        NewWorktree::Commit(c) => vec!["worktree".into(), "add".into(), "--detach".into(), p, c.clone()],
    }
}

/// Default path for a new worktree: `<base>/<repo>-<branch>` with `/` in the branch name
/// turned into `-`; `base` defaults to the main worktree's parent folder.
pub fn default_worktree_path(main: &Path, base: Option<&Path>, branch: &str) -> PathBuf {
    let repo = main.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let slug: String = branch.chars().map(|c| if c == '/' || c == '\\' || c.is_whitespace() || c == ':' { '-' } else { c }).collect();
    let dir = base.map(Path::to_path_buf).or_else(|| main.parent().map(Path::to_path_buf)).unwrap_or_else(|| main.to_path_buf());
    dir.join(format!("{repo}-{}", slug.trim_matches('-')))
}

impl Repo {
    /// The directory git commands run in: the working tree, or the git dir when bare.
    pub(crate) fn cwd(&self) -> PathBuf {
        self.root()
    }

    pub(crate) fn out(&self, args: &[&str]) -> Result<Vec<u8>> {
        git::output(&self.cwd(), args)
    }

    fn out_str(&self, args: &[&str]) -> Option<String> {
        self.out(args).ok().map(|v| String::from_utf8_lossy(&v).trim().to_string()).filter(|s| !s.is_empty())
    }

    fn ref_exists(&self, r: &str) -> bool {
        self.out(&["rev-parse", "--verify", "-q", &format!("{r}^{{commit}}")]).is_ok()
    }

    /// The shared `.git` directory (the main worktree's), where objects and refs live.
    pub fn common_dir(&self) -> PathBuf {
        self.inner.to_thread_local().common_dir().to_path_buf()
    }

    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        Ok(parse_worktree_list(&self.out(&["worktree", "list", "--porcelain", "-z"])?))
    }

    /// The default branch first (`preferred` if it exists, else what `origin/HEAD` points to,
    /// else the first of [`BASE_NAMES`]), then the other usual base branches that exist.
    pub fn base_branches(&self, preferred: Option<&str>) -> Vec<BaseRef> {
        let remotes = self.remotes().unwrap_or_default();
        let remote = remotes.iter().find(|r| *r == "origin").or(remotes.first()).cloned();
        let make = |name: &str| -> Option<BaseRef> {
            let local = format!("refs/heads/{name}");
            let local = self.ref_exists(&local).then_some(local);
            let remote_ref = remote.as_ref().map(|r| format!("refs/remotes/{r}/{name}")).filter(|r| self.ref_exists(r));
            let rev = remote_ref.or_else(|| local.clone())?;
            Some(BaseRef { name: name.to_string(), rev, local })
        };
        let origin_head = remote.as_ref().and_then(|r| self.out_str(&["symbolic-ref", "-q", &format!("refs/remotes/{r}/HEAD")]).and_then(|s| s.strip_prefix(&format!("refs/remotes/{r}/")).map(str::to_string)));
        let mut order: Vec<String> = Vec::new();
        order.extend(preferred.map(str::trim).filter(|p| !p.is_empty()).map(str::to_string));
        order.extend(origin_head);
        order.extend(BASE_NAMES.iter().map(|s| s.to_string()));
        let mut out: Vec<BaseRef> = Vec::new();
        for name in order {
            if out.iter().any(|b| b.name == name) {
                continue;
            }
            // the preferred / origin/HEAD name may be any branch; the rest only if present
            if let Some(b) = make(&name) {
                out.push(b);
            }
        }
        out
    }

    /// Every local branch with its upstream state, last commit and distance to each of `bases`.
    pub fn branch_facts(&self, bases: &[BaseRef]) -> Result<Vec<BranchFacts>> {
        let mut fmt = String::from("%(refname)%00%(objectname)%00%(upstream:short)%00%(upstream:track,nobracket)%00%(committerdate:unix)%00%(contents:subject)");
        for b in bases {
            fmt.push_str(&format!("%00%(ahead-behind:{})", b.rev));
        }
        let raw = match self.out(&["for-each-ref", &format!("--format={fmt}"), "refs/heads"]) {
            Ok(r) => r,
            // %(ahead-behind:…) needs git 2.41; older ones get the rest
            Err(_) if !bases.is_empty() => return self.branch_facts(&[]),
            Err(e) => return Err(e),
        };
        Ok(String::from_utf8_lossy(&raw)
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\0').collect();
                let name = f.first()?.strip_prefix("refs/heads/")?.to_string();
                let (ahead, behind, gone) = parse_track(f.get(3).copied().unwrap_or(""));
                let vs = f.iter().skip(6).filter_map(|v| {
                    let (a, b) = v.split_once(' ')?;
                    Some((a.parse().ok()?, b.parse().ok()?))
                }).collect();
                Some(BranchFacts {
                    name,
                    oid: f.get(1).and_then(|o| Oid::from_hex(o.as_bytes()).ok()),
                    upstream: f.get(2).filter(|u| !u.is_empty()).map(|u| u.to_string()),
                    ahead,
                    behind,
                    gone,
                    time: f.get(4).and_then(|t| t.parse().ok()).unwrap_or(0),
                    subject: f.get(5).map(|s| s.to_string()).unwrap_or_default(),
                    vs,
                })
            })
            .collect())
    }

    /// (ahead, behind) of `commit` against `base`.
    fn ahead_behind(&self, base: &str, commit: &str) -> Option<(u32, u32)> {
        let s = self.out_str(&["rev-list", "--left-right", "--count", &format!("{base}...{commit}")])?;
        let mut it = s.split_whitespace().map(|n| n.parse::<u32>().ok());
        let behind = it.next()??;
        let ahead = it.next()??;
        Some((ahead, behind))
    }

    /// Local branches merged into any of `revs` (`git branch --merged`).
    pub fn merged_into(&self, revs: &[&str]) -> HashSet<String> {
        let mut out = HashSet::new();
        for r in revs {
            if let Ok(raw) = self.out(&["for-each-ref", "--format=%(refname)", &format!("--merged={r}"), "refs/heads"]) {
                out.extend(String::from_utf8_lossy(&raw).lines().filter_map(|l| l.strip_prefix("refs/heads/")).map(str::to_string));
            }
        }
        out
    }

    /// Status of one worktree directory, without taking its index lock.
    pub fn status_at(dir: &Path) -> Result<WorkStatus> {
        Ok(parse_status(&git::output(dir, &["--no-optional-locks", "status", "--porcelain=v2", "-z", "--branch", "--untracked-files=normal"])?))
    }

    /// Every worktree with its status and distances. Statuses run a few at a time in parallel;
    /// the rest is a handful of `for-each-ref` calls however many branches there are.
    pub fn worktree_overview(&self, bases: &[BaseRef]) -> Result<Vec<WorktreeEntry>> {
        let wts = self.worktrees()?;
        let facts: HashMap<String, BranchFacts> = self.branch_facts(bases).unwrap_or_default().into_iter().map(|f| (f.name.clone(), f)).collect();
        let merged = bases.first().map(|b| self.merged_into(&b.revs())).unwrap_or_default();
        let statuses = parallel(&wts, 4, |w| if w.bare || w.prunable.is_some() || !w.path.is_dir() { None } else { Repo::status_at(&w.path).ok() });
        let mut out = Vec::with_capacity(wts.len());
        for (wt, status) in wts.into_iter().zip(statuses) {
            let mut info = WorktreeInfo { status, ..Default::default() };
            match wt.branch.as_ref().and_then(|b| facts.get(b)) {
                Some(f) => {
                    info.upstream = f.upstream.clone();
                    (info.ahead, info.behind, info.gone) = (f.ahead, f.behind, f.gone);
                    info.time = Some(f.time);
                    info.subject = Some(f.subject.clone());
                    info.merged = merged.contains(&f.name);
                    info.vs = if f.vs.len() == bases.len() {
                        bases.iter().zip(&f.vs).map(|(b, (a, bh))| (b.name.clone(), *a, *bh)).collect()
                    } else {
                        let oid = f.oid.map(|o| o.to_string()).unwrap_or_default();
                        bases.iter().filter_map(|b| self.ahead_behind(&b.rev, &oid).map(|(a, bh)| (b.name.clone(), a, bh))).collect()
                    };
                }
                None => {
                    if let Some(h) = wt.head {
                        let h = h.to_string();
                        if let Some(s) = self.out_str(&["log", "-1", "--format=%ct%x1f%s", &h]) {
                            let (t, subj) = s.split_once('\x1f').unwrap_or((&s, ""));
                            info.time = t.parse().ok();
                            info.subject = Some(subj.to_string());
                        }
                        info.vs = bases.iter().filter_map(|b| self.ahead_behind(&b.rev, &h).map(|(a, bh)| (b.name.clone(), a, bh))).collect();
                        info.merged = bases.first().is_some_and(|b| b.revs().iter().any(|r| self.out(&["merge-base", "--is-ancestor", &h, r]).is_ok()));
                    }
                }
            }
            out.push(WorktreeEntry { wt, info });
        }
        Ok(out)
    }

    /// What `git worktree prune` would remove (`--dry-run --verbose` lines).
    pub fn worktree_prune_preview(&self) -> Result<Vec<String>> {
        // git reports what it would prune on stderr
        let out = git::command(&self.cwd()).args(["worktree", "prune", "--dry-run", "--verbose"]).stdin(std::process::Stdio::null()).output()?;
        if !out.status.success() {
            return Err(Box::new(crate::GitError::Failed(String::from_utf8_lossy(&out.stderr).into_owned())));
        }
        let mut raw = out.stdout;
        raw.extend_from_slice(&out.stderr);
        Ok(String::from_utf8_lossy(&raw).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
    }

    /// Run a worktree write through the CLI (`add`, `remove`, `lock`, `unlock`, `prune`).
    pub fn run_args(&self, args: &[String], cancel: &Cancel, on_line: &mut dyn FnMut(&str, bool)) -> Result<Run> {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        git::run(&self.cwd(), &a, None, cancel, on_line)
    }

    /// `git worktree remove [--force] <path>`. Without `force` git refuses when the worktree
    /// has changes or untracked files; the app passes `force` only after a second confirmation.
    /// Locked worktrees are always refused (unlock first).
    pub fn worktree_remove_args(path: &Path, force: bool) -> Vec<String> {
        let mut v = vec!["worktree".to_string(), "remove".to_string()];
        if force {
            v.push("--force".into());
        }
        v.push(path.display().to_string());
        v
    }

    pub fn worktree_lock_args(path: &Path, reason: &str) -> Vec<String> {
        let mut v = vec!["worktree".to_string(), "lock".to_string()];
        if !reason.trim().is_empty() {
            v.extend(["--reason".to_string(), reason.trim().to_string()]);
        }
        v.push(path.display().to_string());
        v
    }

    pub fn worktree_unlock_args(path: &Path) -> Vec<String> {
        vec!["worktree".into(), "unlock".into(), path.display().to_string()]
    }

    /// Size of the shared `.git` directory.
    pub fn git_dir_usage(&self) -> DiskUsage {
        disk_usage(&self.common_dir(), &[], &Cancel::default())
    }
}

/// `f` over `items` on up to `n` threads, results in order.
pub(crate) fn parallel<T: Sync, R: Send>(items: &[T], n: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<Option<R>>> = std::sync::Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..n.clamp(1, items.len().max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                let r = f(item);
                results.lock().unwrap()[i] = Some(r);
            });
        }
    });
    results.into_inner().unwrap().into_iter().map(|r| r.expect("every item ran")).collect()
}
