//! The side panel's Conversation tab: an agent session's transcript, read-only, newest at
//! the bottom, following new messages while you're at the bottom, with find and "load
//! earlier". The log is read only while this tab is open and only on this machine.
use crate::agents::Agent;
use crate::app::RamaApp;
use crate::theme::{MONO, Tokens, tokens};
use crate::transcript::{self, Item, Tool};
use crate::ui::{self, icon};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{IconName, Sizable as _};
use gpui_component::input::{Input, InputEvent, InputState};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

pub struct Convo {
    pub worktree: PathBuf,
    pub file: PathBuf,
    pub kind: &'static str,
    pub title: Option<String>,
    items: Rc<Vec<Item>>,
    /// Indices into `items` that match the find field (all when it's empty).
    shown: Rc<Vec<usize>>,
    list: ListState,
    expanded: Rc<RefCell<HashSet<usize>>>,
    window: u64,
    from: u64,
    len: u64,
    loading: bool,
    failed: bool,
    search: Entity<InputState>,
    query: String,
    _sub: Subscription,
}

/// Which items to show for `query` (case-insensitive substring).
pub fn filter(items: &[Item], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    (0..items.len()).filter(|&i| q.is_empty() || items[i].haystack().contains(&q)).collect()
}

/// First index where two transcripts differ (a tool result fills in an earlier item).
fn first_change(old: &[Item], new: &[Item]) -> usize {
    old.iter().zip(new).position(|(a, b)| a != b).unwrap_or(old.len().min(new.len()))
}

