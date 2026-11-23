//! Change branch (any checkout) and Swap with main checkout: the confirmations with what travels
//! where, the result when something was kept aside, the swap badges and the map animation. The
//! git side is `repo::swap`.
use crate::app::RamaApp;
use crate::cleanup::backdrop;
use crate::theme::{MONO, Tokens};
use crate::ui;
use crate::work::{Op, SwapSlot};
use gpui_component::Sizable as _;
use gpui::*;
use repo::swap::{self, Blocker, ChangePreview, KeptStash, Ledger, SwapError, SwapPreview, Target};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the two cards trade branch labels after a swap.
pub const EXCHANGE: Duration = Duration::from_millis(650);
/// One pulse of the edge between the two cards while the swap runs.
const PULSE: f32 = 0.9;

pub enum SwapDialog {
    /// Waiting for the background check.
    Checking,
    Change(ChangePreview, Target),
    Swap(SwapPreview),
    /// The branch is checked out in `worktree`: offer the swap instead.
    Elsewhere { dir: PathBuf, branch: String, worktree: PathBuf },
    Blocked(Blocker),
    /// Something was kept aside or couldn't be undone.
    Outcome { title: String, body: String, kept: Vec<KeptStash> },
}

pub struct SwapAnim {
    pub a: PathBuf,
    pub b: PathBuf,
    pub started: Instant,
    /// Set when the swap finished; the label exchange runs from here.
    pub done: Option<Instant>,
}

pub struct SwapUi {
    pub ledger: Ledger,
    pub dialog: Option<SwapDialog>,
    pub anim: Option<SwapAnim>,
}

/// `<data root>/rama/swaps.tsv`: the swaps in effect, so the badges survive a restart.
fn ledger_file() -> PathBuf {
    crate::storage::app_dir().join("swaps.tsv")
}

impl Default for SwapUi {
    fn default() -> Self {
        SwapUi { ledger: Ledger::load(&ledger_file()), dialog: None, anim: None }
    }
}

