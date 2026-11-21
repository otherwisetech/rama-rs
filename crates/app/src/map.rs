//! Worktree map state: the worktree list with status and distances (loaded in the background),
//! disk sizes (measured one worktree at a time, cached on disk, plus cargo target folders kept
//! outside the repository), pan / zoom, the dialogs, and the actions (terminal, agent, remove,
//! lock, prune, new worktree).
use crate::app::RamaApp;
use crate::launch::{self, Launch, Term};
use crate::work::{GitKind, Op};
use gpui::*;
use gpui_component::input::{InputEvent, InputState};
use repo::reclaim::Suggestion;
use repo::{BaseRef, Cancel, DiskUsage, NewWorktree, Repo, WorktreeEntry};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Sizes measured more recently than this are shown without measuring again.
const SIZE_FRESH: i64 = 15 * 60;
pub const ZOOM_MIN: f32 = 0.25;
pub const ZOOM_MAX: f32 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Name,
    Size,
    Age,
    Merged,
}

#[derive(Debug, Clone)]
pub enum Size {
    Measuring,
    Done { usage: DiskUsage, at: i64 },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NewMode {
    Existing,
    NewBranch,
    Commit,
}

pub struct NewForm {
    pub mode: NewMode,
    pub name: Entity<InputState>,
    pub start: Entity<InputState>,
    pub path: Entity<InputState>,
    /// The path last filled in automatically; once the user edits it, it stays theirs.
    pub auto_path: String,
    pub agent: Option<String>,
    pub _subs: Vec<Subscription>,
}

pub struct SettingsForm {
    pub default_branch: Entity<InputState>,
    pub protected: Entity<InputState>,
    pub base_dir: Entity<InputState>,
    pub agents: Entity<InputState>,
    pub terminal: Option<String>,
    pub badges: bool,
    pub reduce_motion: bool,
    pub notify: bool,
    pub today: bool,
    pub stale_days: Entity<InputState>,
    pub disk_today: bool,
}

pub enum Dialog {
    New(NewForm),
    /// `force`: the second step, after git (or our check) found changes.
    Remove { entry: Box<WorktreeEntry>, force: bool, delete_branch: bool },
    Prune(Option<Result<Vec<String>, String>>),
    /// The exact command, shown before running it the first time.
    Launch { launch: Launch, key: String },
    Lock { path: PathBuf, reason: Entity<InputState> },
    Settings(SettingsForm),
}

pub struct MapState {
    pub entries: Option<Result<Rc<Vec<WorktreeEntry>>, String>>,
    pub bases: Rc<Vec<BaseRef>>,
    pub loading: bool,
    load_gen: u64,
    pub sizes: HashMap<PathBuf, Size>,
    size_cancel: Cancel,
    pub selected: Option<PathBuf>,
    pub list: bool,
    pub grouped: bool,
    pub sort: Sort,
    pub zoom: f32,
    pub pan: (f32, f32),
    pub drag: Option<(Point<Pixels>, (f32, f32))>,
    /// The pointer moved since the button went down: the click that follows isn't a click.
    pub moved: bool,
    /// Bounds of the map canvas from the last paint, for zooming around the cursor.
    pub viewport: Rc<Cell<Bounds<Pixels>>>,
    pub dialog: Option<Dialog>,
    /// The listed worktree this window has open.
    pub current: Option<PathBuf>,
    pub terminals: Rc<Vec<Term>>,
    pub agents: Rc<Vec<String>>,
    detected: bool,
    /// Cargo target folders outside the repository → the worktrees building there.
    pub shared_users: BTreeMap<PathBuf, Vec<PathBuf>>,
    /// What "Free up space…" would offer, from the sizes measured so far.
    pub reclaim: Rc<Vec<Suggestion>>,
    pub reclaim_dialog: Option<crate::reclaim::ReclaimDialog>,
    /// Open "Free up space…" once the worktrees are loaded (a Today link asked for it).
    pub want_reclaim: bool,
    /// diskmap is installed (for "Open in diskmap").
    pub diskmap: bool,
}

impl Default for MapState {
    fn default() -> Self {
        let s = crate::settings::Settings::load();
        MapState {
            entries: None,
            bases: Rc::new(Vec::new()),
            loading: false,
            load_gen: 0,
            sizes: load_size_cache(),
            size_cancel: Cancel::default(),
            selected: None,
            list: s.map_mode.as_deref() == Some("list"),
            grouped: s.map_layout.as_deref() == Some("grouped"),
            sort: Sort::Name,
            zoom: 1.0,
            pan: (0., 0.),
            drag: None,
            moved: false,
            viewport: Rc::new(Cell::new(Bounds::default())),
            dialog: None,
            current: None,
            terminals: Rc::new(Vec::new()),
            agents: Rc::new(Vec::new()),
            detected: false,
            shared_users: BTreeMap::new(),
            reclaim: Rc::new(Vec::new()),
            reclaim_dialog: None,
            want_reclaim: false,
            diskmap: false,
        }
    }
}

impl MapState {
    pub fn list(&self) -> Option<&Rc<Vec<WorktreeEntry>>> {
        self.entries.as_ref().and_then(|e| e.as_ref().ok())
    }

