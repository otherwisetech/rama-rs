//! The commit list: refs | graph | message | author | date | sha. The graph is painted per row
//! (lines and curves in a `canvas`), with the commit as a coloured avatar on top.
use crate::agents_view::BranchBadges;
use crate::app::RamaApp;
use crate::theme::{MONO, Tokens, tokens};
use crate::util::{datetime, initials};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::IconName;
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use crate::work::{Op, PromptKind};
use repo::{History, RefInfo, RefKind};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub const ROW_H: f32 = 30.;
const LANE_W: f32 = 18.;
/// Lanes shown before the graph column is clipped (busy repos can have hundreds).
const MAX_LANES: u16 = 16;
const GRAPH_PAD: f32 = 10.;
const REFS_W: f32 = 190.;
const AUTHOR_W: f32 = 150.;
const DATE_W: f32 = 128.;
const SHA_W: f32 = 70.;
const AVATAR: f32 = 20.;
const MERGE_DOT: f32 = 9.;

fn lane_x(lane: u16) -> f32 {
    GRAPH_PAD + LANE_W * (lane as f32 + 0.5)
}

impl RamaApp {
    pub(crate) fn render_graph(&mut self, t: &Tokens, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(h) = self.history.clone() else { return div().into_any_element() };
        if h.commits.is_empty() {
            let wip = self.render_wip_row(t, lane_x(0), t.lane(0), GRAPH_PAD * 2. + LANE_W, REFS_W, cx);
            return div().size_full().flex().flex_col().bg(t.surface).children(wip).child(div().flex_1().flex().items_center().justify_center().text_color(t.ink_muted).child(i18n::t!("app.graph.empty"))).into_any_element();
        }
        let lanes = h.rows.iter().map(|r| r.width).max().unwrap_or(1).clamp(1, MAX_LANES);
        let graph_w = GRAPH_PAD * 2. + LANE_W * lanes as f32;
        let refs_by_row = self.refs_by_row.clone();
        let mask = self.match_mask.clone();
        let selected = self.selected;
        let header_cell = |w: Option<f32>, label: String| {
            let d = div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).truncate().child(label);
            match w {
                Some(w) => d.w(px(w)).flex_none(),
                None => d.flex_1().min_w_0(),
            }
        };
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(30.))
            .px(px(8.))
            .gap(px(8.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(header_cell(Some(REFS_W), i18n::t!("app.graph.col_refs")))
            .child(header_cell(Some(graph_w), i18n::t!("app.graph.col_graph")))
            .child(header_cell(None, i18n::t!("app.graph.col_message")))
            .child(header_cell(Some(AUTHOR_W), i18n::t!("app.graph.col_author")))
            .child(header_cell(Some(DATE_W), i18n::t!("app.graph.col_date")))
            .child(header_cell(Some(SHA_W), i18n::t!("app.graph.col_sha")));
        let head_row = h.head.and_then(|id| h.index.get(&id).copied()).map(|r| &h.rows[r]);
        let (wip_x, wip_color) = head_row.map(|r| (lane_x(r.lane.min(lanes)), t.lane(r.color))).unwrap_or((lane_x(0), t.lane(0)));
        let wip = self.render_wip_row(t, wip_x, wip_color, graph_w, REFS_W, cx);
        let weak = cx.entity().downgrade();
        let badges = self.branch_badges();
        let list = uniform_list(
            "commits",
            h.commits.len(),
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                let t = tokens(cx);
                range.map(|ix| commit_row(&t, &h, &refs_by_row, mask.as_ref(), ix, graph_w, lanes, selected == Some(ix), &badges, weak.clone(), cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.list_scroll)
        .flex_1();
        let truncated = self.history.as_ref().is_some_and(|h| h.truncated);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.surface)
            .child(header)
            .children(wip)
            .child(list)
            .when(truncated, |d| d.child(div().flex_none().px(px(12.)).py(px(6.)).border_t_1().border_color(t.border).text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.graph.truncated"))))
            .into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn commit_row(t: &Tokens, h: &Arc<History>, refs_by_row: &HashMap<usize, Vec<usize>>, mask: Option<&Rc<Vec<bool>>>, ix: usize, graph_w: f32, lanes: u16, selected: bool, badges: &BranchBadges, weak: WeakEntity<RamaApp>, cx: &mut Context<RamaApp>) -> AnyElement {
    let c = &h.commits[ix];
    let row = &h.rows[ix];
    let color = t.lane(row.color);
    let is_head = h.head == Some(c.id);
    let merge = c.parents.len() > 1;
    let hover = if t.dark { t.border } else { t.canvas };
    // while searching, rows that don't match fade back
    let dim = mask.is_some_and(|m| !m.get(ix).copied().unwrap_or(false));

    // refs: first one as a pill, the rest as "+N"
    let refs: Vec<&RefInfo> = refs_by_row.get(&ix).map(|v| v.iter().map(|&i| &h.refs[i]).collect()).unwrap_or_default();
    let shown = pick_ref(&refs, h.head_branch.as_deref(), badges);
    // a branch with an agent at work in its worktree: the robot inside its pill, or next to
    // the pill shown when that branch is folded into "+N"
    let agent_branch = refs.iter().find(|r| r.kind == RefKind::Local && badges.has(&r.name)).map(|r| r.name.clone());
    let in_pill = shown.is_some_and(|r| Some(&r.name) == agent_branch.as_ref() && r.kind == RefKind::Local);
    let robot = agent_branch.and_then(|b| badges.badge(t, &b, 1.5, ("ref-agent", ix).into(), cx));
    let (pill_robot, side_robot) = if in_pill { (robot, None) } else { (None, robot) };
    let refs_cell = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_end()
        .gap(px(4.))
        .w(px(REFS_W))
        .overflow_hidden()
        .children(side_robot)
        .children(shown.map(|r| ref_pill(t, r, h.head_branch.as_deref() == Some(r.name.as_str()) && r.kind == RefKind::Local, color).children(pill_robot)))
        .when(refs.len() > 1, |d| d.child(div().flex_none().px(px(5.)).rounded(px(5.)).bg(t.canvas).text_size(px(10.)).text_color(t.ink_muted).child(format!("+{}", refs.len() - 1))));

    // graph cell: edges in a canvas, the commit node on top
    let up = row.up.clone();
    let down = row.down.clone();
    let tt = *t;
    let edges = canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let mid = ROW_H / 2.;
            let clip = |l: u16| l.min(lanes);
            for (e, y0, y1) in up.iter().map(|e| (e, 0., mid)).chain(down.iter().map(|e| (e, mid, ROW_H))) {
                if e.from >= lanes && e.to >= lanes {
                    continue;
                }
                let (x0, x1) = (lane_x(clip(e.from)), lane_x(clip(e.to)));
                let p = |x: f32, y: f32| point(b.origin.x + px(x), b.origin.y + px(y));
                let mut pb = PathBuilder::stroke(px(2.));
                pb.move_to(p(x0, y0));
                if x0 == x1 {
                    pb.line_to(p(x1, y1));
                } else {
                    let ym = (y0 + y1) / 2.;
                    pb.cubic_bezier_to(p(x1, y1), p(x0, ym), p(x1, ym));
                }
                if let Ok(path) = pb.build() {
                    window.paint_path(path, tt.lane(e.color));
                }
            }
        },
    )
    .absolute()
    .inset_0();
    let x = lane_x(row.lane.min(lanes));
    let node = if merge {
        div().absolute().left(px(x - MERGE_DOT / 2.)).top(px((ROW_H - MERGE_DOT) / 2.)).size(px(MERGE_DOT)).rounded_full().bg(color).border_2().border_color(t.surface)
    } else {
        div()
            .absolute()
            .left(px(x - AVATAR / 2.))
            .top(px((ROW_H - AVATAR) / 2.))
            .size(px(AVATAR))
            .rounded_full()
            .bg(color)
            .border_2()
            .border_color(if is_head { t.ink } else { t.surface })
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(8.5))
            .font_weight(FontWeight::BOLD)
            .text_color(gpui::white())
            .child(initials(&c.author))
    };
    let graph_cell = div().relative().flex_none().w(px(graph_w)).h(px(ROW_H)).overflow_hidden().child(edges).child(node);

    div()
        .id(("commit", ix))
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(ROW_H))
        .px(px(8.))
        .cursor_pointer()
        .when(selected, |d| d.bg(t.accent_soft))
        .when(!selected, |d| d.hover(move |s| s.bg(hover)))
        .when(dim, |d| d.opacity(0.35))
        .child(refs_cell)
        .child(graph_cell)
        .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).when(merge, |d| d.text_color(t.ink_muted)).when(is_head, |d| d.font_weight(FontWeight::SEMIBOLD)).child(c.summary.clone()))
        .child(div().flex_none().w(px(AUTHOR_W)).truncate().text_size(px(12.)).text_color(t.ink_muted).child(c.author.clone()))
        .child(div().flex_none().w(px(DATE_W)).text_size(px(12.)).text_color(t.ink_muted).child(datetime(c.time, c.offset)))
        .child(div().flex_none().w(px(SHA_W)).font_family(MONO).text_size(px(11.)).text_color(t.ink_muted).child(c.short()))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.list_focus.focus(window, cx);
            this.select(ix, false, cx);
        }))
        .context_menu({
            let id = c.id;
            let branches: Vec<String> = refs.iter().filter(|r| r.kind == RefKind::Local && h.head_branch.as_deref() != Some(r.name.as_str())).map(|r| r.name.clone()).collect();
            move |mut menu, _, _| {
                for b in &branches {
                    let (w, b) = (weak.clone(), b.clone());
                    menu = menu.item(PopupMenuItem::new(i18n::t!("app.menu.checkout", name = b)).on_click(move |_, _, cx| {
                        w.update(cx, |this, cx| this.run_op(Op::Switch(b.clone()), cx)).ok();
                    }));
                }
                let w = weak.clone();
                menu = menu.item(PopupMenuItem::new(i18n::t!("app.menu.new_branch")).on_click(move |_, window, cx| {
                    w.update(cx, |this, cx| this.open_prompt(PromptKind::NewBranch(id), window, cx)).ok();
                }));
                let w = weak.clone();
                menu.separator().item(PopupMenuItem::new(i18n::t!("app.menu.copy_sha")).on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(id.to_string()));
                    w.update(cx, |this, cx| this.toast(i18n::t!("app.toast.copied", sha = id.to_hex_with_len(7)), cx)).ok();
                }))
            }
        })
        .into_any_element()
}

