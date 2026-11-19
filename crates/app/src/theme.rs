//! Design tokens (same family as genealogy-rs and diskmap) plus the branch lane palette.
use gpui::{App, Hsla, rgb};
use gpui_component::ActiveTheme;

#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub canvas: Hsla,
    pub surface: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub ink: Hsla,
    pub ink_muted: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    pub danger: Hsla,
    pub added_bg: Hsla,
    pub removed_bg: Hsla,
    pub added: Hsla,
    pub removed: Hsla,
    pub tag: Hsla,
    /// logo badge background (the icon's violet)
    pub brand: Hsla,
    pub dark: bool,
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn a(hex: u32, alpha: f32) -> Hsla {
    let mut h = c(hex);
    h.a = alpha;
    h
}

impl Tokens {
    pub fn light() -> Self {
        Tokens {
            canvas: c(0xF2F1EE),
            surface: c(0xFFFFFF),
            border: c(0xE2DFD8),
            border_strong: c(0xC9C5BB),
            ink: c(0x1C1B19),
            ink_muted: c(0x6B675F),
            accent: c(0x5B4B9A),
            accent_soft: c(0xECE8F7),
            danger: c(0xA3382A),
            added_bg: a(0x2E9E5B, 0.12),
            removed_bg: a(0xD0503F, 0.12),
            added: c(0x1F7A44),
            removed: c(0xB03A2C),
            tag: c(0xA86A12),
            brand: c(0x5B4B9A),
            dark: false,
        }
    }

    pub fn dark() -> Self {
        Tokens {
            canvas: c(0x141413),
            surface: c(0x1C1B1A),
            border: c(0x302E2B),
            border_strong: c(0x4A4741),
            ink: c(0xF2F0EA),
            ink_muted: c(0xA19C93),
            accent: c(0x9D8CE0),
            accent_soft: a(0x7B68C8, 0.22),
            danger: c(0xD0604F),
            added_bg: a(0x3FBF74, 0.14),
            removed_bg: a(0xE5675A, 0.14),
            added: c(0x6FD39A),
            removed: c(0xF08A7E),
            tag: c(0xE0B04D),
            brand: c(0x6B5AAE),
            dark: true,
        }
    }

    pub fn on_fill(&self) -> Hsla {
        if self.dark { c(0x141413) } else { c(0xFFFFFF) }
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
    if cx.theme().mode.is_dark() { Tokens::dark() } else { Tokens::light() }
}

pub const SANS: &str = "Inter";
#[cfg(target_os = "macos")]
pub const MONO: &str = "Menlo";
#[cfg(target_os = "windows")]
pub const MONO: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const MONO: &str = "DejaVu Sans Mono";
