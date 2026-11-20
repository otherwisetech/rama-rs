//! Working copy state and git operations: status refresh, the file watcher, and running one
//! write at a time off the UI thread with its output streamed into the Git output panel.
use crate::app::RamaApp;
use gpui::*;
use gpui_component::input::{InputState, TextareaState};
use repo::{Cancel, FileChange, Oid, Repo, Run, Stash, WorkFile, WorkStatus};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Lines kept in the Git output panel.
const LOG_MAX: usize = 5000;
/// File-system events are collected this long after the last one before refreshing.
const QUIET: Duration = Duration::from_millis(350);
/// A streamed command still running after this opens the output panel.
const AUTO_OPEN: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone)]
pub enum Op {
    Stage(Vec<String>),
    StageAll,
    Unstage(Vec<WorkFile>),
    UnstageAll,
    Discard(Vec<WorkFile>),
    Commit { message: String, amend: bool, no_verify: bool },
    StashPush { message: String, untracked: bool },
    StashApply(usize),
    StashPop(usize),
    StashDrop(usize),
    Switch(String),
    SwitchRemote(String),
    CreateBranch { name: String, at: Oid, checkout: bool },
    DeleteBranch(String),
    /// `git branch -D`, only after the second confirmation.
    ForceDeleteBranch(String),
    RenameBranch { old: String, new: String },
    /// `git push <remote> --delete <branch>`, as shown in the confirmation.
    DeleteRemote(Vec<String>),
    /// Fast-forward a local branch from its upstream.
    PullFf { name: String, head: bool },
    Fetch,
    Pull,
    /// `git push …` with the arguments the user saw (upstream included).
    Push(Vec<String>),
}

impl Op {
    /// The command line shown while it runs (push shows its real arguments once known).
    fn command(&self) -> String {
        match self {
            Op::Stage(p) => format!("git add -A -- ({})", p.len()), // i18n-ok: command line
            Op::StageAll => "git add -A".into(),                            // i18n-ok
            Op::Unstage(p) => format!("git reset -q -- ({})", p.len()), // i18n-ok
            Op::UnstageAll => "git reset -q".into(),                          // i18n-ok
            Op::Discard(p) => format!("git restore --worktree -- ({})", p.len()), // i18n-ok
            Op::Commit { amend, no_verify, .. } => format!("git commit{}{}", if *amend { " --amend" } else { "" }, if *no_verify { " --no-verify" } else { "" }), // i18n-ok
            Op::StashPush { untracked, .. } => format!("git stash push{}", if *untracked { " --include-untracked" } else { "" }), // i18n-ok
            Op::StashApply(i) => format!("git stash apply stash@{{{i}}}"), // i18n-ok
            Op::StashPop(i) => format!("git stash pop stash@{{{i}}}"),     // i18n-ok
            Op::StashDrop(i) => format!("git stash drop stash@{{{i}}}"),   // i18n-ok
            Op::Switch(b) => format!("git switch {b}"),                     // i18n-ok
            Op::SwitchRemote(b) => format!("git switch --track {b}"),       // i18n-ok
            Op::CreateBranch { name, checkout, .. } => if *checkout { format!("git switch -c {name}") } else { format!("git branch {name}") }, // i18n-ok
            Op::DeleteBranch(b) => format!("git branch -d {b}"),            // i18n-ok
            Op::ForceDeleteBranch(b) => format!("git branch -D {b}"),       // i18n-ok
            Op::RenameBranch { old, new } => format!("git branch -m {old} {new}"), // i18n-ok
            Op::DeleteRemote(args) | Op::Push(args) => format!("git {}", args.join(" ")), // i18n-ok
            Op::PullFf { name, .. } => format!("git pull --ff-only ({name})"), // i18n-ok
            Op::Fetch => "git fetch --all".into(),                          // i18n-ok
            Op::Pull => "git pull".into(),                                  // i18n-ok
        }
    }

