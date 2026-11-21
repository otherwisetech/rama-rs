//! Worktree map: the main worktree in the centre and every other worktree as a card linked to
//! it (radial, or grouped by the base branch each one is closest to), pan with drag or scroll,
//! zoom with ⌘+scroll, pinch or ⌘+/-; a sortable list for many worktrees; the side panel and
//! the dialogs.
use crate::app::RamaApp;
use crate::cleanup::{backdrop, code_block};
use crate::detail::DETAIL_W;
use crate::map::{Dialog, NewMode, Sort, ZOOM_MAX, ZOOM_MIN};
use crate::theme::{MONO, Tokens};
use crate::ui::{self, icon};
use crate::util::{ago, bytes, now};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable as _, IconName, Sizable as _};
use repo::WorktreeEntry;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const CARD_W: f32 = 250.;
const CARD_H: f32 = 150.;
const RING: f32 = 330.;

/// Card centres and (label, x, y) group headings.
type Layout = (Vec<(f32, f32)>, Vec<(String, f32, f32)>);
/// Title, body, OK label, danger, ready, OK action.
type DialogParts = (String, AnyElement, String, bool, bool, fn(&mut RamaApp, &mut Context<RamaApp>));

/// Where each card's centre goes, in map units (the main worktree at 0,0), plus group labels.
fn layout(entries: &[WorktreeEntry], grouped: bool, default: Option<&str>) -> Layout {
    let mut pos = vec![(0., 0.); entries.len()];
    let others: Vec<usize> = (1..entries.len()).collect();
    let mut labels = Vec::new();
    if others.is_empty() {
        return (pos, labels);
    }
    if !grouped {
        // fill rings outward, as many cards per ring as fit around it
        let (mut ring, mut placed) = (0usize, 0usize);
        while placed < others.len() {
            let r = RING + ring as f32 * (CARD_H + 70.);
            let fit = ((std::f32::consts::TAU * r) / (CARD_W + 30.)).floor().max(1.) as usize;
            let n = fit.min(others.len() - placed);
            for k in 0..n {
                let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * k as f32 / n as f32 + ring as f32 * 0.3;
                pos[others[placed + k]] = (r * a.cos() * 1.25, r * a.sin());
            }
            placed += n;
            ring += 1;
        }
        return (pos, labels);
    }
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for &i in &others {
        let base = entries[i].info.base().unwrap_or("—").to_string(); // i18n-ok: placeholder
        match groups.iter_mut().find(|(b, _)| *b == base) {
            Some((_, v)) => v.push(i),
            None => groups.push((base, vec![i])),
        }
    }
    groups.sort_by_key(|(b, _)| (Some(b.as_str()) != default, b.clone()));
    let g = groups.len() as f32;
    for (gi, (base, idx)) in groups.iter().enumerate() {
        let x = (gi as f32 - (g - 1.) / 2.) * (CARD_W + 60.);
        labels.push((base.clone(), x, CARD_H + 40.));
        for (j, &i) in idx.iter().enumerate() {
            pos[i] = (x, CARD_H * 1.5 + 60. + j as f32 * (CARD_H + 22.));
        }
    }
    (pos, labels)
}

/// `/src/app/.worktrees/x` → `.worktrees/x`; `$HOME/…` → `~/…`.
fn short_path(p: &Path, main: Option<&Path>) -> String {
    if let Some(m) = main
        && p != m
        && let Ok(rel) = p.strip_prefix(m)
    {
        return rel.display().to_string();
    }
    if let Some(h) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rel) = p.strip_prefix(&h)
    {
        return format!("~/{}", rel.display());
    }
    p.display().to_string()
}

/// The map's toolbar loses its button labels when the column between the sidebar and the side
/// panel is narrower than this.
const TOOLBAR_FULL_W: f32 = 900.;

fn toolbar_compact(window_w: f32) -> bool {
    window_w - crate::sidebar::SIDE_W - DETAIL_W < TOOLBAR_FULL_W
}

fn ab(a: u32, b: u32) -> String {
    format!("↑{a} ↓{b}") // i18n-ok: arrows
}

impl RamaApp {
    fn lane_color(&self, t: &Tokens, e: &WorktreeEntry, i: usize) -> Hsla {
        let row = e.wt.head.and_then(|h| self.history.as_ref().and_then(|hist| hist.index.get(&h).map(|&r| hist.rows[r].color)));
        t.lane(row.unwrap_or(i as u16))
    }

    fn is_current(&self, e: &WorktreeEntry) -> bool {
        self.map.current.as_deref() == Some(e.wt.path.as_path())
    }

    fn size_text(&self, e: &WorktreeEntry) -> Option<String> {
        if e.wt.prunable.is_some() {
            return None;
        }
        match self.map.usage(&e.wt.path) {
            Some(u) => {
                let mut s = bytes(u.total);
                if let Some((name, n)) = u.parts.iter().find(|(n, _)| n == "node_modules").or(u.parts.first()) {
                    s.push_str(&format!(" · {name} {}", bytes(*n))); // i18n-ok: separator
                }
                Some(s)
            }
            None => Some(i18n::t!("app.map.measuring")),
        }
    }

    fn flags(&self, t: &Tokens, e: &WorktreeEntry) -> Vec<(String, Hsla)> {
        let mut f = Vec::new();
        if self.is_current(e) {
            f.push((i18n::t!("app.map.flag_current"), t.accent));
        }
        if e.wt.locked.is_some() {
            f.push((i18n::t!("app.map.flag_locked"), t.tag));
        }
        if e.wt.prunable.is_some() {
            f.push((i18n::t!("app.map.flag_prunable"), t.danger));
        }
        if e.info.merged && !e.wt.main {
            f.push((i18n::t!("app.map.flag_merged"), t.added));
        }
        if e.info.gone {
            f.push((i18n::t!("app.map.flag_gone"), t.tag));
        }
        f
    }

    fn head_label(e: &WorktreeEntry) -> String {
        match (&e.wt.branch, e.wt.head) {
            (Some(b), _) => b.clone(),
            (None, Some(h)) => i18n::t!("app.topbar.detached", sha = h.to_hex_with_len(7)),
            (None, None) if e.wt.bare => i18n::t!("app.map.bare"),
            _ => String::new(),
        }
    }

    pub(crate) fn render_map(&mut self, t: &Tokens, window_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let toolbar = self.map_toolbar(t, toolbar_compact(window_w), cx);
        let body: AnyElement = match &self.map.entries {
            None => div().size_full().flex().items_center().justify_center().gap(px(8.)).text_color(t.ink_muted).child(Spinner::new().small().color(t.accent)).child(i18n::t!("app.map.loading")).into_any_element(),
            Some(Err(e)) => div().size_full().flex().items_center().justify_center().text_color(t.danger).child(e.clone()).into_any_element(),
            Some(Ok(list)) => {
                let list = list.clone();
                if self.map.list { self.map_list(t, &list, cx) } else { self.map_canvas(t, &list, cx) }
            }
        };
        div().flex().flex_col().size_full().bg(t.canvas).child(toolbar).child(div().flex_1().min_h_0().child(body)).children(self.agent_legend(t)).into_any_element()
    }