fn folder(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

fn files(n: usize) -> String {
    i18n::tn!("app.swap.files", n)
}

/// Title and explanation of why nothing was started.
pub fn blocker_text(b: &Blocker) -> (String, String) {
    match b {
        Blocker::Missing(p) => (i18n::t!("app.swap.missing_title"), i18n::t!("app.swap.missing", path = p.display())),
        Blocker::Detached(p) => (i18n::t!("app.swap.detached_title"), i18n::t!("app.swap.detached", name = folder(p))),
        Blocker::InProgress { path, what } => (i18n::t!("app.swap.in_progress_title", what = what), i18n::t!("app.swap.in_progress", name = folder(path), what = what)),
        Blocker::Conflicts(p) => (i18n::t!("app.swap.conflicts_title"), i18n::t!("app.swap.conflicts", name = folder(p))),
        Blocker::Submodule { path, submodule } => (i18n::t!("app.swap.submodule_title"), i18n::t!("app.swap.submodule", submodule = submodule, name = folder(path))),
        Blocker::LfsMissing(p) => (i18n::t!("app.swap.lfs_title"), i18n::t!("app.swap.lfs", name = folder(p))),
        Blocker::Elsewhere { branch, worktree } => (i18n::t!("app.swap.elsewhere_title", branch = branch), i18n::t!("app.swap.elsewhere", branch = branch, path = worktree.display())),
        Blocker::AlreadySwapped { path, with } => (i18n::t!("app.swap.already_title"), i18n::t!("app.swap.already", name = folder(path), with = folder(with))),
        Blocker::Nothing => (i18n::t!("app.swap.nothing_title"), i18n::t!("app.swap.nothing")),
    }
}

fn kept_lines(kept: &[KeptStash]) -> String {
    kept.iter().map(|k| i18n::t!("app.swap.kept_line", tag = k.tag, sha = &k.sha[..k.sha.len().min(10)], branch = k.branch, path = k.path.display())).collect::<Vec<_>>().join("\n")
}

impl RamaApp {
    fn main_checkout_path(&self) -> Option<PathBuf> {
        self.map.list().and_then(|l| l.first()).map(|e| e.wt.path.clone()).or_else(|| self.repo.as_ref().and_then(|r| r.worktrees().ok()).and_then(|w| w.into_iter().next()).map(|w| w.path))
    }

    /// The name a card goes by: "main checkout", else its folder.
    pub(crate) fn checkout_name(&self, p: &Path) -> String {
        if self.main_checkout_path().is_some_and(|m| m == p) { i18n::t!("app.swap.main_checkout") } else { folder(p) }
    }

    /// The partner of `p` in a swap in effect.
    pub(crate) fn swapped_with(&self, p: &Path) -> Option<PathBuf> {
        self.swap.ledger.find(p).and_then(|r| r.partner(p)).map(Path::to_path_buf)
    }

    /// Forget swaps the checkouts no longer reflect, from a fresh worktree list.
    pub(crate) fn prune_swaps(&mut self) {
        let Some(list) = self.map.list().cloned() else { return };
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let branch_of = |p: &Path| list.iter().find(|e| canon(&e.wt.path) == canon(p)).and_then(|e| e.wt.branch.clone());
        if self.swap.ledger.retain_live(&branch_of) {
            let _ = self.swap.ledger.save(&ledger_file());
        }
    }

    /// "Change branch…" on a card: the picker acts on `dir`.
    pub(crate) fn change_branch_of(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_branch_picker_for(Some(dir), window, cx);
    }

    /// A branch picked for `dir`: check in the background, then confirm (or run straight away
    /// when there is nothing to carry).
    pub(crate) fn ask_change_branch(&mut self, dir: PathBuf, target: Target, cx: &mut Context<Self>) {
        self.swap.dialog = Some(SwapDialog::Checking);
        let (t, d) = (target.clone(), dir.clone());
        let task = cx.background_executor().spawn(async move { swap::preview_change(&d, &t) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                this.swap.dialog = match res {
                    Ok(p) if p.changed == 0 => {
                        this.run_change(dir, target, cx);
                        None
                    }
                    Ok(p) => Some(SwapDialog::Change(p, target)),
                    Err(Blocker::Elsewhere { branch, worktree }) => Some(SwapDialog::Elsewhere { dir, branch, worktree }),
                    Err(b) => Some(SwapDialog::Blocked(b)),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn run_change(&mut self, dir: PathBuf, target: Target, cx: &mut Context<Self>) {
        let out: SwapSlot = Arc::new(Mutex::new(None));
        self.run_op(Op::ChangeBranch { dir, target, id: swap::new_id(), out }, cx);
    }

    /// "Swap with main checkout" / "Swap back": check, then confirm with what travels where.
    pub(crate) fn ask_swap(&mut self, a: PathBuf, b: PathBuf, cx: &mut Context<Self>) {
        self.swap.dialog = Some(SwapDialog::Checking);
        let ledger = self.swap.ledger.clone();
        let task = cx.background_executor().spawn(async move { swap::preview_swap(&a, &b, &ledger) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                this.swap.dialog = Some(match res {
                    Ok(p) => SwapDialog::Swap(p),
                    Err(b) => SwapDialog::Blocked(b),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Swap `p` with the main checkout (a worktree card), or with whatever it is swapped with.
    pub(crate) fn ask_swap_with_main(&mut self, p: &Path, cx: &mut Context<Self>) {
        match self.swapped_with(p).or_else(|| self.main_checkout_path()) {
            Some(m) if m != p => self.ask_swap(m, p.to_path_buf(), cx),
            _ => self.toast(i18n::t!("app.swap.pick_one"), cx),
        }
    }

    fn run_swap(&mut self, p: SwapPreview, cx: &mut Context<Self>) {
        let out: SwapSlot = Arc::new(Mutex::new(None));
        self.swap.anim = Some(SwapAnim { a: p.a.clone(), b: p.b.clone(), started: Instant::now(), done: None });
        self.run_op(Op::Swap { a: p.a, b: p.b, id: swap::new_id(), ledger: self.swap.ledger.clone(), out }, cx);
        self.drive_anim(cx);
    }

    /// Redraw while the swap runs and while the labels trade places (not with reduced motion).
    fn drive_anim(&mut self, cx: &mut Context<Self>) {
        if self.agents.reduce_motion {
            return;
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                let go = this.update(cx, |this, cx| {
                    let live = this.swap.anim.as_ref().is_some_and(|a| a.done.is_none_or(|d| d.elapsed() < EXCHANGE));
                    if !live {
                        this.swap.anim = None;
                    }
                    cx.notify();
                    live
                });
                if !matches!(go, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// After a change of branch or a swap: record it, report anything kept aside.
    pub(crate) fn after_swap_op(&mut self, op: &Op, cx: &mut Context<Self>) {
        let (out, pair) = match op {
            Op::ChangeBranch { out, .. } => (out, None),
            Op::Swap { a, b, id, out, .. } => (out, Some((a.clone(), b.clone(), id.clone()))),
            _ => return,
        };
        let Some(outcome) = out.lock().unwrap().take() else { return };
        let moved_branches = matches!(outcome.result, Ok(_) | Err(SwapError::Conflict { .. }));
        if let (Some((a, b, id)), Some(p), true) = (&pair, &outcome.preview, moved_branches) {
            self.swap.ledger.after_swap(id, a, b, &p.a_branch, &p.b_branch, crate::util::now());
            if let Err(e) = self.swap.ledger.save(&ledger_file()) {
                self.toast(i18n::t!("app.swap.save_failed", error = e), cx);
            }
        }
        let reduce = self.agents.reduce_motion;
        if let Some(anim) = self.swap.anim.as_mut() {
            if moved_branches && !reduce {
                anim.done = Some(Instant::now());
            } else {
                self.swap.anim = None;
            }
        }
        let back = outcome.preview.as_ref().is_some_and(|p| p.back);
        match outcome.result {
            Ok(moved) => {
                let mut msg = match (&pair, &outcome.preview) {
                    (Some(_), Some(p)) if back => i18n::t!("app.swap.done_back", a = p.a_branch, b = p.b_branch),
                    (Some((a, b, _)), Some(p)) => i18n::t!("app.swap.done", a = self.checkout_name(a), a_branch = p.b_branch, b = self.checkout_name(b), b_branch = p.a_branch),
                    _ => i18n::t!("app.swap.done_change"),
                };
                if moved.carried != (0, 0) {
                    msg.push(' ');
                    msg.push_str(&i18n::t!("app.swap.done_carried"));
                }
                if let Some(h) = moved.hook_warnings.first() {
                    msg.push(' ');
                    msg.push_str(&i18n::t!("app.swap.done_hook", hook = h));
                }
                self.toast(msg, cx);
            }
            Err(SwapError::Blocked(b)) => self.swap.dialog = Some(SwapDialog::Blocked(b)),
            Err(SwapError::RolledBack { step, output }) => self.swap.dialog = Some(SwapDialog::Outcome { title: i18n::t!("app.swap.rolled_back_title"), body: i18n::t!("app.swap.rolled_back", step = step, output = output.trim()), kept: Vec::new() }),
            Err(SwapError::RollbackFailed { step, output, rollback, kept }) => self.swap.dialog = Some(SwapDialog::Outcome { title: i18n::t!("app.swap.halfway_title"), body: i18n::t!("app.swap.halfway", step = step, undo = rollback.join("\n"), output = output.trim()), kept }),
            Err(SwapError::Conflict { kept, output }) => self.swap.dialog = Some(SwapDialog::Outcome { title: i18n::t!("app.swap.conflict_title"), body: i18n::t!("app.swap.conflict", output = output.trim()), kept }),
        }
        cx.notify();
    }

    /// Edge pulse strength (0–1) for the card at `p` while a swap runs, and how far the label
    /// exchange is (0–1) once it finished.
    pub(crate) fn swap_motion(&self, p: &Path) -> (Option<f32>, Option<f32>) {
        let Some(a) = self.swap.anim.as_ref().filter(|a| a.a == p || a.b == p) else { return (None, None) };
        match a.done {
            None if self.agents.reduce_motion => (Some(1.), None),
            None => {
                let s = a.started.elapsed().as_secs_f32() / PULSE * std::f32::consts::TAU;
                (Some(0.5 + 0.5 * s.sin()), None)
            }
            Some(d) => (None, Some((d.elapsed().as_secs_f32() / EXCHANGE.as_secs_f32()).min(1.))),
        }
    }

    pub(crate) fn swap_running(&self, p: &Path) -> bool {
        self.swap.anim.as_ref().is_some_and(|a| a.done.is_none() && (a.a == p || a.b == p))
    }

    pub(crate) fn render_swap_dialog(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.swap.dialog.as_ref()?;
        let close = |this: &mut RamaApp, cx: &mut Context<RamaApp>| {
            this.swap.dialog = None;
            cx.notify();
        };
        let title = |s: String| div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(s);
        let para = |s: String| div().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).whitespace_normal().child(s);
        let mono = |s: String| div().font_family(MONO).text_size(px(12.)).text_color(t.ink_muted).whitespace_normal().child(s);
        let primary = |id: &'static str, label: String| ui::filled_button(t, id, t.accent).h(px(30.)).child(label);
        let secondary = |id: &'static str, label: String| ui::outline_button(t, id).h(px(30.)).child(label);
        let ignored = i18n::t!("app.swap.ignored_stay");
        let cancel = secondary("swap-cancel", i18n::t!("app.confirm.cancel")).on_click(cx.listener(move |this, _, _, cx| close(this, cx)));
        let mut body = div().flex().flex_col().gap(px(10.)).p(px(18.));
        let mut buttons = div().flex().flex_wrap().gap(px(8.));
        match d {
            SwapDialog::Checking => {
                body = body.child(div().flex().items_center().gap(px(8.)).child(gpui_component::spinner::Spinner::new().small().color(t.accent)).child(para(i18n::t!("app.swap.checking"))));
                buttons = buttons.child(cancel);
            }
            SwapDialog::Change(pv, target) => {
                let from = pv.from.clone().unwrap_or_else(|| i18n::t!("app.swap.detached_head"));
                let name = self.checkout_name(&pv.path);
                body = body
                    .child(title(i18n::t!("app.swap.change_title", name = name, branch = pv.to)))
                    .child(para(i18n::t!("app.swap.change_body", name = name, files = files(pv.changed), from = from, branch = pv.to)))
                    .child(para(ignored))
                    .child(para(i18n::t!("app.swap.change_conflict")));
                let (dir, tg) = (pv.path.clone(), target.clone());
                buttons = buttons
                    .child(primary("swap-change-go", i18n::t!("app.swap.change_go", branch = pv.to)).on_click(cx.listener(move |this, _, _, cx| {
                        this.swap.dialog = None;
                        this.run_change(dir.clone(), tg.clone(), cx);
                    })))
                    .child(cancel);
            }
            SwapDialog::Swap(pv) => {
                let (an, bn) = (self.checkout_name(&pv.a), self.checkout_name(&pv.b));
                let head = if pv.back { i18n::t!("app.swap.back_title", a = an, b = bn) } else { i18n::t!("app.swap.swap_title", a = an, b = bn) };
                let what = if pv.back { i18n::t!("app.swap.back_body", a = an, a_branch = pv.b_branch, b = bn, b_branch = pv.a_branch) } else { i18n::t!("app.swap.swap_body", a = an, a_branch = pv.b_branch, b = bn, b_branch = pv.a_branch) };
                let changes = match (pv.a_changed, pv.b_changed) {
                    (0, 0) => i18n::t!("app.swap.no_changes"),
                    (x, y) => i18n::t!("app.swap.travel", a = an, a_files = files(x), b = bn, b_files = files(y)),
                };
                body = body.child(title(head)).child(para(what)).child(para(changes)).child(para(ignored)).child(mono(format!("{}  {} ⇄ {}  {}", folder(&pv.a), pv.a_branch, pv.b_branch, folder(&pv.b)))); // i18n-ok: names and arrows
                let pv2 = pv.clone();
                buttons = buttons
                    .child(primary("swap-go", if pv.back { i18n::t!("app.swap.go_back") } else { i18n::t!("app.swap.go") }).on_click(cx.listener(move |this, _, _, cx| {
                        this.swap.dialog = None;
                        this.run_swap(pv2.clone(), cx);
                    })))
                    .child(cancel);
            }
            SwapDialog::Elsewhere { dir, branch, worktree } => {
                body = body.child(title(i18n::t!("app.swap.elsewhere_title", branch = branch))).child(mono(worktree.display().to_string())).child(para(i18n::t!("app.swap.elsewhere_offer", name = self.checkout_name(dir))));
                let (a, b, w) = (dir.clone(), worktree.clone(), worktree.clone());
                buttons = buttons
                    .child(primary("swap-elsewhere", i18n::t!("app.swap.elsewhere_go")).on_click(cx.listener(move |this, _, _, cx| this.ask_swap(a.clone(), b.clone(), cx))))
                    .child(secondary("swap-open-wt", i18n::t!("app.swap.elsewhere_open")).on_click(cx.listener(move |this, _, _, cx| {
                        this.swap.dialog = None;
                        this.open_in_new_window(w.clone(), cx);
                    })))
                    .child(cancel);
            }
            SwapDialog::Blocked(b) => {
                let (h, text) = blocker_text(b);
                body = body.child(title(h)).child(para(text));
                buttons = buttons.child(secondary("swap-ok", i18n::t!("app.swap.ok")).on_click(cx.listener(move |this, _, _, cx| close(this, cx))));
            }
            SwapDialog::Outcome { title: h, body: text, kept } => {
                body = body.child(title(h.clone())).child(para(text.clone()));
                if !kept.is_empty() {
                    body = body.child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink_muted).child(i18n::t!("app.swap.kept"))).child(mono(kept_lines(kept)));
                }
                buttons = buttons.child(secondary("swap-ok", i18n::t!("app.swap.ok")).on_click(cx.listener(move |this, _, _, cx| close(this, cx))));
            }
        }
        let card = div().id("swap-card").w(px(500.)).max_w(relative(0.94)).max_h(relative(0.85)).overflow_y_scroll().flex().flex_col().rounded(px(14.)).border_1().border_color(t.border).bg(t.surface).shadow_lg().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(body.child(buttons));
        Some(backdrop(t, "swap-backdrop").on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx))).child(card).into_any_element())
    }
}
