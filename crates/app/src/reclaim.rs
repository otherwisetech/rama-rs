//! "Free up space…": what merged worktrees and build folders could give back
//! (`repo::reclaim::plan` over the measured sizes), a dialog with nothing selected that lists
//! every path and size, what each one is and what deleting it costs (and, for a merged
//! worktree, the files git doesn't track in it), a second native confirmation listing every
//! path, the run in the background (everything to the Trash), a Today entry when a repository
//! could free more than 10 GB, and "Open in diskmap". Cargo folders outside the repository are
//! shown for information only.
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
use repo::reclaim::{self, Category, PlanInput, ReclaimKind, Report, Skip, Suggestion};
use repo::{Cancel, DiskUsage};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub struct ReclaimDialog {
    /// Every suggestion with whether the user ticked it: none is ticked when the dialog opens.
    pub items: Vec<(Suggestion, bool)>,
    /// Merged worktrees → files git doesn't track in them (build output aside), once listed;
    /// `None` inside when git couldn't tell.
    pub keepers: HashMap<PathBuf, Option<Vec<String>>>,
    /// Also `git branch -d` the branches of removed worktrees.
    pub branches: bool,
    pub running: bool,
    pub report: Option<Result<Report, String>>,
}

impl ReclaimDialog {
    /// A fresh dialog over `plan`: nothing selected.
    pub fn new(plan: &[Suggestion]) -> Self {
        ReclaimDialog { items: plan.iter().map(|s| (s.clone(), false)).collect(), keepers: HashMap::new(), branches: false, running: false, report: None }
    }

    fn selected(&self) -> Vec<Suggestion> {
        self.items.iter().filter(|(_, on)| *on).map(|(s, _)| s.clone()).collect()
    }

    /// Every selected merged worktree's untracked files are listed already.
    fn keepers_known(&self) -> bool {
        self.items.iter().filter(|(s, on)| *on && s.kind == ReclaimKind::MergedWorktree).all(|(s, _)| self.keepers.contains_key(&s.owner))
    }

    /// The second confirmation's body: every path that goes to the Trash, the untracked files
    /// travelling with each worktree, and the branches `git branch -d` would delete.
    pub fn confirm_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut branches = Vec::new();
        for s in self.selected() {
            for (p, b) in &s.paths {
                out.push(format!("{}  {}", bytes(*b), p.display())); // i18n-ok: size and path
            }
            if let Some(Some(k)) = self.keepers.get(&s.owner).filter(|_| s.kind == ReclaimKind::MergedWorktree) {
                out.extend(k.iter().map(|f| format!("    + {f}"))); // i18n-ok: file name
            }
            branches.extend(s.branch.clone().filter(|_| self.branches));
        }
        if !branches.is_empty() {
            out.push(i18n::t!("app.reclaim.confirm_branches", names = branches.join(", ")));
        }
        out
    }
}

fn category_text(c: Category) -> (String, String) {
    match c {
        Category::Worktree => (i18n::t!("app.reclaim.cat_worktree"), i18n::t!("app.reclaim.what_worktree")),
        Category::Incremental => (i18n::t!("app.reclaim.cat_incremental"), i18n::t!("app.reclaim.what_incremental")),
        Category::Target => (i18n::t!("app.reclaim.cat_target"), i18n::t!("app.reclaim.what_target")),
        Category::NodeModules => (i18n::t!("app.reclaim.cat_node_modules"), i18n::t!("app.reclaim.what_node_modules")),
        Category::Next => (i18n::t!("app.reclaim.cat_next"), i18n::t!("app.reclaim.what_next")),
        Category::BuildOutput => (i18n::t!("app.reclaim.cat_build"), i18n::t!("app.reclaim.what_build")),
        Category::Gradle => (i18n::t!("app.reclaim.cat_gradle"), i18n::t!("app.reclaim.what_gradle")),
        Category::DerivedData => (i18n::t!("app.reclaim.cat_derived"), i18n::t!("app.reclaim.what_derived")),
    }
}

