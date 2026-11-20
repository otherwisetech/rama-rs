//! AI agent badges: the poll (every few seconds, in the background, only while the window can
//! be seen), the typing animation, and the robot element with its tooltip and click.
use crate::agents::{self, Agent, AgentState, Found, Tracker};
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

    fn any_working(&self) -> bool {
        self.found.values().flatten().any(|a| a.state == AgentState::Working)
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
        let cached = self.agents.worktrees.as_ref().filter(|(at, _)| at.elapsed() < WORKTREES_FRESH).map(|(_, w)| w.clone());
        let cached_list = cached.as_ref().map(|w| w.as_ref().clone());
        let root = repo.root();
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
                a.tracker.apply(&mut found);
                let list = a.worktrees.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
                a.branches = Rc::new(list.iter().filter(|w| found.contains_key(&w.path)).filter_map(|w| Some((w.branch.clone()?, w.path.clone()))).collect());
                let changed = *a.found != found || a.current != current;
                a.found = Rc::new(found);
                a.current = current;
                if changed {
                    cx.notify();
                }
                this.ensure_agent_animation(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Redraw at the sprite's frame rate while some shown agent is typing.
    fn ensure_agent_animation(&mut self, cx: &mut Context<Self>) {
        let wanted = |this: &RamaApp| this.badges_shown() && !this.agents.reduce_motion && this.agents.any_working();
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

    pub fn set_agent_prefs(&mut self, enabled: bool, reduce_motion: bool, cx: &mut Context<Self>) {
        self.agents.enabled = enabled;
        self.agents.reduce_motion = reduce_motion;
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

pub fn state_label(s: AgentState) -> String {
    match s {
        AgentState::Working => i18n::t!("app.agents.working"),
        AgentState::Waiting => i18n::t!("app.agents.waiting"),
        AgentState::Idle => i18n::t!("app.agents.idle"),
    }
}

/// Tooltip lines: per agent its name and state, session title, start and last state change;
/// then what a click does.
pub fn describe(agents: &[Agent], now: i64) -> Vec<String> {
    let mut lines = Vec::new();
    for a in agents {
        lines.push(i18n::t!("app.agents.line", agent = a.kind, state = state_label(a.state)));
        if let Some(title) = &a.title {
            lines.push(i18n::t!("app.agents.session", title = title));
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
    let state = agents::top_state(agents).unwrap_or(AgentState::Idle);
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

    #[test]
    fn tooltip_lines() {
        i18n::set_locale("en");
        let a = Agent { kind: "claude", pid: Some(1), state: AgentState::Waiting, since: 1000, started: Some(400), last_activity: Some(1000), title: Some("Fix parser".into()), host_app: None };
        let l = describe(&[a], 1000 + 180);
        assert_eq!(l[0], "claude · waiting for you");
        assert_eq!(l[1], "Session: Fix parser");
        assert_eq!(l[2], "Started 13 minutes ago");
        assert_eq!(l[3], "State changed 3 minutes ago");
        assert_eq!(l.last().unwrap(), "Click to open a terminal here");
    }
}
