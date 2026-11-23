//! The branch switcher of the top bar's branch pill (the open checkout) and of each worktree
//! card's "Change branch…": search, recent branches first, remote branches checked out as
//! tracking branches. What follows (changes carried along, a branch checked out elsewhere) is
//! `swap_ui`.
use crate::app::RamaApp;
use crate::cleanup::backdrop;
use crate::theme::Tokens;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use repo::swap::Target;
use repo::{History, RefKind};
use std::path::PathBuf;

pub struct BranchPicker {
    pub input: Entity<InputState>,
    pub selected: usize,
    /// The checkout whose branch changes (a worktree card's), or the open one.
    pub dir: PathBuf,
    /// Its branch now, left out of the list.
    pub current: Option<String>,
    _sub: Subscription,
}

/// (name, is remote) in picker order: local branches by latest commit, then remote branches with
/// no local branch of the same name. `query` matches anywhere, ASCII case ignored.
pub fn candidates(h: &History, current: Option<&str>, query: &str) -> Vec<(String, bool)> {
    let q = query.trim().to_ascii_lowercase();
    let time = |target| h.index.get(&target).map(|&r| h.commits[r].time).unwrap_or(0);
    let mut local: Vec<(&str, i64)> = h.refs.iter().filter(|r| r.kind == RefKind::Local).map(|r| (r.name.as_str(), time(r.target))).collect();
    local.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let locals: Vec<&str> = local.iter().map(|(n, _)| *n).collect();
    let mut out: Vec<(String, bool)> = locals.iter().filter(|n| Some(**n) != current).map(|n| (n.to_string(), false)).collect();
    let mut remote: Vec<(&str, i64)> = h
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Remote)
        .filter(|r| !locals.contains(&r.name.split_once('/').map(|(_, b)| b).unwrap_or(&r.name)))
        .map(|r| (r.name.as_str(), time(r.target)))
        .collect();
    remote.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out.extend(remote.into_iter().map(|(n, _)| (n.to_string(), true)));
    out.retain(|(n, _)| q.is_empty() || n.to_ascii_lowercase().contains(&q));
    out.truncate(200);
    out
}