const KINDS: [ReclaimKind; 4] = [ReclaimKind::MergedWorktree, ReclaimKind::Incremental, ReclaimKind::StaleTarget, ReclaimKind::StaleDeps];

impl RamaApp {
    /// Recompute the plan from the sizes measured so far, and the Today entry with it.
    pub fn refresh_reclaim(&mut self, _cx: &mut Context<Self>) {
        let Some(list) = self.map.list().cloned() else { return };
        let usage: HashMap<PathBuf, DiskUsage> = self.map.sizes.iter().filter_map(|(p, s)| if let Size::Done { usage, .. } = s { Some((p.clone(), usage.clone())) } else { None }).collect();
        let s = crate::settings::Settings::load();
        let protected = s.protected_patterns();
        let home = repo::safety::user_home();
        let plan = reclaim::plan(&PlanInput { entries: &list, usage: &usage, current: self.map.current.as_deref(), protected: &protected, home: home.as_deref(), now: crate::util::now(), stale_days: s.stale_days() });
        let total = reclaim::reclaimable(&plan);
        self.map.reclaim = Rc::new(plan);
        if let (Some(repo), Some(main)) = (self.repo.as_ref(), list.first()) {
            let event = s.disk_today().then(|| crate::otherwise::reclaim_event(&main.wt.path, total, chrono::Utc::now())).flatten();
            crate::otherwise::set_disk_event(&repo.root(), event);
        }
    }