impl RamaApp {
    pub fn open_convo(&mut self, worktree: PathBuf, agent: Agent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = agent.session_file.clone() else { return };
        self.convo_tab = true;
        if self.convo.as_ref().is_some_and(|c| c.file == file) {
            cx.notify();
            return;
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t!("app.convo.search")));
        let sub = cx.subscribe_in(&search, window, |this, input, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                let q = input.read(cx).value().to_string();
                if let Some(c) = this.convo.as_mut() {
                    c.query = q;
                    c.shown = Rc::new(filter(&c.items, &c.query));
                    c.list.reset(c.shown.len());
                    c.list.scroll_to_end();
                }
                cx.notify();
            }
        });
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        list.set_follow_mode(FollowMode::Tail);
        self.convo = Some(Convo { worktree, file, kind: agent.kind, title: agent.title.clone(), items: Rc::new(Vec::new()), shown: Rc::new(Vec::new()), list, expanded: Rc::new(RefCell::new(HashSet::new())), window: transcript::WINDOW, from: 0, len: 0, loading: false, failed: false, search, query: String::new(), _sub: sub });
        self.load_convo(false, cx);
        cx.notify();
    }

    pub fn close_convo(&mut self, cx: &mut Context<Self>) {
        self.convo = None;
        self.convo_tab = false;
        cx.notify();
    }

    /// Read the log in the background. `grow`: the file got longer; read from the same start
    /// so the earlier items stay put.
    fn load_convo(&mut self, grow: bool, cx: &mut Context<Self>) {
        let Some(c) = self.convo.as_mut() else { return };
        if c.loading {
            return;
        }
        c.loading = true;
        let file = c.file.clone();
        let window = if grow && c.from > 0 { u64::MAX } else { c.window };
        let from = c.from;
        let task = cx.background_executor().spawn(async move {
            let len = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            // same start as before, as long as that stays under the cap
            let w = if window == u64::MAX { (len - from.min(len)).max(1) } else { window };
            transcript::load(&file, w.min(transcript::MAX_WINDOW))
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                let Some(c) = this.convo.as_mut() else { return };
                c.loading = false;
                let Some(t) = res else {
                    c.failed = true;
                    cx.notify();
                    return;
                };
                c.failed = false;
                let old = c.items.clone();
                let same_start = grow && t.from == c.from;
                c.from = t.from;
                c.len = t.len;
                c.items = Rc::new(t.items);
                c.shown = Rc::new(filter(&c.items, &c.query));
                if same_start && c.query.trim().is_empty() {
                    let i = first_change(&old, &c.items);
                    c.list.splice(i..old.len(), c.items.len() - i);
                } else {
                    c.expanded.borrow_mut().clear();
                    c.list.reset(c.shown.len());
                    c.list.scroll_to_end();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// From the poll: pick up what the agent wrote since, while the tab is showing.
    pub(crate) fn refresh_convo(&mut self, cx: &mut Context<Self>) {
        if !self.convo_tab || !self.map_view {
            return;
        }
        let Some(c) = self.convo.as_ref() else { return };
        let len = std::fs::metadata(&c.file).map(|m| m.len()).unwrap_or(0);
        if len != c.len && !c.loading {
            self.load_convo(len > c.len, cx);
        }
    }

    fn load_earlier(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.convo.as_mut() {
            c.window = (c.window * 2).min(transcript::MAX_WINDOW);
        }
        self.load_convo(false, cx);
    }

    fn toggle_item(&mut self, ix: usize, item: usize, cx: &mut Context<Self>) {
        if let Some(c) = self.convo.as_mut() {
            {
                let mut e = c.expanded.borrow_mut();
                if !e.remove(&item) {
                    e.insert(item);
                }
            }
            c.list.splice(ix..ix + 1, 1);
            cx.notify();
        }
    }

    pub(crate) fn render_convo(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(c) = self.convo.as_ref() else { return div().into_any_element() };
        let path = c.worktree.clone();
        let header = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(8.))
            .p(px(12.))
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).truncate().child(c.title.clone().unwrap_or_else(|| c.kind.to_string())))
                    .child(ui::icon_button(t, "convo-close", IconName::Close, true).size(px(26.)).on_click(cx.listener(|this, _, _, cx| this.close_convo(cx)))),
            )
            .child(ui::outline_button(t, "convo-term").h(px(28.)).text_size(px(12.)).child(icon(IconName::SquareTerminal, 14., t.ink)).child(i18n::t!("app.convo.open_terminal")).on_click(cx.listener(move |this, _, _, cx| this.focus_agent(path.clone(), cx))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(30.))
                    .px(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.canvas)
                    .child(icon(IconName::Search, 13., t.ink_muted))
                    .child(div().flex_1().min_w_0().text_size(px(12.)).child(Input::new(&c.search).appearance(false).small()))
                    .when(!c.query.trim().is_empty(), |d| d.child(div().flex_none().text_size(px(11.)).text_color(t.ink_muted).child(i18n::tn!("app.convo.matches", c.shown.len())))),
            )
            .child(div().text_size(px(10.5)).text_color(t.ink_muted).child(i18n::t!("app.convo.read_only")));
        let top: AnyElement = if c.from > 0 && c.window < transcript::MAX_WINDOW {
            div().id("convo-earlier").flex_none().py(px(6.)).text_center().text_size(px(12.)).text_color(t.accent).cursor_pointer().child(i18n::t!("app.convo.load_earlier")).on_click(cx.listener(|this, _, _, cx| this.load_earlier(cx))).into_any_element()
        } else if c.from > 0 {
            div().flex_none().py(px(6.)).text_center().text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.convo.limit")).into_any_element()
        } else {
            div().flex_none().py(px(6.)).text_center().text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.convo.start")).into_any_element()
        };
        let body: AnyElement = if c.failed {
            div().p(px(16.)).text_size(px(12.)).text_color(t.danger).child(i18n::t!("app.convo.unreadable")).into_any_element()
        } else if c.items.is_empty() {
            div().p(px(16.)).text_size(px(12.)).text_color(t.ink_muted).child(if c.loading { i18n::t!("app.convo.loading") } else { i18n::t!("app.convo.empty") }).into_any_element()
        } else {
            let (items, shown, expanded, kind) = (c.items.clone(), c.shown.clone(), c.expanded.clone(), c.kind);
            let weak = cx.entity().downgrade();
            list(c.list.clone(), move |ix, _window, cx| {
                let t = tokens(cx);
                let Some(&i) = shown.get(ix) else { return div().into_any_element() };
                let open = expanded.borrow().contains(&i);
                render_item(&t, &items[i], kind, open, ix, i, weak.clone())
            })
            .flex_1()
            .into_any_element()
        };
        div().flex().flex_col().size_full().min_h_0().child(header).child(top).child(div().flex().flex_col().flex_1().min_h_0().child(body)).into_any_element()
    }
}

fn tool_label(tool: &Tool) -> String {
    match tool {
        Tool::Edited(f) => i18n::t!("app.tool.edited", file = f),
        Tool::Wrote(f) => i18n::t!("app.tool.wrote", file = f),
        Tool::Read(f) => i18n::t!("app.tool.read", file = f),
        Tool::Ran(c) => i18n::t!("app.tool.ran", command = c),
        Tool::Searched(p) => i18n::t!("app.tool.searched", pattern = p),
        Tool::Fetched(u) => i18n::t!("app.tool.fetched", url = u),
        Tool::Web(q) => i18n::t!("app.tool.web", query = q),
        Tool::Agent(d) => i18n::t!("app.tool.agent", task = d),
        Tool::Asked => i18n::t!("app.tool.asked"),
        Tool::Todo => i18n::t!("app.tool.todo"),
        Tool::Other(n) => i18n::t!("app.tool.other", tool = n),
    }
}