    pub fn entry(&self, path: &Path) -> Option<&WorktreeEntry> {
        self.list()?.iter().find(|e| e.wt.path == path)
    }

    pub fn usage(&self, path: &Path) -> Option<&DiskUsage> {
        match self.sizes.get(path) {
            Some(Size::Done { usage, .. }) => Some(usage),
            _ => None,
        }
    }

    pub fn measuring(&self, path: &Path) -> bool {
        matches!(self.sizes.get(path), Some(Size::Measuring))
    }

    /// The agent commands offered: the configured list, else the CLIs found on PATH.
    pub fn agent_commands(&self) -> Vec<String> {
        let s = crate::settings::Settings::load();
        if s.agents.is_empty() { self.agents.as_ref().clone() } else { s.agents }
    }

    /// Stop measuring (the repository is being replaced).
    pub fn cancel_sizes(&self) {
        self.size_cancel.cancel();
    }

    pub fn zoom_by(&mut self, factor: f32, around: Option<Point<Pixels>>) {
        let z = (self.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        let b = self.viewport.get();
        let c = b.center();
        let p = around.unwrap_or(c);
        let (px_, py_) = (f32::from(p.x - c.x), f32::from(p.y - c.y));
        let wx = (px_ - self.pan.0) / self.zoom;
        let wy = (py_ - self.pan.1) / self.zoom;
        self.pan = (px_ - wx * z, py_ - wy * z);
        self.zoom = z;
    }
}

fn size_cache_file() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("sizes.json"))
}

fn pairs(v: &[(PathBuf, u64)]) -> serde_json::Value {
    serde_json::Value::Array(v.iter().map(|(p, n)| serde_json::json!([p.to_string_lossy(), n])).collect())
}

fn unpairs(v: Option<&serde_json::Value>) -> Vec<(PathBuf, u64)> {
    v.and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| Some((PathBuf::from(x.get(0)?.as_str()?), x.get(1)?.as_u64()?))).collect()).unwrap_or_default()
}

/// `{ "<path>": { total, files, at, touched, parts: {name: bytes}, artifacts: [[path, bytes]…], incremental: […] } }`
pub fn parse_size_cache(text: &str) -> HashMap<PathBuf, Size> {
    let Ok(serde_json::Value::Object(m)) = serde_json::from_str::<serde_json::Value>(text) else { return HashMap::new() };
    m.into_iter()
        .filter_map(|(k, v)| {
            let mut parts: Vec<(String, u64)> = v.get("parts").and_then(|p| p.as_object()).map(|o| o.iter().filter_map(|(n, b)| Some((n.clone(), b.as_u64()?))).collect()).unwrap_or_default();
            parts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let usage = DiskUsage {
                total: v.get("total")?.as_u64()?,
                files: v.get("files")?.as_u64()?,
                parts,
                partial: false,
                touched: v.get("touched").and_then(|t| t.as_i64()).unwrap_or(0),
                artifacts: unpairs(v.get("artifacts")),
                incremental: unpairs(v.get("incremental")),
            };
            Some((PathBuf::from(k), Size::Done { usage, at: v.get("at")?.as_i64()? }))
        })
        .collect()
}

