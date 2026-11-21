//! AI agent badges: the poll (every few seconds, in the background, only while the window can
//! be seen), the animation, the robot element with its tooltip and click, the side panel's
//! agent rows and the legend.
use crate::agents::{self, Agent, AgentState, ErrKind, Found, Need, Tracker};
use crate::app::{Phase, RamaApp};
use crate::theme::Tokens;
use crate::util::{ago, now};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::tooltip::Tooltip;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_secs(3);
/// `git worktree list` is re-read this often while polling.
const WORKTREES_FRESH: Duration = Duration::from_secs(15);

pub struct AgentBadges {
    pub enabled: bool,
    pub reduce_motion: bool,
    /// A system notification when an agent starts needing you (macOS).
    pub notify: bool,
    /// Agents that need you go to Otherwise Apps' Today.
    pub today: bool,
    /// Agents per worktree path (as git lists it).
    pub found: Rc<Found>,
    /// Checked-out branch → its worktree, for worktrees with agents.
    pub branches: Rc<HashMap<String, PathBuf>>,
    /// The listed worktree this window shows.
    pub current: Option<PathBuf>,
    worktrees: Option<(Instant, Rc<Vec<repo::Worktree>>)>,
    tracker: Tracker,
    polling: bool,
    scanning: bool,
    animating: bool,
    epoch: Instant,
}

impl Default for AgentBadges {
    fn default() -> Self {
        let s = crate::settings::Settings::load();
        AgentBadges {
            enabled: s.agent_badges(),
            reduce_motion: s.reduce_motion(),
            notify: s.agent_notify(),
            today: s.agent_today(),
            found: Rc::new(Found::new()),
            branches: Rc::new(HashMap::new()),
            current: None,
            worktrees: None,
            tracker: Tracker::default(),
            polling: false,
            scanning: false,
            animating: false,
            epoch: Instant::now(),
        }
    }
}

impl AgentBadges {
    pub fn ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn any_animated(&self) -> bool {
        self.found.values().flatten().any(|a| crate::robot::animated(a.state))
    }

    /// Worktrees as git last listed them.
    pub fn listed(&self) -> Vec<PathBuf> {
        self.worktrees.as_ref().map(|(_, w)| w.iter().map(|w| w.path.clone()).collect()).unwrap_or_default()
    }

    pub fn clear(&mut self) {
        self.found = Rc::new(Found::new());
        self.branches = Rc::new(HashMap::new());
        self.worktrees = None;
    }
}

/// Some part of a window of ours is on screen (not hidden, minimised or covered).
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2 macros check a `cargo-clippy` feature
fn app_visible() -> bool {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    const VISIBLE: u64 = 1 << 1; // NSApplicationOcclusionStateVisible
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let state: u64 = msg_send![app, occlusionState];
        state & VISIBLE != 0
    }
}

#[cfg(not(target_os = "macos"))]
fn app_visible() -> bool {
    true
}

impl RamaApp {
    fn badges_shown(&self) -> bool {
        self.agents.enabled && matches!(self.phase, Phase::Ready) && (self.map_view || self.diff.is_none())
    }

