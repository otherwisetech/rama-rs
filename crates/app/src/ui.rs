//! Small styled building blocks shared by the top bar, panel and screens.
use crate::theme::Tokens;
use gpui::*;
use gpui_component::IconName;
use otherwise_ui::tokens::icon as icon_size;
pub use otherwise_ui::{filled_button, icon, outline_button, section_title};

/// The family's square icon-only button with one of gpui-component's icons.
pub fn icon_button(t: &Tokens, id: impl Into<ElementId>, name: IconName, enabled: bool) -> Stateful<Div> {
    otherwise_ui::icon_button(t, id, enabled).child(icon(name, icon_size::TOOLBAR, if enabled { t.ink } else { t.ink_faint }))
}

/// Branch glyph (the app's own mark).
pub fn branch_icon(size: f32, color: Hsla) -> Svg {
    svg().path(crate::LOGO_SVG).flex_none().size(px(size)).text_color(color)
}