    /// Wraps onto more rows when narrow; `compact` drops the button labels (kept as tooltips).
    fn map_toolbar(&self, t: &Tokens, compact: bool, cx: &mut Context<Self>) -> Div {
        let seg = |id: &'static str, label: String, on: bool| {
            div().id(id).flex().items_center().h(px(26.)).px(px(10.)).rounded(px(7.)).text_size(px(12.)).cursor_pointer().when(on, |d| d.bg(t.surface).font_weight(FontWeight::SEMIBOLD).shadow_sm()).when(!on, |d| d.text_color(t.ink_muted)).child(label)
        };
        let group = |a: Stateful<Div>, b: Stateful<Div>| div().flex().flex_none().items_center().gap(px(2.)).p(px(2.)).rounded(px(9.)).bg(t.border.opacity(0.6)).child(a).child(b);
        let list = self.map.list;
        let mode = group(seg("map-mode-map", i18n::t!("app.map.mode_map"), !list).on_click(cx.listener(|this, _, _, cx| this.set_map_mode(false, cx))), seg("map-mode-list", i18n::t!("app.map.mode_list"), list).on_click(cx.listener(|this, _, _, cx| this.set_map_mode(true, cx))));
        let grouped = self.map.grouped;
        let layout = group(seg("map-radial", i18n::t!("app.map.layout_radial"), !grouped).on_click(cx.listener(|this, _, _, cx| this.set_map_layout(false, cx))), seg("map-grouped", i18n::t!("app.map.layout_grouped"), grouped).on_click(cx.listener(|this, _, _, cx| this.set_map_layout(true, cx))));
        let small = |id: &'static str, ic: IconName, tip: String| ui::icon_button(t, id, ic, true).size(px(28.)).rounded(px(7.)).tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
        let zoom = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .child(small("map-zoom-out", IconName::Minus, i18n::t!("app.map.zoom_out")).on_click(cx.listener(|this, _, _, cx| this.map_zoom_key(Some(1. / 1.2), cx))))
            .child(div().id("map-zoom-reset").w(px(44.)).text_center().text_size(px(11.5)).text_color(t.ink_muted).cursor_pointer().child(format!("{:.0}%", self.map.zoom * 100.)).on_click(cx.listener(|this, _, _, cx| this.map_zoom_key(None, cx))))
            .child(small("map-zoom-in", IconName::Plus, i18n::t!("app.map.zoom_in")).on_click(cx.listener(|this, _, _, cx| this.map_zoom_key(Some(1.2), cx))));
        let busy = self.work.job.is_some();
        let btn = |id: &'static str, ic: IconName, label: String| {
            let b = ui::outline_button(t, id).h(px(30.)).px(px(if compact { 8. } else { 10. })).text_size(px(12.5)).child(icon(ic, 14., t.ink)).when(busy, |d| d.opacity(0.5));
            if compact { b.tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx)) } else { b.child(label) }
        };
        let count = self.map.list().map(|l| i18n::tn!("app.map.count", l.len())).unwrap_or_default();
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(mode)
            .when(!list, |d| d.child(layout).child(zoom))
            .when(list, |d| d.child(self.sort_menu(t, cx)))
            .child(div().flex_none().text_size(px(12.)).text_color(t.ink_muted).child(count))
            .when(self.map.loading, |d| d.child(Spinner::new().small().color(t.accent)))
            .child(div().flex_1())
            .child(small("map-refresh", IconName::Redo, i18n::t!("app.map.refresh")).on_click(cx.listener(|this, _, _, cx| this.reload_map(true, cx))))
            .child(btn("map-new", IconName::Plus, i18n::t!("app.map.new")).when(!busy, |d| d.on_click(cx.listener(|this, _, window, cx| this.open_new_worktree(None, window, cx)))))
            .child(btn("map-prune", IconName::Delete, i18n::t!("app.map.prune")).when(!busy, |d| d.on_click(cx.listener(|this, _, _, cx| this.ask_prune(cx)))))
            .child(btn("map-clean", IconName::Delete, i18n::t!("app.clean.menu")).when(!busy, |d| d.on_click(cx.listener(|this, _, window, cx| this.open_cleanup(window, cx)))))
            .child({
                let free = self.reclaimable(None);
                let label = if free > 0 { i18n::t!("app.reclaim.menu_size", size = bytes(free)) } else { i18n::t!("app.reclaim.menu") };
                btn("map-reclaim", IconName::HardDrive, label).when(!busy, |d| d.on_click(cx.listener(|this, _, _, cx| this.open_reclaim(cx))))
            })
            .child(small("map-settings", IconName::Settings, i18n::t!("app.map.settings")).on_click(cx.listener(|this, _, window, cx| this.open_map_settings(window, cx))))
    }

    fn sort_menu(&self, _t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let cur = self.map.sort;
        let label = |s: Sort| match s {
            Sort::Name => i18n::t!("app.map.sort_name"),
            Sort::Size => i18n::t!("app.map.sort_size"),
            Sort::Age => i18n::t!("app.map.sort_age"),
            Sort::Merged => i18n::t!("app.map.sort_merged"),
        };
        let weak = cx.entity().downgrade();
        Button::new("map-sort").ghost().small().icon(IconName::SortAscending).label(i18n::t!("app.map.sort_by", by = label(cur))).dropdown_menu(move |mut menu, _, _| {
            for s in [Sort::Name, Sort::Size, Sort::Age, Sort::Merged] {
                let w = weak.clone();
                menu = menu.item(PopupMenuItem::new(label(s)).checked(s == cur).on_click(move |_, _, cx| {
                    w.update(cx, |this, cx| {
                        this.map.sort = s;
                        cx.notify();
                    })
                    .ok();
                }));
            }
            menu
        })
    }

    fn map_canvas(&mut self, t: &Tokens, list: &Rc<Vec<WorktreeEntry>>, cx: &mut Context<Self>) -> AnyElement {
        let default = self.map.bases.first().map(|b| b.name.clone());
        let (pos, labels) = layout(list, self.map.grouped, default.as_deref());
        let (zoom, pan) = (self.map.zoom, self.map.pan);
        let colors: Vec<Hsla> = list.iter().enumerate().map(|(i, e)| self.lane_color(t, e, i)).collect();
        let dim: Vec<bool> = list.iter().map(|e| e.info.merged || e.wt.prunable.is_some()).collect();
        let viewport = self.map.viewport.clone();
        let p2 = pos.clone();
        let edge_colors = colors.clone();
        let edges = canvas(
            move |b, _, _| viewport.set(b),
            move |b, _, window, _| {
                let c = b.center();
                let at = |(x, y): (f32, f32)| point(c.x + px(pan.0 + x * zoom), c.y + px(pan.1 + y * zoom));
                let from = at(p2[0]);
                for i in 1..p2.len() {
                    let to = at(p2[i]);
                    let mut pb = PathBuilder::stroke(px((2.5 * zoom).max(1.)));
                    pb.move_to(from);
                    let mid_y = (from.y + to.y) / 2.;
                    pb.cubic_bezier_to(to, point(from.x, mid_y), point(to.x, mid_y));
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, if dim[i] { edge_colors[i].opacity(0.3) } else { edge_colors[i] });
                    }
                }
            },
        )
        .absolute()
        .inset_0();
        let z = |v: f32| px(v * zoom);
        let mut layer = div().absolute().left(relative(0.5)).top(relative(0.5)).size_0();
        for (label, x, y) in labels {
            layer = layer.child(div().absolute().left(z(x - CARD_W / 2.) + px(pan.0)).top(z(y) + px(pan.1)).w(z(CARD_W)).text_center().text_size(z(12.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(i18n::t!("app.map.group_base", base = label)));
        }
        let agents = self.map.agent_commands();
        for (i, e) in list.iter().enumerate() {
            layer = layer.child(self.map_card(t, i, e, pos[i], colors[i], &agents, cx));
        }
        let weak = cx.entity().downgrade();
        div()
            .id("map-canvas")
            .relative()
            .size_full()
            .overflow_hidden()
            .cursor(if self.map.drag.is_some() { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                this.map.drag = Some((ev.position, this.map.pan));
                this.map.moved = false;
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if let Some((start, pan)) = this.map.drag {
                    if ev.pressed_button != Some(MouseButton::Left) {
                        this.map.drag = None;
                    } else {
                        let (dx, dy) = (f32::from(ev.position.x - start.x), f32::from(ev.position.y - start.y));
                        this.map.moved |= dx.abs() + dy.abs() > 3.;
                        this.map.pan = (pan.0 + dx, pan.1 + dy);
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.map.drag = None;
                cx.notify();
            }))
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                let d = ev.delta.pixel_delta(px(16.));
                if ev.modifiers.platform || ev.modifiers.control {
                    this.map.zoom_by((1. + f32::from(d.y) / 300.).clamp(0.5, 2.), Some(ev.position));
                } else {
                    this.map.pan.0 += f32::from(d.x);
                    this.map.pan.1 += f32::from(d.y);
                }
                cx.notify();
            }))
            .on_pinch(move |ev: &PinchEvent, _, cx| {
                weak.update(cx, |this, cx| {
                    this.map.zoom_by(1. + ev.delta, Some(ev.position));
                    cx.notify();
                })
                .ok();
            })
            .on_click(cx.listener(|this, ev: &ClickEvent, _, cx| {
                // a click on empty space (not the end of a drag) clears the selection
                if ev.click_count() == 1 && !this.map.moved {
                    this.map.selected = None;
                    cx.notify();
                }
            }))
            .child(edges)
            .child(layer)
            .child(div().absolute().bottom(px(10.)).left(px(12.)).text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.map.hint", min = format!("{:.0}", ZOOM_MIN * 100.), max = format!("{:.0}", ZOOM_MAX * 100.))))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn map_card(&self, t: &Tokens, i: usize, e: &WorktreeEntry, (x, y): (f32, f32), color: Hsla, agents: &[String], cx: &mut Context<Self>) -> AnyElement {
        let zoom = self.map.zoom;
        let pan = self.map.pan;
        let z = |v: f32| px(v * zoom);
        let main_path = self.map.list().and_then(|l| l.first()).map(|m| m.wt.path.clone());
        let selected = self.map.selected.as_deref() == Some(e.wt.path.as_path());
        let dim = e.info.merged && !e.wt.main;
        let line = |text: String, c: Hsla, size: f32| div().truncate().text_size(z(size)).line_height(z(size * 1.35)).text_color(c).child(text);
        let mut card = div()
            .id(("map-card", i))
            .absolute()
            .left(z(x - CARD_W / 2.) + px(pan.0))
            .top(z(y - CARD_H / 2.) + px(pan.1))
            .w(z(CARD_W))
            .flex()
            .flex_col()
            .gap(z(3.))
            .p(z(10.))
            .rounded(z(12.))
            .bg(t.surface)
            .border(z(if selected { 2.5 } else { 1.5 }))
            .border_color(if selected { t.accent } else { color.opacity(if dim { 0.4 } else { 0.9 }) })
            .shadow_md()
            .cursor_pointer()
            .when(dim, |d| d.opacity(0.7))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let title = if e.wt.main { self.repo.as_ref().map(|r| r.name()).unwrap_or_default() } else { short_path(&e.wt.path, main_path.as_deref()) };
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(z(6.))
                .child(div().flex_none().size(z(9.)).rounded_full().bg(color))
                .child(div().flex_1().min_w_0().truncate().text_size(z(13.)).font_weight(FontWeight::SEMIBOLD).child(title))
                .children(self.agent_badge(t, &e.wt.path, 2. * zoom, ("map-card-agent", i), cx))
                .when(e.wt.main, |d| d.child(div().flex_none().text_size(z(10.)).text_color(t.accent).child(i18n::t!("app.map.main")))),
        );
        card = card.child(line(Self::head_label(e), t.ink, 12.));
        if zoom >= 0.55 {
            let mut dist = Vec::new();
            if let Some(up) = &e.info.upstream {
                dist.push(if e.info.gone { i18n::t!("app.map.upstream_gone", upstream = up) } else { format!("{up} {}", ab(e.info.ahead, e.info.behind)) }); // i18n-ok: name and arrows
            }
            if let (Some((a, b)), Some(base)) = (e.info.vs_default(), self.map.bases.first()) {
                dist.push(format!("{} {}", base.name, ab(a, b))); // i18n-ok: name and arrows
            }
            card = card.child(line(dist.join(" · "), t.ink_muted, 11.)); // i18n-ok: separator
            let changed = e.info.changed();
            let state = if e.wt.prunable.is_some() {
                i18n::t!("app.map.missing")
            } else if changed > 0 {
                i18n::tn!("app.map.changed", changed)
            } else {
                i18n::t!("app.wip.clean")
            };
            card = card.child(line(state, if changed > 0 { t.accent } else { t.ink_muted }, 11.));
            if let (Some(time), Some(subj)) = (e.info.time, &e.info.subject) {
                card = card.child(line(format!("{} · {subj}", ago(time, now())), t.ink_muted, 11.)); // i18n-ok: separator
            }
            if let Some(s) = self.size_text(e) {
                card = card.child(line(s, t.ink_muted, 11.));
            }
            let flags = self.flags(t, e);
            if !flags.is_empty() {
                card = card.child(div().flex().flex_wrap().gap(z(4.)).children(flags.into_iter().map(|(f, c)| div().px(z(5.)).rounded(z(4.)).bg(c.opacity(0.14)).text_color(c).text_size(z(10.)).child(f))));
            }
        }
        let path = e.wt.path.clone();
        let menu = self.node_menu(e, agents, cx);
        card.on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
            cx.stop_propagation();
            this.map.selected = Some(path.clone());
            this.convo_tab = false;
            if ev.click_count() == 2 {
                this.open_in_new_window(path.clone(), cx);
            }
            let _ = window;
            cx.notify();
        }))
        .context_menu(move |m, window, cx| menu.build(m, window, cx))
        .into_any_element()
    }

    fn map_list(&mut self, t: &Tokens, list: &Rc<Vec<WorktreeEntry>>, cx: &mut Context<Self>) -> AnyElement {
        let mut idx: Vec<usize> = (0..list.len()).collect();
        let size = |i: usize| self.map.usage(&list[i].wt.path).map(|u| u.total).unwrap_or(0);
        match self.map.sort {
            Sort::Name => idx.sort_by_key(|&i| (!list[i].wt.main, list[i].wt.path.clone())),
            Sort::Size => idx.sort_by_key(|&i| std::cmp::Reverse(size(i))),
            Sort::Age => idx.sort_by_key(|&i| list[i].info.time.unwrap_or(i64::MIN)),
            Sort::Merged => idx.sort_by_key(|&i| (!list[i].info.merged, !list[i].info.gone, list[i].wt.path.clone())),
        }
        let main_path = list.first().map(|m| m.wt.path.clone());
        let head = |w: Option<f32>, s: String| {
            let d = div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).truncate().child(s);
            match w {
                Some(w) => d.w(px(w)).flex_none(),
                None => d.flex_1().min_w_0(),
            }
        };
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(30.))
            .px(px(12.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(head(None, i18n::t!("app.map.col_path")))
            .child(head(Some(200.), i18n::t!("app.map.col_branch")))
            .child(head(Some(150.), i18n::t!("app.map.col_distance")))
            .child(head(Some(90.), i18n::t!("app.map.col_changes")))
            .child(head(Some(110.), i18n::t!("app.map.col_age")))
            .child(head(Some(170.), i18n::t!("app.map.col_size")))
            .child(head(Some(140.), i18n::t!("app.map.col_flags")));
        let hover = if t.dark { t.border } else { t.canvas };
        let agents = self.map.agent_commands();
        let mut rows = div().id("map-list").flex().flex_col().flex_1().min_h_0().overflow_y_scroll();
        for (k, &i) in idx.iter().enumerate() {
            let e = &list[i];
            let color = self.lane_color(t, e, i);
            let selected = self.map.selected.as_deref() == Some(e.wt.path.as_path());
            let mut dist = Vec::new();
            if let Some(_up) = &e.info.upstream {
                dist.push(if e.info.gone { i18n::t!("app.map.flag_gone") } else { ab(e.info.ahead, e.info.behind) });
            }
            if let (Some((a, b)), Some(base)) = (e.info.vs_default(), self.map.bases.first()) {
                dist.push(format!("{} {}", base.name, ab(a, b))); // i18n-ok: name and arrows
            }
            let changed = e.info.changed();
            let path = e.wt.path.clone();
            let menu = self.node_menu(e, &agents, cx);
            rows = rows.child(
                div()
                    .id(("map-row", k))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(32.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(t.border.opacity(0.5))
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.accent_soft))
                    .when(!selected, |d| d.hover(move |s| s.bg(hover)))
                    .when(e.info.merged && !e.wt.main, |d| d.text_color(t.ink_muted))
                    .child(div().flex().flex_1().min_w_0().items_center().gap(px(6.)).child(div().flex_none().size(px(8.)).rounded_full().bg(color)).children(self.agent_badge(t, &e.wt.path, 1.5, ("map-row-agent", k), cx)).child(div().truncate().when(e.wt.main, |d| d.font_weight(FontWeight::SEMIBOLD)).child(if e.wt.main { self.repo.as_ref().map(|r| r.name()).unwrap_or_default() } else { short_path(&e.wt.path, main_path.as_deref()) })))
                    .child(div().flex_none().w(px(200.)).truncate().child(Self::head_label(e)))
                    .child(div().flex_none().w(px(150.)).truncate().text_size(px(11.5)).text_color(t.ink_muted).child(dist.join(" · "))) // i18n-ok: separator
                    .child(div().flex_none().w(px(90.)).text_size(px(11.5)).text_color(if changed > 0 { t.accent } else { t.ink_muted }).child(if changed > 0 { i18n::tn!("app.map.changed", changed) } else { i18n::t!("app.wip.clean") }))
                    .child(div().flex_none().w(px(110.)).text_size(px(11.5)).text_color(t.ink_muted).child(e.info.time.map(|tm| ago(tm, now())).unwrap_or_default()))
                    .child(div().flex_none().w(px(170.)).truncate().text_size(px(11.5)).text_color(t.ink_muted).child(self.size_text(e).unwrap_or_default()))
                    .child(div().flex_none().w(px(140.)).flex().gap(px(3.)).overflow_hidden().children(self.flags(t, e).into_iter().map(|(f, c)| div().flex_none().px(px(5.)).rounded(px(4.)).bg(c.opacity(0.14)).text_color(c).text_size(px(10.)).child(f))))
                    .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                        this.map.selected = Some(path.clone());
            this.convo_tab = false;
                        if ev.click_count() == 2 {
                            this.open_in_new_window(path.clone(), cx);
                        }
                        cx.notify();
                    }))
                    .context_menu(move |m, window, cx| menu.build(m, window, cx)),
            );
        }
        div().flex().flex_col().size_full().bg(t.surface).child(header).child(rows).into_any_element()
    }

    fn node_menu(&self, e: &WorktreeEntry, agents: &[String], cx: &mut Context<Self>) -> NodeMenu {
        NodeMenu {
            weak: cx.entity().downgrade(),
            path: e.wt.path.clone(),
            main: e.wt.main,
            locked: e.wt.locked.is_some(),
            missing: e.wt.prunable.is_some(),
            agents: agents.to_vec(),
            busy: self.work.job.is_some(),
        }
    }

    pub(crate) fn render_map_panel(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        if self.convo.is_some() {
            let tab = |id: &'static str, label: String, on: bool| div().id(id).flex().items_center().h(px(26.)).px(px(10.)).rounded(px(7.)).text_size(px(12.)).cursor_pointer().when(on, |d| d.bg(t.surface).font_weight(FontWeight::SEMIBOLD).shadow_sm()).when(!on, |d| d.text_color(t.ink_muted)).child(label);
            let convo = self.convo_tab;
            let tabs = div()
                .flex()
                .flex_none()
                .p(px(8.))
                .border_b_1()
                .border_color(t.border)
                .child(
                    div()
                        .flex()
                        .gap(px(2.))
                        .p(px(2.))
                        .rounded(px(9.))
                        .bg(t.border.opacity(0.6))
                        .child(tab("panel-details", i18n::t!("app.convo.tab_details"), !convo).on_click(cx.listener(|this, _, _, cx| {
                            this.convo_tab = false;
                            cx.notify();
                        })))
                        .child(tab("panel-convo", i18n::t!("app.convo.tab"), convo).on_click(cx.listener(|this, _, _, cx| {
                            this.convo_tab = true;
                            this.refresh_convo(cx);
                            cx.notify();
                        }))),
                );
            let body = if convo { self.render_convo(t, cx) } else { self.render_map_details(t, cx) };
            return div().flex().flex_col().flex_none().w(px(DETAIL_W)).h_full().bg(t.surface).border_l_1().border_color(t.border).child(tabs).child(div().flex().flex_col().flex_1().min_h_0().child(body)).into_any_element();
        }
        self.render_map_details(t, cx)
    }

    fn render_map_details(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let panel = div().id("map-panel").flex().flex_col().flex_none().w(px(DETAIL_W)).h_full().bg(t.surface).border_l_1().border_color(t.border).overflow_y_scroll();
        let Some(list) = self.map.list() else { return panel.into_any_element() };
        let Some(e) = self.map.selected.as_ref().and_then(|p| self.map.entry(p)).cloned() else {
            // nothing selected: totals
            let total: u64 = list.iter().filter_map(|e| self.map.usage(&e.wt.path)).map(|u| u.total).sum();
            let merged = list.iter().filter(|e| e.info.merged && !e.wt.main).count();
            let dirty = list.iter().filter(|e| e.info.changed() > 0).count();
            let prunable = list.iter().filter(|e| e.wt.prunable.is_some()).count();
            let free = self.reclaimable(None);
            let shared: u64 = self.map.shared_users.keys().filter_map(|p| self.map.usage(p)).map(|u| u.total).sum();
            // the main worktree's parent holds the other worktrees (and maybe their siblings)
            let diskmap = self.map.diskmap.then(|| list.first().map(|m| m.wt.path.parent().map(Path::to_path_buf).unwrap_or_else(|| m.wt.path.clone()))).flatten();
            return panel
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .p(px(16.))
                        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.map.title")))
                        .child(div().text_size(px(13.)).child(i18n::tn!("app.map.count", list.len())))
                        .child(div().text_size(px(13.)).text_color(t.ink_muted).child(i18n::t!("app.map.total_size", size = bytes(total))))
                        .child(div().text_size(px(13.)).text_color(t.ink_muted).child(i18n::tn!("app.map.n_merged", merged)))
                        .child(div().text_size(px(13.)).text_color(t.ink_muted).child(i18n::tn!("app.map.n_dirty", dirty)))
                        .when(prunable > 0, |d| d.child(div().text_size(px(13.)).text_color(t.danger).child(i18n::tn!("app.map.n_prunable", prunable))))
                        .child(div().text_size(px(13.)).text_color(if free > 0 { t.accent } else { t.ink_muted }).child(i18n::t!("app.reclaim.total", size = bytes(free))))
                        .when(shared > 0, |d| d.child(div().text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.reclaim.shared_total", size = bytes(shared)))))
                        .child(ui::outline_button(t, "totals-reclaim").h(px(30.)).text_size(px(12.5)).child(icon(IconName::HardDrive, 14., t.ink)).child(i18n::t!("app.reclaim.menu")).on_click(cx.listener(|this, _, _, cx| this.open_reclaim(cx))))
                        .when_some(diskmap, |d, scan| d.child(ui::outline_button(t, "totals-diskmap").h(px(30.)).text_size(px(12.5)).child(icon(IconName::ChartPie, 14., t.ink)).child(i18n::t!("app.reclaim.open_diskmap")).on_click(cx.listener(move |this, _, _, cx| this.open_in_diskmap(scan.clone(), cx)))))
                        .child(div().pt(px(8.)).text_size(px(12.)).line_height(px(17.)).text_color(t.ink_muted).child(i18n::t!("app.map.select_hint"))),
                )
                .into_any_element();
        };
        let main_path = list.first().map(|m| m.wt.path.clone());
        let busy = self.work.job.is_some();
        let kv = |k: String, v: String| div().flex().flex_col().gap(px(2.)).child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(k)).child(div().text_size(px(12.5)).line_height(px(17.)).child(v));
        let mut info = div().flex().flex_col().gap(px(10.)).p(px(16.)).border_b_1().border_color(t.border);
        info = info
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).line_height(px(20.)).child(if e.wt.main { self.repo.as_ref().map(|r| r.name()).unwrap_or_default() } else { short_path(&e.wt.path, main_path.as_deref()) }))
            .child(div().font_family(MONO).text_size(px(11.)).text_color(t.ink_muted).child(e.wt.path.display().to_string()))
            .child(div().flex().flex_wrap().gap(px(4.)).children(self.flags(t, &e).into_iter().map(|(f, c)| div().px(px(6.)).rounded(px(5.)).bg(c.opacity(0.14)).text_color(c).text_size(px(11.)).child(f))))
            .child(kv(i18n::t!("app.map.col_branch"), Self::head_label(&e)));
        if let Some(rows) = self.agent_rows(t, &e.wt.path, cx) {
            info = info.child(rows);
        }
        if let Some(up) = &e.info.upstream {
            info = info.child(kv(i18n::t!("app.map.upstream"), if e.info.gone { i18n::t!("app.map.upstream_gone", upstream = up) } else { format!("{up}  {}", ab(e.info.ahead, e.info.behind)) })); // i18n-ok: name and arrows
        }
        for (base, a, b) in &e.info.vs {
            info = info.child(kv(i18n::t!("app.map.vs", base = base), i18n::t!("app.map.ahead_behind", ahead = a, behind = b)));
        }
        if let Some(l) = &e.wt.locked {
            info = info.child(kv(i18n::t!("app.map.flag_locked"), if l.is_empty() { i18n::t!("app.map.no_reason") } else { l.clone() }));
        }
        if let Some(p) = &e.wt.prunable {
            info = info.child(kv(i18n::t!("app.map.flag_prunable"), p.clone()));
        }
        if let (Some(time), Some(subj)) = (e.info.time, &e.info.subject) {
            info = info.child(kv(i18n::t!("app.map.last_commit"), format!("{} · {subj}", ago(time, now())))); // i18n-ok: separator
        }
        let changed = e.info.changed();
        let files: Vec<String> = e.info.status.as_ref().map(|s| s.staged.iter().chain(s.unstaged.iter()).map(|f| f.path.clone()).collect()).unwrap_or_default();
        let mut uniq: Vec<String> = Vec::new();
        for f in files {
            if !uniq.contains(&f) {
                uniq.push(f);
            }
        }
        info = info.child(kv(i18n::t!("app.map.col_changes"), if changed > 0 { i18n::tn!("app.map.changed", changed) } else { i18n::t!("app.wip.clean") }));
        if !uniq.is_empty() {
            info = info.child(div().flex().flex_col().text_size(px(11.5)).font_family(MONO).text_color(t.ink_muted).children(uniq.iter().take(12).map(|f| div().truncate().child(f.clone()))).when(uniq.len() > 12, |d| d.child(div().child(i18n::tn!("app.work.more", uniq.len() - 12)))));
        }
        let size: AnyElement = match self.map.usage(&e.wt.path) {
            Some(u) => div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(12.5)).child(bytes(u.total)))
                .children(u.parts.iter().map(|(n, b)| div().flex().text_size(px(11.5)).text_color(t.ink_muted).child(div().flex_1().font_family(MONO).child(n.clone())).child(bytes(*b))))
                .into_any_element(),
            None if self.map.measuring(&e.wt.path) => div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(t.ink_muted).child(Spinner::new().small().color(t.accent)).child(i18n::t!("app.map.measuring")).into_any_element(),
            None => div().into_any_element(),
        };
        info = info.child(div().flex().flex_col().gap(px(2.)).child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(i18n::t!("app.map.col_size"))).child(size));
        let free = self.reclaimable(Some(&e.wt.path));
        if free > 0 {
            info = info.child(div().id("wt-reclaim").text_size(px(12.)).text_color(t.accent).cursor_pointer().child(i18n::t!("app.reclaim.worktree", size = bytes(free))).on_click(cx.listener(|this, _, _, cx| this.open_reclaim(cx))));
        }

        let path = e.wt.path.clone();
        let action = |id: &'static str, ic: IconName, label: String, enabled: bool| ui::outline_button(t, id).w_full().h(px(30.)).text_size(px(12.5)).child(icon(ic, 14., t.ink)).child(label).when(!enabled, |d| d.opacity(0.45).cursor_default());
        let mut actions = div().flex().flex_col().gap(px(6.)).p(px(16.));
        let missing = e.wt.prunable.is_some();
        let p = path.clone();
        actions = actions.child(action("act-finder", IconName::FolderOpen, i18n::t!("app.map.open_finder"), !missing).when(!missing, |d| d.on_click(cx.listener(move |this, _, _, cx| this.reveal(&p, cx)))));
        let p = path.clone();
        actions = actions.child(action("act-rama", IconName::ExternalLink, i18n::t!("app.map.open_rama"), !missing).when(!missing, |d| d.on_click(cx.listener(move |this, _, _, cx| this.open_in_new_window(p.clone(), cx)))));
        let p = path.clone();
        actions = actions.child(action("act-copy", IconName::Copy, i18n::t!("app.map.copy_path"), true).on_click(cx.listener(move |this, _, _, cx| this.copy_path(&p, cx))));
        if self.map.diskmap {
            let p = path.clone();
            actions = actions.child(action("act-diskmap", IconName::ChartPie, i18n::t!("app.reclaim.open_diskmap"), !missing).when(!missing, |d| d.on_click(cx.listener(move |this, _, _, cx| this.open_in_diskmap(p.clone(), cx)))));
        }
        let p = path.clone();
        actions = actions.child(action("act-term", IconName::SquareTerminal, i18n::t!("app.map.open_terminal"), !missing).when(!missing, |d| d.on_click(cx.listener(move |this, _, _, cx| this.launch(p.clone(), None, cx)))));
        let agents = self.map.agent_commands();
        let weak = cx.entity().downgrade();
        let p = path.clone();
        let agent_btn = Button::new("act-agent").outline().small().w_full().icon(IconName::Bot).label(i18n::t!("app.map.start_agent")).disabled(missing).dropdown_menu(move |mut menu, _, _| {
            if agents.is_empty() {
                menu = menu.item(PopupMenuItem::new(i18n::t!("app.map.no_agents")).disabled(true));
            }
            for a in &agents {
                let (w, p, a) = (weak.clone(), p.clone(), a.clone());
                menu = menu.item(PopupMenuItem::new(a.clone()).on_click(move |_, _, cx| {
                    w.update(cx, |this, cx| this.launch(p.clone(), Some(a.clone()), cx)).ok();
                }));
            }
            menu
        });
        actions = actions.child(agent_btn);
        if !e.wt.main {
            let p = path.clone();
            actions = actions.child(if e.wt.locked.is_some() {
                action("act-lock", IconName::Eye, i18n::t!("app.map.unlock"), !busy).when(!busy, |d| d.on_click(cx.listener(move |this, _, _, cx| this.unlock(&p, cx))))
            } else {
                action("act-lock", IconName::EyeOff, i18n::t!("app.map.lock"), !busy && !missing).when(!busy && !missing, |d| d.on_click(cx.listener(move |this, _, window, cx| this.ask_lock(p.clone(), window, cx))))
            });
            let p = path.clone();
            let can_remove = !busy && e.wt.locked.is_none();
            actions = actions.child(action("act-remove", IconName::Delete, i18n::t!("app.map.remove"), can_remove).text_color(t.danger).when(can_remove, |d| d.on_click(cx.listener(move |this, _, _, cx| this.ask_remove_worktree(&p, cx)))));
        }
        panel.child(info).child(actions).into_any_element()
    }

    pub(crate) fn render_map_dialog(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.map.dialog.as_ref()?;
        let busy = self.work.job.is_some();
        let close = |this: &mut RamaApp, cx: &mut Context<RamaApp>| {
            this.map.dialog = None;
            cx.notify();
        };
        let text = |s: String| div().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).child(s);
        let (title, body, ok_label, danger, ready, ok): DialogParts = match d {
            Dialog::Launch { launch, .. } => {
                let mut b = div().flex().flex_col().gap(px(10.)).child(text(i18n::t!("app.map.launch_body"))).child(code_block(t, "launch-cmd", launch.display()));
                if let Some(c) = &launch.copy {
                    b = b.child(text(i18n::t!("app.map.launch_copy", command = c)));
                }
                b = b.child(div().text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.map.launch_once")));
                (i18n::t!("app.map.launch_title"), b.into_any_element(), i18n::t!("app.map.launch_ok"), false, true, |this, cx| this.confirm_launch(cx))
            }
            Dialog::Remove { entry, force, delete_branch } => {
                let path = entry.wt.path.display().to_string();
                let mut b = div().flex().flex_col().gap(px(10.));
                if *force {
                    b = b.child(text(i18n::t!("app.map.remove_force_body", path = path)));
                    let files: Vec<String> = entry.info.status.as_ref().map(|s| s.staged.iter().chain(s.unstaged.iter()).map(|f| f.path.clone()).collect()).unwrap_or_default();
                    let mut uniq: Vec<String> = Vec::new();
                    for f in files {
                        if !uniq.contains(&f) {
                            uniq.push(f);
                        }
                    }
                    if !uniq.is_empty() {
                        b = b.child(code_block(t, "remove-files", uniq.join("\n")));
                    }
                    b = b.child(code_block(t, "remove-cmd", format!("git worktree remove --force {path}"))); // i18n-ok: command line
                } else {
                    b = b.child(text(i18n::t!("app.map.remove_body", path = path))).child(code_block(t, "remove-cmd", format!("git worktree remove {path}"))); // i18n-ok: command line
                }
                if let Some(branch) = &entry.wt.branch {
                    let merged = entry.info.merged;
                    let label = if merged { i18n::t!("app.map.remove_branch", name = branch) } else { i18n::t!("app.map.remove_branch_unmerged", name = branch) };
                    b = b.child(Checkbox::new("remove-branch").small().label(label).checked(*delete_branch && merged).when(!merged, |c| c.disabled(true)).on_click(cx.listener(|this, on: &bool, _, cx| {
                        if let Some(Dialog::Remove { delete_branch, .. }) = this.map.dialog.as_mut() {
                            *delete_branch = *on;
                        }
                        cx.notify();
                    })));
                }
                let (title, ok) = if *force { (i18n::t!("app.map.remove_force_title"), i18n::t!("app.map.remove_force_ok")) } else { (i18n::t!("app.map.remove_title"), i18n::t!("app.map.remove_ok")) };
                (title, b.into_any_element(), ok, true, !busy, |this, cx| this.confirm_remove(cx))
            }
            Dialog::Prune(preview) => {
                let (b, ready) = match preview {
                    None => (div().flex().items_center().gap(px(8.)).child(Spinner::new().small().color(t.accent)).child(text(i18n::t!("app.detail.loading"))).into_any_element(), false),
                    Some(Err(e)) => (div().text_size(px(12.)).text_color(t.danger).child(e.clone()).into_any_element(), false),
                    Some(Ok(lines)) if lines.is_empty() => (text(i18n::t!("app.map.prune_none")).into_any_element(), false),
                    Some(Ok(lines)) => (div().flex().flex_col().gap(px(10.)).child(text(i18n::tn!("app.map.prune_body", lines.len()))).child(code_block(t, "prune-list", lines.join("\n"))).into_any_element(), true),
                };
                (i18n::t!("app.map.prune_title"), b, i18n::t!("app.map.prune_ok"), true, ready && !busy, |this, cx| {
                    this.map.dialog = None;
                    this.run_op(crate::work::Op::Git { args: vec!["worktree".into(), "prune".into(), "--verbose".into()], kind: crate::work::GitKind::Prune }, cx);
                })
            }
            Dialog::Lock { path, reason } => (
                i18n::t!("app.map.lock_title"),
                div().flex().flex_col().gap(px(10.)).child(text(i18n::t!("app.map.lock_body", path = path.display()))).child(Input::new(reason)).into_any_element(),
                i18n::t!("app.map.lock"),
                false,
                !busy,
                |this, cx| this.confirm_lock(cx),
            ),
            Dialog::New(f) => {
                let seg = |id: &'static str, label: String, mode: NewMode| {
                    let on = f.mode == mode;
                    div().id(id).flex().items_center().h(px(26.)).px(px(10.)).rounded(px(7.)).text_size(px(12.)).cursor_pointer().when(on, |d| d.bg(t.surface).font_weight(FontWeight::SEMIBOLD).shadow_sm()).when(!on, |d| d.text_color(t.ink_muted)).child(label).on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(Dialog::New(f)) = this.map.dialog.as_mut() {
                            f.mode = mode;
                        }
                        cx.notify();
                    }))
                };
                let modes = div().flex().items_center().gap(px(2.)).p(px(2.)).rounded(px(9.)).bg(t.border.opacity(0.6)).child(seg("new-mode-new", i18n::t!("app.map.new_mode_new"), NewMode::NewBranch)).child(seg("new-mode-branch", i18n::t!("app.map.new_mode_branch"), NewMode::Existing)).child(seg("new-mode-commit", i18n::t!("app.map.new_mode_commit"), NewMode::Commit));
                let label = |s: String| div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(s);
                let name_label = match f.mode {
                    NewMode::NewBranch => i18n::t!("app.map.new_name"),
                    NewMode::Existing => i18n::t!("app.map.new_branch"),
                    NewMode::Commit => i18n::t!("app.map.new_commit"),
                };
                let agents = self.map.agent_commands();
                let current = f.agent.clone();
                let weak = cx.entity().downgrade();
                let agent = Button::new("new-agent").outline().small().icon(IconName::Bot).label(current.clone().map(|a| i18n::t!("app.map.new_agent_is", agent = a)).unwrap_or_else(|| i18n::t!("app.map.new_agent_none"))).dropdown_menu(move |mut menu, _, _| {
                    let w = weak.clone();
                    menu = menu.item(PopupMenuItem::new(i18n::t!("app.map.new_agent_none")).checked(current.is_none()).on_click(move |_, _, cx| {
                        w.update(cx, |this, cx| {
                            if let Some(Dialog::New(f)) = this.map.dialog.as_mut() {
                                f.agent = None;
                            }
                            cx.notify();
                        })
                        .ok();
                    }));
                    for a in &agents {
                        let (w, a2) = (weak.clone(), a.clone());
                        menu = menu.item(PopupMenuItem::new(a.clone()).checked(current.as_deref() == Some(a.as_str())).on_click(move |_, _, cx| {
                            w.update(cx, |this, cx| {
                                if let Some(Dialog::New(f)) = this.map.dialog.as_mut() {
                                    f.agent = Some(a2.clone());
                                }
                                cx.notify();
                            })
                            .ok();
                        }));
                    }
                    menu
                });
                let mut b = div().flex().flex_col().gap(px(10.)).child(modes).child(label(name_label)).child(Input::new(&f.name));
                if f.mode == NewMode::NewBranch {
                    b = b.child(label(i18n::t!("app.map.new_start"))).child(Input::new(&f.start));
                }
                b = b.child(label(i18n::t!("app.map.new_path"))).child(Input::new(&f.path)).child(div().flex().items_center().gap(px(8.)).child(div().text_size(px(12.)).child(i18n::t!("app.map.new_after"))).child(agent));
                let ready = !f.name.read(cx).value().trim().is_empty() && !f.path.read(cx).value().trim().is_empty() && !busy;
                (i18n::t!("app.map.new_title"), b.into_any_element(), i18n::t!("app.map.new_ok"), false, ready, |this, cx| this.submit_new_worktree(cx))
            }
            Dialog::Settings(f) => {
                let label = |s: String| div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(s);
                let hint = |s: String| div().text_size(px(11.5)).text_color(t.ink_muted).child(s);
                let terms = self.map.terminals.clone();
                let cur = f.terminal.clone();
                let picked = crate::launch::pick_terminal(cur.as_deref(), &terms);
                let weak = cx.entity().downgrade();
                let term = Button::new("set-terminal").outline().small().icon(IconName::SquareTerminal).label(picked.map(|t| t.label()).unwrap_or_else(|| i18n::t!("app.map.no_terminal"))).dropdown_menu(move |mut menu, _, _| {
                    for tm in terms.iter().copied() {
                        let w = weak.clone();
                        menu = menu.item(PopupMenuItem::new(tm.label()).checked(Some(tm) == picked).on_click(move |_, _, cx| {
                            w.update(cx, |this, cx| {
                                if let Some(Dialog::Settings(f)) = this.map.dialog.as_mut() {
                                    f.terminal = Some(tm.id().to_string());
                                }
                                cx.notify();
                            })
                            .ok();
                        }));
                    }
                    menu
                });
                let b = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(label(i18n::t!("app.map.set_default")))
                    .child(Input::new(&f.default_branch))
                    .child(hint(i18n::t!("app.map.set_default_hint")))
                    .child(label(i18n::t!("app.map.set_protected")))
                    .child(Input::new(&f.protected))
                    .child(label(i18n::t!("app.map.set_base")))
                    .child(Input::new(&f.base_dir))
                    .child(label(i18n::t!("app.map.set_agents")))
                    .child(Input::new(&f.agents))
                    .child(hint(i18n::t!("app.map.set_agents_hint")))
                    .child(div().flex().items_center().gap(px(8.)).child(label(i18n::t!("app.map.set_terminal"))).child(term))
                    .child(Checkbox::new("set-badges").small().label(i18n::t!("app.map.set_badges")).checked(f.badges).on_click(cx.listener(|this, on: &bool, _, cx| {
                        if let Some(Dialog::Settings(f)) = this.map.dialog.as_mut() {
                            f.badges = *on;
                        }
                        cx.notify();
                    })))
                    .child(hint(i18n::t!("app.map.set_badges_hint")))
                    .child(Checkbox::new("set-motion").small().label(i18n::t!("app.map.set_reduce_motion")).checked(f.reduce_motion).on_click(cx.listener(|this, on: &bool, _, cx| {
                        if let Some(Dialog::Settings(f)) = this.map.dialog.as_mut() {
                            f.reduce_motion = *on;
                        }
                        cx.notify();
                    })))
                    .when(cfg!(target_os = "macos"), |d| {
                        d.child(Checkbox::new("set-notify").small().label(i18n::t!("app.map.set_notify")).checked(f.notify).on_click(cx.listener(|this, on: &bool, _, cx| {
                            if let Some(Dialog::Settings(f)) = this.map.dialog.as_mut() {
                                f.notify = *on;
                            }
                            cx.notify();
                        })))
                    })
                    .child(Checkbox::new("set-today").small().label(i18n::t!("app.map.set_today")).checked(f.today).on_click(cx.listener(|this, on: &bool, _, cx| {
                        if let Some(Dialog::Settings(f)) = this.map.dialog.as_mut() {
                            f.today = *on;
                        }
                        cx.notify();
                    })))
                    .child(hint(i18n::t!("app.map.set_today_hint")))
                    .child(label(i18n::t!("app.reclaim.set_days")))
                    .child(Input::new(&f.stale_days))
                    .child(hint(i18n::t!("app.reclaim.set_days_hint")))
                    .child(Checkbox::new("set-disk-today").small().label(i18n::t!("app.reclaim.set_today")).checked(f.disk_today).on_click(cx.listener(|this, on: &bool, _, cx| {
                        if let Some(Dialog::Settings(f)) = this.map.dialog.as_mut() {
                            f.disk_today = *on;
                        }
                        cx.notify();
                    })))
                    .into_any_element();
                (i18n::t!("app.map.settings"), b, i18n::t!("app.map.save"), false, true, |this, cx| this.save_map_settings(cx))
            }
        };
        let ok_btn = ui::filled_button(t, "map-dialog-ok", if danger { t.danger } else { t.accent }).child(ok_label).when(!ready, |d| d.opacity(0.4).cursor_default()).when(ready, |d| d.on_click(cx.listener(move |this, _, _, cx| ok(this, cx))));
        let card = div()
            .id("map-dialog")
            .flex()
            .flex_col()
            .gap(px(14.))
            .w(px(520.))
            .max_h(relative(0.9))
            .overflow_y_scroll()
            .p(px(22.))
            .rounded(px(16.))
            .border_1()
            .border_color(t.border)
            .bg(t.surface)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(body)
            .child(div().flex().justify_end().gap(px(8.)).child(ui::outline_button(t, "map-dialog-cancel").child(i18n::t!("app.confirm.cancel")).on_click(cx.listener(move |this, _, _, cx| close(this, cx)))).child(ok_btn));
        Some(backdrop(t, "map-backdrop").on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx))).child(card).into_any_element())
    }
}

