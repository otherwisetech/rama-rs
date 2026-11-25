//! "Clean up branches…": a preview of local branches that look merged (into the default
//! branch, upstream gone, or squash-merged), what deleting them takes (`-d`, or `-D` behind a
//! second confirmation), optional remote deletion and worktree removal, an undo log for the
//! session, and an honest note on disk space with `git gc`.
use crate::app::RamaApp;
use crate::theme::{MONO, Tokens};
use crate::ui::{self, icon};
use crate::util::{ago, bytes, now};
use crate::work::{GitKind, Op, push_log};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::spinner::Spinner;
use gpui_component::{IconName, Sizable as _};
use repo::{Cancel, Candidate, Oid, Reason, Repo, Run};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct PlanItem {
    pub name: String,
    pub oid: Oid,
    pub force: bool,
    /// Remove this worktree first (plain `git worktree remove`: refused if it has changes).
    pub worktree: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub deleted: Vec<(String, Oid)>,
    pub failed: Vec<(String, String)>,
    pub worktrees: usize,
    pub remote_deleted: usize,
    pub remote_failed: Vec<String>,
}

#[derive(Debug)]
pub struct Plan {
    pub items: Vec<PlanItem>,
    /// `git push <remote> --delete <branches…>` per remote.
    pub remote: Vec<Vec<String>>,
    pub report: Mutex<Report>,
}

/// Run a cleanup plan in order, collecting what worked; one failure doesn't stop the rest.
pub fn execute(repo: &Repo, plan: &Plan, cancel: &Cancel, log: &Arc<Mutex<Vec<String>>>, sink: &mut dyn FnMut(&str, bool)) -> Result<Run, repo::Error> {
    let mut report = Report::default();
    for it in &plan.items {
        if cancel.is_cancelled() {
            break;
        }
        if let Some(wt) = &it.worktree {
            let args = Repo::worktree_remove_args(wt, false);
            push_log(log, &format!("$ git {}", args.join(" ")), false); // i18n-ok: command line
            match repo.run_args(&args, cancel, sink) {
                Ok(r) if r.success => report.worktrees += 1,
                Ok(r) => {
                    report.failed.push((it.name.clone(), r.output));
                    continue;
                }
                Err(e) => {
                    report.failed.push((it.name.clone(), e.to_string()));
                    continue;
                }
            }
        }
        let flag = if it.force { "-D" } else { "-d" };
        push_log(log, &format!("$ git branch {flag} {}", it.name), false); // i18n-ok: command line
        let res = if it.force { repo.delete_branch_force(&it.name) } else { repo.delete_branch(&it.name) };
        match res {
            Ok(_) => report.deleted.push((it.name.clone(), it.oid)),
            Err(e) => {
                push_log(log, &e.to_string(), false);
                report.failed.push((it.name.clone(), e.to_string()));
            }
        }
    }
    for args in &plan.remote {
        if cancel.is_cancelled() {
            break;
        }
        push_log(log, &format!("$ git {}", args.join(" ")), false); // i18n-ok: command line
        match repo.run_args(args, cancel, sink) {
            Ok(r) if r.success => report.remote_deleted += args.len() - 3,
            _ => report.remote_failed.extend(args[3..].iter().cloned()),
        }
    }
    *plan.report.lock().unwrap() = report;
    Ok(Run { success: !cancel.is_cancelled(), cancelled: cancel.is_cancelled(), ..Default::default() })
}

pub struct Row {
    pub c: Candidate,
    pub checked: bool,
    pub remove_wt: bool,
    /// Changed files in its worktree (it can't be removed without force then).
    pub wt_dirty: Option<usize>,
}

impl Row {
    /// Selected and actually deletable (a branch in a worktree only with "also remove").
    fn active(&self) -> bool {
        self.checked && (self.c.worktree.is_none() || (self.remove_wt && self.wt_dirty == Some(0)))
    }
}

#[derive(PartialEq, Eq)]
pub enum Step {
    List,
    /// Second confirmation for the branches that need `git branch -D`.
    ConfirmForce,
    /// Type the number of remote branches to delete them.
    ConfirmRemote,
    Running,
    Done,
    /// `git gc --prune=now` asks first.
    ConfirmGc,
}