    /// Network and hook-running ops stream their output while they run.
    fn streams(&self) -> bool {
        matches!(self, Op::Commit { .. } | Op::Fetch | Op::Pull | Op::PullFf { .. } | Op::Push(_) | Op::DeleteRemote(_))
    }

    /// Ops that leave history and refs alone only need a status refresh.
    fn touches_history(&self) -> bool {
        !matches!(self, Op::Stage(_) | Op::StageAll | Op::Unstage(_) | Op::UnstageAll | Op::Discard(_))
    }
}

pub struct Job {
    pub command: String,
    pub cancel: Cancel,
    pub started: Instant,
    pub commit: bool,
    pub op: Op,
    /// Streams output worth watching (commit hooks, fetch / pull / push).
    streams: bool,
}

/// A commit git refused, kept until the next attempt.
pub struct CommitFailure {
    /// The hook that rejected it, if it was a hook.
    pub hook: Option<String>,
    pub output: String,
}

/// A failed non-commit operation, shown above the graph until dismissed or the next run.
pub struct OpFailure {
    pub command: String,
    pub hook: Option<String>,
    pub output: String,
}

pub enum Confirm {
    Discard(Vec<WorkFile>),
    DropStash(usize),
    /// `merged` decides between a plain delete and the path to a forced one.
    DeleteBranch { name: String, merged: bool },
    ForceDeleteBranch(String),
    /// A push that will set the upstream: its full arguments.
    SetUpstream(Vec<String>),
    NoVerify,
}

pub enum PromptKind {
    NewBranch(Oid),
    Stash,
    RenameBranch(String),
    /// Remote-tracking name (`origin/x`), the branch name to type, and the push arguments.
    DeleteRemote { name: String, branch: String, args: Vec<String> },
}

pub struct Prompt {
    pub kind: PromptKind,
    pub input: Entity<InputState>,
    pub untracked: bool,
    pub checkout: bool,
}

pub struct StashView {
    pub index: usize,
    pub stash: Stash,
    pub changes: Option<Result<Rc<Vec<FileChange>>, String>>,
}