    /// Bytes the current plan frees in `worktree` (or everywhere with `None`).
    pub fn reclaimable(&self, worktree: Option<&Path>) -> u64 {
        self.map.reclaim.iter().filter(|s| worktree.is_none_or(|w| s.owner == w)).map(|s| s.bytes).sum()
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
        self.map.reclaim_dialog = Some(ReclaimDialog::new(&self.map.reclaim));
        // what git doesn't track in each merged worktree (read-only), shown before anything goes
        let merged: Vec<PathBuf> = self.map.reclaim.iter().filter(|s| s.kind == ReclaimKind::MergedWorktree).map(|s| s.owner.clone()).collect();
        if !merged.is_empty() {
            let task = cx.background_executor().spawn(async move { merged.into_iter().map(|w| (w.clone(), reclaim::untracked_keepers(&w))).collect::<Vec<_>>() });
            cx.spawn(async move |this, cx| {
                let found = task.await;
                this.update(cx, |this, cx| {
                    if let Some(d) = this.map.reclaim_dialog.as_mut() {
                        d.keepers.extend(found);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
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

    /// The second confirmation (native, every path listed), then the run.
    fn confirm_reclaim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.map.reclaim_dialog.as_ref() else { return };
        let items = d.selected();
        if d.running || items.is_empty() || !d.keepers_known() {
            return;
        }
        let size: u64 = items.iter().map(|s| s.bytes).sum();
        let n: usize = items.iter().map(|s| s.paths.len()).sum();
        let msg = i18n::tn!("app.reclaim.confirm", n as i64, size = bytes(size));
        let detail = format!("{}\n\n{}", i18n::t!("app.reclaim.confirm_detail"), d.confirm_lines().join("\n")); // i18n-ok: layout
        let (ok, cancel) = (i18n::t!("app.reclaim.confirm_ok"), i18n::t!("app.confirm.cancel"));
        let rx = window.prompt(PromptLevel::Warning, &msg, Some(&detail), &[ok.as_str(), cancel.as_str()], cx);
        cx.spawn(async move |this, cx| {
            if rx.await == Ok(0) {
                this.update(cx, |this, cx| this.run_reclaim(cx)).ok();
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
        let task = cx.background_executor().spawn(async move { let home = repo::safety::user_home();
            repo.reclaim(&items, &default_rev, branches, home.as_deref(), &mut reclaim::system_trash, &Cancel::default(), &mut |_, _| {}).map_err(|e| e.to_string()) });
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
        match &main {
            Some(m) if *m == s.owner => self.repo.as_ref().map(|r| r.name()).unwrap_or_default(),
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
        let small = |s: String| div().text_size(px(11.5)).line_height(px(16.)).text_color(t.ink_muted).child(s);
        let footer_ok: Option<AnyElement>;
        if let Some(rep) = &d.report {
            match rep {
                Err(e) => body = body.child(div().text_size(px(12.5)).text_color(t.danger).child(e.clone())),
                Ok(r) => {
                    body = body.child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.reclaim.freed", size = bytes(r.freed))));
                    for (w, files) in &r.kept_in_trash {
                        let name = w.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        body = body.child(text(i18n::tn!("app.reclaim.kept_in_trash", files.len() as i64, name = name)));
                        body = body.child(div().p(px(8.)).rounded(px(6.)).bg(t.canvas).font_family(MONO).text_size(px(11.5)).line_height(px(16.)).children(files.iter().map(|f| div().child(f.clone()))));
                    }
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
                                    Skip::Unpushed => i18n::t!("app.reclaim.skip_unpushed"),
                                    Skip::Locked => i18n::t!("app.reclaim.skip_locked"),
                                    Skip::NotArtifact => i18n::t!("app.reclaim.skip_not_artifact"),
                                    Skip::Protected => i18n::t!("app.reclaim.skip_protected"),
                                    Skip::Tracked => i18n::t!("app.reclaim.skip_tracked"),
                                    Skip::Unreadable => i18n::t!("app.reclaim.skip_unreadable"),
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
                    ReclaimKind::Incremental => (i18n::t!("app.reclaim.kind_incremental"), i18n::t!("app.reclaim.how_trash")),
                    ReclaimKind::StaleTarget => (i18n::tn!("app.reclaim.kind_stale_target", days as i64), i18n::t!("app.reclaim.how_trash")),
                    ReclaimKind::StaleDeps => (i18n::tn!("app.reclaim.kind_stale_deps", days as i64), i18n::t!("app.reclaim.how_trash")),
                };
                let mut sec = div().flex().flex_col().gap(px(6.)).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(title)).child(small(how));
                for (i, s, on) in rows {
                    let mut label = self.reclaim_owner_label(s);
                    if kind != ReclaimKind::MergedWorktree && kind != ReclaimKind::Incremental {
                        label = format!("{label} · {}", i18n::tn!("app.reclaim.idle", s.idle_days)); // i18n-ok: separator
                    }
                    let lead = Checkbox::new(("reclaim-item", i)).small().checked(on).when(d.running, |c| c.disabled(true)).on_click(cx.listener(move |this, on: &bool, _, cx| {
                        if let Some((_, v)) = this.map.reclaim_dialog.as_mut().and_then(|d| d.items.get_mut(i)) {
                            *v = *on;
                        }
                        cx.notify();
                    }));
                    // each path: size and path, then what it is and what deleting it costs
                    let mut detail = div().ml(px(26.)).p(px(6.)).rounded(px(6.)).bg(t.canvas).flex().flex_col().gap(px(4.));
                    for (p, b) in &s.paths {
                        let (cat, what) = category_text(reclaim::category(s.kind, p));
                        detail = detail.child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().font_family(MONO).text_size(px(11.)).line_height(px(15.)).text_color(t.ink_muted).child(format!("{:>9}  {}", bytes(*b), p.display()))) // i18n-ok: size and path
                                .child(div().text_size(px(11.5)).line_height(px(16.)).child(div().font_weight(FontWeight::MEDIUM).child(cat)).child(div().text_color(t.ink_muted).child(what))),
                        );
                    }
                    if kind == ReclaimKind::MergedWorktree {
                        match d.keepers.get(&s.owner) {
                            None => detail = detail.child(small(i18n::t!("app.reclaim.keepers_loading"))),
                            Some(None) => detail = detail.child(div().text_size(px(11.5)).text_color(t.danger).child(i18n::t!("app.reclaim.keepers_unknown"))),
                            Some(Some(k)) if k.is_empty() => detail = detail.child(small(i18n::t!("app.reclaim.keepers_none"))),
                            Some(Some(k)) => {
                                detail = detail.child(div().text_size(px(11.5)).line_height(px(16.)).text_color(t.danger).child(i18n::tn!("app.reclaim.keepers", k.len() as i64))).child(div().font_family(MONO).text_size(px(11.)).line_height(px(15.)).children(k.iter().map(|f| div().child(f.clone()))));
                            }
                        }
                    }
                    sec = sec.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(div().flex().items_center().gap(px(8.)).child(lead).child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).child(label)).when_some(s.branch.as_ref(), |d, b| d.child(div().flex_none().text_size(px(11.)).font_family(MONO).text_color(t.ink_muted).child(b.clone()))).child(div().flex_none().text_size(px(12.5)).child(bytes(s.bytes))))
                            .child(detail),
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
            // cargo folders outside the repository: information only, never offered
            let shared: Vec<(PathBuf, u64, usize)> = self.map.shared_users.iter().filter_map(|(p, users)| self.map.usage(p).map(|u| (p.clone(), u.total, users.len()))).filter(|(_, b, _)| *b > 0).collect();
            if !shared.is_empty() {
                let mut sec = div().flex().flex_col().gap(px(6.)).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(i18n::t!("app.reclaim.shared_title"))).child(small(i18n::t!("app.reclaim.shared_how")));
                for (p, b, n) in shared {
                    sec = sec.child(div().ml(px(26.)).font_family(MONO).text_size(px(11.)).text_color(t.ink_muted).child(i18n::tn!("app.reclaim.shared", n as i64, path = p.display().to_string(), size = bytes(b))));
                }
                body = body.child(sec);
            }
            body = body.child(text(i18n::t!("app.reclaim.never")));
            let sel: u64 = d.items.iter().filter(|(_, on)| *on).map(|(s, _)| s.bytes).sum();
            let ready = sel > 0 && !d.running && d.keepers_known();
            footer_ok = Some(
                ui::filled_button(t, "reclaim-ok", t.danger)
                    .when(d.running, |b| b.child(Spinner::new().small().color(t.surface)))
                    .child(if d.running { i18n::t!("app.reclaim.running") } else { i18n::t!("app.reclaim.ok", size = bytes(sel)) })
                    .when(!ready, |b| b.opacity(0.4).cursor_default())
                    .when(ready, |b| b.on_click(cx.listener(|this, _, window, cx| this.confirm_reclaim(window, cx))))
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

#[cfg(test)]
mod tests {
    use super::ReclaimDialog;
    use repo::reclaim::{ReclaimKind, Suggestion};
    use std::path::PathBuf;

    fn sugg(kind: ReclaimKind, owner: &str, paths: &[&str], branch: Option<&str>) -> Suggestion {
        let paths: Vec<(PathBuf, u64)> = paths.iter().map(|p| (PathBuf::from(p), 1_000)).collect();
        Suggestion { kind, owner: owner.into(), branch: branch.map(str::to_string), bytes: paths.iter().map(|(_, b)| b).sum(), paths, idle_days: 30 }
    }

    #[test]
    fn nothing_is_preselected_and_the_confirm_lists_every_path() {
        let plan = vec![sugg(ReclaimKind::MergedWorktree, "/w/done", &["/w/done"], Some("done")), sugg(ReclaimKind::StaleDeps, "/w/old", &["/w/old/.next", "/w/old/node_modules"], None)];
        let mut d = ReclaimDialog::new(&plan);
        assert!(d.items.iter().all(|(_, on)| !on), "nothing selected when the dialog opens");
        assert!(d.selected().is_empty() && d.confirm_lines().is_empty());
        for (_, on) in d.items.iter_mut() {
            *on = true;
        }
        // the run waits until the merged worktree's untracked files are known
        assert!(!d.keepers_known());
        d.keepers.insert("/w/done".into(), Some(vec![".env".into()]));
        assert!(d.keepers_known());
        d.branches = true;
        let lines = d.confirm_lines().join("\n");
        for p in ["/w/done", "+ .env", "/w/old/.next", "/w/old/node_modules", "done"] {
            assert!(lines.contains(p), "{p} missing from {lines}");
        }
    }
}
