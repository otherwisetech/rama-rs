//! "Free up space…": what merged worktrees and build folders could give back
//! (`repo::reclaim::plan` over the measured sizes), a dialog that lists every path and size
//! and says how each goes before anything happens, the run in the background, a Today entry
//! when a repository could free more than 10 GB, and "Open in diskmap".
use crate::app::RamaApp;
use crate::cleanup::backdrop;
use crate::map::Size;
use crate::theme::{MONO, Tokens};
use crate::ui;
use crate::util::bytes;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::checkbox::Checkbox;
use gpui_component::spinner::Spinner;
use gpui_component::{Disableable as _, IconName, Sizable as _};
use repo::reclaim::{self, PlanInput, ReclaimKind, Report, SharedTarget, Skip, Suggestion};
use repo::{Cancel, DiskUsage};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub struct ReclaimDialog {
    pub items: Vec<(Suggestion, bool)>,
    /// Also `git branch -d` the branches of removed worktrees.
    pub branches: bool,
    pub running: bool,
    pub report: Option<Result<Report, String>>,
}

impl ReclaimDialog {
    fn selected(&self) -> Vec<Suggestion> {
        self.items.iter().filter(|(_, on)| *on).map(|(s, _)| s.clone()).collect()
    }
}

const KINDS: [ReclaimKind; 4] = [ReclaimKind::MergedWorktree, ReclaimKind::Incremental, ReclaimKind::StaleTarget, ReclaimKind::StaleDeps];

impl RamaApp {
    /// Recompute the plan from the sizes measured so far, and the Today entry with it.
    pub fn refresh_reclaim(&mut self, _cx: &mut Context<Self>) {
        let Some(list) = self.map.list().cloned() else { return };
        let usage: HashMap<PathBuf, DiskUsage> = self.map.sizes.iter().filter_map(|(p, s)| if let Size::Done { usage, .. } = s { Some((p.clone(), usage.clone())) } else { None }).collect();
        let shared: Vec<SharedTarget> = self.map.shared_users.iter().filter_map(|(p, users)| usage.get(p).map(|u| SharedTarget { path: p.clone(), usage: u.clone(), users: users.clone() })).collect();
        let s = crate::settings::Settings::load();
        let protected = s.protected_patterns();
        let plan = reclaim::plan(&PlanInput { entries: &list, usage: &usage, shared: &shared, current: self.map.current.as_deref(), protected: &protected, now: crate::util::now(), stale_days: s.stale_days() });
        let total = reclaim::reclaimable(&plan);
        self.map.reclaim = Rc::new(plan);
        if let (Some(repo), Some(main)) = (self.repo.as_ref(), list.first()) {
            let event = s.disk_today().then(|| crate::otherwise::reclaim_event(&main.wt.path, total, chrono::Utc::now())).flatten();
            crate::otherwise::set_disk_event(&repo.root(), event);
        }
    }

    /// Bytes the current plan frees in `worktree` (or everywhere with `None`).
    pub fn reclaimable(&self, worktree: Option<&Path>) -> u64 {
        self.map.reclaim.iter().filter(|s| worktree.is_none_or(|w| s.owner == w || s.users.iter().any(|u| u == w))).map(|s| s.bytes).sum()
    }

    /// A Today link asked for "Free up space…": show the map, open it once loaded.
    pub fn request_reclaim(&mut self, cx: &mut Context<Self>) {
        self.map.want_reclaim = true;
        if matches!(self.phase, crate::app::Phase::Ready) {
            self.show_map(true, cx);
        } else {
            self.map_view = true;
        }
        cx.notify();
    }

    pub fn open_reclaim(&mut self, cx: &mut Context<Self>) {
        self.detect_tools(cx);
        self.map.reclaim_dialog = Some(ReclaimDialog { items: self.map.reclaim.iter().map(|s| (s.clone(), true)).collect(), branches: false, running: false, report: None });
        cx.notify();
    }