pub struct Work {
    pub status: Option<Rc<WorkStatus>>,
    pub stashes: Rc<Vec<Stash>>,
    pub job: Option<Job>,
    pub log: Arc<Mutex<Vec<String>>>,
    pub output_open: bool,
    pub output_scroll: UniformListScrollHandle,
    pub failure: Option<CommitFailure>,
    pub op_failure: Option<OpFailure>,
    /// Local branch → upstream (`origin/main`).
    pub upstreams: Rc<HashMap<String, String>>,
    pub remotes: Rc<Vec<String>>,
    pub confirm: Option<Confirm>,
    pub prompt: Option<Prompt>,
    pub git_missing: bool,
    pub amend: bool,
    /// What the box held before Amend prefilled it, restored when Amend is turned off.
    pub draft: Option<String>,
    pub message: Entity<TextareaState>,
    pub clear_message: bool,
    /// The "Uncommitted changes" row is selected.
    pub wip: bool,
    pub stash: Option<StashView>,
    refresh_gen: u64,
    watch_gen: u64,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl Work {
    pub fn new(message: Entity<TextareaState>) -> Self {
        Work {
            status: None,
            stashes: Rc::new(Vec::new()),
            job: None,
            log: Arc::new(Mutex::new(Vec::new())),
            output_open: false,
            output_scroll: UniformListScrollHandle::new(),
            failure: None,
            op_failure: None,
            upstreams: Rc::new(HashMap::new()),
            remotes: Rc::new(Vec::new()),
            confirm: None,
            prompt: None,
            git_missing: false,
            amend: false,
            draft: None,
            message,
            clear_message: false,
            wip: false,
            stash: None,
            refresh_gen: 0,
            watch_gen: 0,
            _watcher: None,
        }
    }

    pub fn dirty(&self) -> bool {
        self.status.as_ref().is_some_and(|s| !s.is_clean())
    }
}

fn push_log(log: &Mutex<Vec<String>>, line: &str, replace: bool) {
    let mut l = log.lock().unwrap();
    if replace && !l.is_empty() {
        *l.last_mut().unwrap() = line.to_string();
    } else {
        l.push(line.to_string());
    }
    let n = l.len();
    if n > LOG_MAX {
        l.drain(..n - LOG_MAX);
    }
}

fn execute(repo: &Repo, op: &Op, cancel: &Cancel, log: &Arc<Mutex<Vec<String>>>) -> Result<Run, repo::Error> {
    let lg = log.clone();
    let mut sink = move |line: &str, replace: bool| push_log(&lg, line, replace);
    match op {
        Op::Stage(p) => repo.stage(p),
        Op::StageAll => repo.stage_all(),
        Op::Unstage(f) => repo.unstage(f),
        Op::UnstageAll => repo.unstage_all(),
        Op::Discard(f) => repo.discard(f).map(|_| Run { success: true, ..Default::default() }),
        Op::Commit { message, amend, no_verify } => repo.commit(message, *amend, *no_verify, cancel, &mut sink),
        Op::StashPush { message, untracked } => repo.stash_push(message, *untracked),
        Op::StashApply(i) => repo.stash_apply(*i),
        Op::StashPop(i) => repo.stash_pop(*i),
        Op::StashDrop(i) => repo.stash_drop(*i),
        Op::Switch(b) => repo.switch(b),
        Op::SwitchRemote(b) => repo.switch_remote(b),
        Op::CreateBranch { name, at, checkout } => repo.create_branch(name, *at, *checkout),
        Op::DeleteBranch(b) => repo.delete_branch(b),
        Op::ForceDeleteBranch(b) => repo.delete_branch_force(b),
        Op::RenameBranch { old, new } => repo.rename_branch(old, new),
        Op::Fetch => repo.run(&["fetch", "--all", "--progress"], cancel, &mut sink),
        Op::Pull => repo.run(&["pull", "--no-edit", "--progress"], cancel, &mut sink),
        Op::PullFf { name, head } => {
            let args = repo.pull_ff_args(name, *head)?;
            push_log(log, &format!("$ git {}", args.join(" ")), false); // i18n-ok: command line
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            repo.run(&args, cancel, &mut sink)
        }
        Op::Push(args) | Op::DeleteRemote(args) => {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            repo.run(&args, cancel, &mut sink)
        }
    }
}

/// Paths inside `.git` that mean refs or HEAD moved (history must reload); other `.git` churn
/// (objects, logs, lock files) is ignored, and anything in the worktree only refreshes status.
fn classify(git_dir: &Path, path: &Path) -> Option<bool> {
    let Ok(rel) = path.strip_prefix(git_dir) else { return Some(false) };
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.ends_with(".lock") || s.starts_with("objects") || s.starts_with("logs/HEAD") || s.starts_with("modules") {
        return None;
    }
    if s == "HEAD" || s.starts_with("refs") || s == "packed-refs" || s.starts_with("logs/refs/stash") || s == "MERGE_HEAD" {
        return Some(true);
    }
    (s == "index").then_some(false)
}

impl RamaApp {
    /// Check that git is installed (this also resolves the login-shell PATH, off the UI thread).
    pub fn check_git(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move { repo::git_version() });
        cx.spawn(async move |this, cx| {
            let v = task.await;
            this.update(cx, |this, cx| {
                this.work.git_missing = v.is_none();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reload status and stashes, and with `history` also the commit graph, keeping the
    /// selection. Results from an older refresh are dropped.
    pub fn refresh(&mut self, history: bool, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        if self.work.git_missing {
            return;
        }
        self.work.refresh_gen += 1;
        let generation = self.work.refresh_gen;
        let task = cx.background_executor().spawn(async move {
            let status = repo.status().ok();
            let stashes = repo.stashes().unwrap_or_default();
            let upstreams = repo.upstreams().unwrap_or_default();
            let remotes = repo.remotes().unwrap_or_default();
            let h = if history { repo.history(crate::app::HISTORY_LIMIT).ok() } else { None };
            (status, stashes, upstreams, remotes, h)
        });
        cx.spawn(async move |this, cx| {
            let (status, stashes, upstreams, remotes, h) = task.await;
            this.update(cx, |this, cx| {
                if this.work.refresh_gen != generation {
                    return;
                }
                if let Some(h) = h {
                    this.replace_history(h, cx);
                }
                this.work.status = status.map(Rc::new);
                this.work.stashes = Rc::new(stashes);
                this.work.upstreams = Rc::new(upstreams);
                this.work.remotes = Rc::new(remotes);
                if let Some(sv) = &this.work.stash
                    && !this.work.stashes.iter().any(|s| s.id == sv.stash.id)
                {
                    this.work.stash = None;
                }
                this.reload_work_diff(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Watch the repository and refresh after changes settle. Our own `git status` does not
    /// write the index (`--no-optional-locks`), so it can't retrigger itself.
    pub fn watch(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        use notify::Watcher as _;
        self.work.watch_gen += 1;
        let generation = self.work.watch_gen;
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        let git_dir = root.join(".git");
        let gd = git_dir.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, notify::EventKind::Access(_)) {
                return;
            }
            if let Some(full) = ev.paths.iter().filter_map(|p| classify(&gd, p)).reduce(|a, b| a || b) {
                let _ = tx.send(full);
            }
        });
        let Ok(mut watcher) = watcher else {
            self.work._watcher = None;
            return;
        };
        if watcher.watch(&root, notify::RecursiveMode::Recursive).is_err() {
            self.work._watcher = None;
            return;
        }
        self.work._watcher = Some(watcher);
        cx.spawn(async move |this, cx| {
            let mut pending: Option<(bool, Instant)> = None;
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let mut got = false;
                while let Ok(full) = rx.try_recv() {
                    got = true;
                    let f = pending.map(|p| p.0).unwrap_or(false) || full;
                    pending = Some((f, Instant::now()));
                }
                let fire = !got && pending.is_some_and(|(_, at)| at.elapsed() >= QUIET);
                let alive = this.update(cx, |this, cx| {
                    if this.work.watch_gen != generation {
                        return false;
                    }
                    // a running op refreshes when it ends
                    if fire && this.work.job.is_none() {
                        this.refresh(pending.map(|p| p.0).unwrap_or(false), cx);
                    }
                    true
                });
                if fire && matches!(alive, Ok(true)) {
                    pending = None;
                }
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Run one git operation in the background; its output goes to the Git output panel.
    pub fn run_op(&mut self, op: Op, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        if self.work.git_missing {
            self.toast(i18n::t!("app.git.missing_short"), cx);
            return;
        }
        if self.work.job.is_some() {
            self.toast(i18n::t!("app.git.busy"), cx);
            return;
        }
        let cancel = Cancel::default();
        let command = op.command();
        let commit = matches!(op, Op::Commit { .. });
        {
            let mut l = self.work.log.lock().unwrap();
            if !l.is_empty() {
                l.push(String::new());
            }
            l.push(format!("$ {command}")); // i18n-ok: command line
        }
        if commit {
            self.work.failure = None;
        }
        let started = Instant::now();
        let streams = op.streams();
        self.work.op_failure = None;
        self.work.job = Some(Job { command, cancel: cancel.clone(), started, commit, op: op.clone(), streams });
        let log = self.work.log.clone();
        let op2 = op.clone();
        let task = cx.background_executor().spawn(async move { execute(&repo, &op2, &cancel, &log).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| this.finish_op(op, res, cx)).ok();
        })
        .detach();
        // keep the output panel and the spinner moving
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(120)).await;
                let running = this.update(cx, |this, cx| {
                    let Some(j) = this.work.job.as_ref().filter(|j| j.started == started) else { return false };
                    // a hook or a push that takes a while: show what it's doing
                    if j.streams && !this.work.output_open && j.started.elapsed() > AUTO_OPEN {
                        this.work.output_open = true;
                    }
                    cx.notify();
                    true
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn finish_op(&mut self, op: Op, res: Result<Run, String>, cx: &mut Context<Self>) {
        self.work.job = None;
        let log = self.work.log.clone();
        let (ok, cancelled, hook, output) = match res {
            Ok(run) => {
                // quiet ops return their output at the end
                if !op.streams() {
                    for l in run.output.lines() {
                        push_log(&log, l, false);
                    }
                }
                (run.success, run.cancelled, run.failed_hook, run.output)
            }
            Err(e) => {
                for l in e.lines() {
                    push_log(&log, l, false);
                }
                (false, false, None, e)
            }
        };
        if ok {
            match &op {
                Op::Commit { .. } => {
                    // set_value needs the window; the next render clears the box
                    self.work.clear_message = true;
                    self.work.amend = false;
                    self.work.draft = None;
                    self.work.failure = None;
                    self.toast(i18n::t!("app.git.committed"), cx);
                }
                Op::Fetch => self.toast(i18n::t!("app.git.fetched"), cx),
                Op::Pull | Op::PullFf { .. } => self.toast(i18n::t!("app.git.pulled"), cx),
                Op::Push(_) => self.toast(i18n::t!("app.git.pushed"), cx),
                Op::DeleteBranch(b) | Op::ForceDeleteBranch(b) => self.toast(i18n::t!("app.git.branch_deleted", name = b), cx),
                Op::DeleteRemote(_) => self.toast(i18n::t!("app.git.remote_deleted"), cx),
                Op::RenameBranch { new, .. } => self.toast(i18n::t!("app.git.renamed", name = new), cx),
                Op::StashPush { .. } => self.toast(i18n::t!("app.git.stashed"), cx),
                Op::Discard(_) => self.toast(i18n::t!("app.git.discarded"), cx),
                _ => {}
            }
        } else if cancelled {
            push_log(&log, &i18n::t!("app.git.cancelled"), false);
            self.toast(i18n::t!("app.git.cancelled"), cx);
        } else {
            if matches!(op, Op::Commit { .. }) {
                self.work.failure = Some(CommitFailure { hook: hook.clone(), output });
            } else {
                self.work.output_open = true;
                self.work.op_failure = Some(OpFailure { command: op.command(), hook: hook.clone(), output });
            }
            self.toast(match hook {
                Some(h) => i18n::t!("app.git.hook_failed_toast", hook = h),
                None => i18n::t!("app.git.failed"),
            }, cx);
        }
        self.refresh(op.touches_history(), cx);
        cx.notify();
    }

    pub fn cancel_job(&mut self, cx: &mut Context<Self>) {
        if let Some(j) = &self.work.job {
            j.cancel.cancel();
            cx.notify();
        }
    }

    pub fn select_wip(&mut self, cx: &mut Context<Self>) {
        self.work.wip = true;
        self.work.stash = None;
        self.selected = None;
        self.details = None;
        self.diff = None;
        cx.notify();
    }

    pub fn select_stash(&mut self, index: usize, cx: &mut Context<Self>) {
        let (Some(repo), Some(stash)) = (self.repo.clone(), self.work.stashes.get(index).cloned()) else { return };
        self.work.wip = false;
        self.selected = None;
        self.details = None;
        self.diff = None;
        let id = stash.id;
        self.work.stash = Some(StashView { index, stash, changes: None });
        let task = cx.background_executor().spawn(async move { repo.changes(id).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                if let Some(sv) = this.work.stash.as_mut().filter(|s| s.stash.id == id) {
                    sv.changes = Some(res.map(Rc::new));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Diff of a working-copy file: worktree vs index, or index vs HEAD when `staged`.
    pub fn open_work_diff(&mut self, f: WorkFile, staged: bool, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let fc = FileChange::detached(f.path.clone(), crate::detail::work_status(&f.state), 0, 0, false);
        self.diff = Some(crate::app::DiffState { file: fc, lines: None, work: Some((f.clone(), staged)) });
        self.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.load_work_diff(repo, f, staged, cx);
        cx.notify();
    }

    fn load_work_diff(&mut self, repo: Repo, f: WorkFile, staged: bool, cx: &mut Context<Self>) {
        let key = (f.path.clone(), staged);
        let task = cx.background_executor().spawn(async move { repo.work_diff(&f, staged).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                let Some(d) = this.diff.as_mut().filter(|d| d.work.as_ref().is_some_and(|(w, s)| (w.path.clone(), *s) == key)) else { return };
                match res {
                    Ok((lines, added, removed, binary)) => {
                        d.file.added = added;
                        d.file.removed = removed;
                        d.file.binary = binary;
                        d.lines = Some(Ok(Rc::new(lines)));
                    }
                    Err(e) => d.lines = Some(Err(e)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// After a refresh, an open working-copy diff shows the file's new content, or closes when
    /// the file left that list.
    fn reload_work_diff(&mut self, cx: &mut Context<Self>) {
        let Some((f, staged)) = self.diff.as_ref().and_then(|d| d.work.clone()) else { return };
        let still = self.work.status.as_ref().and_then(|s| (if staged { &s.staged } else { &s.unstaged }).iter().find(|x| x.path == f.path).cloned());
        match (still, self.repo.clone()) {
            (Some(f), Some(repo)) => self.load_work_diff(repo, f, staged, cx),
            _ => self.diff = None,
        }
    }

    pub fn toggle_amend(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if on == self.work.amend {
            return;
        }
        self.work.amend = on;
        if on {
            let current = self.work.message.read(cx).value().to_string();
            let last = self.repo.as_ref().and_then(|r| r.head_message()).unwrap_or_default();
            self.work.draft = Some(current.clone());
            if current.trim().is_empty() {
                self.work.message.update(cx, |m, cx| m.set_value(last, window, cx));
            }
        } else if let Some(d) = self.work.draft.take() {
            self.work.message.update(cx, |m, cx| m.set_value(d, window, cx));
        }
        cx.notify();
    }

    /// Commit button / ⌘↩. Hooks always run unless `no_verify` was chosen explicitly.
    pub fn commit(&mut self, no_verify: bool, cx: &mut Context<Self>) {
        let message = self.work.message.read(cx).value().trim_end().to_string();
        if message.lines().next().is_none_or(|l| l.trim().is_empty()) {
            self.toast(i18n::t!("app.work.need_message"), cx);
            return;
        }
        let staged = self.work.status.as_ref().is_some_and(|s| !s.staged.is_empty());
        if !staged && !self.work.amend {
            self.toast(i18n::t!("app.work.nothing_staged"), cx);
            return;
        }
        self.run_op(Op::Commit { message, amend: self.work.amend, no_verify }, cx);
    }

    pub fn open_prompt(&mut self, kind: PromptKind, window: &mut Window, cx: &mut Context<Self>) {
        let (placeholder, value) = match &kind {
            PromptKind::NewBranch(_) => (i18n::t!("app.prompt.branch_placeholder"), String::new()),
            PromptKind::Stash => (i18n::t!("app.prompt.stash_placeholder"), String::new()),
            PromptKind::RenameBranch(old) => (i18n::t!("app.prompt.branch_placeholder"), old.clone()),
            PromptKind::DeleteRemote { branch, .. } => (branch.clone(), String::new()),
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value));
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &gpui_component::input::InputEvent, window, cx| {
            match ev {
                gpui_component::input::InputEvent::PressEnter { .. } => this.submit_prompt(window, cx),
                // the delete-remote confirmation enables its button as the name is typed
                gpui_component::input::InputEvent::Change => cx.notify(),
                _ => {}
            }
        });
        self.prompt_sub = Some(sub);
        self.work.prompt = Some(Prompt { kind, input, untracked: true, checkout: true });
        cx.notify();
    }

    pub fn submit_prompt(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.work.prompt.take() else { return };
        let text = p.input.read(cx).value().trim().to_string();
        match p.kind {
            PromptKind::NewBranch(at) => {
                if text.is_empty() {
                    self.work.prompt = Some(p);
                    return;
                }
                self.run_op(Op::CreateBranch { name: text, at, checkout: p.checkout }, cx);
            }
            PromptKind::Stash => self.run_op(Op::StashPush { message: text, untracked: p.untracked }, cx),
            PromptKind::RenameBranch(old) => {
                if text.is_empty() || text == old {
                    self.work.prompt = Some(Prompt { kind: PromptKind::RenameBranch(old), ..p });
                    return;
                }
                self.run_op(Op::RenameBranch { old, new: text }, cx);
            }
            PromptKind::DeleteRemote { name, branch, args } => {
                if text != branch {
                    self.work.prompt = Some(Prompt { kind: PromptKind::DeleteRemote { name, branch, args }, ..p });
                    return;
                }
                self.run_op(Op::DeleteRemote(args), cx);
            }
        }
        self.prompt_sub = None;
        cx.notify();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.work.confirm.take() else { return };
        match c {
            Confirm::Discard(files) => self.run_op(Op::Discard(files), cx),
            Confirm::DropStash(i) => self.run_op(Op::StashDrop(i), cx),
            Confirm::DeleteBranch { name, merged: true } => self.run_op(Op::DeleteBranch(name), cx),
            // not merged: a second, separate confirmation before -D
            Confirm::DeleteBranch { name, merged: false } => self.work.confirm = Some(Confirm::ForceDeleteBranch(name)),
            Confirm::ForceDeleteBranch(name) => self.run_op(Op::ForceDeleteBranch(name), cx),
            Confirm::SetUpstream(args) => self.run_op(Op::Push(args), cx),
            Confirm::NoVerify => self.commit(true, cx),
        }
        cx.notify();
    }

    /// Push a branch: straight away when it has an upstream, after a confirmation showing the
    /// command when the push will set one.
    pub fn push_branch(&mut self, name: String, head: bool, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let task = cx.background_executor().spawn(async move { repo.push_branch_args(&name, head).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(args) if args.iter().any(|a| a == "--set-upstream") => this.work.confirm = Some(Confirm::SetUpstream(args)),
                    Ok(args) => this.run_op(Op::Push(args), cx),
                    Err(e) => this.toast(i18n::t!("app.git.no_remote", detail = e), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Top-bar Push: the checked-out branch.
    pub fn push_head(&mut self, cx: &mut Context<Self>) {
        match self.work.status.as_ref().and_then(|s| s.branch.head.clone()) {
            Some(b) => self.push_branch(b, true, cx),
            None => self.toast(i18n::t!("app.work.detached"), cx),
        }
    }

    /// Ask before deleting a local branch; whether it is merged decides the wording and
    /// whether a forced delete is offered.
    pub fn ask_delete_branch(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let n = name.clone();
        let task = cx.background_executor().spawn(async move { repo.branch_merged(&n) });
        cx.spawn(async move |this, cx| {
            let merged = task.await;
            this.update(cx, |this, cx| {
                this.work.confirm = Some(Confirm::DeleteBranch { name, merged });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ask before deleting a branch on its remote; the user types the branch name.
    pub fn ask_delete_remote(&mut self, remote_branch: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((remote, branch)) = repo::split_remote_name(&self.work.remotes, &remote_branch) else {
            self.toast(i18n::t!("app.git.no_remote", detail = remote_branch), cx);
            return;
        };
        let args = vec!["push".to_string(), remote, "--delete".to_string(), branch.clone()];
        self.open_prompt(PromptKind::DeleteRemote { name: remote_branch, branch, args }, window, cx);
    }
}
