//! Window state: start screen → loading → sidebar | graph (or diff) | commit detail.
use crate::theme::{SANS, Tokens, tokens};
use crate::ui::{self, icon};
use crate::util::group;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::input::{Input, InputEvent, InputState, TextareaState};
use gpui_component::{IconName, Sizable as _, Theme};
use crate::work::Work;
use repo::{DiffLine, FileChange, History, Oid, RefKind, Repo, WorkFile};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) const CONTEXT: &str = "RamaApp";
/// Key context of the commit list area, so j/k/↑/↓ never reach the search field.
pub(crate) const LIST_CONTEXT: &str = "CommitList";
#[cfg(target_os = "macos")]
const SEARCH_KEYS: &str = "⌘F"; // i18n-ok: shortcut
#[cfg(not(target_os = "macos"))]
const SEARCH_KEYS: &str = "Ctrl+F"; // i18n-ok: shortcut
#[cfg(target_os = "macos")]
const FILTER_KEYS: &str = "⌘⇧F"; // i18n-ok: shortcut
#[cfg(not(target_os = "macos"))]
const FILTER_KEYS: &str = "Ctrl+Shift+F"; // i18n-ok: shortcut
#[cfg(target_os = "macos")]
const MAP_KEYS: &str = "⌘M"; // i18n-ok: shortcut
#[cfg(not(target_os = "macos"))]
const MAP_KEYS: &str = "Ctrl+M"; // i18n-ok: shortcut
const SEARCH_MIN: f32 = 180.;
const SEARCH_MAX: f32 = 520.;
const TOAST_FOR: Duration = Duration::from_secs(4);
/// Commits loaded per repository; enough for years of history in most projects.
pub(crate) const HISTORY_LIMIT: usize = 50_000;

actions!(rama, [OpenRepo, Reload, SelectPrev, SelectNext, CloseDiff, CopySha, FocusSearch, FocusFilter, ShowAbout, ToggleMap, ZoomIn, ZoomOut, ZoomReset]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-o", OpenRepo, Some(CONTEXT)),
        KeyBinding::new("secondary-r", Reload, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrev, Some(LIST_CONTEXT)),
        KeyBinding::new("k", SelectPrev, Some(LIST_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(LIST_CONTEXT)),
        KeyBinding::new("j", SelectNext, Some(LIST_CONTEXT)),
        KeyBinding::new("secondary-c", CopySha, Some(LIST_CONTEXT)),
        KeyBinding::new("escape", CloseDiff, Some(CONTEXT)),
        KeyBinding::new("secondary-f", FocusSearch, Some(CONTEXT)),
        KeyBinding::new("secondary-shift-f", FocusFilter, Some(CONTEXT)),
        // only while the list has focus, so "/" still types in text fields
        KeyBinding::new("/", FocusFilter, Some(LIST_CONTEXT)),
        KeyBinding::new("secondary-m", ToggleMap, Some(CONTEXT)),
        KeyBinding::new("secondary-=", ZoomIn, Some(CONTEXT)),
        KeyBinding::new("secondary-+", ZoomIn, Some(CONTEXT)),
        KeyBinding::new("secondary--", ZoomOut, Some(CONTEXT)),
        KeyBinding::new("secondary-0", ZoomReset, Some(CONTEXT)),
    ]);
}

pub(crate) fn sync_theme(window: Option<&mut Window>, cx: &mut App) {
    Theme::sync_system_appearance(window, cx);
    Theme::global_mut(cx).font_family = SANS.into();
}

pub enum Phase {
    Start,
    Loading { path: PathBuf, started: Instant },
    Ready,
    Failed { path: PathBuf, message: String },
}

/// What the right panel shows for the selected commit; filled in from a background task.
pub struct Details {
    pub id: Oid,
    pub message: Option<String>,
    pub changes: Option<Result<Rc<Vec<FileChange>>, String>>,
}

pub struct DiffState {
    pub file: FileChange,
    pub lines: Option<Result<Rc<Vec<DiffLine>>, String>>,
    /// A working-copy file, and whether this is its staged side.
    pub work: Option<(WorkFile, bool)>,
}

pub struct RamaApp {
    pub phase: Phase,
    pub repo: Option<Repo>,
    pub history: Option<Arc<History>>,
    /// refs (indices into `history.refs`) per display row
    pub refs_by_row: Rc<HashMap<usize, Vec<usize>>>,
    load_gen: u64,
    pub selected: Option<usize>,
    pub details: Option<Details>,
    pub diff: Option<DiffState>,
    pub collapsed: HashSet<crate::sidebar::Sec>,
    /// Sections folded by hand while a filter is active; reset when the filter changes.
    pub filter_collapsed: HashSet<crate::sidebar::Sec>,
    pub side_search: Entity<InputState>,
    /// Local branches by most recent commit instead of by name.
    pub side_recent: bool,
    pub(crate) side_cache: Option<crate::sidebar::SideCache>,
    pub list_scroll: UniformListScrollHandle,
    pub side_scroll: UniformListScrollHandle,
    pub diff_scroll: UniformListScrollHandle,
    pub recent: Vec<PathBuf>,
    pub search: Entity<InputState>,
    /// Rows matching the search, top to bottom; `None` when the field is empty.
    pub matches: Option<Arc<Vec<usize>>>,
    /// `matches` as a per-row mask, for dimming the rest of the graph.
    pub match_mask: Option<Rc<Vec<bool>>>,
    pub match_ix: usize,
    search_gen: u64,
    placeholder_locale: &'static str,
    focus: FocusHandle,
    pub list_focus: FocusHandle,
    toast: Option<(String, Instant)>,
    /// The About modal is open.
    pub about: bool,
    logo: Arc<Image>,
    pub work: Work,
    /// The worktree map replaces the graph.
    pub map_view: bool,
    pub map: crate::map::MapState,
    pub cleanup: Option<crate::cleanup::CleanupState>,
    /// Branches deleted by cleanup this session, to restore (name, commit).
    pub undo: Vec<(String, Oid)>,
    pub agents: crate::agents_view::AgentBadges,
    /// The agent conversation open in the map's side panel, and whether its tab shows.
    pub convo: Option<crate::convo_view::Convo>,
    pub convo_tab: bool,
    pub(crate) prompt_sub: Option<Subscription>,
    _subs: Vec<Subscription>,
}

