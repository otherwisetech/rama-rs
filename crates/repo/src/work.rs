//! Working copy: status, stage / unstage / discard, commit, stash, branches and remotes, all
//! through the `git` CLI (see `git.rs`). Every function here writes only when called.
use crate::git::{self, Cancel, GitError, Run};
use crate::{DiffLine, LineKind, Oid, Repo, Result};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    Added,
    Modified,
    Deleted,
    Renamed { from: String },
    Copied { from: String },
    TypeChanged,
    Untracked,
    /// Unmerged after a merge, rebase, cherry-pick or stash pop.
    Conflicted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkFile {
    pub path: String,
    pub state: FileState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchStatus {
    /// Checked-out branch; `None` when detached.
    pub head: Option<String>,
    /// `None` before the first commit.
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkStatus {
    pub branch: BranchStatus,
    /// Index vs HEAD.
    pub staged: Vec<WorkFile>,
    /// Worktree vs index, untracked and conflicted files.
    pub unstaged: Vec<WorkFile>,
}

impl WorkStatus {
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty()
    }
}

fn state(c: u8, from: Option<&str>) -> Option<FileState> {
    Some(match c {
        b'M' => FileState::Modified,
        b'A' => FileState::Added,
        b'D' => FileState::Deleted,
        b'T' => FileState::TypeChanged,
        b'R' => FileState::Renamed { from: from.unwrap_or_default().to_string() },
        b'C' => FileState::Copied { from: from.unwrap_or_default().to_string() },
        _ => return None,
    })
}

/// Parse `git status --porcelain=v2 -z --branch`.
pub fn parse_status(raw: &[u8]) -> WorkStatus {
    let s = String::from_utf8_lossy(raw);
    let mut out = WorkStatus::default();
    let mut it = s.split('\0');
    while let Some(rec) = it.next() {
        if let Some(h) = rec.strip_prefix("# ") {
            let (k, v) = h.split_once(' ').unwrap_or((h, ""));
            match k {
                "branch.oid" => out.branch.oid = (v != "(initial)").then(|| v.to_string()),
                "branch.head" => out.branch.head = (v != "(detached)").then(|| v.to_string()),
                "branch.upstream" => out.branch.upstream = Some(v.to_string()),
                "branch.ab" => {
                    for part in v.split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            out.branch.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            out.branch.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let kind = rec.as_bytes().first().copied();
        match kind {
            Some(b'1') | Some(b'2') => {
                // 1 XY sub mH mI mW hH hI path  /  2 XY sub mH mI mW hH hI Xscore path \0 orig
                let fields = if kind == Some(b'1') { 9 } else { 10 };
                let parts: Vec<&str> = rec.splitn(fields, ' ').collect();
                let (Some(xy), Some(path)) = (parts.get(1), parts.get(fields - 1)) else { continue };
                let from = if kind == Some(b'2') { it.next() } else { None };
                let xy = xy.as_bytes();
                if let Some(st) = state(xy[0], from) {
                    out.staged.push(WorkFile { path: path.to_string(), state: st });
                }
                if let Some(st) = state(xy[1], from) {
                    out.unstaged.push(WorkFile { path: path.to_string(), state: st });
                }
            }
            Some(b'u') => {
                if let Some(path) = rec.splitn(11, ' ').nth(10) {
                    out.unstaged.push(WorkFile { path: path.to_string(), state: FileState::Conflicted });
                }
            }
            Some(b'?') => out.unstaged.push(WorkFile { path: rec[2..].to_string(), state: FileState::Untracked }),
            _ => {}
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    /// Position in `git stash list` (`stash@{index}`).
    pub index: usize,
    pub id: Oid,
    /// Reflog subject: "On main: message" or "WIP on main: abc1234 summary".
    pub message: String,
}

impl Stash {
    pub fn name(&self) -> String {
        format!("stash@{{{}}}", self.index)
    }
}

/// Parse `git stash list -z --format=%H%x1f%gs`.
pub fn parse_stash_list(raw: &[u8]) -> Vec<Stash> {
    String::from_utf8_lossy(raw)
        .split('\0')
        .filter(|r| !r.trim().is_empty())
        .enumerate()
        .filter_map(|(index, r)| {
            let (id, msg) = r.trim_start_matches('\n').split_once('\x1f')?;
            Some(Stash { index, id: Oid::from_hex(id.trim().as_bytes()).ok()?, message: msg.trim().to_string() })
        })
        .collect()
}

/// Parse `git diff` output for one file into lines with numbers, plus added / removed counts.
/// `binary` when git reports "Binary files … differ".
pub fn parse_unified(text: &str) -> (Vec<DiffLine>, u32, u32, bool) {
    let mut out = Vec::new();
    let (mut added, mut removed) = (0, 0);
    let (mut o, mut n) = (0u32, 0u32);
    let mut in_hunk = false;
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("@@ ") {
            // @@ -a,b +c,d @@ context
            let mut nums = rest.split(' ');
            let start = |s: Option<&str>| s.and_then(|s| s[1..].split(',').next()).and_then(|v| v.parse::<u32>().ok()).unwrap_or(1);
            o = start(nums.next());
            n = start(nums.next());
            in_hunk = true;
            out.push(DiffLine { kind: LineKind::Hunk, old_no: None, new_no: None, text: l.to_string() });
            continue;
        }
        if !in_hunk {
            if l.starts_with("Binary files ") {
                return (Vec::new(), 0, 0, true);
            }
            continue;
        }
        let (kind, body) = match l.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &l[1..]),
            Some(b'-') => (LineKind::Removed, &l[1..]),
            Some(b' ') => (LineKind::Context, &l[1..]),
            Some(b'\\') => continue, // "\ No newline at end of file"
            None => (LineKind::Context, ""),
            _ => {
                // the next file's header
                in_hunk = false;
                continue;
            }
        };
        let (old_no, new_no) = match kind {
            LineKind::Added => {
                added += 1;
                n += 1;
                (None, Some(n - 1))
            }
            LineKind::Removed => {
                removed += 1;
                o += 1;
                (Some(o - 1), None)
            }
            _ => {
                o += 1;
                n += 1;
                (Some(o - 1), Some(n - 1))
            }
        };
        out.push(DiffLine { kind, old_no, new_no, text: body.trim_end_matches('\r').to_string() });
    }
    (out, added, removed, false)
}

/// A no-op sink for [`git::run`] output.
fn quiet() -> impl FnMut(&str, bool) {
    |_: &str, _: bool| {}
}

fn ok(run: Run) -> Result<Run> {
    if run.success { Ok(run) } else { Err(Box::new(GitError::Failed(run.output))) }
}

impl Repo {
    fn workdir(&self) -> Result<PathBuf> {
        self.inner.to_thread_local().workdir().map(|p| p.to_path_buf()).ok_or_else(|| Box::new(GitError::Failed(String::from("bare repository"))) as crate::Error) // i18n-ok: internal
    }

    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        git::output(&self.workdir()?, args)
    }

    /// Quiet write: runs git, returns its output, fails with git's message.
    fn write(&self, args: &[&str]) -> Result<Run> {
        ok(git::run(&self.workdir()?, args, None, &Cancel::default(), &mut quiet())?)
    }

    /// Streamed command for the output panel; check [`Run::success`].
    pub fn run(&self, args: &[&str], cancel: &Cancel, on_line: &mut dyn FnMut(&str, bool)) -> Result<Run> {
        git::run(&self.workdir()?, args, None, cancel, on_line)
    }

    /// `git status`, without taking the index lock (so watching the repo doesn't trigger itself).
    pub fn status(&self) -> Result<WorkStatus> {
        Ok(parse_status(&self.git(&["--no-optional-locks", "status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all"])?))
    }

    fn has_head(&self) -> bool {
        self.git(&["rev-parse", "--verify", "-q", "HEAD"]).is_ok()
    }

    pub fn stage(&self, paths: &[String]) -> Result<Run> {
        let mut args = vec!["add", "-A", "--"];
        args.extend(paths.iter().map(String::as_str));
        self.write(&args)
    }

    pub fn stage_all(&self) -> Result<Run> {
        self.write(&["add", "-A"])
    }

    /// Unstage files; a staged rename is unstaged on both its paths.
    pub fn unstage(&self, files: &[WorkFile]) -> Result<Run> {
        let mut paths: Vec<&str> = Vec::new();
        for f in files {
            paths.push(&f.path);
            if let FileState::Renamed { from } = &f.state {
                paths.push(from);
            }
        }
        let mut args = if self.has_head() { vec!["reset", "-q", "--"] } else { vec!["rm", "--cached", "-r", "-q", "--"] };
        args.extend(paths);
        self.write(&args)
    }

    pub fn unstage_all(&self) -> Result<Run> {
        if self.has_head() { self.write(&["reset", "-q"]) } else { self.write(&["rm", "--cached", "-r", "-q", "--", "."]) }
    }

    /// Throw away worktree changes: tracked files go back to their staged (or committed)
    /// content, untracked files are moved to the system Trash — never deleted outright.
    pub fn discard(&self, files: &[WorkFile]) -> Result<()> {
        self.discard_with(files, git::trash_paths)
    }

    /// [`Repo::discard`] with a custom remover for untracked files (tests avoid the real Trash).
    pub fn discard_with(&self, files: &[WorkFile], remove: impl Fn(&[PathBuf]) -> Result<()>) -> Result<()> {
        let root = self.workdir()?;
        let untracked: Vec<PathBuf> = files.iter().filter(|f| f.state == FileState::Untracked).map(|f| root.join(&f.path)).collect();
        let tracked: Vec<&str> = files.iter().filter(|f| f.state != FileState::Untracked).map(|f| f.path.as_str()).collect();
        if !tracked.is_empty() {
            let mut args = vec!["restore", "--worktree", "--"];
            args.extend(tracked);
            self.write(&args)?;
        }
        if !untracked.is_empty() {
            remove(&untracked)?;
        }
        Ok(())
    }

    /// Diff of one working-copy file: worktree vs index (`staged == false`) or index vs HEAD.
    /// Returns (lines, added, removed, binary).
    pub fn work_diff(&self, f: &WorkFile, staged: bool) -> Result<(Vec<DiffLine>, u32, u32, bool)> {
        if f.state == FileState::Untracked {
            let bytes = std::fs::read(self.workdir()?.join(&f.path))?;
            let Some(text) = crate::text(&bytes) else { return Ok((Vec::new(), 0, 0, true)) };
            let lines = crate::unified("", text);
            let added = lines.iter().filter(|l| l.kind == LineKind::Added).count() as u32;
            return Ok((lines, added, 0, false));
        }
        let mut args = vec!["diff", "--no-color", "--no-ext-diff", "-U3", "-M"];
        if staged {
            args.push("--cached");
        }
        args.push("--");
        if let FileState::Renamed { from } = &f.state {
            args.push(from);
        }
        args.push(&f.path);
        let raw = self.git(&args)?;
        Ok(parse_unified(&String::from_utf8_lossy(&raw)))
    }

    /// Commit the index. Hooks run as on the command line unless `no_verify`.
    pub fn commit(&self, message: &str, amend: bool, no_verify: bool, cancel: &Cancel, on_line: &mut dyn FnMut(&str, bool)) -> Result<Run> {
        let mut args = vec!["commit", "--file=-"];
        if amend {
            args.push("--amend");
        }
        if no_verify {
            args.push("--no-verify");
        }
        git::run(&self.workdir()?, &args, Some(message.as_bytes()), cancel, on_line)
    }

    /// Message of HEAD, to prefill an amend.
    pub fn head_message(&self) -> Option<String> {
        let id = self.inner.to_thread_local().head_id().ok()?.detach();
        self.message(id).ok()
    }

    pub fn stashes(&self) -> Result<Vec<Stash>> {
        Ok(parse_stash_list(&self.git(&["stash", "list", "-z", "--format=%H%x1f%gs"])?))
    }

    pub fn stash_push(&self, message: &str, include_untracked: bool) -> Result<Run> {
        let mut args = vec!["stash", "push"];
        if include_untracked {
            args.push("--include-untracked");
        }
        if !message.trim().is_empty() {
            args.extend(["-m", message.trim()]);
        }
        self.write(&args)
    }

    pub fn stash_apply(&self, index: usize) -> Result<Run> {
        self.write(&["stash", "apply", &format!("stash@{{{index}}}")])
    }

    pub fn stash_pop(&self, index: usize) -> Result<Run> {
        self.write(&["stash", "pop", &format!("stash@{{{index}}}")])
    }

    pub fn stash_drop(&self, index: usize) -> Result<Run> {
        self.write(&["stash", "drop", &format!("stash@{{{index}}}")])
    }

    /// Check out a local branch. git refuses when local changes would be overwritten; that
    /// error is returned as is.
    pub fn switch(&self, branch: &str) -> Result<Run> {
        self.write(&["switch", "--no-guess", branch])
    }

    /// Check out a remote branch as a new local branch that tracks it (`origin/x` → `x`).
    pub fn switch_remote(&self, remote_branch: &str) -> Result<Run> {
        self.write(&["switch", "--track", remote_branch])
    }

    pub fn create_branch(&self, name: &str, at: Oid, checkout: bool) -> Result<Run> {
        let at = at.to_string();
        if checkout { self.write(&["switch", "-c", name, &at]) } else { self.write(&["branch", "--", name, &at]) }
    }

    /// Delete a local branch only if it is merged (`git branch -d`, never `-D`).
    pub fn delete_branch(&self, name: &str) -> Result<Run> {
        self.write(&["branch", "-d", "--", name])
    }

    pub fn remotes(&self) -> Result<Vec<String>> {
        Ok(String::from_utf8_lossy(&self.git(&["remote"])?).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
    }

    /// Arguments for a push: plain `push` with an upstream, else `push --set-upstream <remote>
    /// <branch>` to the branch's push remote, `remote.pushDefault`, `origin` or the only remote.
    pub fn push_args(&self, branch: &BranchStatus) -> Result<Vec<String>> {
        let mut args = vec!["push".to_string(), "--progress".to_string()];
        let Some(name) = &branch.head else { return Err(Box::new(GitError::Failed(String::from("HEAD is detached")))) }; // i18n-ok: internal
        if branch.upstream.is_some() {
            return Ok(args);
        }
        let config = |k: &str| self.git(&["config", "--get", k]).ok().map(|v| String::from_utf8_lossy(&v).trim().to_string()).filter(|v| !v.is_empty());
        let remotes = self.remotes()?;
        let remote = config(&format!("branch.{name}.pushRemote"))
            .or_else(|| config("remote.pushDefault"))
            .or_else(|| remotes.iter().find(|r| *r == "origin").cloned())
            .or_else(|| (remotes.len() == 1).then(|| remotes[0].clone()))
            .ok_or_else(|| Box::new(GitError::Failed(String::from("no remote"))) as crate::Error)?; // i18n-ok: internal
        args.extend(["--set-upstream".to_string(), remote, name.clone()]);
        Ok(args)
    }
}
