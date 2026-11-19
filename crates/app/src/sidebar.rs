//! Left sidebar: local branches, remote branches and tags, foldable, virtualised (some repos have
//! tens of thousands of remote refs). Clicking a ref jumps to its commit.
use crate::app::RamaApp;
use crate::theme::{Tokens, tokens};
use crate::ui::icon;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::IconName;
use repo::RefKind;

const SIDE_W: f32 = 250.;
const ITEM_H: f32 = 26.;

#[derive(Clone, Copy)]
enum Item {
    Header(RefKind, usize),
    Ref(usize),
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
            Item::Ref(i) => {
                let Some(h) = &self.history else { return div().into_any_element() };
                let r = &h.refs[i];
                let row = h.index.get(&r.target).copied();
                let is_head = r.kind == RefKind::Local && h.head_branch.as_deref() == Some(r.name.as_str());
                let on_selected = row.is_some() && row == self.selected;
                let dot = row.map(|row| t.lane(h.rows[row].color)).unwrap_or(t.border_strong);
                let target = r.target;
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
                    .on_click(cx.listener(move |this, _, _, cx| this.select_id(target, cx)))
                    .into_any_element()
            }
        }
    }
}