impl RamaApp {
    pub fn new(initial: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        sync_theme(Some(window), cx);
        let search = cx.new(|cx| InputState::new(window, cx));
        let side_search = cx.new(|cx| InputState::new(window, cx));
        let message = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 10));
        let subs = vec![
            cx.subscribe_in(&message, window, |this, _, ev: &InputEvent, window, cx| {
                // ⌘↩ / Ctrl+↩ commits; the textarea has already inserted a newline for it
                if let InputEvent::PressEnter { secondary: true, .. } = ev {
                    let v = this.work.message.read(cx).value().to_string();
                    if let Some(trimmed) = v.strip_suffix('\n') {
                        let trimmed = trimmed.to_string();
                        this.work.message.update(cx, |m, cx| m.set_value(trimmed, window, cx));
                    }
                    this.commit(false, cx);
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() && matches!(this.phase, Phase::Ready) {
                    this.refresh(false, cx);
                    if this.map_view {
                        this.reload_map(false, cx);
                    }
                }
            }),
            window.observe_window_appearance(|window, cx| sync_theme(Some(window), cx)),
            cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => this.run_search(cx),
                InputEvent::PressEnter { shift, .. } => this.step_match(if *shift { -1 } else { 1 }, window, cx),
                _ => {}
            }),
            cx.subscribe_in(&side_search, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    this.filter_collapsed.clear();
                    this.side_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.filter_enter(window, cx),
                _ => {}
            }),
        ];
        let focus = cx.focus_handle();
        let list_focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut this = RamaApp {
            phase: Phase::Start,
            repo: None,
            history: None,
            refs_by_row: Rc::new(HashMap::new()),
            load_gen: 0,
            selected: None,
            details: None,
            diff: None,
            collapsed: HashSet::new(),
            filter_collapsed: HashSet::new(),
            side_search,
            side_recent: crate::settings::Settings::load().side_recent(),
            side_cache: None,
            list_scroll: UniformListScrollHandle::new(),
            side_scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
            recent: crate::settings::recent(),
            search,
            matches: None,
            match_mask: None,
            match_ix: 0,
            search_gen: 0,
            placeholder_locale: "",
            focus,
            list_focus,
            toast: None,
            about: false,
            logo: Arc::new(Image::from_bytes(ImageFormat::Png, crate::ICON_PNG.to_vec())),
            work: Work::new(message),
            map_view: false,
            map: crate::map::MapState::default(),
            cleanup: None,
            undo: Vec::new(),
            agents: crate::agents_view::AgentBadges::default(),
            convo: None,
            convo_tab: false,
            prompt_sub: None,
            _subs: subs,
        };
        this.check_git(cx);
        crate::star::refresh(crate::settings::Settings::load().github_stars(), cx);
        if let Some(p) = initial {
            this.open(p, window, cx);
        }
        this
    }

    pub fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.load_gen += 1;
        let load_gen = self.load_gen;
        self.phase = Phase::Loading { path: path.clone(), started: Instant::now() };
        let p = path.clone();
        let task = cx.background_executor().spawn(async move {
            let repo = Repo::discover(&p)?;
            let history = repo.history(HISTORY_LIMIT)?;
            Ok::<_, repo::Error>((repo, history))
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                if this.load_gen != load_gen {
                    return;
                }
                match res {
                    Ok((repo, history)) => this.loaded(repo, history, window, cx),
                    Err(e) => this.phase = Phase::Failed { path, message: open_error(&e) },
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        // keep the elapsed counter moving
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                let loading = this.update(cx, |this, cx| {
                    let l = this.load_gen == load_gen && matches!(this.phase, Phase::Loading { .. });
                    if l {
                        cx.notify();
                    }
                    l
                });
                if !matches!(loading, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn loaded(&mut self, repo: Repo, history: History, window: &mut Window, cx: &mut Context<Self>) {
        let root = repo.root();
        crate::settings::add_recent(&root);
        self.recent = crate::settings::recent();
        window.set_window_title(&format!("{} — {}", repo.name(), crate::APP_NAME));
        let by_row = refs_by_row(&history);
        // big remote lists start folded so local branches stay in view
        let remotes = history.refs.iter().filter(|r| r.kind == RefKind::Remote).count();
        let tags = history.refs.iter().filter(|r| r.kind == RefKind::Tag).count();
        self.collapsed = HashSet::new();
        if remotes > 40 {
            self.collapsed.insert(crate::sidebar::Sec::Remote);
        }
        if tags > 40 {
            self.collapsed.insert(crate::sidebar::Sec::Tag);
        }
        let same = self.repo.as_ref().is_some_and(|r| r.root() == root);
        let keep = self.selected_id().filter(|_| same);
        if !same {
            let message = self.work.message.clone();
            message.update(cx, |m, cx| m.set_value("", window, cx));
            let git_missing = self.work.git_missing;
            self.work = Work::new(message);
            self.work.git_missing = git_missing;
            self.map.cancel_sizes();
            self.map = crate::map::MapState::default();
            self.cleanup = None;
            self.undo.clear();
            self.agents.clear();
            self.convo = None;
            self.convo_tab = false;
        }
        let wip = self.work.wip;
        self.refs_by_row = Rc::new(by_row);
        self.repo = Some(repo);
        let head_row = history.head.and_then(|h| history.index.get(&h).copied());
        let sel = keep.and_then(|id| history.index.get(&id).copied()).or(head_row).or((!history.commits.is_empty()).then_some(0));
        self.history = Some(Arc::new(history));
        self.phase = Phase::Ready;
        self.list_focus.focus(window, cx);
        self.run_search(cx);
        self.diff = None;
        self.details = None;
        self.selected = None;
        if wip {
            self.select_wip(cx);
        } else if let Some(r) = sel {
            self.select(r, true, cx);
        }
        self.refresh(false, cx);
        self.watch(root, cx);
        self.start_agent_poll(cx);
        if self.map_view {
            self.show_map(true, cx);
        }
    }

    /// A refreshed history after a git operation or a change on disk: keep the selected
    /// commit (its details are still valid) or fall back to HEAD if it's gone.
    pub(crate) fn replace_history(&mut self, history: History, cx: &mut Context<Self>) {
        let keep = self.selected_id();
        self.refs_by_row = Rc::new(refs_by_row(&history));
        let row = keep.and_then(|id| history.index.get(&id).copied());
        let head_row = history.head.and_then(|h| history.index.get(&h).copied());
        self.history = Some(Arc::new(history));
        self.run_search(cx);
        match (keep, row) {
            (Some(_), Some(r)) => self.selected = Some(r),
            (Some(_), None) => {
                self.selected = None;
                self.details = None;
                self.diff = None;
                if let Some(r) = head_row {
                    self.select(r, true, cx);
                }
            }
            (None, _) => {}
        }
    }

    fn selected_id(&self) -> Option<Oid> {
        Some(self.history.as_ref()?.commits.get(self.selected?)?.id)
    }

    pub fn open_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some(i18n::t!("app.start.open_prompt").into()) });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| this.open(p, window, cx)).ok();
            }
        })
        .detach();
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = match (&self.phase, &self.repo) {
            (Phase::Failed { path, .. }, _) => path.clone(),
            (_, Some(r)) => r.root(),
            _ => return,
        };
        self.open(path, window, cx);
    }

    pub fn toast(&mut self, text: String, cx: &mut Context<Self>) {
        let at = Instant::now();
        self.toast = Some((text, at));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_FOR).await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|(_, t)| *t == at) {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Select a commit row and load its message and changed files in the background.
    pub fn select(&mut self, row: usize, scroll: bool, cx: &mut Context<Self>) {
        let (Some(h), Some(repo)) = (&self.history, &self.repo) else { return };
        let Some(c) = h.commits.get(row) else { return };
        if scroll {
            self.list_scroll.scroll_to_item(row, ScrollStrategy::Center);
        }
        if self.selected == Some(row) {
            return;
        }
        self.selected = Some(row);
        self.work.wip = false;
        self.work.stash = None;
        self.diff = None;
        let id = c.id;
        self.details = Some(Details { id, message: None, changes: None });
        let repo = repo.clone();
        let task = cx.background_executor().spawn(async move {
            let msg = repo.message(id).map_err(|e| e.to_string());
            let changes = repo.changes(id).map_err(|e| e.to_string());
            (msg, changes)
        });
        cx.spawn(async move |this, cx| {
            let (msg, changes) = task.await;
            this.update(cx, |this, cx| {
                if let Some(d) = this.details.as_mut().filter(|d| d.id == id) {
                    d.message = Some(msg.unwrap_or_default());
                    d.changes = Some(changes.map(Rc::new));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn select_id(&mut self, id: Oid, cx: &mut Context<Self>) {
        match self.history.as_ref().and_then(|h| h.index.get(&id).copied()) {
            Some(row) => self.select(row, true, cx),
            None => self.toast(i18n::t!("app.toast.not_loaded"), cx),
        }
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.diff.is_some() {
            return;
        }
        let Some(h) = &self.history else { return };
        let n = h.commits.len() as isize;
        if self.selected == Some(0) && delta < 0 && self.work.status.is_some() {
            self.select_wip(cx);
            return;
        }
        if n == 0 {
            return;
        }
        let cur = self.selected.map(|s| s as isize).unwrap_or(-1);
        let next = (cur + delta).clamp(0, n - 1) as usize;
        self.select(next, true, cx);
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().to_string()
    }

    /// Re-run the search in the background; results land only if nothing newer was typed.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        self.search_gen += 1;
        let search_gen = self.search_gen;
        let query = self.query(cx);
        let Some(h) = self.history.clone().filter(|_| !query.trim().is_empty()) else {
            if self.matches.take().is_some() {
                self.match_mask = None;
                cx.notify();
            }
            return;
        };
        let task = cx.background_executor().spawn(async move { h.search(&query) });
        cx.spawn(async move |this, cx| {
            let rows = task.await;
            this.update(cx, |this, cx| {
                if this.search_gen != search_gen {
                    return;
                }
                let n = this.history.as_ref().map(|h| h.commits.len()).unwrap_or(0);
                let mut mask = vec![false; n];
                rows.iter().for_each(|&r| mask[r] = true);
                // jump to the first match at or below the current selection
                let from = this.selected.unwrap_or(0);
                this.match_ix = rows.iter().position(|&r| r >= from).unwrap_or(0);
                let first = rows.get(this.match_ix).copied();
                this.matches = Some(Arc::new(rows));
                this.match_mask = Some(Rc::new(mask));
                if let Some(r) = first {
                    this.select(r, true, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Enter / Shift+Enter in the search field: next / previous match, wrapping around.
    fn step_match(&mut self, delta: isize, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.matches.clone().filter(|m| !m.is_empty()) else { return };
        let n = m.len() as isize;
        self.match_ix = (self.match_ix as isize + delta).rem_euclid(n) as usize;
        self.diff = None;
        self.select(m[self.match_ix], true, cx);
        cx.notify();
    }

    fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Ready) {
            self.side_search.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Ready) {
            self.search.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    /// Esc: close the About modal, else the diff, else clear the search and go back to the list.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.map.dialog.take().is_some() || self.cleanup.take().is_some() {
            cx.notify();
            return;
        }
        if self.about || self.work.confirm.is_some() || self.work.prompt.is_some() {
            self.about = false;
            self.work.confirm = None;
            self.work.prompt = None;
            self.prompt_sub = None;
            cx.notify();
            return;
        }
        if self.side_search.read(cx).focus_handle(cx).is_focused(window) {
            if !self.side_search.read(cx).value().is_empty() {
                self.side_search.update(cx, |s, cx| s.set_value("", window, cx));
                self.filter_collapsed.clear();
                cx.notify();
            } else {
                self.list_focus.focus(window, cx);
            }
            return;
        }
        if self.diff.take().is_some() {
            cx.notify();
            return;
        }
        if !self.query(cx).is_empty() {
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
            self.run_search(cx);
        }
        self.list_focus.focus(window, cx);
    }

    /// The placeholder is stored text: rebuild it when the language changes.
    fn sync_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let loc = i18n::locale();
        if self.placeholder_locale != loc {
            self.placeholder_locale = loc;
            let text = i18n::t!("app.search.placeholder", keys = SEARCH_KEYS);
            self.search.update(cx, |s, cx| s.set_placeholder(text, window, cx));
            let text = i18n::t!("app.side.filter_placeholder", keys = FILTER_KEYS);
            self.side_search.update(cx, |s, cx| s.set_placeholder(text, window, cx));
            let text = i18n::t!("app.work.message_placeholder", keys = crate::work_view::COMMIT_KEYS);
            self.work.message.update(cx, |s, cx| s.set_placeholder(text, window, cx));
        }
        if std::mem::take(&mut self.work.clear_message) {
            self.work.message.update(cx, |s, cx| s.set_value("", window, cx));
        }
    }

    pub fn open_diff(&mut self, file: FileChange, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        self.diff = Some(DiffState { file: file.clone(), lines: None, work: None });
        self.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);
        let path = file.path.clone();
        let task = cx.background_executor().spawn(async move { repo.diff(&file).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                if let Some(d) = this.diff.as_mut().filter(|d| d.file.path == path && d.work.is_none()) {
                    d.lines = Some(res.map(Rc::new));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn copy_sha(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected_id() {
            cx.write_to_clipboard(ClipboardItem::new_string(id.to_string()));
            self.toast(i18n::t!("app.toast.copied", sha = id.to_hex_with_len(7)), cx);
        }
    }

    fn set_language(&mut self, code: &'static str, cx: &mut Context<Self>) {
        if i18n::set_locale(code) {
            crate::settings::Settings::update(|s| s.language = Some(code.to_string()));
            gpui_component::set_locale(code);
            crate::set_app_menus(cx);
            cx.refresh_windows();
        }
    }

    fn set_github_stars(&mut self, on: bool, cx: &mut Context<Self>) {
        crate::settings::Settings::update(|s| s.github_stars = (!on).then(|| "off".into())); // i18n-ok: setting value
        crate::star::refresh(on, cx);
        cx.refresh_windows();
    }

    // ── rendering ─────────────────────────────────────────────────────────────────────────────

    fn commit_count(h: &History) -> String {
        if h.truncated { i18n::t!("app.topbar.commits_truncated", n = group(h.commits.len())) } else { i18n::tn!("app.topbar.commits", h.commits.len(), count = group(h.commits.len())) }
    }

    fn render_topbar(&self, t: &Tokens, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = topbar_mode(width);
        let ready = matches!(self.phase, Phase::Ready);
        let mut left = div().flex().items_center().gap(px(10.)).min_w_0().flex_shrink_1();
        left = left.child(div().flex().flex_none().items_center().justify_center().size(px(30.)).rounded(px(8.)).bg(t.brand).child(svg().path(crate::LOGO_SVG).size(px(17.)).text_color(gpui::white())));
        match (&self.repo, &self.history, &self.phase) {
            (Some(repo), Some(h), Phase::Ready) => {
                let branch = h.head_branch.clone().unwrap_or_else(|| i18n::t!("app.topbar.detached", sha = h.head.map(|id| id.to_hex_with_len(7).to_string()).unwrap_or_default()));
                let tip = (branch.clone(), Self::commit_count(h));
                let muted = t.ink_muted;
                // the view switch never shrinks; the repo name and the branch truncate instead
                left = left
                    .child(div().min_w(px(40.)).flex_shrink_1().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).truncate().child(repo.name()))
                    .child(self.render_view_toggle(t, cx))
                    .child(
                        div()
                            .id("branch-pill")
                            .flex()
                            .flex_shrink_1()
                            .min_w(px(64.))
                            .items_center()
                            .gap(px(5.))
                            .px(px(9.))
                            .py(px(3.))
                            .rounded_full()
                            .bg(t.accent_soft)
                            .text_color(t.accent)
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(ui::branch_icon(12., t.accent))
                            .child(div().min_w_0().truncate().child(branch))
                            .tooltip(move |window, cx| {
                                let (b, n) = tip.clone();
                                gpui_component::tooltip::Tooltip::element(move |_, _| div().flex().flex_col().child(b.clone()).child(div().text_color(muted).child(n.clone()))).build(window, cx)
                            }),
                    );
            }
            _ => left = left.child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(crate::APP_NAME)),
        }
        let can_reload = matches!(self.phase, Phase::Ready | Phase::Failed { .. });
        let current = i18n::locale();
        let weak = cx.entity().downgrade();
        let stars_on = crate::star::enabled(cx);
        let lang = Button::new("lang").ghost().icon(IconName::Globe).with_size(px(34.)).tooltip(i18n::t!("common.language")).dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
            menu = menu.label(i18n::t!("common.language"));
            for (code, name) in i18n::available() {
                let w = weak.clone();
                menu = menu.item(PopupMenuItem::new(name).checked(code == current).on_click(move |_, _, cx| {
                    w.update(cx, |this, cx| this.set_language(code, cx)).ok();
                }));
            }
            let w = weak.clone();
            menu = menu.separator().item(PopupMenuItem::new(i18n::t!("app.star.setting")).checked(stars_on).on_click(move |_, _, cx| {
                w.update(cx, |this, cx| this.set_github_stars(!stars_on, cx)).ok();
            }));
            menu = menu.label(i18n::t!("app.star.setting_hint"));
            let w = weak.clone();
            menu.separator().item(PopupMenuItem::new(i18n::t!("app.about.menu", app = crate::APP_NAME)).on_click(move |_, _, cx| {
                w.update(cx, |this, cx| this.show_about(cx)).ok();
            }))
        });
        let compact = bar != Bar::Full;
        let open_tip = i18n::t!("app.topbar.open");
        let open = ui::outline_button(t, "open")
            .child(icon(IconName::FolderOpen, 15., t.ink))
            .when(compact, |d| d.px(px(9.)).tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(open_tip.clone()).build(window, cx)))
            .when(!compact, |d| d.child(i18n::t!("app.topbar.open")))
            .on_click(cx.listener(|this, _, window, cx| this.open_dialog(window, cx)));
        let tools = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .child(ui::icon_button(t, "reload", IconName::Redo, can_reload).when(can_reload, |d| d.on_click(cx.listener(|this, _, window, cx| this.reload(window, cx)))))
            .child(open)
            .child(lang);
        let one_row = bar != Bar::TwoRows;
        let row1 = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(52.))
            .child(left)
            .child(div().flex_1())
            .when(ready && one_row, |d| d.child(self.render_search(t, false, cx)).child(self.render_remote_buttons(t, compact, cx)))
            .child(tools);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .px(px(12.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(row1)
            .when(ready && !one_row, |d| d.child(div().flex().items_center().gap(px(8.)).pb(px(9.)).child(self.render_search(t, true, cx)).child(self.render_remote_buttons(t, true, cx))))
    }

    /// Graph | Map switch (⌘M).
    fn render_view_toggle(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let map = self.map_view;
        let seg = |id: &'static str, ic: IconName, label: String, on: bool| {
            div().id(id).flex().items_center().gap(px(5.)).h(px(26.)).px(px(9.)).rounded(px(7.)).text_size(px(12.)).cursor_pointer().when(on, |d| d.bg(t.surface).font_weight(FontWeight::SEMIBOLD).shadow_sm()).when(!on, |d| d.text_color(t.ink_muted)).child(icon(ic, 13., if on { t.ink } else { t.ink_muted })).child(label)
        };
        let tip = i18n::t!("app.map.toggle_tip", keys = MAP_KEYS);
        div()
            .id("view-toggle")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .ml(px(6.))
            .p(px(2.))
            .rounded(px(9.))
            .bg(t.border.opacity(0.6))
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
            .child(seg("view-graph", IconName::GalleryVerticalEnd, i18n::t!("app.map.graph"), !map).on_click(cx.listener(|this, _, _, cx| this.show_map(false, cx))))
            .child(seg("view-map", IconName::Map, i18n::t!("app.map.tab"), map).on_click(cx.listener(|this, _, _, cx| this.show_map(true, cx))))
    }

    /// Flexible: up to 520 px, and the first thing to give way (down to 180 px) when the bar is
    /// short of room. `fill`: the second row, where it takes what the buttons leave.
    fn render_search(&self, t: &Tokens, fill: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.matches.as_ref().map(|m| {
            if m.is_empty() {
                (i18n::t!("app.search.none"), t.danger)
            } else {
                (i18n::t!("app.search.count", i = group(self.match_ix + 1), n = group(m.len())), t.ink_muted)
            }
        });
        let nav = |id: &'static str, ic: IconName, delta: isize| {
            ui::icon_button(t, id, ic, true).size(px(24.)).on_click(cx.listener(move |this, _, window, cx| this.step_match(delta, window, cx)))
        };
        let has = self.matches.as_ref().is_some_and(|m| !m.is_empty());
        let mut bar = div().flex().items_center().gap(px(6.)).min_w(px(SEARCH_MIN)).max_w(px(SEARCH_MAX));
        if fill {
            bar = bar.flex_1();
        } else {
            bar = bar.w(px(SEARCH_MAX));
            bar.style().flex_shrink = Some(40.);
        }
        bar.h(px(34.))
            .pl(px(10.))
            .pr(px(4.))
            .rounded(px(10.))
            .border_1()
            .border_color(t.border)
            .bg(t.canvas)
            .child(icon(IconName::Search, 14., t.ink_muted))
            .child(div().flex_1().min_w_0().text_size(px(13.)).child(Input::new(&self.search).appearance(false).small()))
            .children(count.map(|(text, color)| div().flex_none().text_size(px(11.)).text_color(color).child(text)))
            .when(has, |d| d.child(nav("match-prev", IconName::ChevronUp, -1)).child(nav("match-next", IconName::ChevronDown, 1)))
    }

    fn render_start(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let recent = self.recent.iter().enumerate().map(|(i, p)| {
            let p2 = p.clone();
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
            let hover = t.canvas;
            div()
                .id(("recent", i))
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(10.))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(icon(IconName::Folder, 15., t.ink_muted))
                .child(div().flex().flex_col().min_w_0().child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(name)).child(div().text_size(px(11.)).text_color(t.ink_muted).truncate().child(p.display().to_string())))
                .on_click(cx.listener(move |this, _, window, cx| this.open(p2.clone(), window, cx)))
        });
        div()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(22.))
            .size_full()
            .child(img(self.logo.clone()).size(px(112.)))
            .child(div().flex().flex_col().items_center().gap(px(6.)).child(div().text_size(px(26.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.start.title"))).child(div().max_w(px(520.)).text_center().text_size(px(14.)).text_color(t.ink_muted).child(i18n::t!("app.start.subtitle"))))
            .child(ui::filled_button(t, "open-big", t.accent).child(icon(IconName::FolderOpen, 15., t.on_fill())).child(i18n::t!("app.start.open")).on_click(cx.listener(|this, _, window, cx| this.open_dialog(window, cx))))
            .when(!self.recent.is_empty(), |d| d.child(div().flex().flex_col().w(px(420.)).gap(px(2.)).child(div().px(px(12.)).pb(px(4.)).child(ui::section_title(t, i18n::t!("app.start.recent")))).children(recent)))

    }

    fn render_loading(&self, t: &Tokens, path: &std::path::Path, started: Instant) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .size_full()
            .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.loading.title")))
            .child(div().text_size(px(13.)).text_color(t.ink_muted).child(path.display().to_string()))
            .child(div().text_size(px(12.)).text_color(t.ink_muted).child(format!("{:.1} s", started.elapsed().as_secs_f32())))
    }

    fn render_failed(&self, t: &Tokens, path: &std::path::Path, message: &str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .size_full()
            .child(icon(IconName::TriangleAlert, 32., t.danger))
            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.error.title")))
            .child(div().text_size(px(13.)).text_color(t.ink_muted).child(path.display().to_string()))
            .child(div().max_w(px(520.)).text_center().text_size(px(13.)).child(message.to_string()))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(ui::outline_button(t, "back").child(i18n::t!("app.error.back")).on_click(cx.listener(|this, _, window, cx| {
                        this.phase = if this.history.is_some() { Phase::Ready } else { Phase::Start };
                        if this.history.is_none() {
                            window.set_window_title(crate::APP_NAME);
                        }
                        cx.notify();
                    })))
                    .child(ui::filled_button(t, "choose", t.accent).child(i18n::t!("app.start.open")).on_click(cx.listener(|this, _, window, cx| this.open_dialog(window, cx)))),
            )
    }

    pub fn show_about(&mut self, cx: &mut Context<Self>) {
        self.about = true;
        cx.notify();
    }

    /// Slim bar at the bottom of every screen: where you are on the left, About on the right.
    fn render_footer(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let left = match (&self.repo, &self.phase) {
            (Some(r), Phase::Ready) => match &self.history {
                Some(h) => format!("{} · {}", r.root().display(), Self::commit_count(h)), // i18n-ok: separator
                None => r.root().display().to_string(),
            },
            _ => String::new(),
        };
        let ink = t.ink;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(px(26.))
            .px(px(12.))
            .border_t_1()
            .border_color(t.border)
            .bg(t.surface)
            .text_size(px(11.))
            .text_color(t.ink_muted)
            .child(div().flex_1().min_w_0().truncate().child(left))
            .when(matches!(self.phase, Phase::Ready), |d| d.child(self.render_output_toggle(t, cx)))
            .child(crate::star::footer_link(t, cx))
            .child(
                div()
                    .id("footer-about")
                    .flex_none()
                    .cursor_pointer()
                    .hover(move |s| s.text_color(ink).underline())
                    .child(i18n::t!("app.about.footer", app = crate::APP_NAME, version = crate::VERSION, company = crate::COMPANY))
                    .on_click(cx.listener(|this, _, _, cx| this.show_about(cx))),
            )
    }

    fn render_about(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = t.accent;
        let card = div()
            .id("about-card")
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.))
            .w(px(420.))
            .px(px(32.))
            .pt(px(32.))
            .pb(px(26.))
            .rounded(px(18.))
            .border_1()
            .border_color(t.border)
            .bg(t.surface)
            .shadow_lg()
            // clicks inside the card must not reach the backdrop
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().absolute().top(px(10.)).right(px(10.)).child(ui::icon_button(t, "about-close", IconName::Close, true).on_click(cx.listener(|this, _, _, cx| {
                this.about = false;
                cx.notify();
            }))))
            .child(img(self.logo.clone()).size(px(88.)))
            .child(div().flex().items_baseline().gap(px(8.)).child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).child(crate::APP_NAME)).child(div().text_size(px(13.)).text_color(t.ink_muted).child(crate::VERSION)))
            .child(div().text_center().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).child(i18n::t!("app.about.description")))
            .child(div().w_full().h(px(1.)).my(px(4.)).bg(t.border))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(2.))
                    .child(div().text_size(px(13.)).child(i18n::t!("app.about.by", company = crate::COMPANY)))
                    .child(div().text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.about.place"))),
            )
            .child(ui::filled_button(t, "about-site", t.accent).mt(px(4.)).child(i18n::t!("app.about.visit")).on_click(|_, _, cx| cx.open_url(crate::COMPANY_URL)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(div().id("about-source").text_size(px(12.)).text_color(accent).cursor_pointer().hover(|s| s.underline()).child(i18n::t!("app.about.source")).on_click(|_, _, cx| cx.open_url(crate::REPO_URL)))
                    .child(crate::star::about_button(t, cx)),
            )
            .child(crate::star::about_ask(t))
            .child(div().text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.about.license")));
        div()
            .id("about-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(if t.dark { 0.55 } else { 0.32 }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.about = false;
                    cx.notify();
                }),
            )
            .child(card)
    }

    fn render_toast(&self, t: &Tokens) -> Option<impl IntoElement> {
        let (text, _) = self.toast.as_ref()?;
        Some(div().absolute().bottom(px(32.)).left_0().right_0().flex().justify_center().child(div().px(px(16.)).py(px(10.)).rounded(px(12.)).bg(t.ink).text_color(t.on_fill()).text_size(px(13.)).child(text.clone())))
    }
}