/// The ref worth showing first: checked-out branch, then a branch with an agent at work, then
/// local, remote, tag.
fn pick_ref<'a>(refs: &[&'a RefInfo], head: Option<&str>, badges: &BranchBadges) -> Option<&'a RefInfo> {
    let local = |r: &RefInfo| r.kind == RefKind::Local;
    refs.iter().copied().min_by_key(|r| (!(local(r) && Some(r.name.as_str()) == head), !(local(r) && badges.has(&r.name)), r.kind))
}

pub fn ref_pill(t: &Tokens, r: &RefInfo, is_head: bool, lane: Hsla) -> Div {
    let (fg, bg, ic) = match r.kind {
        RefKind::Local => (t.ink, lane.opacity(0.22), if is_head { Some(IconName::Check) } else { None }),
        RefKind::Remote => (t.ink_muted, t.canvas, Some(IconName::Globe)),
        RefKind::Tag => (t.tag, t.tag.opacity(0.14), None),
    };
    div()
        .flex()
        .flex_shrink_1()
        .min_w_0()
        .items_center()
        .gap(px(4.))
        .h(px(20.))
        .px(px(7.))
        .rounded(px(6.))
        .bg(bg)
        .border_1()
        .border_color(if r.kind == RefKind::Local { lane.opacity(0.7) } else { t.border })
        .text_size(px(11.))
        .font_weight(if is_head { FontWeight::BOLD } else { FontWeight::MEDIUM })
        .text_color(fg)
        .children(ic.map(|i| crate::ui::icon(i, 11., fg).flex_none()))
        .child(div().truncate().child(r.name.clone()))
}
