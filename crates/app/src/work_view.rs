//! Working copy panel (staged / unstaged files, commit box, hook failures), stash panel, the
//! "Uncommitted changes" graph row, the Git output panel, remote buttons and the modals.
use crate::app::RamaApp;
use crate::detail::{DETAIL_W, file_row, work_color, work_letter};
use crate::theme::{MONO, Tokens, tokens};
use crate::ui::{self, icon};
use crate::work::{Confirm, Op, PromptKind};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, Textarea};
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;
use gpui_component::{IconName, Sizable as _};
use repo::{FileState, WorkFile};

#[cfg(target_os = "macos")]
pub const COMMIT_KEYS: &str = "⌘↩"; // i18n-ok: shortcut
#[cfg(not(target_os = "macos"))]
pub const COMMIT_KEYS: &str = "Ctrl+Enter"; // i18n-ok: shortcut
/// Files listed per section; beyond that a "+N more" line.
const LIST_MAX: usize = 1000;
const OUTPUT_H: f32 = 210.;
const FAILURE_LINES: usize = 14;

fn small_button(t: &Tokens, id: impl Into<ElementId>, name: IconName, tip: String, enabled: bool) -> Stateful<Div> {
    ui::icon_button(t, id, name, enabled).size(px(24.)).rounded(px(6.)).tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
}

fn split_path(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((d, n)) => (format!("{d}/"), n.to_string()),
        None => (String::new(), path.to_string()),
    }
}

impl RamaApp {
    fn busy(&self) -> bool {
        self.work.job.is_some() || self.work.git_missing
    }