pub struct CleanupState {
    pub rows: Vec<Row>,
    /// Squash check in progress: (done, total).
    pub loading: Option<Arc<(AtomicUsize, AtomicUsize)>>,
    pub quick_loading: bool,
    pub error: Option<String>,
    pub default: Option<String>,
    pub delete_remote: bool,
    pub step: Step,
    pub plan: Option<Arc<Plan>>,
    pub remote_input: Entity<InputState>,
    pub git_before: Option<u64>,
    pub git_now: Option<u64>,
    pub gc_prune_now: bool,
    pub gc_running: bool,
    generation: u64,
    cancel: Cancel,
    _sub: Subscription,
}

impl RamaApp {
    pub fn open_cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx));
        let sub = cx.subscribe_in(&input, window, |_, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        self.cleanup = Some(CleanupState {
            rows: Vec::new(),
            loading: None,
            quick_loading: true,
            error: None,
            default: None,
            delete_remote: false,
            step: Step::List,
            plan: None,
            remote_input: input,
            git_before: None,
            git_now: None,
            gc_prune_now: false,
            gc_running: false,
            generation: 0,
            cancel: Cancel::default(),
            _sub: sub,
        });
        self.load_cleanup(cx);
        self.measure_git_dir(cx);
    }

    fn measure_git_dir(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let task = cx.background_executor().spawn(async move { repo.git_dir_usage().total });
        cx.spawn(async move |this, cx| {
            let n = task.await;
            this.update(cx, |this, cx| {
                if let Some(c) = this.cleanup.as_mut() {
                    c.git_before.get_or_insert(n);
                    c.git_now = Some(n);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Quick pass first (merged and gone, ~1 s), then the squash-merge check with progress.
    pub fn load_cleanup(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let Some(c) = self.cleanup.as_mut() else { return };
        c.cancel.cancel();
        c.cancel = Cancel::default();
        c.generation += 1;
        c.quick_loading = true;
        c.error = None;
        let (generation, cancel) = (c.generation, c.cancel.clone());
        let settings = crate::settings::Settings::load();
        let protected = settings.protected_patterns();
        let preferred = settings.default_branch;
        let progress = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let p2 = progress.clone();
        let quick = cx.background_executor().spawn({
            let (repo, cancel, protected) = (repo.clone(), cancel.clone(), protected.clone());
            async move {
                let default = repo.base_branches(preferred.as_deref()).into_iter().next().ok_or_else(|| i18n::t!("app.clean.no_default"))?;
                let c = repo.cleanup_candidates(&default, &protected, false, &cancel, &|_, _| {}).map_err(|e| e.to_string())?;
                Ok::<_, String>((default, with_dirty(c)))
            }
        });
        cx.spawn(async move |this, cx| {
            let res = quick.await;
            let default = match res {
                Ok((default, rows)) => {
                    let alive = this.update(cx, |this, cx| {
                        let Some(c) = this.cleanup.as_mut().filter(|c| c.generation == generation) else { return false };
                        c.quick_loading = false;
                        c.default = Some(default.short_rev().to_string());
                        c.rows = merge_rows(&c.rows, rows);
                        c.loading = Some(p2.clone());
                        cx.notify();
                        true
                    });
                    if !matches!(alive, Ok(true)) {
                        return;
                    }
                    default
                }
                Err(e) => {
                    this.update(cx, |this, cx| {
                        if let Some(c) = this.cleanup.as_mut().filter(|c| c.generation == generation) {
                            c.quick_loading = false;
                            c.error = Some(e);
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            let full = cx.background_executor().spawn(async move {
                let p = progress.clone();
                let c = repo.cleanup_candidates(&default, &protected, true, &cancel, &move |done, total| {
                    p.0.store(done, Ordering::Relaxed);
                    p.1.store(total, Ordering::Relaxed);
                });
                c.map(with_dirty).map_err(|e| e.to_string())
            });
            // redraw the progress until it's done
            let ticker = this.clone();
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                    let more = ticker.update(cx, |this, cx| {
                        let on = this.cleanup.as_ref().is_some_and(|c| c.generation == generation && c.loading.is_some());
                        if on {
                            cx.notify();
                        }
                        on
                    });
                    if !matches!(more, Ok(true)) {
                        break;
                    }
                }
            })
            .detach();
            let res = full.await;
            this.update(cx, |this, cx| {
                let Some(c) = this.cleanup.as_mut().filter(|c| c.generation == generation) else { return };
                c.loading = None;
                match res {
                    Ok(rows) => c.rows = merge_rows(&c.rows, rows),
                    Err(e) => c.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn selected_rows(&self) -> Vec<&Row> {
        self.cleanup.as_ref().map(|c| c.rows.iter().filter(|r| r.active()).collect()).unwrap_or_default()
    }

    /// Remote branches that would be deleted: the selected ones whose upstream still exists.
    fn remote_args(&self) -> Vec<Vec<String>> {
        let mut by_remote: Vec<(String, Vec<String>)> = Vec::new();
        for r in self.selected_rows() {
            let Some(up) = r.c.upstream.as_ref().filter(|_| !r.c.gone) else { continue };
            let Some((remote, branch)) = repo::split_remote_name(&self.work.remotes, up) else { continue };
            match by_remote.iter_mut().find(|(r, _)| *r == remote) {
                Some((_, v)) => v.push(branch),
                None => by_remote.push((remote, vec![branch])),
            }
        }
        by_remote.into_iter().map(|(remote, branches)| ["push".to_string(), remote, "--delete".to_string()].into_iter().chain(branches).collect()).collect()
    }

    /// Delete button → (force confirmation) → (remote confirmation) → run.
    pub fn cleanup_next(&mut self, cx: &mut Context<Self>) {
        let needs_force = self.selected_rows().iter().any(|r| r.c.needs_force);
        let remote = self.cleanup.as_ref().is_some_and(|c| c.delete_remote) && !self.remote_args().is_empty();
        let Some(c) = self.cleanup.as_mut() else { return };
        let next = match c.step {
            Step::List if needs_force => Step::ConfirmForce,
            Step::List | Step::ConfirmForce if remote => Step::ConfirmRemote,
            Step::List | Step::ConfirmForce | Step::ConfirmRemote => Step::Running,
            _ => return,
        };
        if next != Step::Running {
            c.step = next;
            cx.notify();
            return;
        }
        let items: Vec<PlanItem> = c.rows.iter().filter(|r| r.active()).map(|r| PlanItem { name: r.c.name.clone(), oid: r.c.oid, force: r.c.needs_force, worktree: r.c.worktree.clone().filter(|_| r.remove_wt) }).collect();
        if items.is_empty() {
            return;
        }
        let remote = if c.delete_remote { self.remote_args() } else { Vec::new() };
        let plan = Arc::new(Plan { items, remote, report: Mutex::new(Report::default()) });
        let Some(c) = self.cleanup.as_mut() else { return };
        c.step = Step::Running;
        c.plan = Some(plan.clone());
        self.run_op(Op::Cleanup(plan), cx);
    }

    pub(crate) fn cleanup_after_op(&mut self, op: &Op, ok: bool, cx: &mut Context<Self>) {
        match op {
            Op::Cleanup(plan) => {
                let deleted = plan.report.lock().unwrap().deleted.clone();
                self.undo.extend(deleted);
                if let Some(c) = self.cleanup.as_mut() {
                    c.step = Step::Done;
                    c.rows.clear();
                }
                self.measure_git_dir(cx);
            }
            Op::Git { kind: GitKind::FetchPrune, .. } if self.cleanup.is_some() => self.load_cleanup(cx),
            Op::Git { kind: GitKind::Gc, .. } => {
                if let Some(c) = self.cleanup.as_mut() {
                    c.gc_running = false;
                }
                if ok {
                    self.toast(i18n::t!("app.clean.gc_done"), cx);
                }
                self.measure_git_dir(cx);
            }
            Op::RestoreBranch { name, .. } if ok => {
                self.undo.retain(|(n, _)| n != name);
                self.toast(i18n::t!("app.clean.restored", name = name), cx);
            }
            _ => {}
        }
    }

    pub fn run_gc(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.cleanup.as_mut() else { return };
        if c.gc_prune_now && c.step != Step::ConfirmGc {
            c.step = Step::ConfirmGc;
            cx.notify();
            return;
        }
        let mut args = vec!["gc".to_string()];
        if c.gc_prune_now {
            args.push("--prune=now".into());
        }
        if c.step == Step::ConfirmGc {
            c.step = if c.plan.is_some() { Step::Done } else { Step::List };
        }
        c.gc_running = true;
        self.run_op(Op::Git { args, kind: GitKind::Gc }, cx);
    }

    pub(crate) fn render_cleanup(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.cleanup.as_ref()?;
        let busy = self.work.job.is_some();
        let close = cx.listener(|this, _, _, cx| {
            if let Some(c) = this.cleanup.take() {
                c.cancel.cancel();
            }
            cx.notify();
        });
        let title = div().flex().items_center().gap(px(8.)).child(div().flex_1().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.clean.title"))).child(ui::icon_button(t, "clean-close", IconName::Close, true).on_click(close));
        let mut body = div().flex().flex_col().gap(px(12.));
        let selected = self.selected_rows();
        let n_sel = selected.len();
        let n_force = selected.iter().filter(|r| r.c.needs_force).count();
        let remote_args = self.remote_args();
        let n_remote: usize = remote_args.iter().map(|a| a.len() - 3).sum();
        match c.step {
            Step::List => {
                let sub = c.default.as_ref().map(|d| i18n::t!("app.clean.subtitle", default = d)).unwrap_or_default();
                let fetch = ui::outline_button(t, "clean-fetch").h(px(28.)).px(px(10.)).text_size(px(12.)).child(icon(IconName::Network, 13., t.ink)).child(i18n::t!("app.clean.fetch_prune")).when(busy, |d| d.opacity(0.5)).when(!busy, |d| {
                    d.on_click(cx.listener(|this, _, _, cx| this.run_op(Op::Git { args: vec!["fetch".into(), "--all".into(), "--prune".into(), "--progress".into()], kind: GitKind::FetchPrune }, cx)))
                });
                let progress = if c.quick_loading {
                    Some(i18n::t!("app.clean.loading"))
                } else {
                    c.loading.as_ref().map(|p| i18n::t!("app.clean.squash_progress", done = p.0.load(Ordering::Relaxed), total = p.1.load(Ordering::Relaxed)))
                };
                body = body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(div().flex_1().min_w_0().text_size(px(13.)).text_color(t.ink_muted).child(sub))
                        .children(progress.map(|p| div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(t.ink_muted).child(Spinner::new().small().color(t.accent)).child(p)))
                        .child(fetch),
                );
                if let Some(e) = &c.error {
                    body = body.child(div().text_size(px(12.)).text_color(t.danger).child(e.clone()));
                }
                let mut list = div().id("clean-list").flex().flex_col().max_h(px(360.)).overflow_y_scroll().rounded(px(10.)).border_1().border_color(t.border);
                if c.rows.is_empty() && !c.quick_loading {
                    list = list.child(div().p(px(16.)).text_size(px(13.)).text_color(t.ink_muted).child(i18n::t!("app.clean.none")));
                }
                for (i, r) in c.rows.iter().enumerate() {
                    list = list.child(self.cleanup_row(t, i, r, cx));
                }
                body = body.child(list);
                body = body.child(div().text_size(px(12.)).line_height(px(17.)).text_color(t.ink_muted).child(i18n::t!("app.clean.never")));
                body = body.child(Checkbox::new("clean-remote").small().label(i18n::t!("app.clean.delete_remote")).checked(c.delete_remote).on_click(cx.listener(|this, on: &bool, _, cx| {
                    if let Some(c) = this.cleanup.as_mut() {
                        c.delete_remote = *on;
                    }
                    cx.notify();
                })));
                body = body.child(self.disk_note(t, cx));
                body = body.child(self.undo_list(t, cx));
                let label = if n_force > 0 { i18n::tn!("app.clean.delete_next", n_sel) } else { i18n::tn!("app.clean.delete", n_sel) };
                let ok = ui::filled_button(t, "clean-ok", t.danger).child(label).when(n_sel == 0 || busy, |d| d.opacity(0.4).cursor_default()).when(n_sel > 0 && !busy, |d| d.on_click(cx.listener(|this, _, _, cx| this.cleanup_next(cx))));
                body = body.child(div().flex().justify_end().gap(px(8.)).child(ok));
            }
            Step::ConfirmForce => {
                let names: Vec<String> = selected.iter().filter(|r| r.c.needs_force).map(|r| r.c.name.clone()).collect();
                body = body
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(t.danger).child(i18n::tn!("app.clean.force_title", n_force)))
                    .child(div().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).child(i18n::t!("app.clean.force_body")))
                    .child(code_block(t, "clean-force-list", names.iter().map(|n| format!("git branch -D {n}")).collect::<Vec<_>>().join("\n"))) // i18n-ok: command line
                    .child(self.confirm_buttons(t, i18n::t!("app.confirm.force_ok"), true, cx));
            }
            Step::ConfirmRemote => {
                let typed = c.remote_input.read(cx).value().trim() == n_remote.to_string();
                body = body
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(t.danger).child(i18n::tn!("app.clean.remote_title", n_remote)))
                    .child(div().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).child(i18n::t!("app.clean.remote_body")))
                    .child(code_block(t, "clean-remote-list", remote_args.iter().map(|a| format!("git {}", a.join(" "))).collect::<Vec<_>>().join("\n"))) // i18n-ok: command line
                    .child(div().text_size(px(12.)).child(i18n::t!("app.clean.remote_type", n = n_remote)))
                    .child(Input::new(&c.remote_input))
                    .child(self.confirm_buttons(t, i18n::t!("app.confirm.remote_ok"), typed, cx));
            }
            Step::Running => {
                body = body.child(div().flex().items_center().gap(px(8.)).text_size(px(13.)).child(Spinner::new().small().color(t.accent)).child(i18n::t!("app.clean.running")));
            }
            Step::Done => {
                if let Some(plan) = &c.plan {
                    let rep = plan.report.lock().unwrap();
                    body = body.child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(i18n::tn!("app.clean.done", rep.deleted.len())));
                    if rep.worktrees > 0 {
                        body = body.child(div().text_size(px(13.)).child(i18n::tn!("app.clean.done_worktrees", rep.worktrees)));
                    }
                    if rep.remote_deleted > 0 {
                        body = body.child(div().text_size(px(13.)).child(i18n::tn!("app.clean.done_remote", rep.remote_deleted)));
                    }
                    if !rep.remote_failed.is_empty() {
                        body = body.child(div().text_size(px(13.)).text_color(t.danger).child(i18n::t!("app.clean.remote_failed", names = rep.remote_failed.join(", "))));
                    }
                    for (name, err) in &rep.failed {
                        body = body.child(div().text_size(px(13.)).text_color(t.danger).child(i18n::t!("app.clean.failed", name = name, detail = err.lines().last().unwrap_or_default())));
                    }
                }
                body = body.child(self.disk_note(t, cx)).child(self.undo_list(t, cx));
                body = body.child(div().flex().justify_end().gap(px(8.)).child(ui::outline_button(t, "clean-again").child(i18n::t!("app.clean.again")).on_click(cx.listener(|this, _, _, cx| {
                    if let Some(c) = this.cleanup.as_mut() {
                        c.step = Step::List;
                        c.plan = None;
                    }
                    this.load_cleanup(cx);
                }))));
            }
            Step::ConfirmGc => {
                body = body
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.clean.gc_prune_title")))
                    .child(div().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).child(i18n::t!("app.clean.gc_prune_body")))
                    .child(code_block(t, "clean-gc", "git gc --prune=now".into())) // i18n-ok: command line
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(ui::outline_button(t, "gc-cancel").child(i18n::t!("app.confirm.cancel")).on_click(cx.listener(|this, _, _, cx| {
                                if let Some(c) = this.cleanup.as_mut() {
                                    c.step = if c.plan.is_some() { Step::Done } else { Step::List };
                                }
                                cx.notify();
                            })))
                            .child(ui::filled_button(t, "gc-ok", t.danger).child(i18n::t!("app.clean.gc_run")).on_click(cx.listener(|this, _, _, cx| this.run_gc(cx)))),
                    );
            }
        }
        let card = div()
            .id("clean-card")
            .flex()
            .flex_col()
            .gap(px(14.))
            .w(px(780.))
            .max_h(relative(0.9))
            .p(px(22.))
            .rounded(px(16.))
            .border_1()
            .border_color(t.border)
            .bg(t.surface)
            .shadow_lg()
            .overflow_y_scroll()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(title)
            .child(body);
        Some(backdrop(t, "clean-backdrop").child(card).into_any_element())
    }

    fn confirm_buttons(&self, t: &Tokens, ok: String, ready: bool, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .justify_end()
            .gap(px(8.))
            .child(ui::outline_button(t, "clean-back").child(i18n::t!("app.clean.back")).on_click(cx.listener(|this, _, _, cx| {
                if let Some(c) = this.cleanup.as_mut() {
                    c.step = Step::List;
                }
                cx.notify();
            })))
            .child(ui::filled_button(t, "clean-confirm", t.danger).child(ok).when(!ready, |d| d.opacity(0.4).cursor_default()).when(ready, |d| d.on_click(cx.listener(|this, _, _, cx| this.cleanup_next(cx)))))
    }

    fn cleanup_row(&self, t: &Tokens, i: usize, r: &Row, cx: &mut Context<Self>) -> AnyElement {
        let (reason, color) = match &r.c.reason {
            Reason::Merged(into) => (i18n::t!("app.clean.reason_merged", into = into), t.added),
            Reason::Squashed { confident: true } => (i18n::t!("app.clean.reason_squashed"), t.added),
            Reason::Squashed { confident: false } => (i18n::t!("app.clean.reason_squash_maybe"), t.tag),
            Reason::Gone => (i18n::t!("app.clean.reason_gone"), t.tag),
        };
        let chip = |text: String, c: Hsla| div().flex_none().px(px(6.)).py(px(1.)).rounded(px(5.)).bg(c.opacity(0.14)).text_color(c).text_size(px(11.)).child(text);
        let blocked = r.c.worktree.is_some() && !(r.remove_wt && r.wt_dirty == Some(0));
        let mut row = div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .px(px(12.))
            .py(px(8.))
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(Checkbox::new(("clean-row", i)).small().checked(r.checked).on_click(cx.listener(move |this, on: &bool, _, cx| {
                        if let Some(row) = this.cleanup.as_mut().and_then(|c| c.rows.get_mut(i)) {
                            row.checked = *on;
                        }
                        cx.notify();
                    })))
                    .child(div().flex_none().max_w(px(280.)).truncate().text_size(px(13.)).font_weight(FontWeight::MEDIUM).when(blocked, |d| d.text_color(t.ink_muted)).child(r.c.name.clone()))
                    .child(chip(reason, color))
                    .when(r.c.gone && !matches!(r.c.reason, Reason::Gone), |d| d.child(chip(i18n::t!("app.clean.reason_gone"), t.tag)))
                    .when(r.c.needs_force, |d| d.child(chip("-D".into(), t.danger))) // i18n-ok: flag
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(t.ink_muted).child(r.c.subject.clone()))
                    .child(div().flex_none().text_size(px(12.)).text_color(t.ink_muted).child(ago(r.c.time, now()))),
            );
        if let Some(wt) = &r.c.worktree {
            let dirty = r.wt_dirty.unwrap_or(usize::MAX);
            let note = match r.wt_dirty {
                Some(n) if n > 0 => i18n::tn!("app.clean.wt_dirty", n, path = wt.display()),
                _ => i18n::t!("app.clean.wt_attached", path = wt.display()),
            };
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pl(px(26.))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(if dirty > 0 && dirty != usize::MAX { t.danger } else { t.ink_muted }).child(note))
                    .when(dirty == 0, |d| {
                        d.child(Checkbox::new(("clean-wt", i)).small().label(i18n::t!("app.clean.also_worktree")).checked(r.remove_wt).on_click(cx.listener(move |this, on: &bool, _, cx| {
                            if let Some(row) = this.cleanup.as_mut().and_then(|c| c.rows.get_mut(i)) {
                                row.remove_wt = *on;
                            }
                            cx.notify();
                        })))
                    }),
            );
        }
        row.into_any_element()
    }

    /// Branch refs are tiny; the space is in worktrees and unreachable objects.
    fn disk_note(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let Some(c) = &self.cleanup else { return div() };
        let size = match (c.git_before, c.git_now) {
            (Some(b), Some(n)) if b != n => i18n::t!("app.clean.git_size_change", before = bytes(b), now = bytes(n)),
            (_, Some(n)) => i18n::t!("app.clean.git_size", size = bytes(n)),
            _ => i18n::t!("app.clean.git_size_measuring"),
        };
        let busy = self.work.job.is_some();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(10.))
            .bg(t.canvas)
            .child(div().flex().items_center().gap(px(6.)).child(icon(IconName::HardDrive, 14., t.ink_muted)).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.clean.disk_title"))))
            .child(div().text_size(px(12.)).line_height(px(17.)).text_color(t.ink_muted).child(i18n::t!("app.clean.disk_body")))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex_1().text_size(px(12.)).font_family(MONO).child(size))
                    .child(Checkbox::new("gc-prune").small().label(i18n::t!("app.clean.gc_prune")).checked(c.gc_prune_now).on_click(cx.listener(|this, on: &bool, _, cx| {
                        if let Some(c) = this.cleanup.as_mut() {
                            c.gc_prune_now = *on;
                        }
                        cx.notify();
                    })))
                    .child(
                        ui::outline_button(t, "gc-run")
                            .h(px(28.))
                            .px(px(10.))
                            .text_size(px(12.))
                            .when(c.gc_running, |d| d.child(Spinner::new().small().color(t.accent)))
                            .child(i18n::t!("app.clean.gc_run"))
                            .when(busy, |d| d.opacity(0.5))
                            .when(!busy, |d| d.on_click(cx.listener(|this, _, _, cx| this.run_gc(cx)))),
                    ),
            )
    }

    fn undo_list(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        if self.undo.is_empty() {
            return div();
        }
        let busy = self.work.job.is_some();
        let mut d = div().flex().flex_col().gap(px(4.)).child(ui::section_title(t, i18n::t!("app.clean.undo_title")));
        for (i, (name, oid)) in self.undo.iter().enumerate() {
            let (n, o) = (name.clone(), *oid);
            d = d.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(13.))
                    .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                    .child(div().font_family(MONO).text_size(px(11.)).text_color(t.ink_muted).child(oid.to_hex_with_len(10).to_string()))
                    .child(ui::outline_button(t, ("undo", i)).h(px(24.)).px(px(8.)).text_size(px(12.)).child(i18n::t!("app.clean.restore")).when(busy, |d| d.opacity(0.5)).when(!busy, |d| d.on_click(cx.listener(move |this, _, _, cx| this.run_op(Op::RestoreBranch { name: n.clone(), oid: o }, cx))))),
            );
        }
        d
    }
}

