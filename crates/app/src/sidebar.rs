//! Left sidebar: a filter field over local branches, remote branches, tags and stashes, each
//! foldable and virtualised (some repos have tens of thousands of refs). Clicking a ref jumps
//! to its commit; double-clicking a branch checks it out; right-click for branch actions.
use crate::app::RamaApp;
use crate::theme::{Tokens, tokens};
use crate::ui::icon;
use crate::work::{Op, PromptKind};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use gpui_component::{IconName, Sizable as _};
use repo::{History, RefKind, Stash};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

const SIDE_W: f32 = 260.;
const ITEM_H: f32 = 26.;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Sec {
    Local,
    Remote,
    Tag,
    Stash,
}

fn sec_of(k: RefKind) -> Sec {
    match k {
        RefKind::Local => Sec::Local,
        RefKind::Remote => Sec::Remote,
        RefKind::Tag => Sec::Tag,
    }
}

fn section_name(s: Sec) -> String {
    match s {
        Sec::Local => i18n::t!("app.side.local"),
        Sec::Remote => i18n::t!("app.side.remote"),
        Sec::Tag => i18n::t!("app.side.tags"),
        Sec::Stash => i18n::t!("app.side.stashes"),
    }
}

/// One section after filtering: indices into `history.refs` (or the stash list) with the
/// matched byte ranges of each name.
pub struct SecData {
    sec: Sec,
    total: usize,
    hits: Vec<(usize, Vec<Range<usize>>)>,
}

/// Filtered sections, rebuilt only when the history, stashes, filter text or sort change.
pub struct SideCache {
    key: (usize, usize, String, bool),
    sections: Rc<Vec<SecData>>,
}

#[derive(Clone, Copy)]
enum Item {
    Header(usize),
    Ref(usize, usize),
    Stash(usize, usize),
    NoMatch,
}

fn build(h: &History, stashes: &[Stash], query: &str, recent: bool) -> Vec<SecData> {
    let mut out = Vec::with_capacity(4);
    for kind in [RefKind::Local, RefKind::Remote, RefKind::Tag] {
        let mut total = 0;
        let mut hits: Vec<(usize, Vec<Range<usize>>)> = Vec::new();
        for (i, r) in h.refs.iter().enumerate().filter(|(_, r)| r.kind == kind) {
            total += 1;
            if let Some(m) = repo::ref_match(query, &r.name) {
                hits.push((i, m));
            }
        }
        if kind == RefKind::Local {
            let time = |i: usize| h.index.get(&h.refs[i].target).map(|&row| h.commits[row].time).unwrap_or(i64::MIN);
            let head = h.head_branch.as_deref();
            // checked-out branch first, then by name (refs come sorted) or newest commit
            hits.sort_by_key(|(i, _)| (Some(h.refs[*i].name.as_str()) != head, if recent { std::cmp::Reverse(time(*i)) } else { std::cmp::Reverse(0) }));
        }
        out.push(SecData { sec: sec_of(kind), total, hits });
    }
    let hits = stashes.iter().enumerate().filter_map(|(i, s)| repo::ref_match(query, &s.message).map(|m| (i, m)).or_else(|| repo::ref_match(query, &s.name()).map(|_| (i, Vec::new())))).collect();
    out.push(SecData { sec: Sec::Stash, total: stashes.len(), hits });
    out
}

/// The name with the matched parts in bold accent.
fn highlighted(t: &Tokens, name: &str, ranges: &[Range<usize>]) -> StyledText {
    let style = HighlightStyle { color: Some(t.accent), font_weight: Some(FontWeight::BOLD), background_color: Some(t.accent_soft), ..Default::default() };
    StyledText::new(name.to_string()).with_highlights(ranges.iter().filter(|r| r.end <= name.len()).map(|r| (r.clone(), style)))
}

impl RamaApp {
    fn filter_text(&self, cx: &App) -> String {
        self.side_search.read(cx).value().trim().to_string()
    }