    /// The pinned row above the commit list while the working copy has changes.
    pub(crate) fn render_wip_row(&self, t: &Tokens, x: f32, color: Hsla, graph_w: f32, refs_w: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.work.status.as_ref().filter(|s| !s.is_clean() || self.work.wip)?;
        let selected = self.work.wip;
        let hover = if t.dark { t.border } else { t.canvas };
        let row_h = crate::graph_view::ROW_H;
        let counts = i18n::t!("app.wip.counts", staged = s.staged.len(), unstaged = s.unstaged.len());
        let graph = div()
            .relative()
            .flex_none()
            .w(px(graph_w))
            .h(px(row_h))
            .child(div().absolute().left(px(x - 1.)).top(px(row_h / 2.)).w(px(2.)).h(px(row_h / 2.)).bg(color.opacity(0.45)))
            .child(div().absolute().left(px(x - 8.)).top(px(row_h / 2. - 8.)).size(px(16.)).rounded_full().border_2().border_color(color).bg(t.surface));
        Some(
            div()
                .id("wip-row")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(px(row_h))
                .px(px(8.))
                .border_b_1()
                .border_color(t.border)
                .cursor_pointer()
                .when(selected, |d| d.bg(t.accent_soft))
                .when(!selected, |d| d.hover(move |s| s.bg(hover)))
                .child(div().flex_none().w(px(refs_w)))
                .child(graph)
                .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.wip.title")))
                .child(div().flex_none().text_size(px(12.)).text_color(t.ink_muted).child(counts))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.list_focus.focus(window, cx);
                    this.select_wip(cx);
                }))
                .into_any_element(),
        )
    }

    pub(crate) fn render_work(&mut self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = div().flex().flex_col().flex_none().w(px(DETAIL_W)).h_full().bg(t.surface).border_l_1().border_color(t.border);
        let status = self.work.status.clone();
        let branch = status.as_ref().map(|s| {
            let b = &s.branch;
            let mut text = b.head.clone().unwrap_or_else(|| i18n::t!("app.work.detached"));
            if b.ahead > 0 {
                text.push_str(&format!("  ↑{}", b.ahead)); // i18n-ok: arrows
            }
            if b.behind > 0 {
                text.push_str(&format!("  ↓{}", b.behind)); // i18n-ok: arrows
            }
            text
        });
        let busy = self.busy();
        let header = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .p(px(16.))
            .border_b_1()
            .border_color(t.border)
            .child(div().flex().flex_col().flex_1().min_w_0().child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.wip.title"))).children(branch.map(|b| div().text_size(px(12.)).text_color(t.ink_muted).truncate().child(b))))
            .child(
                ui::outline_button(t, "stash-open")
                    .h(px(28.))
                    .px(px(9.))
                    .text_size(px(12.))
                    .child(icon(IconName::Inbox, 13., t.ink))
                    .child(i18n::t!("app.stash.button"))
                    .when(!busy && self.work.dirty(), |d| d.on_click(cx.listener(|this, _, window, cx| this.open_prompt(PromptKind::Stash, window, cx))))
                    .when(busy || !self.work.dirty(), |d| d.opacity(0.5)),
            );

        let missing = self.work.git_missing.then(|| {
            div().m(px(12.)).p(px(12.)).rounded(px(10.)).bg(t.danger.opacity(0.1)).border_1().border_color(t.danger.opacity(0.4)).flex().flex_col().gap(px(4.)).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(t.danger).child(i18n::t!("app.git.missing_title"))).child(div().text_size(px(12.)).line_height(px(17.)).child(i18n::t!("app.git.missing_body")))
        });

        let panel = panel.child(header).children(missing).child(self.render_commit_box(t, cx));
        let Some(s) = status else {
            return panel.child(div().p(px(16.)).text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.detail.loading")));
        };
        let open = self.diff.as_ref().and_then(|d| d.work.clone());
        let mut lists = div().id("work-files").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().pb(px(12.));
        if s.is_clean() {
            lists = lists.child(div().p(px(16.)).text_size(px(13.)).text_color(t.ink_muted).child(i18n::t!("app.work.clean")));
        }
        if !s.staged.is_empty() {
            let unstage_all = small_button(t, "unstage-all", IconName::Minus, i18n::t!("app.work.unstage_all"), !busy).when(!busy, |d| d.on_click(cx.listener(|this, _, _, cx| this.run_op(Op::UnstageAll, cx))));
            lists = lists.child(section(t, i18n::tn!("app.work.staged", s.staged.len()), vec![unstage_all.into_any_element()]));
            for (i, f) in s.staged.iter().take(LIST_MAX).enumerate() {
                let on = open.as_ref().is_some_and(|(o, st)| *st && o.path == f.path);
                lists = lists.child(self.work_row(t, i, f, true, on, busy, cx));
            }
            lists = lists.when(s.staged.len() > LIST_MAX, |d| d.child(more(t, s.staged.len() - LIST_MAX)));
        }
        if !s.unstaged.is_empty() {
            let all = s.unstaged.clone();
            let discard_all = small_button(t, "discard-all", IconName::Undo, i18n::t!("app.work.discard_all"), !busy).when(!busy, |d| {
                d.on_click(cx.listener(move |this, _, _, cx| {
                    this.work.confirm = Some(Confirm::Discard(all.clone()));
                    cx.notify();
                }))
            });
            let stage_all = small_button(t, "stage-all", IconName::Plus, i18n::t!("app.work.stage_all"), !busy).when(!busy, |d| d.on_click(cx.listener(|this, _, _, cx| this.run_op(Op::StageAll, cx))));
            lists = lists.child(section(t, i18n::tn!("app.work.unstaged", s.unstaged.len()), vec![discard_all.into_any_element(), stage_all.into_any_element()]));
            for (i, f) in s.unstaged.iter().take(LIST_MAX).enumerate() {
                let on = open.as_ref().is_some_and(|(o, st)| !*st && o.path == f.path);
                lists = lists.child(self.work_row(t, i, f, false, on, busy, cx));
            }
            lists = lists.when(s.unstaged.len() > LIST_MAX, |d| d.child(more(t, s.unstaged.len() - LIST_MAX)));
        }
        panel.child(lists)
    }

    #[allow(clippy::too_many_arguments)]
    fn work_row(&self, t: &Tokens, i: usize, f: &WorkFile, staged: bool, open: bool, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let (dir, name) = split_path(&f.path);
        let hover = if t.dark { t.border } else { t.canvas };
        let sc = work_color(t, &f.state);
        // element ids, distinct per list
        let (row_id, btn_a, btn_b) = if staged { ("work-s", "unstage-s", "") } else { ("work-u", "discard-u", "stage-u") };
        let mut actions = div().flex().flex_none().items_center().gap(px(2.));
        if staged {
            let file = f.clone();
            actions = actions.child(small_button(t, (btn_a, i), IconName::Minus, i18n::t!("app.work.unstage"), !busy).when(!busy, |d| {
                d.on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.run_op(Op::Unstage(vec![file.clone()]), cx);
                }))
            }));
        } else {
            let (f1, f2) = (f.clone(), f.clone());
            actions = actions
                .child(small_button(t, (btn_a, i), IconName::Undo, i18n::t!("app.work.discard"), !busy).when(!busy, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.work.confirm = Some(Confirm::Discard(vec![f1.clone()]));
                        cx.notify();
                    }))
                }))
                .child(small_button(t, (btn_b, i), IconName::Plus, i18n::t!("app.work.stage"), !busy).when(!busy, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.run_op(Op::Stage(vec![f2.path.clone()]), cx);
                    }))
                }));
        }
        let file = f.clone();
        let from = match &f.state {
            FileState::Renamed { from } | FileState::Copied { from } => Some(from.clone()),
            _ => None,
        };
        div()
            .id((row_id, i))
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(28.))
            .pl(px(16.))
            .pr(px(10.))
            .cursor_pointer()
            .when(open, |d| d.bg(t.accent_soft))
            .when(!open, |d| d.hover(move |s| s.bg(hover)))
            .child(div().flex().flex_none().items_center().justify_center().size(px(16.)).rounded(px(4.)).bg(sc.opacity(0.16)).text_color(sc).text_size(px(10.)).font_weight(FontWeight::BOLD).child(work_letter(&f.state)))
            .child(div().flex().flex_1().min_w_0().text_size(px(12.5)).child(div().flex_none().text_color(t.ink).child(name)).child(div().min_w_0().truncate().pl(px(6.)).text_color(t.ink_muted).child(from.map(|f| format!("← {f}")).unwrap_or(dir)))) // i18n-ok: arrow
            .child(actions)
            .on_click(cx.listener(move |this, _, _, cx| this.open_work_diff(file.clone(), staged, cx)))
            .into_any_element()
    }

    fn render_commit_box(&mut self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy();
        let amend = self.work.amend;
        let running_commit = self.work.job.as_ref().filter(|j| j.commit).map(|j| j.started.elapsed().as_secs_f32());
        let label = if amend { i18n::t!("app.work.commit_amend") } else { i18n::t!("app.work.commit") };
        let commit = ui::filled_button(t, "commit", t.accent)
            .h(px(30.))
            .child(icon(IconName::Check, 14., t.on_fill()))
            .child(label)
            .child(div().text_size(px(11.)).opacity(0.75).child(COMMIT_KEYS))
            .when(busy, |d| d.opacity(0.5).cursor_default())
            .when(!busy, |d| d.on_click(cx.listener(|this, _, _, cx| this.commit(false, cx))));
        let running = running_commit.map(|secs| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(12.))
                .text_color(t.ink_muted)
                .child(Spinner::new().small().color(t.accent))
                .child(div().flex_1().min_w_0().truncate().child(i18n::t!("app.work.running_hooks")))
                .child(div().flex_none().child(format!("{secs:.0} s")))
                .child(ui::outline_button(t, "cancel-commit").h(px(26.)).px(px(9.)).text_size(px(12.)).child(i18n::t!("app.git.cancel")).on_click(cx.listener(|this, _, _, cx| this.cancel_job(cx))))
        });
        let failure = self.work.failure.as_ref().map(|f| {
            let title = match &f.hook {
                Some(h) => i18n::t!("app.work.hook_rejected", hook = h),
                None => i18n::t!("app.work.commit_failed"),
            };
            let lines: Vec<&str> = f.output.lines().collect();
            let tail = lines[lines.len().saturating_sub(FAILURE_LINES)..].join("\n");
            let hooked = f.hook.is_some();
            let accent = t.accent;
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .p(px(10.))
                .rounded(px(8.))
                .bg(t.danger.opacity(0.08))
                .border_1()
                .border_color(t.danger.opacity(0.45))
                .child(div().flex().items_center().gap(px(6.)).child(icon(IconName::TriangleAlert, 14., t.danger)).child(div().flex_1().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(t.danger).child(title)))
                .child(div().text_size(px(11.5)).text_color(t.ink_muted).child(i18n::t!("app.work.message_kept")))
                .when(!tail.trim().is_empty(), |d| d.child(div().id("failure-output").max_h(px(180.)).overflow_y_scroll().p(px(8.)).rounded(px(6.)).bg(t.canvas).font_family(MONO).text_size(px(11.)).line_height(px(15.)).child(tail)))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(12.))
                        .text_size(px(12.))
                        .child(div().id("show-output").text_color(accent).cursor_pointer().hover(|s| s.underline()).child(i18n::t!("app.git.show_output")).on_click(cx.listener(|this, _, _, cx| {
                            this.work.output_open = true;
                            cx.notify();
                        })))
                        .when(hooked, |d| {
                            d.child(div().id("no-verify").text_color(t.ink_muted).cursor_pointer().hover(|s| s.underline()).child(i18n::t!("app.work.no_verify")).on_click(cx.listener(|this, _, _, cx| {
                                this.work.confirm = Some(Confirm::NoVerify);
                                cx.notify();
                            })))
                        }),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .border_b_1()
            .border_color(t.border)
            .child(div().rounded(px(8.)).border_1().border_color(t.border).bg(t.canvas).px(px(8.)).py(px(6.)).text_size(px(13.)).child(Textarea::new(&self.work.message)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().text_size(px(12.)).child(Checkbox::new("amend").small().label(i18n::t!("app.work.amend")).checked(amend).on_click(cx.listener(|this, on: &bool, window, cx| this.toggle_amend(*on, window, cx)))))
                    .child(commit),
            )
            .children(running)
            .children(failure)
    }

    pub(crate) fn render_stash(&mut self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = div().flex().flex_col().flex_none().w(px(DETAIL_W)).h_full().bg(t.surface).border_l_1().border_color(t.border);
        let Some(sv) = &self.work.stash else { return panel };
        let (index, name, message, short) = (sv.index, sv.stash.name(), sv.stash.message.clone(), sv.stash.id.to_hex_with_len(12).to_string());
        let busy = self.busy();
        let btn = |id: &'static str, label: String| ui::outline_button(t, id).h(px(28.)).px(px(10.)).text_size(px(12.)).child(label).when(busy, |d| d.opacity(0.5));
        let header = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(16.))
            .border_b_1()
            .border_color(t.border)
            .child(div().flex().items_center().gap(px(8.)).child(icon(IconName::Inbox, 15., t.ink_muted)).child(div().font_family(MONO).text_size(px(12.)).text_color(t.ink_muted).child(name)))
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).line_height(px(21.)).child(message))
            .child(div().font_family(MONO).text_size(px(11.)).text_color(t.ink_muted).child(short))
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .child(btn("stash-apply", i18n::t!("app.stash.apply")).when(!busy, |d| d.on_click(cx.listener(move |this, _, _, cx| this.run_op(Op::StashApply(index), cx)))))
                    .child(btn("stash-pop", i18n::t!("app.stash.pop")).when(!busy, |d| d.on_click(cx.listener(move |this, _, _, cx| this.run_op(Op::StashPop(index), cx)))))
                    .child(btn("stash-drop", i18n::t!("app.stash.drop")).text_color(t.danger).when(!busy, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            this.work.confirm = Some(Confirm::DropStash(index));
                            cx.notify();
                        }))
                    })),
            );
        let open_path = self.diff.as_ref().filter(|d| d.work.is_none()).map(|d| d.file.path.clone());
        let files: AnyElement = match &sv.changes {
            None => div().p(px(16.)).text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.detail.loading")).into_any_element(),
            Some(Err(e)) => div().p(px(16.)).text_size(px(12.)).text_color(t.danger).child(e.clone()).into_any_element(),
            Some(Ok(list)) => {
                let list = list.clone();
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(div().px(px(16.)).pt(px(14.)).pb(px(6.)).child(ui::section_title(t, i18n::tn!("app.detail.files", list.len()))))
                    .child(div().id("stash-files").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().pb(px(10.)).children(list.iter().enumerate().map(|(i, f)| file_row(t, i, f, open_path.as_deref() == Some(f.path.as_str()), cx))))
                    .into_any_element()
            }
        };
        panel.child(header).child(files)
    }

    pub(crate) fn render_output_toggle(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = t.ink;
        div()
            .id("output-toggle")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(5.))
            .cursor_pointer()
            .hover(move |s| s.text_color(ink))
            .when(self.work.job.is_some(), |d| d.child(Spinner::new().xsmall().color(t.accent)))
            .when(self.work.job.is_none(), |d| d.child(icon(IconName::SquareTerminal, 12., t.ink_muted)))
            .child(i18n::t!("app.git.output"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.work.output_open = !this.work.output_open;
                cx.notify();
            }))
    }

    /// Collapsible panel under the graph with the running command and git's (and hooks') output.
    pub(crate) fn render_output(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let open = self.work.output_open;
        let job = self.work.job.as_ref();
        if !open && job.is_none() {
            return div().into_any_element();
        }
        let bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(32.))
            .px(px(10.))
            .border_t_1()
            .border_color(t.border)
            .bg(t.surface)
            .text_size(px(12.))
            .child(icon(IconName::SquareTerminal, 14., t.ink_muted))
            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.git.output")))
            .children(job.map(|j| {
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(8.))
                    .child(Spinner::new().small().color(t.accent))
                    .child(div().min_w_0().truncate().font_family(MONO).text_size(px(11.5)).text_color(t.ink_muted).child(j.command.clone()))
                    .child(div().flex_none().text_color(t.ink_muted).child(format!("{:.0} s", j.started.elapsed().as_secs_f32())))
                    .child(ui::outline_button(t, "cancel-job").h(px(24.)).px(px(8.)).text_size(px(12.)).child(i18n::t!("app.git.cancel")).on_click(cx.listener(|this, _, _, cx| this.cancel_job(cx))))
            }))
            .when(job.is_none(), |d| d.child(div().flex_1()))
            .when(open, |d| {
                d.child(small_button(t, "clear-output", IconName::Delete, i18n::t!("app.git.clear"), true).on_click(cx.listener(|this, _, _, cx| {
                    this.work.log.lock().unwrap().clear();
                    cx.notify();
                })))
            })
            .child(small_button(t, "toggle-output", if open { IconName::ChevronDown } else { IconName::ChevronUp }, i18n::t!("app.git.output"), true).on_click(cx.listener(|this, _, _, cx| {
                this.work.output_open = !this.work.output_open;
                cx.notify();
            })));
        if !open {
            return bar.into_any_element();
        }
        let lines = std::rc::Rc::new(self.work.log.lock().unwrap().clone());
        let n = lines.len();
        if job.is_some() && n > 0 {
            self.work.output_scroll.scroll_to_item(n - 1, ScrollStrategy::Bottom);
        }
        let body = if n == 0 {
            div().flex_1().p(px(10.)).text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.git.output_empty")).into_any_element()
        } else {
            uniform_list(
                "git-output",
                n,
                cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                    let t = tokens(cx);
                    range
                        .map(|i| {
                            let l = &lines[i];
                            let cmd = l.starts_with("$ ");
                            div().id(("out", i)).h(px(17.)).px(px(10.)).whitespace_nowrap().overflow_hidden().font_family(MONO).text_size(px(11.5)).text_color(if cmd { t.accent } else { t.ink }).child(l.replace('\t', "    ")).into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.work.output_scroll)
            .flex_1()
            .into_any_element()
        };
        div().flex().flex_col().flex_none().h(px(OUTPUT_H)).bg(t.canvas).child(bar).child(body).into_any_element()
    }

    pub(crate) fn render_remote_buttons(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = !self.busy() && self.repo.is_some();
        let (ahead, behind) = self.work.status.as_ref().map(|s| (s.branch.ahead, s.branch.behind)).unwrap_or((0, 0));
        let button = |id: &'static str, ic: IconName, tip: String, count: u32, op: Op| {
            let b = ui::icon_button(t, id, ic, enabled).w_auto().min_w(px(34.)).px(px(8.)).gap(px(3.)).tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)).when(count > 0, |d| d.child(div().text_size(px(11.)).text_color(t.ink_muted).child(count.to_string())));
            b.when(enabled, |d| d.on_click(cx.listener(move |this, _, _, cx| this.run_op(op.clone(), cx))))
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .child(button("fetch", IconName::Network, i18n::t!("app.git.fetch"), 0, Op::Fetch))
            .child(button("pull", IconName::ArrowDown, i18n::t!("app.git.pull"), behind, Op::Pull))
            .child(button("push", IconName::ArrowUp, i18n::t!("app.git.push"), ahead, Op::Push))
    }

    /// Confirmation or input modal, over everything.
    pub(crate) fn render_modal(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, body, ok_label, danger, input): (String, Option<String>, String, bool, Option<AnyElement>) = if let Some(c) = &self.work.confirm {
            match c {
                Confirm::Discard(files) => {
                    let body = match files.as_slice() {
                        [one] => i18n::t!("app.confirm.discard_one", path = one.path),
                        many => i18n::t!("app.confirm.discard_many", n = many.len()),
                    };
                    let untracked = files.iter().any(|f| f.state == FileState::Untracked);
                    let body = if untracked { format!("{body}\n\n{}", i18n::t!("app.confirm.discard_trash")) } else { body };
                    (i18n::t!("app.confirm.discard_title"), Some(body), i18n::t!("app.confirm.discard_ok"), true, None)
                }
                Confirm::DropStash(i) => (i18n::t!("app.confirm.drop_title", name = format!("stash@{{{i}}}")), Some(i18n::t!("app.confirm.drop_body")), i18n::t!("app.stash.drop"), true, None),
                Confirm::DeleteBranch(b) => (i18n::t!("app.confirm.delete_title", name = b), Some(i18n::t!("app.confirm.delete_body")), i18n::t!("app.confirm.delete_ok"), true, None),
                Confirm::NoVerify => (i18n::t!("app.confirm.no_verify_title"), Some(i18n::t!("app.confirm.no_verify_body")), i18n::t!("app.confirm.no_verify_ok"), true, None),
            }
        } else if let Some(p) = &self.work.prompt {
            let (title, ok, option) = match p.kind {
                PromptKind::NewBranch(at) => (
                    i18n::t!("app.prompt.branch_title", sha = at.to_hex_with_len(7)),
                    i18n::t!("app.prompt.branch_ok"),
                    Checkbox::new("prompt-checkout").small().label(i18n::t!("app.prompt.branch_checkout")).checked(p.checkout).on_click(cx.listener(|this, on: &bool, _, cx| {
                        if let Some(p) = this.work.prompt.as_mut() {
                            p.checkout = *on;
                        }
                        cx.notify();
                    })),
                ),
                PromptKind::Stash => (
                    i18n::t!("app.prompt.stash_title"),
                    i18n::t!("app.stash.button"),
                    Checkbox::new("prompt-untracked").small().label(i18n::t!("app.prompt.stash_untracked")).checked(p.untracked).on_click(cx.listener(|this, on: &bool, _, cx| {
                        if let Some(p) = this.work.prompt.as_mut() {
                            p.untracked = *on;
                        }
                        cx.notify();
                    })),
                ),
            };
            let input = div().flex().flex_col().gap(px(10.)).child(Input::new(&p.input)).child(div().text_size(px(12.)).child(option)).into_any_element();
            (title, None, ok, false, Some(input))
        } else {
            return None;
        };
        let is_prompt = input.is_some();
        let ok = ui::filled_button(t, "modal-ok", if danger { t.danger } else { t.accent }).child(ok_label).on_click(cx.listener(move |this, _, window, cx| {
            if is_prompt {
                this.submit_prompt(window, cx);
            } else {
                this.confirm(cx);
            }
        }));
        let close = |this: &mut RamaApp, cx: &mut Context<RamaApp>| {
            this.work.confirm = None;
            this.work.prompt = None;
            this.prompt_sub = None;
            cx.notify();
        };
        let card = div()
            .id("modal-card")
            .flex()
            .flex_col()
            .gap(px(14.))
            .w(px(440.))
            .p(px(22.))
            .rounded(px(16.))
            .border_1()
            .border_color(t.border)
            .bg(t.surface)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(title))
            .children(body.map(|b| div().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).child(b)))
            .children(input)
            .child(div().flex().justify_end().gap(px(8.)).child(ui::outline_button(t, "modal-cancel").child(i18n::t!("app.confirm.cancel")).on_click(cx.listener(move |this, _, _, cx| close(this, cx)))).child(ok));
        Some(
            div()
                .id("modal-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::black().opacity(if t.dark { 0.55 } else { 0.32 }))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx)))
                .child(card)
                .into_any_element(),
        )
    }
}

fn section(t: &Tokens, title: String, actions: Vec<AnyElement>) -> Div {
    div().flex().items_center().gap(px(4.)).pl(px(16.)).pr(px(10.)).pt(px(12.)).pb(px(4.)).child(div().flex_1().child(ui::section_title(t, title))).children(actions)
}

fn more(t: &Tokens, n: usize) -> Div {
    div().px(px(16.)).py(px(6.)).text_size(px(12.)).text_color(t.ink_muted).child(i18n::tn!("app.work.more", n))
}
