//! Left sidebar: local branches, remote branches, tags and stashes, foldable, virtualised (some
//! repos have tens of thousands of remote refs). Clicking a ref jumps to its commit;
//! double-clicking a branch checks it out; right-click for branch and stash actions.
use crate::app::RamaApp;
use crate::theme::{Tokens, tokens};
use crate::ui::icon;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::IconName;
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use crate::work::{Confirm, Op, PromptKind};
use repo::RefKind;

const SIDE_W: f32 = 250.;
const ITEM_H: f32 = 26.;

#[derive(Clone, Copy)]
enum Item {
    Header(RefKind, usize),
    Ref(usize),
    StashHeader(usize),
    Stash(usize),
}

fn section_name(k: RefKind) -> String {
    match k {
        RefKind::Local => i18n::t!("app.side.local"),
        RefKind::Remote => i18n::t!("app.side.remote"),
        RefKind::Tag => i18n::t!("app.side.tags"),
    }
}

impl RamaApp {
    fn side_items(&self) -> Vec<Item> {
        let Some(h) = &self.history else { return Vec::new() };
        let mut items = Vec::new();
        for kind in [RefKind::Local, RefKind::Remote, RefKind::Tag] {
            let refs: Vec<usize> = (0..h.refs.len()).filter(|&i| h.refs[i].kind == kind).collect();
            items.push(Item::Header(kind, refs.len()));
            if !self.collapsed.contains(&kind) {
                items.extend(refs.into_iter().map(Item::Ref));
            }
        }
        let n = self.work.stashes.len();
        if n > 0 {
            items.push(Item::StashHeader(n));
            if !self.stashes_collapsed {
                items.extend((0..n).map(Item::Stash));
            }
        }
        items
    }

    pub(crate) fn render_sidebar(&mut self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let items = std::rc::Rc::new(self.side_items());
        let count = items.len();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDE_W))
            .h_full()
            .bg(t.surface)
            .border_r_1()
            .border_color(t.border)
            .child(
                uniform_list(
                    "refs",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                        let t = tokens(cx);
                        range.filter_map(|ix| items.get(ix).map(|it| this.side_item(&t, ix, *it, cx))).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.side_scroll)
                .flex_1()
                .py(px(6.)),
            )
    }

    fn side_item(&self, t: &Tokens, ix: usize, item: Item, cx: &mut Context<Self>) -> AnyElement {
        let hover = if t.dark { t.border } else { t.canvas };
        match item {
            Item::Header(kind, n) => {
                let open = !self.collapsed.contains(&kind);
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
                    .child(div().flex_1().child(section_name(kind)))
                    .child(div().text_size(px(11.)).child(crate::util::group(n)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&kind) {
                            this.collapsed.insert(kind);
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Item::StashHeader(n) => {
                let open = !self.stashes_collapsed;
                div()
                    .id(("side", ix))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(ITEM_H))
                    .px(px(12.))
                    .mt(px(6.))
                    .cursor_pointer()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.ink_muted)
                    .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12., t.ink_muted))
                    .child(div().flex_1().child(i18n::t!("app.side.stashes")))
                    .child(div().text_size(px(11.)).child(crate::util::group(n)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stashes_collapsed = !this.stashes_collapsed;
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Item::Stash(i) => {
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
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).child(st.message.clone()))
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
                                this.work.confirm = Some(Confirm::DropStash(i));
                                cx.notify();
                            }))
                    })
                    .into_any_element()
            }
            Item::Ref(i) => {
                let Some(h) = &self.history else { return div().into_any_element() };
                let r = &h.refs[i];
                let row = h.index.get(&r.target).copied();
                let is_head = r.kind == RefKind::Local && h.head_branch.as_deref() == Some(r.name.as_str());
                let on_selected = row.is_some() && row == self.selected;
                let dot = row.map(|row| t.lane(h.rows[row].color)).unwrap_or(t.border_strong);
                let target = r.target;
                let kind = r.kind;
                let name = r.name.clone();
                let menu_name = r.name.clone();
                let weak = cx.entity().downgrade();
                let ic_color = if is_head { t.accent } else { t.ink_muted };
                let ic: AnyElement = match r.kind {
                    RefKind::Local if is_head => icon(IconName::Check, 12., ic_color).flex_none().into_any_element(),
                    RefKind::Local => crate::ui::branch_icon(12., ic_color).into_any_element(),
                    RefKind::Remote => icon(IconName::Globe, 12., ic_color).flex_none().into_any_element(),
                    RefKind::Tag => icon(IconName::Star, 12., ic_color).flex_none().into_any_element(),
                };
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
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).when(is_head, |d| d.font_weight(FontWeight::SEMIBOLD)).when(row.is_none(), |d| d.text_color(t.ink_muted)).child(r.name.clone()))
                    .child(div().flex_none().size(px(7.)).rounded_full().bg(dot))
                    .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                        if ev.click_count() == 2 && !is_head {
                            match kind {
                                RefKind::Local => this.run_op(Op::Switch(name.clone()), cx),
                                RefKind::Remote => this.run_op(Op::SwitchRemote(name.clone()), cx),
                                RefKind::Tag => {}
                            }
                        } else {
                            this.select_id(target, cx);
                        }
                    }))
                    .context_menu(move |mut menu, _, _| {
                        let w = weak.clone();
                        let n = menu_name.clone();
                        match kind {
                            RefKind::Local => {
                                menu = menu.item(PopupMenuItem::new(i18n::t!("app.menu.checkout", name = n)).disabled(is_head).on_click(move |_, _, cx| {
                                    w.update(cx, |this, cx| this.run_op(Op::Switch(n.clone()), cx)).ok();
                                }));
                            }
                            RefKind::Remote => {
                                menu = menu.item(PopupMenuItem::new(i18n::t!("app.menu.checkout_remote", name = n)).on_click(move |_, _, cx| {
                                    w.update(cx, |this, cx| this.run_op(Op::SwitchRemote(n.clone()), cx)).ok();
                                }));
                            }
                            RefKind::Tag => {}
                        }
                        let w = weak.clone();
                        menu = menu.item(PopupMenuItem::new(i18n::t!("app.menu.new_branch")).on_click(move |_, window, cx| {
                            w.update(cx, |this, cx| this.open_prompt(PromptKind::NewBranch(target), window, cx)).ok();
                        }));
                        if kind == RefKind::Local {
                            let (w, n) = (weak.clone(), menu_name.clone());
                            menu = menu.separator().item(PopupMenuItem::new(i18n::t!("app.menu.delete_branch")).disabled(is_head).on_click(move |_, _, cx| {
                                w.update(cx, |this, cx| {
                                    this.work.confirm = Some(Confirm::DeleteBranch(n.clone()));
                                    cx.notify();
                                })
                                .ok();
                            }));
                        }
                        menu
                    })
                    .into_any_element()
            }
        }
    }
}