    pub fn open_in_diskmap(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move { crate::otherwise::open_in_diskmap(&path).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                this.update(cx, |this, cx| this.toast(i18n::t!("app.reclaim.diskmap_failed", detail = e), cx)).ok();
            }
        })
        .detach();
    }

    fn run_reclaim(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let default_rev = self.map.bases.first().map(|b| b.rev.clone()).unwrap_or_default();
        let Some(d) = self.map.reclaim_dialog.as_mut() else { return };
        let items = d.selected();
        if d.running || items.is_empty() {
            return;
        }
        d.running = true;
        let branches = d.branches;
        let owners: Vec<PathBuf> = items.iter().map(|s| s.owner.clone()).collect();
        let task = cx.background_executor().spawn(async move { repo.reclaim(&items, &default_rev, branches, &Cancel::default(), &mut |_, _| {}).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                for o in &owners {
                    this.map.sizes.remove(o);
                }
                if let Some(d) = this.map.reclaim_dialog.as_mut() {
                    d.running = false;
                    d.report = Some(res);
                }
                this.reload_map(false, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn reclaim_owner_label(&self, s: &Suggestion) -> String {
        let main = self.map.list().and_then(|l| l.first()).map(|m| m.wt.path.clone());
        match (&main, s.shared) {
            (_, true) => s.owner.display().to_string(),
            (Some(m), false) if *m == s.owner => self.repo.as_ref().map(|r| r.name()).unwrap_or_default(),
            _ => s.owner.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| s.owner.display().to_string()),
        }
    }

    pub(crate) fn render_reclaim(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.map.reclaim_dialog.as_ref()?;
        let close = |this: &mut RamaApp, cx: &mut Context<RamaApp>| {
            if this.map.reclaim_dialog.as_ref().is_some_and(|d| !d.running) {
                this.map.reclaim_dialog = None;
            }
            cx.notify();
        };
        let text = |s: String| div().text_size(px(12.5)).line_height(px(18.)).text_color(t.ink_muted).child(s);
        let mut body = div().flex().flex_col().gap(px(12.));
        let main = self.map.list().and_then(|l| l.first()).map(|m| m.wt.path.clone());
        let footer_ok: Option<AnyElement>;
        if let Some(rep) = &d.report {
            match rep {
                Err(e) => body = body.child(div().text_size(px(12.5)).text_color(t.danger).child(e.clone())),
                Ok(r) => {
                    body = body.child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.reclaim.freed", size = bytes(r.freed))));
                    if !r.branches_deleted.is_empty() {
                        body = body.child(text(i18n::tn!("app.reclaim.branches_deleted", r.branches_deleted.len() as i64, names = r.branches_deleted.join(", "))));
                    }
                    for (b, e) in &r.branches_failed {
                        body = body.child(div().text_size(px(12.)).text_color(t.danger).child(i18n::t!("app.reclaim.branch_failed", name = b, detail = e)));
                    }
                    if !r.skipped.is_empty() {
                        body = body.child(text(i18n::tn!("app.reclaim.skipped", r.skipped.len() as i64)));
                        let lines: Vec<String> = r
                            .skipped
                            .iter()
                            .map(|(p, why)| {
                                let why = match why {
                                    Skip::Changed => i18n::t!("app.reclaim.skip_changed"),
                                    Skip::NotMerged => i18n::t!("app.reclaim.skip_not_merged"),
                                    Skip::NotArtifact => i18n::t!("app.reclaim.skip_not_artifact"),
                                    Skip::Tracked => i18n::t!("app.reclaim.skip_tracked"),
                                    Skip::Failed(e) => e.lines().next().unwrap_or_default().to_string(),
                                };
                                format!("{} — {why}", p.display()) // i18n-ok: separator
                            })
                            .collect();
                        body = body.child(crate::cleanup::code_block(t, "reclaim-skipped", lines.join("\n")));
                    }
                }
            }
            footer_ok = None;
        } else if d.items.is_empty() {
            let measuring = self.map.sizes.values().any(|s| matches!(s, Size::Measuring));
            body = body.child(text(if measuring { i18n::t!("app.reclaim.none_yet") } else { i18n::t!("app.reclaim.none") }));
            footer_ok = None;
        } else {
            body = body.child(text(i18n::t!("app.reclaim.intro")));
            let days = crate::settings::Settings::load().stale_days();
            for kind in KINDS {
                let rows: Vec<(usize, &Suggestion, bool)> = d.items.iter().enumerate().filter(|(_, (s, _))| s.kind == kind).map(|(i, (s, on))| (i, s, *on)).collect();
                if rows.is_empty() {
                    continue;
                }
                let (title, how) = match kind {
                    ReclaimKind::MergedWorktree => (i18n::t!("app.reclaim.kind_merged"), i18n::t!("app.reclaim.how_merged")),
                    ReclaimKind::Incremental => (i18n::t!("app.reclaim.kind_incremental"), i18n::t!("app.reclaim.how_delete")),
                    ReclaimKind::StaleTarget => (i18n::tn!("app.reclaim.kind_stale_target", days as i64), i18n::t!("app.reclaim.how_delete")),
                    ReclaimKind::StaleDeps => (i18n::tn!("app.reclaim.kind_stale_deps", days as i64), i18n::t!("app.reclaim.how_deps")),
                };
                let mut sec = div().flex().flex_col().gap(px(6.)).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(title)).child(div().text_size(px(11.5)).line_height(px(16.)).text_color(t.ink_muted).child(how));
                for (i, s, on) in rows {
                    let mut label = self.reclaim_owner_label(s);
                    if s.shared {
                        label = i18n::tn!("app.reclaim.shared", s.users.len() as i64, path = label);
                    } else if kind != ReclaimKind::MergedWorktree && kind != ReclaimKind::Incremental {
                        label = format!("{label} · {}", i18n::tn!("app.reclaim.idle", s.idle_days)); // i18n-ok: separator
                    }
                    let paths: Vec<String> = s.paths.iter().map(|(p, b)| format!("{:>9}  {}", bytes(*b), p.display())).collect(); // i18n-ok: size and path
                    let lead = Checkbox::new(("reclaim-item", i)).small().checked(on).when(d.running, |c| c.disabled(true)).on_click(cx.listener(move |this, on: &bool, _, cx| {
                        if let Some((_, v)) = this.map.reclaim_dialog.as_mut().and_then(|d| d.items.get_mut(i)) {
                            *v = *on;
                        }
                        cx.notify();
                    }));
                    sec = sec.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(div().flex().items_center().gap(px(8.)).child(lead).child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).child(label)).when_some(s.branch.as_ref(), |d, b| d.child(div().flex_none().text_size(px(11.)).font_family(MONO).text_color(t.ink_muted).child(b.clone()))).child(div().flex_none().text_size(px(12.5)).child(bytes(s.bytes))))
                            .child(div().ml(px(26.)).p(px(6.)).rounded(px(6.)).bg(t.canvas).font_family(MONO).text_size(px(11.)).line_height(px(15.)).text_color(t.ink_muted).children(paths.into_iter().map(|l| div().child(l))).when(kind == ReclaimKind::MergedWorktree, |d| d.child(div().child(format!("git worktree remove {}", s.owner.display()))))), // i18n-ok: command line
                    );
                }
                if kind == ReclaimKind::MergedWorktree && d.items.iter().any(|(s, _)| s.branch.is_some()) {
                    sec = sec.child(Checkbox::new("reclaim-branches").small().label(i18n::t!("app.reclaim.delete_branches")).checked(d.branches).on_click(cx.listener(|this, on: &bool, _, cx| {
                        if let Some(d) = this.map.reclaim_dialog.as_mut() {
                            d.branches = *on;
                        }
                        cx.notify();
                    })));
                }
                body = body.child(sec);
            }
            body = body.child(text(i18n::t!("app.reclaim.never")));
            let sel: u64 = d.items.iter().filter(|(_, on)| *on).map(|(s, _)| s.bytes).sum();
            let ready = sel > 0 && !d.running;
            footer_ok = Some(
                ui::filled_button(t, "reclaim-ok", t.danger)
                    .when(d.running, |b| b.child(Spinner::new().small().color(t.surface)))
                    .child(if d.running { i18n::t!("app.reclaim.running") } else { i18n::t!("app.reclaim.ok", size = bytes(sel)) })
                    .when(!ready, |b| b.opacity(0.4).cursor_default())
                    .when(ready, |b| b.on_click(cx.listener(|this, _, _, cx| this.run_reclaim(cx))))
                    .into_any_element(),
            );
        }
        let diskmap = (self.map.diskmap && main.is_some()).then(|| {
            let m = main.clone().unwrap_or_default();
            let scan = m.parent().map(Path::to_path_buf).unwrap_or(m);
            ui::outline_button(t, "reclaim-diskmap").child(ui::icon(IconName::ChartPie, 14., t.ink)).child(i18n::t!("app.reclaim.open_diskmap")).on_click(cx.listener(move |this, _, _, cx| this.open_in_diskmap(scan.clone(), cx)))
        });
        let done = d.report.is_some();
        let card = div()
            .id("reclaim-dialog")
            .flex()
            .flex_col()
            .gap(px(14.))
            .w(px(640.))
            .max_h(relative(0.9))
            .overflow_y_scroll()
            .p(px(22.))
            .rounded(px(16.))
            .border_1()
            .border_color(t.border)
            .bg(t.surface)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.reclaim.title")))
            .child(body)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .children(diskmap)
                    .child(div().flex_1())
                    .child(ui::outline_button(t, "reclaim-cancel").child(if done { i18n::t!("app.reclaim.close") } else { i18n::t!("app.confirm.cancel") }).on_click(cx.listener(move |this, _, _, cx| close(this, cx))))
                    .children(footer_ok),
            );
        Some(backdrop(t, "reclaim-backdrop").on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx))).child(card).into_any_element())
    }
}