pub fn size_cache_json(sizes: &HashMap<PathBuf, Size>) -> String {
    let mut m = serde_json::Map::new();
    let mut keys: Vec<&PathBuf> = sizes.keys().collect();
    keys.sort();
    for p in keys {
        if let Some(Size::Done { usage, at }) = sizes.get(p)
            && !usage.partial
        {
            let parts: serde_json::Map<String, serde_json::Value> = usage.parts.iter().map(|(k, v)| (k.clone(), (*v).into())).collect();
            m.insert(p.to_string_lossy().into_owned(), serde_json::json!({ "total": usage.total, "files": usage.files, "at": at, "touched": usage.touched, "parts": parts, "artifacts": pairs(&usage.artifacts), "incremental": pairs(&usage.incremental) }));
        }
    }
    serde_json::to_string(&serde_json::Value::Object(m)).unwrap_or_default()
}

fn load_size_cache() -> HashMap<PathBuf, Size> {
    size_cache_file().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse_size_cache(&t)).unwrap_or_default()
}

fn save_size_cache(sizes: &HashMap<PathBuf, Size>) {
    let Some(f) = size_cache_file() else { return };
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, size_cache_json(sizes));
}

/// Cargo target folders outside each worktree (`CARGO_TARGET_DIR` from the login shell, or
/// `build.target-dir` in a `.cargo/config.toml`), with the worktrees that use them.
fn detect_shared(worktrees: &[PathBuf]) -> BTreeMap<PathBuf, Vec<PathBuf>> {
    let env = repo::shell_env::resolved();
    let var = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let cargo_home = var("CARGO_HOME").map(PathBuf::from).or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".cargo"))); // i18n-ok: env names
    let target_env = var("CARGO_TARGET_DIR"); // i18n-ok: env name
    let mut out: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for w in worktrees {
        if let Some(t) = repo::reclaim::cargo_target_dir(w, target_env.as_deref(), cargo_home.as_deref()) {
            out.entry(t).or_default().push(w.clone());
        }
    }
    for v in out.values_mut() {
        v.sort();
    }
    out
}

/// `<data root>/rama/worktrees.txt`: every worktree rama has listed, one path per line, so
/// diskmap can mark them. Paths that no longer exist drop out.
fn remember_worktrees(paths: &[PathBuf]) {
    let Some(f) = crate::settings::config_dir().map(|d| d.join("worktrees.txt")) else { return };
    let old = std::fs::read_to_string(&f).unwrap_or_default();
    let mut all: Vec<String> = old.lines().filter(|l| !l.trim().is_empty() && Path::new(l).is_dir()).map(str::to_string).collect();
    all.extend(paths.iter().filter(|p| p.is_dir()).map(|p| p.to_string_lossy().into_owned()));
    all.sort();
    all.dedup();
    let text = all.join("\n") + "\n";
    if text != old {
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, text);
    }
}

impl RamaApp {
    pub fn show_map(&mut self, on: bool, cx: &mut Context<Self>) {
        self.map_view = on;
        if on {
            self.reload_map(false, cx);
            self.detect_tools(cx);
        }
        cx.notify();
    }