    fn side_sections(&mut self, cx: &App) -> Rc<Vec<SecData>> {
        let Some(h) = self.history.clone() else { return Rc::new(Vec::new()) };
        let key = (Arc::as_ptr(&h) as usize, Rc::as_ptr(&self.work.stashes) as *const u8 as usize, self.filter_text(cx), self.side_recent);
        if let Some(c) = self.side_cache.as_ref().filter(|c| c.key == key) {
            return c.sections.clone();
        }
        let sections = Rc::new(build(&h, &self.work.stashes, &key.2, self.side_recent));
        self.side_cache = Some(SideCache { key, sections: sections.clone() });
        sections
    }

    fn sec_open(&self, sec: Sec, filtering: bool) -> bool {
        if filtering { !self.filter_collapsed.contains(&sec) } else { !self.collapsed.contains(&sec) }
    }

    fn side_items(&self, sections: &[SecData], filtering: bool) -> Vec<Item> {
        let mut items = Vec::new();
        for (si, s) in sections.iter().enumerate() {
            // while filtering only sections with matches show; stashes only when there are some
            if (filtering && s.hits.is_empty()) || (s.sec == Sec::Stash && s.total == 0) {
                continue;
            }
            items.push(Item::Header(si));
            if self.sec_open(s.sec, filtering) {
                if s.sec == Sec::Stash {
                    items.extend((0..s.hits.len()).map(|j| Item::Stash(si, j)));
                } else {
                    items.extend((0..s.hits.len()).map(|j| Item::Ref(si, j)));
                }
            }
        }
        if filtering && items.is_empty() {
            items.push(Item::NoMatch);
        }
        items
    }