    /// Start the poll loop once per window; it idles while the badges can't be seen.
    pub fn start_agent_poll(&mut self, cx: &mut Context<Self>) {
        if self.agents.polling {
            return;
        }
        self.agents.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |this, cx| this.poll_agents(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(POLL).await;
            }
        })
        .detach();
    }

    fn poll_agents(&mut self, cx: &mut Context<Self>) {
        if !self.agents.enabled || !self.agents.today {
            if let Some(r) = &self.repo {
                crate::otherwise::forget_repo(&r.root());
            }
        }
        if !self.agents.enabled {
            if !self.agents.found.is_empty() {
                self.agents.clear();
                cx.notify();
            }
            return;
        }
        if self.agents.scanning || !self.badges_shown() || !app_visible() {
            return;
        }
        let Some(repo) = self.repo.clone() else { return };
        self.detect_tools(cx);
        self.agents.scanning = true;
        let cached_list = self.agents.worktrees.as_ref().filter(|(at, _)| at.elapsed() < WORKTREES_FRESH).map(|(_, w)| w.as_ref().clone());
        let root = repo.root();
        let feed_root = root.clone();
        let task = cx.background_executor().spawn(async move {
            let fresh = cached_list.is_none();
            let wts = match cached_list {
                Some(w) => w,
                None => repo.worktrees().unwrap_or_default(),
            };
            let paths: Vec<PathBuf> = wts.iter().filter(|w| w.prunable.is_none() && !w.bare).map(|w| w.path.clone()).collect();
            let found = agents::detect(&paths);
            let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let here = canon(&root);
            let current = wts.iter().find(|w| canon(&w.path) == here).map(|w| w.path.clone());
            (fresh.then_some(wts), found, current)
        });
        cx.spawn(async move |this, cx| {
            let (wts, mut found, current) = task.await;
            this.update(cx, |this, cx| {
                let a = &mut this.agents;
                a.scanning = false;
                if !a.enabled {
                    return;
                }
                if let Some(w) = wts {
                    a.worktrees = Some((Instant::now(), Rc::new(w)));
                }
                let asks = a.tracker.apply(&mut found, now());
                if a.notify {
                    for (w, agent) in &asks {
                        notify_needs_you(w, agent);
                    }
                }
                let list = a.worktrees.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
                a.branches = Rc::new(list.iter().filter(|w| found.contains_key(&w.path)).filter_map(|w| Some((w.branch.clone()?, w.path.clone()))).collect());
                let changed = *a.found != found || a.current != current;
                a.found = Rc::new(found);
                a.current = current;
                if a.today {
                    crate::otherwise::set_repo_events(&feed_root, crate::otherwise::agent_events(&a.found, chrono::Utc::now()));
                }
                if changed {
                    cx.notify();
                }
                this.ensure_agent_animation(cx);
                this.refresh_convo(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Redraw at the sprite's frame rate while some shown agent is typing or waving.
    fn ensure_agent_animation(&mut self, cx: &mut Context<Self>) {
        let wanted = |this: &RamaApp| this.badges_shown() && !this.agents.reduce_motion && this.agents.any_animated();
        if self.agents.animating || !wanted(self) {
            return;
        }
        self.agents.animating = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(crate::robot::FRAME_MS)).await;
                let go = this.update(cx, |this, cx| {
                    let go = wanted(this) && app_visible();
                    if go {
                        cx.notify();
                    } else {
                        this.agents.animating = false;
                    }
                    go
                });
                if !matches!(go, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn set_agent_prefs(&mut self, enabled: bool, reduce_motion: bool, notify: bool, cx: &mut Context<Self>) {
        self.agents.enabled = enabled;
        self.agents.reduce_motion = reduce_motion;
        self.agents.notify = notify;
        if !enabled {
            self.agents.clear();
        }
        self.poll_agents(cx);
        cx.notify();
    }

    /// Bring the agent's terminal forward when we know its app (macOS), else open a terminal
    /// in the worktree.
    pub fn focus_agent(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let host = self.agents.found.get(&path).and_then(|v| v.iter().find_map(|a| a.host_app.clone()));
        if let Some(app) = host.filter(|_| cfg!(target_os = "macos")) {
            let open = crate::launch::Launch { program: "open".into(), args: vec!["-a".into(), app.display().to_string()], cwd: path.clone(), copy: None };
            if open.spawn().is_ok() {
                return;
            }
        }
        self.launch(path, None, cx);
    }

    /// The badge for the worktree at `path`, if agents are at work there.
    pub(crate) fn agent_badge(&self, t: &Tokens, path: &Path, cell: f32, id: impl Into<ElementId>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let agents = self.agents.found.get(path)?;
        Some(badge(t, agents, path.to_path_buf(), self.agents.ms(), self.agents.reduce_motion, cell, id.into(), cx))
    }

    /// The badge for a branch checked out in a worktree with agents.
    pub(crate) fn branch_badges(&self) -> BranchBadges {
        let found = self.agents.found.clone();
        let map = self.agents.branches.iter().filter_map(|(b, p)| Some((b.clone(), (p.clone(), found.get(p)?.clone())))).collect();
        BranchBadges { map: Rc::new(map), ms: self.agents.ms(), reduce_motion: self.agents.reduce_motion }
    }

    /// The side panel's list of a worktree's agents, one row per session; a click opens its
    /// conversation.
    pub(crate) fn agent_rows(&self, t: &Tokens, path: &Path, cx: &mut Context<Self>) -> Option<AnyElement> {
        let agents = self.agents.found.get(path)?;
        let (ms, reduce) = (self.agents.ms(), self.agents.reduce_motion);
        let hover = if t.dark { t.border } else { t.canvas };
        let n = now();
        let mut list = div().flex().flex_col().gap(px(2.)).child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(i18n::t!("app.agents.title")));
        for (i, a) in agents.iter().enumerate() {
            let readable = a.session_file.is_some();
            let open = self.convo.as_ref().is_some_and(|c| a.session_file.as_ref() == Some(&c.file));
            let mut sub = Vec::new();
            if let Some(title) = &a.title {
                sub.push(title.clone());
            }
            if let Some(s) = a.started {
                sub.push(i18n::t!("app.agents.started", ago = ago(s, n)));
            }
            if a.procs > 1 {
                sub.push(i18n::tn!("app.agents.procs", a.procs));
            }
            let (w, agent) = (path.to_path_buf(), a.clone());
            list = list.child(
                div()
                    .id(("agent-row", i))
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .px(px(6.))
                    .py(px(5.))
                    .rounded(px(7.))
                    .when(open, |d| d.bg(t.accent_soft))
                    .when(readable && !open, |d| d.cursor_pointer().hover(move |s| s.bg(hover)))
                    .child(div().pt(px(2.)).child(crate::robot::robot(t, a.state, ms, reduce, 2.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).child(i18n::t!("app.agents.line", agent = a.kind, state = state_label(a))))
                            .children(sub.into_iter().map(|s| div().text_size(px(11.5)).text_color(t.ink_muted).truncate().child(s)))
                            .when(!readable, |d| d.child(div().text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.convo.unavailable", agent = a.kind)))),
                    )
                    .when(readable, |d| d.child(crate::ui::icon(gpui_component::IconName::ChevronRight, 14., t.ink_muted)))
                    .when(readable, |d| d.on_click(cx.listener(move |this, _, window, cx| this.open_convo(w.clone(), agent.clone(), window, cx)))),
            );
        }
        Some(list.into_any_element())
    }

    /// What each robot means, under the map.
    pub(crate) fn agent_legend(&self, t: &Tokens) -> Option<AnyElement> {
        if !self.agents.enabled {
            return None;
        }
        let item = |state: AgentState, label: String| div().flex().flex_none().items_center().gap(px(5.)).child(crate::robot::robot(t, state, 0, true, 1.5)).child(label);
        Some(
            div()
                .flex()
                .flex_none()
                .flex_wrap()
                .items_center()
                .gap(px(12.))
                .px(px(12.))
                .py(px(5.))
                .border_t_1()
                .border_color(t.border)
                .bg(t.surface)
                .text_size(px(11.))
                .text_color(t.ink_muted)
                .child(item(AgentState::NeedsYou, i18n::t!("app.agents.legend_needs")))
                .child(item(AgentState::Working, i18n::t!("app.agents.legend_working")))
                .child(item(AgentState::Waiting, i18n::t!("app.agents.legend_waiting")))
                .child(item(AgentState::Idle, i18n::t!("app.agents.legend_idle")))
                .child(item(AgentState::Error, i18n::t!("app.agents.legend_error")))
                .into_any_element(),
        )
    }
}

fn notify_needs_you(worktree: &Path, a: &Agent) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let name = worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let title = i18n::t!("app.agents.notify_title", agent = a.kind);
    let body = i18n::t!("app.agents.notify_body", worktree = name, state = state_label(a));
    let script = format!("display notification {} with title {}", crate::launch::applescript_string(&body), crate::launch::applescript_string(&title));
    let _ = crate::launch::Launch { program: "osascript".into(), args: vec!["-e".into(), script], cwd: worktree.to_path_buf(), copy: None }.spawn();
}

#[derive(Clone)]
pub struct BranchBadges {
    map: Rc<HashMap<String, (PathBuf, Vec<Agent>)>>,
    ms: u64,
    reduce_motion: bool,
}

impl BranchBadges {
    pub fn has(&self, branch: &str) -> bool {
        self.map.contains_key(branch)
    }

    pub fn badge(&self, t: &Tokens, branch: &str, cell: f32, id: ElementId, cx: &mut Context<RamaApp>) -> Option<AnyElement> {
        let (path, agents) = self.map.get(branch)?;
        Some(badge(t, agents, path.clone(), self.ms, self.reduce_motion, cell, id, cx))
    }
}

pub(crate) fn error_label(e: ErrKind) -> String {
    match e {
        ErrKind::RateLimit => i18n::t!("app.agents.err_rate_limit"),
        ErrKind::Overloaded => i18n::t!("app.agents.err_overloaded"),
        ErrKind::Auth => i18n::t!("app.agents.err_auth"),
        ErrKind::Connection => i18n::t!("app.agents.err_connection"),
        ErrKind::Server => i18n::t!("app.agents.err_server"),
        ErrKind::Request => i18n::t!("app.agents.err_request"),
        ErrKind::Exited => i18n::t!("app.agents.err_exited"),
        ErrKind::Other => i18n::t!("app.agents.err_other"),
    }
}

/// "working", "has a question for you", "rate limited"…
pub fn state_label(a: &Agent) -> String {
    match a.state {
        AgentState::Working => i18n::t!("app.agents.working"),
        AgentState::Waiting => i18n::t!("app.agents.waiting"),
        AgentState::Idle => i18n::t!("app.agents.idle"),
        AgentState::NeedsYou if a.need == Some(Need::Permission) => i18n::t!("app.agents.permission"),
        AgentState::NeedsYou => i18n::t!("app.agents.question"),
        AgentState::Error => error_label(a.error.unwrap_or(ErrKind::Other)),
    }
}

/// Tooltip lines: per agent its name and state, session title, processes, start and last
/// state change; then what a click does.
pub fn describe(agents: &[Agent], now: i64) -> Vec<String> {
    let mut lines = Vec::new();
    for a in agents {
        lines.push(i18n::t!("app.agents.line", agent = a.kind, state = state_label(a)));
        if let Some(title) = &a.title {
            lines.push(i18n::t!("app.agents.session", title = title));
        }
        if a.procs > 1 {
            lines.push(i18n::tn!("app.agents.procs", a.procs));
        }
        if let Some(s) = a.started {
            lines.push(i18n::t!("app.agents.started", ago = ago(s, now)));
        }
        lines.push(i18n::t!("app.agents.since", ago = ago(a.since, now)));
    }
    let focus = cfg!(target_os = "macos") && agents.iter().any(|a| a.host_app.is_some());
    lines.push(if focus { i18n::t!("app.agents.click_focus") } else { i18n::t!("app.agents.click_open") });
    lines
}

#[allow(clippy::too_many_arguments)]
fn badge(t: &Tokens, agents: &[Agent], path: PathBuf, ms: u64, reduce_motion: bool, cell: f32, id: ElementId, cx: &mut Context<RamaApp>) -> AnyElement {
    let state = agents::top(agents).map(|a| a.state).unwrap_or(AgentState::Idle);
    let lines = describe(agents, now());
    let tt = *t;
    div()
        .id(id)
        .flex_none()
        .cursor_pointer()
        .child(crate::robot::robot(t, state, ms, reduce_motion, cell))
        .tooltip(move |window, cx| {
            let lines = lines.clone();
            Tooltip::element(move |_, _| div().flex().flex_col().py(px(2.)).children(lines.iter().enumerate().map(|(i, l)| div().when(i + 1 == lines.len(), |d| d.pt(px(3.)).text_color(tt.ink_muted)).child(l.clone())))).build(window, cx)
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.focus_agent(path.clone(), cx);
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // not `super::*`: gpui's `test` macro would shadow the built-in one
    use super::{Agent, AgentState, describe};
    use crate::agents::{ErrKind, Need};

    fn agent(state: AgentState) -> Agent {
        Agent { kind: "claude", pid: Some(1), state, need: None, error: None, since: 1000, started: Some(400), last_activity: Some(1000), title: Some("Fix parser".into()), session_id: None, session_file: None, procs: 1, host_app: None }
    }

    #[test]
    fn tooltip_lines() {
        i18n::set_locale("en");
        let l = describe(&[agent(AgentState::Waiting)], 1000 + 180);
        assert_eq!(l[0], "claude · waiting for you");
        assert_eq!(l[1], "Session: Fix parser");
        assert_eq!(l[2], "Started 13 minutes ago");
        assert_eq!(l[3], "State changed 3 minutes ago");
        assert_eq!(l.last().unwrap(), "Click to open a terminal here");
        let mut a = agent(AgentState::NeedsYou);
        a.need = Some(Need::Question);
        a.procs = 3;
        let l = describe(&[a], 1000);
        assert_eq!(l[0], "claude · has a question for you");
        assert_eq!(l[2], "3 processes");
        let mut a = agent(AgentState::Error);
        a.error = Some(ErrKind::RateLimit);
        assert_eq!(describe(&[a], 1000)[0], "claude · rate limited");
    }
}