/// What a worktree's right-click menu needs.
struct NodeMenu {
    weak: WeakEntity<RamaApp>,
    path: PathBuf,
    main: bool,
    locked: bool,
    missing: bool,
    agents: Vec<String>,
    busy: bool,
}

impl NodeMenu {
    fn item(&self, label: String, enabled: bool, f: impl Fn(&mut RamaApp, &Path, &mut Window, &mut Context<RamaApp>) + 'static) -> PopupMenuItem {
        let (w, p) = (self.weak.clone(), self.path.clone());
        PopupMenuItem::new(label).disabled(!enabled).on_click(move |_, window, cx| {
            w.update(cx, |this, cx| f(this, &p, window, cx)).ok();
        })
    }

    fn build(&self, mut menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let ok = !self.missing;
        menu = menu
            .item(self.item(i18n::t!("app.map.open_finder"), ok, |this, p, _, cx| this.reveal(p, cx)))
            .item(self.item(i18n::t!("app.map.open_rama"), ok, |this, p, _, cx| this.open_in_new_window(p.to_path_buf(), cx)))
            .item(self.item(i18n::t!("app.map.copy_path"), true, |this, p, _, cx| this.copy_path(p, cx)))
            .separator()
            .item(self.item(i18n::t!("app.map.open_terminal"), ok, |this, p, _, cx| this.launch(p.to_path_buf(), None, cx)));
        let (agents, w, p) = (self.agents.clone(), self.weak.clone(), self.path.clone());
        menu = menu.submenu(i18n::t!("app.map.start_agent"), window, cx, move |mut m, _, _| {
            if agents.is_empty() {
                m = m.item(PopupMenuItem::new(i18n::t!("app.map.no_agents")).disabled(true));
            }
            for a in &agents {
                let (w, p, a) = (w.clone(), p.clone(), a.clone());
                m = m.item(PopupMenuItem::new(a.clone()).on_click(move |_, _, cx| {
                    w.update(cx, |this, cx| this.launch(p.clone(), Some(a.clone()), cx)).ok();
                }));
            }
            m
        });
        if !self.main {
            menu = menu.separator();
            menu = if self.locked {
                menu.item(self.item(i18n::t!("app.map.unlock"), !self.busy, |this, p, _, cx| this.unlock(p, cx)))
            } else {
                menu.item(self.item(i18n::t!("app.map.lock"), !self.busy && ok, |this, p, window, cx| this.ask_lock(p.to_path_buf(), window, cx)))
            };
            menu = menu.item(self.item(i18n::t!("app.map.remove"), !self.busy && !self.locked, |this, p, _, cx| this.ask_remove_worktree(p, cx)));
        }
        menu
    }
}

