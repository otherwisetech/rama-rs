//! Unified diff of one file, replacing the graph until closed (Esc).
use crate::app::RamaApp;
use crate::detail::{status_color, status_letter};
use crate::theme::{MONO, Tokens, tokens};
use crate::ui::{self, icon};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::IconName;
use repo::{LineKind, Status};

const LINE_H: f32 = 20.;
const NO_W: f32 = 48.;

impl RamaApp {
    pub(crate) fn render_diff(&mut self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(d) = &self.diff else { return div().into_any_element() };
        let f = &d.file;
        let sc = status_color(t, &f.status);
        let renamed = match &f.status {
            Status::Renamed { from } | Status::Copied { from } => Some(i18n::t!("app.diff.from", path = from)),
            _ => None,
        };
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(44.))
            .px(px(10.))
            .border_b_1()
            .border_color(t.border)
            .child(ui::icon_button(t, "close-diff", IconName::ArrowLeft, true).on_click(cx.listener(|this, _, _, cx| {
                this.diff = None;
                cx.notify();
            })))
            .child(div().flex().flex_none().items_center().justify_center().size(px(18.)).rounded(px(4.)).bg(sc.opacity(0.16)).text_color(sc).text_size(px(10.)).font_weight(FontWeight::BOLD).child(status_letter(&f.status)))
            .child(div().flex().flex_col().min_w_0().flex_1().child(div().font_family(MONO).text_size(px(13.)).truncate().child(f.path.clone())).children(renamed.map(|r| div().text_size(px(11.)).text_color(t.ink_muted).truncate().child(r))))
            .child(div().flex_none().text_size(px(12.)).text_color(t.added).child(format!("+{}", f.added)))
            .child(div().flex_none().text_size(px(12.)).text_color(t.removed).child(format!("−{}", f.removed)))
            .child(div().flex_none().text_size(px(11.)).text_color(t.ink_muted).child(i18n::t!("app.diff.esc")));
        let body: AnyElement = match &d.lines {
            None => note(t, i18n::t!("app.detail.loading")),
            Some(Err(e)) => note(t, e.clone()),
            Some(Ok(lines)) if lines.is_empty() => note(t, if f.binary { i18n::t!("app.diff.binary") } else { i18n::t!("app.diff.empty") }),
            Some(Ok(lines)) => {
                let lines = lines.clone();
                uniform_list(
                    "diff",
                    lines.len(),
                    cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                        let t = tokens(cx);
                        range
                            .map(|i| {
                                let l = &lines[i];
                                let (bg, fg, sign) = match l.kind {
                                    LineKind::Added => (Some(t.added_bg), t.ink, "+"),
                                    LineKind::Removed => (Some(t.removed_bg), t.ink, "-"),
                                    LineKind::Context => (None, t.ink, " "),
                                    LineKind::Hunk => (Some(t.accent_soft), t.accent, ""),
                                };
                                let no = |n: Option<u32>| div().flex_none().w(px(NO_W)).pr(px(8.)).text_right().text_color(t.ink_muted.opacity(0.7)).child(n.map(|n| n.to_string()).unwrap_or_default());
                                div()
                                    .id(("line", i))
                                    .flex()
                                    .h(px(LINE_H))
                                    .items_center()
                                    .font_family(MONO)
                                    .text_size(px(12.))
                                    .when_some(bg, |d, bg| d.bg(bg))
                                    .child(no(l.old_no))
                                    .child(no(l.new_no))
                                    .child(div().flex_none().w(px(14.)).text_color(fg.opacity(0.6)).child(sign))
                                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(fg).child(l.text.replace('\t', "    ")))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.diff_scroll)
                .flex_1()
                .into_any_element()
            }
        };
        div().flex().flex_col().size_full().bg(t.surface).child(header).child(body).into_any_element()
    }
}

fn note(t: &Tokens, text: String) -> AnyElement {
    div().flex_1().flex().items_center().justify_center().gap(px(8.)).text_size(px(13.)).text_color(t.ink_muted).child(icon(IconName::Info, 14., t.ink_muted)).child(text).into_any_element()
}