impl RamaApp {
    pub(crate) fn open_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_branch_picker_for(None, window, cx);
    }

    /// `dir`: a worktree card's checkout; `None` is the open one.
    pub(crate) fn open_branch_picker_for(&mut self, dir: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.repo.as_ref().map(|r| r.root()) else { return };
        let dir = dir.unwrap_or(root.clone());
        let current = if dir == root { self.history.as_ref().and_then(|h| h.head_branch.clone()) } else { self.map.entry(&dir).and_then(|e| e.wt.branch.clone()) };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t!("app.swap.picker_placeholder")));
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _window, cx| match ev {
            InputEvent::Change => {
                if let Some(b) = this.branch_picker.as_mut() {
                    b.selected = 0;
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.pick_selected_branch(cx),
            _ => {}
        });
        self.branch_picker = Some(BranchPicker { input, selected: 0, dir, current, _sub: sub });
        cx.notify();
    }

    fn picker_items(&self, cx: &App) -> Vec<(String, bool)> {
        let (Some(h), Some(b)) = (self.history.as_ref(), self.branch_picker.as_ref()) else { return Vec::new() };
        candidates(h, b.current.as_deref(), &b.input.read(cx).value())
    }

    fn pick_selected_branch(&mut self, cx: &mut Context<Self>) {
        let items = self.picker_items(cx);
        let Some((name, remote)) = self.branch_picker.as_ref().and_then(|b| items.get(b.selected)).cloned() else { return };
        self.choose_branch(name, remote, cx);
    }

    /// Checked out elsewhere, local changes and refusals are all decided by the swap dialog.
    fn choose_branch(&mut self, name: String, remote: bool, cx: &mut Context<Self>) {
        let Some(b) = self.branch_picker.take() else { return };
        self.ask_change_branch(b.dir, if remote { Target::Remote(name) } else { Target::Local(name) }, cx);
        cx.notify();
    }

    fn picker_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let n = self.picker_items(cx).len();
        let Some(b) = self.branch_picker.as_mut() else { return };
        match ev.keystroke.key.as_str() {
            "down" => b.selected = (b.selected + 1).min(n.saturating_sub(1)),
            "up" => b.selected = b.selected.saturating_sub(1),
            "escape" => self.branch_picker = None,
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn render_branch_picker(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let b = self.branch_picker.as_ref()?;
        let card = div().id("branch-card").w(px(460.)).max_h(relative(0.7)).flex().flex_col().rounded(px(14.)).border_1().border_color(t.border).bg(t.surface).shadow_lg().overflow_hidden().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let title = if self.repo.as_ref().is_some_and(|r| r.root() == b.dir) { None } else { Some(i18n::t!("app.swap.picker_title", name = self.checkout_name(&b.dir))) };
        let body: AnyElement = {
            {
                let items = self.picker_items(cx);
                let mut list = div().id("bp-list").flex().flex_col().p(px(6.)).overflow_y_scroll().max_h(px(380.));
                for (i, (name, remote)) in items.iter().enumerate() {
                    let on = i == b.selected;
                    let (n, r) = (name.clone(), *remote);
                    list = list.child(
                        div()
                            .id(("bp-row", i))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .h(px(28.))
                            .px(px(10.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .when(on, |d| d.bg(t.accent_soft))
                            .when(!on, |d| d.hover(|s| s.bg(t.canvas)))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).child(name.clone()))
                            .when(r, |d| d.child(div().flex_none().text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.swap.picker_remote"))))
                            .on_click(cx.listener(move |this, _, _, cx| this.choose_branch(n.clone(), r, cx))),
                    );
                }
                if items.is_empty() {
                    list = list.child(div().p(px(12.)).text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.swap.picker_empty")));
                }
                div()
                    .flex()
                    .flex_col()
                    .children(title.map(|t2| div().px(px(12.)).pt(px(10.)).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(t2)))
                    .child(div().p(px(10.)).border_b_1().border_color(t.border).text_size(px(13.)).child(Input::new(&b.input).appearance(false)))
                    .child(list)
                    .child(div().px(px(12.)).py(px(7.)).border_t_1().border_color(t.border).text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.swap.picker_hint")))
                    .into_any_element()
            }
        };
        Some(
            backdrop(t, "branch-backdrop")
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.branch_picker = None;
                    cx.notify();
                }))
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.picker_key(ev, cx)))
                .child(card.child(body))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::candidates;
    use repo::{Commit, History, RefInfo, RefKind, arrange};

    fn oid(n: u8) -> repo::Oid {
        repo::Oid::from_hex(format!("{n:02x}").repeat(20).as_bytes()).unwrap()
    }

    #[test]
    fn local_branches_by_recency_then_untracked_remotes() {
        let c = |n: u8, t: i64| Commit { id: oid(n), summary: String::new(), author: String::new(), email: String::new(), time: t, offset: 0, parents: vec![] };
        let r = |name: &str, kind: RefKind, n: u8| RefInfo { name: name.into(), kind, target: oid(n) };
        let h: History = arrange(
            vec![c(1, 100), c(2, 300), c(3, 200), c(4, 400)],
            vec![r("main", RefKind::Local, 1), r("feat/new", RefKind::Local, 2), r("old", RefKind::Local, 3), r("origin/feat/new", RefKind::Remote, 2), r("origin/hotfix", RefKind::Remote, 4)],
            Some(oid(1)),
            Some("main".into()),
            false,
        );
        let names: Vec<(String, bool)> = candidates(&h, Some("main"), "");
        assert_eq!(names, vec![("feat/new".to_string(), false), ("old".to_string(), false), ("origin/hotfix".to_string(), true)], "the current branch and remotes with a local twin are left out");
        assert_eq!(candidates(&h, Some("main"), "HOT"), vec![("origin/hotfix".to_string(), true)]);
    }
}
