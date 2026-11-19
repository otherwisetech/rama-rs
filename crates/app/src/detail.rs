//! Right panel: the selected commit's message, author, ids, refs and changed files.
use crate::app::RamaApp;
use crate::graph_view::ref_pill;
use crate::theme::{MONO, Tokens};
use crate::ui;
use crate::util::{datetime, initials};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::IconName;
use repo::{FileChange, RefKind, Status};

const DETAIL_W: f32 = 380.;

pub fn status_letter(s: &Status) -> &'static str {
    match s {
        Status::Added => "A",
        Status::Deleted => "D",
        Status::Modified => "M",
        Status::Renamed { .. } => "R",
        Status::Copied { .. } => "C",
    }
}

pub fn status_color(t: &Tokens, s: &Status) -> Hsla {
    match s {
        Status::Added => t.added,
        Status::Deleted => t.removed,
        Status::Modified => t.tag,
        Status::Renamed { .. } | Status::Copied { .. } => t.accent,
    }
}

impl RamaApp {
    pub(crate) fn render_detail(&mut self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = div().flex().flex_col().flex_none().w(px(DETAIL_W)).h_full().bg(t.surface).border_l_1().border_color(t.border);
        let (Some(h), Some(row), Some(d)) = (self.history.clone(), self.selected, self.details.as_ref()) else {
            return panel.child(div().p(px(20.)).text_size(px(13.)).text_color(t.ink_muted).child(i18n::t!("app.detail.none")));
        };
        let c = &h.commits[row];
        let lane = t.lane(h.rows[row].color);
        let message = d.message.clone().unwrap_or_else(|| c.summary.clone());
        let (title, body) = match message.split_once('\n') {
            Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
            None => (message.trim().to_string(), String::new()),
        };
        let refs: Vec<_> = self.refs_by_row.get(&row).map(|v| v.iter().map(|&i| h.refs[i].clone()).collect()).unwrap_or_default();

        let parents = c.parents.iter().enumerate().map(|(i, &p)| {
            let short = p.to_hex_with_len(7).to_string();
            let accent = t.accent;
            div().id(("parent", i)).font_family(MONO).text_size(px(12.)).text_color(accent).cursor_pointer().hover(|s| s.underline()).child(short).on_click(cx.listener(move |this, _, _, cx| this.select_id(p, cx)))
        });

        let header = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(16.))
            .border_b_1()
            .border_color(t.border)
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).line_height(px(21.)).child(title))
            .when(!body.is_empty(), |el| el.child(div().text_size(px(13.)).line_height(px(19.)).text_color(t.ink_muted).child(body)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex().flex_none().items_center().justify_center().size(px(32.)).rounded_full().bg(lane).text_color(gpui::white()).text_size(px(12.)).font_weight(FontWeight::BOLD).child(initials(&c.author)))
                    .child(div().flex().flex_col().min_w_0().child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).truncate().child(c.author.clone())).child(div().text_size(px(12.)).text_color(t.ink_muted).truncate().child(c.email.clone()))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .child(div().flex().gap(px(8.)).child(div().w(px(70.)).text_color(t.ink_muted).child(i18n::t!("app.detail.date"))).child(datetime(c.time, c.offset)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().w(px(70.)).text_color(t.ink_muted).child(i18n::t!("app.detail.commit")))
                            .child(div().font_family(MONO).child(c.id.to_hex_with_len(12).to_string()))
                            .child(ui::icon_button(t, "copy", IconName::Copy, true).size(px(24.)).on_click(cx.listener(|this, _, _, cx| this.copy_sha(cx)))),
                    )
                    .when(!c.parents.is_empty(), |el| el.child(div().flex().gap(px(8.)).child(div().w(px(70.)).text_color(t.ink_muted).child(i18n::tn!("app.detail.parents", c.parents.len()))).children(parents))),
            )
            .when(!refs.is_empty(), |el| {
                el.child(div().flex().flex_wrap().gap(px(5.)).children(refs.iter().map(|r| ref_pill(t, r, r.kind == RefKind::Local && h.head_branch.as_deref() == Some(r.name.as_str()), lane))))
            });

        let files: AnyElement = match &d.changes {
            None => div().p(px(16.)).text_size(px(12.)).text_color(t.ink_muted).child(i18n::t!("app.detail.loading")).into_any_element(),
            Some(Err(e)) => div().p(px(16.)).text_size(px(12.)).text_color(t.danger).child(e.clone()).into_any_element(),
            Some(Ok(list)) => {
                let (adds, dels) = list.iter().fold((0u32, 0u32), |(a, r), f| (a + f.added, r + f.removed));
                let open_path = self.diff.as_ref().map(|d| d.file.path.clone());
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(16.))
                            .pt(px(14.))
                            .pb(px(6.))
                            .child(div().flex_1().child(ui::section_title(t, i18n::tn!("app.detail.files", list.len()))))
                            .child(div().text_size(px(11.)).text_color(t.added).child(format!("+{adds}")))
                            .child(div().text_size(px(11.)).text_color(t.removed).child(format!("−{dels}"))),
                    )
                    .child(div().id("files").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().pb(px(10.)).children(list.iter().enumerate().map(|(i, f)| file_row(t, i, f, open_path.as_deref() == Some(f.path.as_str()), cx))))
                    .into_any_element()
            }
        };
        panel.child(header).child(files)
    }
}

fn file_row(t: &Tokens, i: usize, f: &FileChange, open: bool, cx: &mut Context<RamaApp>) -> AnyElement {
    let (dir, name) = match f.path.rsplit_once('/') {
        Some((d, n)) => (format!("{d}/"), n.to_string()),
        None => (String::new(), f.path.clone()),
    };
    let hover = if t.dark { t.border } else { t.canvas };
    let sc = status_color(t, &f.status);
    let file = f.clone();
    div()
        .id(("file", i))
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(28.))
        .px(px(16.))
        .cursor_pointer()
        .when(open, |d| d.bg(t.accent_soft))
        .when(!open, |d| d.hover(move |s| s.bg(hover)))
        .child(div().flex().flex_none().items_center().justify_center().size(px(16.)).rounded(px(4.)).bg(sc.opacity(0.16)).text_color(sc).text_size(px(10.)).font_weight(FontWeight::BOLD).child(status_letter(&f.status)))
        .child(div().flex().flex_1().min_w_0().text_size(px(12.5)).child(div().flex_none().text_color(t.ink).child(name)).child(div().min_w_0().truncate().pl(px(6.)).text_color(t.ink_muted).child(dir)))
        .when(f.binary, |d| d.child(div().flex_none().text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.detail.binary"))))
        .when(!f.binary, |d| d.child(div().flex_none().flex().gap(px(6.)).text_size(px(11.)).child(div().text_color(t.added).child(format!("+{}", f.added))).child(div().text_color(t.removed).child(format!("−{}", f.removed)))))
        .on_click(cx.listener(move |this, _, _, cx| this.open_diff(file.clone(), cx)))
        .into_any_element()
}