    /// Terminals and agent CLIs (needs the login-shell PATH), once.
    pub fn detect_tools(&mut self, cx: &mut Context<Self>) {
        if self.map.detected {
            return;
        }
        self.map.detected = true;
        let task = cx.background_executor().spawn(async move { (launch::detect_terminals(), launch::detect_agents(), crate::otherwise::diskmap_installed()) });
        cx.spawn(async move |this, cx| {
            let (terms, agents, diskmap) = task.await;
            this.update(cx, |this, cx| {
                this.map.terminals = Rc::new(terms);
                this.map.agents = Rc::new(agents);
                this.map.diskmap = diskmap;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-read the worktree list in the background; `sizes` also measures every size again.
    pub fn reload_map(&mut self, sizes: bool, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        if self.work.git_missing {
            return;
        }
        self.map.load_gen += 1;
        let generation = self.map.load_gen;
        self.map.loading = true;
        let preferred = crate::settings::Settings::load().default_branch;
        let task = cx.background_executor().spawn(async move {
            let bases = repo.base_branches(preferred.as_deref());
            let entries = repo.worktree_overview(&bases).map_err(|e| e.to_string());
            let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let here = canon(&repo.root());
            let current = entries.as_ref().ok().and_then(|l| l.iter().find(|e| canon(&e.wt.path) == here).map(|e| e.wt.path.clone()));
            (bases, entries, current)
        });
        cx.spawn(async move |this, cx| {
            let (bases, entries, current) = task.await;
            this.update(cx, |this, cx| {
                if this.map.load_gen != generation {
                    return;
                }
                this.map.loading = false;
                this.map.bases = Rc::new(bases);
                this.map.current = current;
                let ok = entries.is_ok();
                this.map.entries = Some(entries.map(Rc::new));
                if let Some(sel) = this.map.selected.clone()
                    && this.map.entry(&sel).is_none()
                {
                    this.map.selected = None;
                }
                if ok {
                    if let Some(l) = this.map.list() {
                        let paths: Vec<PathBuf> = l.iter().filter(|e| e.wt.prunable.is_none()).map(|e| e.wt.path.clone()).collect();
                        cx.background_executor().spawn(async move { remember_worktrees(&paths) }).detach();
                    }
                    this.measure_sizes(sizes, cx);
                    this.refresh_reclaim(cx);
                    if std::mem::take(&mut this.map.want_reclaim) {
                        this.open_reclaim(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Measure worktree sizes one after another (big trees take a while; one at a time keeps
    /// the disk responsive). Nested worktrees are left out of their parent's size.
    fn measure_sizes(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(list) = self.map.list().cloned() else { return };
        // a run in progress covers the list; only an explicit refresh restarts it
        if !force && self.map.sizes.values().any(|s| matches!(s, Size::Measuring)) {
            return;
        }
        self.map.size_cancel.cancel();
        let cancel = Cancel::default();
        self.map.size_cancel = cancel.clone();
        let now = crate::util::now();
        let all: Vec<PathBuf> = list.iter().filter(|e| e.wt.prunable.is_none()).map(|e| e.wt.path.clone()).collect();
        let mut todo: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
        for p in &all {
            let fresh = matches!(self.map.sizes.get(p), Some(Size::Done { at, .. }) if now - at < SIZE_FRESH);
            if force || !fresh {
                let skip = all.iter().filter(|o| *o != p && o.starts_with(p)).cloned().collect();
                todo.push((p.clone(), skip));
                if !matches!(self.map.sizes.get(p), Some(Size::Done { .. })) {
                    self.map.sizes.insert(p.clone(), Size::Measuring);
                }
            }
        }
        cx.spawn(async move |this, cx| {
            for (path, skip) in todo {
                if cancel.is_cancelled() {
                    break;
                }
                let (p, c) = (path.clone(), cancel.clone());
                let usage = cx.background_executor().spawn(async move { repo::disk_usage(&p, &skip, &c) }).await;
                let alive = this.update(cx, |this, cx| {
                    if !usage.partial {
                        this.map.sizes.insert(path, Size::Done { usage, at: crate::util::now() });
                        save_size_cache(&this.map.sizes);
                    }
                    this.refresh_reclaim(cx);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
            if cancel.is_cancelled() {
                return;
            }
            let shared = cx.background_executor().spawn(async move { detect_shared(&all) }).await;
            let targets: Vec<PathBuf> = shared.keys().cloned().collect();
            if this.update(cx, |this, _| this.map.shared_users = shared).is_err() {
                return;
            }
            for t in targets {
                let stale = this.update(cx, |this, _| force || !matches!(this.map.sizes.get(&t), Some(Size::Done { at, .. }) if crate::util::now() - at < SIZE_FRESH)).unwrap_or(false);
                if !stale || cancel.is_cancelled() {
                    continue;
                }
                let (p, c) = (t.clone(), cancel.clone());
                let usage = cx.background_executor().spawn(async move { repo::target_usage(&p, &c) }).await;
                let alive = this.update(cx, |this, cx| {
                    if !usage.partial {
                        this.map.sizes.insert(t, Size::Done { usage, at: crate::util::now() });
                        save_size_cache(&this.map.sizes);
                    }
                    this.refresh_reclaim(cx);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
            this.update(cx, |this, cx| this.refresh_reclaim(cx)).ok();
        })
        .detach();
    }

    /// Open a terminal at `dir`, running `command` if given; the first time a terminal and
    /// command pair is used, the exact command line is shown first.
    pub fn launch(&mut self, dir: PathBuf, command: Option<String>, cx: &mut Context<Self>) {
        let s = crate::settings::Settings::load();
        let Some(term) = launch::pick_terminal(s.terminal.as_deref(), &self.map.terminals) else {
            self.toast(i18n::t!("app.map.no_terminal"), cx);
            return;
        };
        let plan = launch::plan(term, &dir, command.as_deref(), &launch::user_shell());
        let key = launch::approval_key(term, command.as_deref());
        if s.launch_ok.contains(&key) {
            self.spawn_launch(plan, cx);
        } else {
            self.map.dialog = Some(Dialog::Launch { launch: plan, key });
        }
        cx.notify();
    }

    pub fn spawn_launch(&mut self, plan: Launch, cx: &mut Context<Self>) {
        match plan.spawn() {
            Ok(()) => {
                if let Some(c) = &plan.copy {
                    cx.write_to_clipboard(ClipboardItem::new_string(c.clone()));
                    self.toast(i18n::t!("app.map.command_copied", command = c), cx);
                }
            }
            Err(e) => self.toast(i18n::t!("app.map.launch_failed", detail = e), cx),
        }
    }

    pub fn confirm_launch(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Launch { launch, key }) = self.map.dialog.take() else { return };
        crate::settings::Settings::update(|s| {
            if !s.launch_ok.contains(&key) {
                s.launch_ok.push(key);
            }
        });
        self.spawn_launch(launch, cx);
        cx.notify();
    }

    pub fn reveal(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if let Err(e) = launch::reveal(dir) {
            self.toast(i18n::t!("app.map.launch_failed", detail = e), cx);
        }
    }

    pub fn copy_path(&mut self, dir: &Path, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(dir.display().to_string()));
        self.toast(i18n::t!("app.toast.copied_name", name = dir.display()), cx);
    }

    pub fn open_in_new_window(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        crate::open_window(Some(dir), cx);
    }

    /// Remove asks first; a dirty worktree goes straight to the forced step, which lists what
    /// would be lost.
    pub fn ask_remove_worktree(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(e) = self.map.entry(path).cloned() else { return };
        if e.wt.main {
            return;
        }
        if e.wt.locked.is_some() {
            self.toast(i18n::t!("app.map.locked_remove"), cx);
            return;
        }
        if self.map.current.as_deref() == Some(path) {
            self.toast(i18n::t!("app.map.current_remove"), cx);
            return;
        }
        let dirty = e.info.changed() > 0;
        let delete_branch = false;
        self.map.dialog = Some(Dialog::Remove { entry: Box::new(e), force: dirty, delete_branch });
        cx.notify();
    }

    pub fn confirm_remove(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Remove { entry, force, delete_branch }) = self.map.dialog.take() else { return };
        let branch = entry.wt.branch.clone().filter(|_| delete_branch);
        self.run_op(Op::RemoveWorktree { path: entry.wt.path.clone(), force, branch }, cx);
    }

    pub fn ask_prune(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        self.map.dialog = Some(Dialog::Prune(None));
        let task = cx.background_executor().spawn(async move { repo.worktree_prune_preview().map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                if let Some(Dialog::Prune(p)) = this.map.dialog.as_mut() {
                    *p = Some(res);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn ask_lock(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let reason = cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t!("app.map.lock_placeholder")));
        reason.update(cx, |s, cx| s.focus(window, cx));
        self.map.dialog = Some(Dialog::Lock { path, reason });
        cx.notify();
    }

    pub fn confirm_lock(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Lock { path, reason }) = self.map.dialog.take() else { return };
        let r = reason.read(cx).value().to_string();
        self.run_op(Op::Git { args: Repo::worktree_lock_args(&path, &r), kind: GitKind::Lock }, cx);
    }

    pub fn unlock(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.run_op(Op::Git { args: Repo::worktree_unlock_args(path), kind: GitKind::Unlock }, cx);
    }

    fn main_path(&self) -> Option<PathBuf> {
        self.map.list().and_then(|l| l.first()).map(|e| e.wt.path.clone()).or_else(|| self.repo.as_ref().map(|r| r.root()))
    }

    fn suggested_path(&self, name: &str) -> String {
        let Some(main) = self.main_path() else { return String::new() };
        let base = crate::settings::Settings::load().worktree_base.map(PathBuf::from);
        let name = if name.trim().is_empty() { "worktree" } else { name.trim() }; // i18n-ok: folder name
        repo::default_worktree_path(&main, base.as_deref(), name).display().to_string()
    }

    pub fn open_new_worktree(&mut self, branch: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.detect_tools(cx);
        let mode = if branch.is_some() { NewMode::Existing } else { NewMode::NewBranch };
        let start_default = self.map.bases.first().map(|b| b.short_rev().to_string()).unwrap_or_else(|| "HEAD".into()); // i18n-ok: ref
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t!("app.prompt.branch_placeholder")).default_value(branch.clone().unwrap_or_default()));
        let start = cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t!("app.map.new_start_placeholder")).default_value(start_default));
        let auto = self.suggested_path(branch.as_deref().unwrap_or_default());
        let path = cx.new(|cx| InputState::new(window, cx).default_value(auto.clone()));
        let subs = vec![
            cx.subscribe_in(&name, window, |this, input, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    let v = input.read(cx).value().to_string();
                    let mut v = v.trim().to_string();
                    if let Some(Dialog::New(f)) = &this.map.dialog
                        && f.mode == NewMode::Commit
                    {
                        v = v.chars().take(10).collect();
                    }
                    let suggested = this.suggested_path(&v);
                    if let Some(Dialog::New(f)) = this.map.dialog.as_mut() {
                        let current = f.path.read(cx).value().to_string();
                        if current == f.auto_path {
                            f.auto_path = suggested.clone();
                            f.path.update(cx, |p, cx| p.set_value(suggested, window, cx));
                        }
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.submit_new_worktree(cx),
                _ => {}
            }),
            cx.subscribe_in(&path, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.submit_new_worktree(cx);
                }
            }),
        ];
        name.update(cx, |s, cx| s.focus(window, cx));
        self.map.dialog = Some(Dialog::New(NewForm { mode, name, start, path, auto_path: auto, agent: None, _subs: subs }));
        cx.notify();
    }

    pub fn submit_new_worktree(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::New(f)) = &self.map.dialog else { return };
        let name = f.name.read(cx).value().trim().to_string();
        let start = f.start.read(cx).value().trim().to_string();
        let path = f.path.read(cx).value().trim().to_string();
        if name.is_empty() || path.is_empty() {
            return;
        }
        let path = PathBuf::from(expand_home(&path));
        let how = match f.mode {
            NewMode::Existing => NewWorktree::Branch(name),
            NewMode::NewBranch => NewWorktree::NewBranch { name, start: if start.is_empty() { "HEAD".into() } else { start } }, // i18n-ok: ref
            NewMode::Commit => NewWorktree::Commit(name),
        };
        let launch = f.agent.clone().map(|a| (path.clone(), a));
        self.map.dialog = None;
        self.run_op(Op::Git { args: repo::worktree_add_args(&path, &how), kind: GitKind::WorktreeAdd { launch } }, cx);
        self.map.selected = Some(path);
        cx.notify();
    }

    pub fn open_map_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.detect_tools(cx);
        let s = crate::settings::Settings::load();
        let agents = if s.agents.is_empty() { self.map.agents.join("\n") } else { s.agents.join("\n") };
        let mk = |v: String, ph: String, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v));
        let default_branch = mk(s.default_branch.clone().unwrap_or_default(), self.map.bases.first().map(|b| b.name.clone()).unwrap_or_default(), window, cx);
        let protected = mk(s.protected_patterns().join(", "), String::new(), window, cx);
        let base_dir = mk(s.worktree_base.clone().unwrap_or_default(), self.main_path().and_then(|m| m.parent().map(|p| p.display().to_string())).unwrap_or_default(), window, cx);
        let agents = mk(agents.replace('\n', ", "), launch::AGENTS.join(", "), window, cx);
        let stale_days = mk(s.stale_days().to_string(), repo::reclaim::DEFAULT_STALE_DAYS.to_string(), window, cx);
        self.map.dialog = Some(Dialog::Settings(SettingsForm { default_branch, protected, base_dir, agents, terminal: s.terminal.clone(), badges: s.agent_badges(), reduce_motion: s.reduce_motion(), notify: s.agent_notify(), today: s.agent_today(), stale_days, disk_today: s.disk_today() }));
        cx.notify();
    }

    pub fn save_map_settings(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Settings(f)) = self.map.dialog.take() else { return };
        let val = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let split = |v: String| v.split([',', '\n']).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect::<Vec<_>>();
        let (db, prot, base, agents) = (val(&f.default_branch), split(val(&f.protected)), val(&f.base_dir), split(val(&f.agents)));
        let detected = self.map.agents.as_ref().clone();
        let stale = val(&f.stale_days).parse::<u32>().ok().filter(|d| *d > 0 && *d != repo::reclaim::DEFAULT_STALE_DAYS);
        crate::settings::Settings::update(|s| {
            s.default_branch = (!db.is_empty()).then_some(db);
            // the built-in list is stored as "unset" so later defaults still apply
            s.protected = if prot == repo::PROTECTED.iter().map(|x| x.to_string()).collect::<Vec<_>>() { Vec::new() } else { prot };
            s.worktree_base = (!base.is_empty()).then_some(base);
            s.agents = if agents == detected { Vec::new() } else { agents };
            s.terminal = f.terminal.clone();
            s.agent_badges = (!f.badges).then(|| "off".into()); // i18n-ok: setting value
            s.reduce_motion = f.reduce_motion.then(|| "on".into()); // i18n-ok: setting value
            s.agent_notify = f.notify.then(|| "on".into()); // i18n-ok: setting value
            s.agent_today = (!f.today).then(|| "off".into()); // i18n-ok: setting value
            s.stale_days = stale.map(|d| d.to_string());
            s.disk_today = (!f.disk_today).then(|| "off".into()); // i18n-ok: setting value
        });
        self.agents.today = f.today;
        self.set_agent_prefs(f.badges, f.reduce_motion, f.notify, cx);
        self.reload_map(false, cx);
        self.refresh_reclaim(cx);
        cx.notify();
    }

    pub fn set_map_mode(&mut self, list: bool, cx: &mut Context<Self>) {
        self.map.list = list;
        crate::settings::Settings::update(|s| s.map_mode = Some(if list { "list" } else { "map" }.into())); // i18n-ok: setting value
        cx.notify();
    }

    pub fn set_map_layout(&mut self, grouped: bool, cx: &mut Context<Self>) {
        self.map.grouped = grouped;
        self.map.pan = (0., 0.);
        crate::settings::Settings::update(|s| s.map_layout = Some(if grouped { "grouped" } else { "radial" }.into())); // i18n-ok: setting value
        cx.notify();
    }

    /// Follow-ups once a git operation ends: reload the map, start the agent in a new
    /// worktree, refresh the cleanup dialog or the .git size after gc.
    pub fn after_op(&mut self, op: &Op, ok: bool, cx: &mut Context<Self>) {
        match op {
            Op::Git { kind: GitKind::WorktreeAdd { launch: Some((path, agent)) }, .. } if ok => {
                self.launch(path.clone(), Some(agent.clone()), cx);
            }
            Op::Git { kind: GitKind::WorktreeAdd { .. }, .. } if ok => self.toast(i18n::t!("app.map.created"), cx),
            Op::RemoveWorktree { path, .. } if ok => {
                self.map.sizes.remove(path);
                if self.map.selected.as_deref() == Some(path) {
                    self.map.selected = None;
                }
                self.toast(i18n::t!("app.map.removed"), cx);
            }
            // a refused plain remove: offer the forced one with the list of what is lost
            Op::RemoveWorktree { path, force: false, .. } => {
                if let Some(e) = self.map.entry(path).cloned()
                    && e.wt.locked.is_none()
                {
                    self.map.dialog = Some(Dialog::Remove { entry: Box::new(e), force: true, delete_branch: false });
                }
            }
            Op::Git { kind: GitKind::Prune, .. } if ok => self.toast(i18n::t!("app.map.pruned"), cx),
            _ => {}
        }
        self.cleanup_after_op(op, ok, cx);
        if matches!(op, Op::Git { .. } | Op::RemoveWorktree { .. } | Op::Cleanup(_) | Op::RestoreBranch { .. } | Op::DeleteBranch(_) | Op::ForceDeleteBranch(_) | Op::Switch(_) | Op::Fetch | Op::Pull) && (self.map_view || self.map.entries.is_some()) {
            self.reload_map(false, cx);
        }
    }

    /// The map's own key bindings, when it is showing.
    pub fn map_zoom_key(&mut self, factor: Option<f32>, cx: &mut Context<Self>) {
        if !self.map_view || self.map.list {
            return;
        }
        match factor {
            Some(f) => self.map.zoom_by(f, None),
            None => {
                self.map.zoom = 1.0;
                self.map.pan = (0., 0.);
            }
        }
        cx.notify();
    }
}

/// `~/x` → `$HOME/x`.
pub fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(h)) => format!("{h}/{rest}"),
        _ => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Size, parse_size_cache, size_cache_json};
    use repo::DiskUsage;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn size_cache_round_trips_build_folders() {
        let p = PathBuf::from("/w/app \"ñ\"\tx");
        let usage = DiskUsage { total: 900, files: 4, parts: vec![("target".into(), 500), ("node_modules".into(), 200)], partial: false, touched: 1_700_000_000, artifacts: vec![(p.join("node_modules"), 200), (p.join("target"), 500)], incremental: vec![(p.join("target/debug/incremental"), 100)] };
        let mut sizes = HashMap::new();
        sizes.insert(p.clone(), Size::Done { usage: usage.clone(), at: 42 });
        sizes.insert(PathBuf::from("/w/measuring"), Size::Measuring);
        sizes.insert(PathBuf::from("/w/partial"), Size::Done { usage: DiskUsage { partial: true, ..Default::default() }, at: 1 });
        let json = size_cache_json(&sizes);
        assert_eq!(json, size_cache_json(&sizes), "stable output");
        let back = parse_size_cache(&json);
        assert_eq!(back.len(), 1);
        match back.get(&p) {
            Some(Size::Done { usage: u, at }) => assert_eq!((u, *at), (&usage, 42)),
            other => panic!("{other:?}"),
        }
        assert!(parse_size_cache("not json").is_empty());
    }
}