impl Render for RamaApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_placeholder(window, cx);
        let t = tokens(cx);
        let body: AnyElement = match &self.phase {
            Phase::Start => self.render_start(&t, cx).into_any_element(),
            Phase::Loading { path, started } => {
                let (path, started) = (path.clone(), *started);
                self.render_loading(&t, &path, started).into_any_element()
            }
            Phase::Failed { path, message } => {
                let (path, message) = (path.clone(), message.clone());
                self.render_failed(&t, &path, &message, cx).into_any_element()
            }
            Phase::Ready if self.map_view => {
                let center = self.render_map(&t, f32::from(window.viewport_size().width), cx);
                let right = self.render_map_panel(&t, cx);
                div()
                    .flex()
                    .size_full()
                    .min_h_0()
                    .child(self.render_sidebar(&t, cx))
                    .child(div().flex().flex_col().flex_1().min_w_0().h_full().children(self.render_op_failure(&t, cx)).child(div().flex_1().min_h_0().child(center)).child(self.render_output(&t, cx)))
                    .child(right)
                    .into_any_element()
            }
            Phase::Ready => {
                let center = if self.diff.is_some() { self.render_diff(&t, cx).into_any_element() } else { self.render_graph(&t, window, cx).into_any_element() };
                let right = if self.work.wip {
                    self.render_work(&t, cx).into_any_element()
                } else if self.work.stash.is_some() {
                    self.render_stash(&t, cx).into_any_element()
                } else {
                    self.render_detail(&t, cx).into_any_element()
                };
                // j/k/↑/↓ belong to the list; the commit box on the right must get its letters
                // the sidebar sits outside the list context: its filter field must get j / k / "/"
                div()
                    .flex()
                    .size_full()
                    .min_h_0()
                    .child(self.render_sidebar(&t, cx))
                    .child(
                        div()
                            .key_context(LIST_CONTEXT)
                            .track_focus(&self.list_focus)
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(div().flex().flex_col().flex_1().min_w_0().h_full().children(self.render_op_failure(&t, cx)).child(div().flex_1().min_h_0().child(center)).child(self.render_output(&t, cx))),
                    )
                    .child(right)
                    .into_any_element()
            }
        };
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &OpenRepo, window, cx| this.open_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &Reload, window, cx| this.reload(window, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &CloseDiff, window, cx| this.escape(window, cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| this.focus_search(window, cx)))
            .on_action(cx.listener(|this, _: &FocusFilter, window, cx| this.focus_filter(window, cx)))
            .on_action(cx.listener(|this, _: &ShowAbout, _, cx| this.show_about(cx)))
            .on_action(cx.listener(|this, _: &CopySha, _, cx| this.copy_sha(cx)))
            .on_action(cx.listener(|this, _: &ToggleMap, _, cx| {
                if matches!(this.phase, Phase::Ready) {
                    this.show_map(!this.map_view, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.map_zoom_key(Some(1.2), cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.map_zoom_key(Some(1. / 1.2), cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.map_zoom_key(None, cx)))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.canvas)
            .text_color(t.ink)
            .font_family(SANS)
            .child(self.render_topbar(&t, f32::from(window.viewport_size().width), cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_footer(&t, cx))
            .children(self.render_toast(&t))
            .when(self.about, |d| d.child(self.render_about(&t, cx)))
            .children(self.render_modal(&t, cx))
            .children(self.render_cleanup(&t, cx))
            .children(self.render_map_dialog(&t, cx))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bar {
    /// Labelled buttons on one row.
    Full,
    /// Icons and counts only.
    Compact,
    /// Search and git buttons on a second row.
    TwoRows,
}

/// Rough widths of what the top bar holds (labels in the longest language, French), to decide
/// before layout how much it can show.
const BAR_LEFT_MIN: f32 = 30. + 40. + 160. + 64. + 30.;
const BAR_BUTTONS_FULL: f32 = 130. + 8. + 3. * 95. + 24.;
const BAR_BUTTONS_COMPACT: f32 = 64. + 8. + 3. * 52. + 24.;
const BAR_TOOLS_FULL: f32 = 34. + 105. + 34. + 16.;
const BAR_TOOLS_COMPACT: f32 = 34. + 34. + 34. + 16.;
const BAR_CHROME: f32 = 24. + 4. * 8.;
/// Room left for a long branch name before labels go.
const BAR_SLACK: f32 = 120.;

pub(crate) fn topbar_mode(width: f32) -> Bar {
    let need = |buttons: f32, tools: f32| BAR_CHROME + BAR_LEFT_MIN + SEARCH_MIN + buttons + tools;
    if width >= need(BAR_BUTTONS_FULL, BAR_TOOLS_FULL) + BAR_SLACK {
        Bar::Full
    } else if width >= need(BAR_BUTTONS_COMPACT, BAR_TOOLS_COMPACT) {
        Bar::Compact
    } else {
        Bar::TwoRows
    }
}

fn refs_by_row(history: &History) -> HashMap<usize, Vec<usize>> {
    let mut by_row: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, r) in history.refs.iter().enumerate() {
        if let Some(&row) = history.index.get(&r.target) {
            by_row.entry(row).or_default().push(i);
        }
    }
    by_row
}

/// Not-a-repository is the common case; anything else shows the library's own message.
fn open_error(e: &repo::Error) -> String {
    let s = e.to_string();
    if s.contains("Could not find a git repository") || s.contains("not a git repository") {
        i18n::t!("app.error.not_repo")
    } else {
        i18n::t!("app.error.other", detail = s)
    }
}

#[cfg(test)]
mod tests {
    use super::{BAR_BUTTONS_COMPACT, BAR_CHROME, BAR_LEFT_MIN, BAR_TOOLS_COMPACT, Bar, SEARCH_MIN, topbar_mode};

    #[test]
    fn topbar_fits() {
        assert_eq!(topbar_mode(1000.), Bar::Compact);
        assert_eq!(topbar_mode(1280.), Bar::Compact);
        assert_eq!(topbar_mode(1440.), Bar::Full);
        assert_eq!(topbar_mode(1920.), Bar::Full);
        assert_eq!(topbar_mode(800.), Bar::TwoRows);
        // compact really fits at 1000 px: everything at its minimum plus the search's
        const { assert!(BAR_CHROME + BAR_LEFT_MIN + SEARCH_MIN + BAR_BUTTONS_COMPACT + BAR_TOOLS_COMPACT <= 1000.) };
        // the second row holds the search and the compact buttons down to 480 px
        const { assert!(24. + SEARCH_MIN + 8. + BAR_BUTTONS_COMPACT <= 480.) };
        let mut last = Bar::TwoRows;
        for w in (400..2400).step_by(20) {
            let m = topbar_mode(w as f32);
            let rank = |b: Bar| match b {
                Bar::TwoRows => 0,
                Bar::Compact => 1,
                Bar::Full => 2,
            };
            assert!(rank(m) >= rank(last), "wider never shows less ({w} px)");
            last = m;
        }
    }
}
