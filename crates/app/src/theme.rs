//! The family's tokens come from otherwise-ui (github.com/otherwisetech/otherwise-ui, by tag):
//! Inter, the type scale, the warm neutrals and status colours, with Rama's violet accent
//! (`otherwise_ui::accents::RAMA`, set in main.rs). `Tokens` derefs to `otherwise_ui::Theme`
//! (so `&Tokens` passes wherever the crate wants `&Theme`) and adds the content's own colours:
//! the diff's added / removed lines, tags and the branch lane palette.
use gpui::{App, Hsla, Window};
use otherwise_ui::theme::{hex as c, hex_a as a};
pub use otherwise_ui::tokens::{MONO, SANS};

#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub ui: otherwise_ui::Theme,
    pub added_bg: Hsla,
    pub removed_bg: Hsla,
    pub added: Hsla,
    pub removed: Hsla,
    /// tags and the "swapped" badge (the family's warn amber)
    pub tag: Hsla,
}

impl std::ops::Deref for Tokens {
    type Target = otherwise_ui::Theme;
    fn deref(&self) -> &otherwise_ui::Theme {
        &self.ui
    }
}

impl Tokens {
    pub fn new(ui: otherwise_ui::Theme) -> Self {
        if ui.dark {
            Tokens { ui, added_bg: a(0x3FBF74, 0.14), removed_bg: a(0xE5675A, 0.14), added: c(0x6FD39A), removed: c(0xF08A7E), tag: ui.warn }
        } else {
            Tokens { ui, added_bg: a(0x2E9E5B, 0.12), removed_bg: a(0xD0503F, 0.12), added: c(0x1F7A44), removed: c(0xB03A2C), tag: ui.warn }
        }
    }

    /// Colour of a branch lane.
    pub fn lane(&self, i: u16) -> Hsla {
        const LANES: [u32; 12] = [0x2F9BEA, 0xE5484D, 0x24B3A0, 0xF0A020, 0x8E5BE0, 0x46A758, 0xE5579B, 0xF07C22, 0x3BC4E8, 0x94B83D, 0xD9654B, 0x7383DE];
        let mut h = c(LANES[i as usize % LANES.len()]);
        if !self.dark {
            h.l *= 0.92;
        }
        h
    }
}

pub fn tokens(cx: &App) -> Tokens {
    Tokens::new(otherwise_ui::theme(cx))
}

/// Follows the system's light / dark appearance and sets the family font on gpui-component.
pub fn apply(window: Option<&mut Window>, cx: &mut App) {
    otherwise_ui::apply(otherwise_ui::Appearance::System, window, cx);
}