    /// Enter in the filter: jump to the first matching branch or tag.
    pub(crate) fn filter_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sections = self.side_sections(cx);
        let first = sections.iter().filter(|s| s.sec != Sec::Stash).find_map(|s| s.hits.first().map(|(i, _)| *i));
        if let (Some(i), Some(h)) = (first, self.history.clone()) {
            self.list_focus.focus(window, cx);
            self.select_id(h.refs[i].target, cx);
        }
    }

    pub(crate) fn render_sidebar(&mut self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let filtering = !self.filter_text(cx).is_empty();
        let sections = self.side_sections(cx);
        let items = Rc::new(self.side_items(&sections, filtering));
        let count = items.len();
        let recent = self.side_recent;
        let sort_tip = if recent { i18n::t!("app.side.sort_recent") } else { i18n::t!("app.side.sort_name") };
        let sort = crate::ui::icon_button(t, "side-sort", if recent { IconName::Calendar } else { IconName::SortAscending }, true)
            .size(px(24.))
            .rounded(px(6.))
            .tooltip(move |window, cx| Tooltip::new(sort_tip.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| {
                this.side_recent = !this.side_recent;
                let recent = this.side_recent;
                crate::settings::Settings::update(|s| s.branch_sort = Some(if recent { "recent" } else { "name" }.to_string())); // i18n-ok: setting value
                cx.notify();
            }));
        let clear = filtering.then(|| {
            crate::ui::icon_button(t, "side-clear", IconName::Close, true).size(px(22.)).rounded(px(6.)).on_click(cx.listener(|this, _, window, cx| {
                this.side_search.update(cx, |s, cx| s.set_value("", window, cx));
                this.filter_collapsed.clear();
                cx.notify();
            }))
        });
        let field = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .m(px(8.))
            .mb(px(2.))
            .h(px(30.))
            .pl(px(8.))
            .pr(px(3.))
            .rounded(px(8.))
            .border_1()
            .border_color(if filtering { t.accent } else { t.border })
            .bg(t.canvas)
            .child(icon(IconName::Search, 13., t.ink_muted))
            .child(div().flex_1().min_w_0().text_size(px(12.5)).child(Input::new(&self.side_search).appearance(false).small()))
            .children(clear)
            .child(sort);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDE_W))
            .h_full()
            .bg(t.surface)
            .border_r_1()
            .border_color(t.border)
            .child(field)
            .child(
                uniform_list(
                    "refs",
                    count,
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        let t = tokens(cx);
                        range.filter_map(|ix| items.get(ix).map(|it| this.side_item(&t, ix, *it, &sections, filtering, cx))).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.side_scroll)
                .flex_1()
                .py(px(4.)),
            )
    }

    /// "⋯" on the local branches header: cleanup and worktrees.
    fn local_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let busy = self.work.job.is_some();
        div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(Button::new("local-menu").ghost().xsmall().icon(IconName::Ellipsis).dropdown_menu(move |menu, _, _| {
            let (w1, w2, w3) = (weak.clone(), weak.clone(), weak.clone());
            menu.item(PopupMenuItem::new(i18n::t!("app.clean.menu")).disabled(busy).on_click(move |_, window, cx| {
                w1.update(cx, |this, cx| this.open_cleanup(window, cx)).ok();
            }))
            .item(PopupMenuItem::new(i18n::t!("app.map.new")).disabled(busy).on_click(move |_, window, cx| {
                w2.update(cx, |this, cx| this.open_new_worktree(None, window, cx)).ok();
            }))
            .item(PopupMenuItem::new(i18n::t!("app.map.show")).on_click(move |_, _, cx| {
                w3.update(cx, |this, cx| this.show_map(true, cx)).ok();
            }))
        }))
    }

    fn side_item(&self, t: &Tokens, ix: usize, item: Item, sections: &Rc<Vec<SecData>>, filtering: bool, cx: &mut Context<Self>) -> AnyElement {
        let hover = if t.dark { t.border } else { t.canvas };
        match item {
            Item::NoMatch => div().id(("side", ix)).px(px(14.)).py(px(10.)).text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.side.no_match")).into_any_element(),
            Item::Header(si) => {
                let s = &sections[si];
                let sec = s.sec;
                let open = self.sec_open(sec, filtering);
                let n = if filtering { i18n::t!("app.side.match_count", n = crate::util::group(s.hits.len()), total = crate::util::group(s.total)) } else { crate::util::group(s.total) };
                div()
                    .id(("side", ix))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(ITEM_H))
                    .px(px(12.))
                    .mt(px(if ix == 0 { 0. } else { 6. }))
                    .cursor_pointer()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.ink_muted)
                    .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12., t.ink_muted))
                    .child(div().flex_1().child(section_name(sec)))
                    .child(div().text_size(px(11.)).when(filtering, |d| d.text_color(t.accent)).child(n))
                    .when(sec == Sec::Local, |d| d.child(self.local_menu(cx)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let set = if filtering { &mut this.filter_collapsed } else { &mut this.collapsed };
                        if !set.remove(&sec) {
                            set.insert(sec);
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Item::Stash(si, j) => {
                let (i, ranges) = &sections[si].hits[j];
                let i = *i;
                let Some(st) = self.work.stashes.get(i) else { return div().into_any_element() };
                let on = self.work.stash.as_ref().is_some_and(|s| s.stash.id == st.id);
                let weak = cx.entity().downgrade();
                div()
                    .id(("side", ix))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(ITEM_H))
                    .pl(px(28.))
                    .pr(px(12.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(t.accent_soft))
                    .when(!on, |d| d.hover(move |s| s.bg(hover)))
                    .child(icon(IconName::Inbox, 12., t.ink_muted).flex_none())
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).child(highlighted(t, &st.message, ranges)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_stash(i, cx)))
                    .context_menu(move |menu, _, _| {
                        let item = |label: String, f: fn(&mut RamaApp, usize, &mut Context<RamaApp>)| {
                            let w = weak.clone();
                            PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                w.update(cx, |this, cx| f(this, i, cx)).ok();
                            })
                        };
                        menu.item(item(i18n::t!("app.stash.apply"), |this, i, cx| this.run_op(Op::StashApply(i), cx)))
                            .item(item(i18n::t!("app.stash.pop"), |this, i, cx| this.run_op(Op::StashPop(i), cx)))
                            .separator()
                            .item(item(i18n::t!("app.stash.drop_menu"), |this, i, cx| {
                                this.work.confirm = Some(crate::work::Confirm::DropStash(i));
                                cx.notify();
                            }))
                    })
                    .into_any_element()
            }
            Item::Ref(si, j) => {
                let Some(h) = &self.history else { return div().into_any_element() };
                let (i, ranges) = &sections[si].hits[j];
                let r = &h.refs[*i];
                let row = h.index.get(&r.target).copied();
                let is_head = r.kind == RefKind::Local && h.head_branch.as_deref() == Some(r.name.as_str());
                let on_selected = row.is_some() && row == self.selected;
                let dot = row.map(|row| t.lane(h.rows[row].color)).unwrap_or(t.border_strong);
                let (target, kind, name) = (r.target, r.kind, r.name.clone());
                let ic_color = if is_head { t.accent } else { t.ink_muted };
                let ic: AnyElement = match r.kind {
                    RefKind::Local if is_head => icon(IconName::Check, 12., ic_color).flex_none().into_any_element(),
                    RefKind::Local => crate::ui::branch_icon(12., ic_color).into_any_element(),
                    RefKind::Remote => icon(IconName::Globe, 12., ic_color).flex_none().into_any_element(),
                    RefKind::Tag => icon(IconName::Star, 12., ic_color).flex_none().into_any_element(),
                };
                // the checked-out branch shows how far it is from its upstream
                let track = is_head
                    .then(|| self.work.status.as_ref().map(|s| &s.branch))
                    .flatten()
                    .map(|b| {
                        let mut s = String::new();
                        if b.ahead > 0 {
                            s.push_str(&format!("↑{} ", b.ahead)); // i18n-ok: arrows
                        }
                        if b.behind > 0 {
                            s.push_str(&format!("↓{}", b.behind)); // i18n-ok: arrows
                        }
                        s.trim().to_string()
                    })
                    .filter(|s| !s.is_empty());
                let menu_ctx = RefMenu { weak: cx.entity().downgrade(), name: name.clone(), kind, target, is_head, upstream: self.work.upstreams.get(&name).cloned(), busy: self.work.job.is_some() };
                div()
                    .id(("side", ix))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(ITEM_H))
                    .pl(px(28.))
                    .pr(px(12.))
                    .cursor_pointer()
                    .when(on_selected, |d| d.bg(t.accent_soft))
                    .when(!on_selected, |d| d.hover(move |s| s.bg(hover)))
                    .child(ic)
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).when(is_head, |d| d.font_weight(FontWeight::SEMIBOLD)).when(row.is_none(), |d| d.text_color(t.ink_muted)).child(highlighted(t, &r.name, ranges)))
                    .children(track.map(|s| div().flex_none().text_size(px(11.)).text_color(t.accent).child(s)))
                    .child(div().flex_none().size(px(7.)).rounded_full().bg(dot))
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        if ev.click_count() == 2 && !is_head {
                            match kind {
                                RefKind::Local => this.run_op(Op::Switch(name.clone()), cx),
                                RefKind::Remote => this.run_op(Op::SwitchRemote(name.clone()), cx),
                                RefKind::Tag => {}
                            }
                        } else {
                            this.list_focus.focus(window, cx);
                            this.select_id(target, cx);
                        }
                    }))
                    .context_menu(move |menu, _, _| menu_ctx.build(menu))
                    .into_any_element()
            }
        }
    }
}