#[cfg(test)]
mod tests {
    use super::{CARD_H, CARD_W, layout, short_path, toolbar_compact};
    use repo::{Worktree, WorktreeEntry, WorktreeInfo};
    use std::path::{Path, PathBuf};

    fn entry(path: &str, base: &str, ahead: u32) -> WorktreeEntry {
        let wt = Worktree { path: PathBuf::from(path), head: None, branch: None, bare: false, detached: true, locked: None, prunable: None, main: path == "/r" };
        WorktreeEntry { wt, info: WorktreeInfo { vs: vec![("main".into(), if base == "main" { ahead } else { 99 }, 0), ("dev".into(), if base == "dev" { ahead } else { 99 }, 0)], ..Default::default() } }
    }

    #[test]
    fn layouts() {
        let list: Vec<WorktreeEntry> = std::iter::once(entry("/r", "main", 0)).chain((0..30).map(|i| entry(&format!("/r-{i}"), if i % 3 == 0 { "dev" } else { "main" }, 1))).collect();
        let (pos, labels) = layout(&list, false, Some("main"));
        assert_eq!(pos[0], (0., 0.));
        assert!(labels.is_empty());
        // no two cards on top of each other
        for i in 1..pos.len() {
            for j in i + 1..pos.len() {
                let (a, b) = (pos[i], pos[j]);
                assert!((a.0 - b.0).abs() >= CARD_W * 0.9 || (a.1 - b.1).abs() >= CARD_H * 0.9, "{i} and {j} overlap: {a:?} {b:?}");
            }
        }
        let (pos, labels) = layout(&list, true, Some("main"));
        assert_eq!(labels.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["main", "dev"]);
        assert!(pos[2].0 < 0. && pos[2].0 == pos[3].0);
        assert!(pos[4].0 > 0., "a dev worktree sits in the dev column");
        assert_eq!(short_path(Path::new("/r/.worktrees/x"), Some(Path::new("/r"))), ".worktrees/x");
        assert!(toolbar_compact(1000.) && toolbar_compact(1280.) && !toolbar_compact(1920.));
    }
}