fn render_item(t: &Tokens, item: &Item, kind: &'static str, open: bool, ix: usize, i: usize, weak: WeakEntity<RamaApp>) -> AnyElement {
    let wrap = div().px(px(12.)).py(px(5.));
    match item {
        Item::User(text) => wrap
            .child(div().flex().flex_col().gap(px(3.)).p(px(8.)).rounded(px(8.)).bg(t.accent_soft).child(div().text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(t.accent).child(i18n::t!("app.convo.you"))).child(md(t, text)))
            .into_any_element(),
        Item::Assistant(text) => wrap.child(div().flex().flex_col().gap(px(3.)).child(div().text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(kind)).child(md(t, text))).into_any_element(),
        Item::Error(e) => wrap.child(div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(t.danger).child(icon(IconName::TriangleAlert, 13., t.danger)).child(crate::agents_view::error_label(*e))).into_any_element(),
        Item::Tool { tool, input, result, failed, .. } => {
            let color = if *failed { t.danger } else { t.ink_muted };
            let mut label = tool_label(tool);
            if *failed {
                label = i18n::t!("app.convo.tool_failed", tool = label);
            }
            let row = div()
                .id(("convo-tool", i))
                .flex()
                .items_center()
                .gap(px(5.))
                .text_size(px(11.5))
                .text_color(color)
                .cursor_pointer()
                .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12., color))
                .child(div().min_w_0().truncate().child(label))
                .on_click(move |_, _, cx| {
                    weak.update(cx, |this, cx| this.toggle_item(ix, i, cx)).ok();
                });
            let detail = open.then(|| {
                let mut text = input.clone();
                if let Some(r) = result {
                    text.push_str("\n\n");
                    text.push_str(r);
                }
                div().mt(px(4.)).p(px(8.)).rounded(px(6.)).bg(t.canvas).font_family(MONO).text_size(px(10.5)).text_color(t.ink).child(text)
            });
            wrap.py(px(2.)).child(row).children(detail).into_any_element()
        }
    }
}

/// Just enough markdown for chat text: fenced code in monospace, headings bold, the rest as
/// written.
fn md(t: &Tokens, text: &str) -> Div {
    let mut out = div().flex().flex_col().gap(px(2.)).text_size(px(12.5)).line_height(px(18.));
    let mut code: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match code.take() {
                Some(lines) => out = out.child(div().p(px(6.)).rounded(px(5.)).bg(t.canvas).font_family(MONO).text_size(px(11.)).line_height(px(15.)).child(lines.join("\n"))),
                None => code = Some(Vec::new()),
            }
            continue;
        }
        if let Some(c) = code.as_mut() {
            c.push(line);
            continue;
        }
        let trimmed = line.trim_start();
        let heading = trimmed.starts_with('#');
        let plain = trimmed.trim_start_matches('#').trim_start();
        let shown = if heading { plain } else { line };
        let shown = shown.replace("**", "");
        out = out.child(if shown.trim().is_empty() { div().h(px(4.)) } else { div().when(heading, |d| d.font_weight(FontWeight::SEMIBOLD)).child(shown) });
    }
    if let Some(lines) = code {
        out = out.child(div().p(px(6.)).rounded(px(5.)).bg(t.canvas).font_family(MONO).text_size(px(11.)).child(lines.join("\n")));
    }
    out
}

#[cfg(test)]
mod tests {
    // not `super::*`: gpui's `test` macro would shadow the built-in one
    use super::{filter, first_change};
    use crate::transcript::Item;

    #[test]
    fn find_and_growth() {
        let items = vec![Item::User("Fix the Parser".into()), Item::Assistant("done".into()), Item::User("thanks".into())];
        assert_eq!(filter(&items, ""), [0, 1, 2]);
        assert_eq!(filter(&items, "parser"), [0]);
        assert_eq!(filter(&items, " THANKS "), [2]);
        let mut grown = items.clone();
        grown.push(Item::Assistant("more".into()));
        assert_eq!(first_change(&items, &grown), 3);
        grown[1] = Item::Assistant("done!".into());
        assert_eq!(first_change(&items, &grown), 1);
    }
}