/// What a ref's right-click menu needs.
struct RefMenu {
    weak: WeakEntity<RamaApp>,
    name: String,
    kind: RefKind,
    target: repo::Oid,
    is_head: bool,
    upstream: Option<String>,
    busy: bool,
}

impl RefMenu {
    fn item(&self, label: String, enabled: bool, f: impl Fn(&mut RamaApp, &mut Window, &mut Context<RamaApp>) + 'static) -> PopupMenuItem {
        let w = self.weak.clone();
        PopupMenuItem::new(label).disabled(!enabled).on_click(move |_, window, cx| {
            w.update(cx, |this, cx| f(this, window, cx)).ok();
        })
    }

    fn build(&self, mut menu: PopupMenu) -> PopupMenu {
        let (name, target, head, busy) = (self.name.clone(), self.target, self.is_head, self.busy);
        match self.kind {
            RefKind::Local => {
                let n = name.clone();
                menu = menu.item(self.item(i18n::t!("app.menu.checkout", name = name), !head && !busy, move |this, _, cx| this.run_op(Op::Switch(n.clone()), cx)));
                let n = name.clone();
                menu = menu.item(self.item(i18n::t!("app.menu.pull_ff"), self.upstream.is_some() && !busy, move |this, _, cx| this.run_op(Op::PullFf { name: n.clone(), head }, cx)));
                let n = name.clone();
                let push = if self.upstream.is_some() { i18n::t!("app.menu.push", upstream = self.upstream.clone().unwrap_or_default()) } else { i18n::t!("app.menu.publish") };
                menu = menu.item(self.item(push, !busy, move |this, _, cx| this.push_branch(n.clone(), head, cx)));
                menu = menu.separator();
                menu = menu.item(self.item(i18n::t!("app.menu.new_branch"), !busy, move |this, window, cx| this.open_prompt(PromptKind::NewBranch(target), window, cx)));
                let n = name.clone();
                menu = menu.item(self.item(i18n::t!("app.map.new_from_branch"), !busy && !head, move |this, window, cx| this.open_new_worktree(Some(n.clone()), window, cx)));
                let n = name.clone();
                menu = menu.item(self.item(i18n::t!("app.menu.rename"), !busy, move |this, window, cx| this.open_prompt(PromptKind::RenameBranch(n.clone()), window, cx)));
                menu = menu.item(self.copy_item());
                menu = menu.separator();
                let n = name.clone();
                menu = menu.item(self.item(i18n::t!("app.menu.delete_branch", name = name), !head && !busy, move |this, _, cx| this.ask_delete_branch(n.clone(), cx)));
                if let Some(up) = self.upstream.clone() {
                    menu = menu.item(self.item(i18n::t!("app.menu.delete_remote", name = up), !busy, move |this, window, cx| this.ask_delete_remote(up.clone(), window, cx)));
                }
            }
            RefKind::Remote => {
                let n = name.clone();
                menu = menu.item(self.item(i18n::t!("app.menu.checkout_remote", name = name), !busy, move |this, _, cx| this.run_op(Op::SwitchRemote(n.clone()), cx)));
                menu = menu.item(self.item(i18n::t!("app.menu.new_branch"), !busy, move |this, window, cx| this.open_prompt(PromptKind::NewBranch(target), window, cx)));
                menu = menu.item(self.copy_item());
                menu = menu.separator();
                let n = name.clone();
                menu = menu.item(self.item(i18n::t!("app.menu.delete_remote", name = name), !busy, move |this, window, cx| this.ask_delete_remote(n.clone(), window, cx)));
            }
            RefKind::Tag => {
                menu = menu.item(self.item(i18n::t!("app.menu.new_branch"), !busy, move |this, window, cx| this.open_prompt(PromptKind::NewBranch(target), window, cx)));
                menu = menu.item(self.copy_item());
            }
        }
        menu
    }

    fn copy_item(&self) -> PopupMenuItem {
        let n = self.name.clone();
        self.item(i18n::t!("app.menu.copy_name"), true, move |this, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(n.clone()));
            this.toast(i18n::t!("app.toast.copied_name", name = n), cx);
        })
    }
}