/// Dirty counts for candidates checked out in a worktree.
fn with_dirty(cands: Vec<Candidate>) -> Vec<Row> {
    cands
        .into_iter()
        .map(|c| {
            // a missing folder removes fine (git just forgets it); a failing status blocks it
            let wt_dirty = match &c.worktree {
                Some(w) if !w.is_dir() => Some(0),
                Some(w) => Repo::status_at(w).ok().map(|s| s.staged.len() + s.unstaged.len()),
                None => None,
            };
            Row { checked: c.suggested() && c.worktree.is_none(), remove_wt: false, wt_dirty, c }
        })
        .collect()
}

/// New results keep the user's checkbox choices for branches already listed.
fn merge_rows(old: &[Row], new: Vec<Row>) -> Vec<Row> {
    new.into_iter()
        .map(|mut r| {
            if let Some(o) = old.iter().find(|o| o.c.name == r.c.name && o.c.reason == r.c.reason) {
                r.checked = o.checked;
                r.remove_wt = o.remove_wt;
            }
            r
        })
        .collect()
}

pub fn code_block(t: &Tokens, id: &'static str, text: String) -> Stateful<Div> {
    div().id(id).max_h(px(160.)).overflow_y_scroll().p(px(8.)).rounded(px(6.)).bg(t.canvas).font_family(MONO).text_size(px(12.)).line_height(px(16.)).child(text)
}

pub fn backdrop(t: &Tokens, id: &'static str) -> Stateful<Div> {
    div().id(id).absolute().inset_0().flex().items_center().justify_center().bg(t.backdrop)
}
