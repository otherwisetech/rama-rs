//! The agent badge: a tiny pixel-art robot typing on a keyboard, painted cell by cell in a
//! canvas (no image assets) with the theme's colours.
use crate::agents::AgentState;
use crate::theme::Tokens;
use gpui::*;

/// Frames per second while working.
pub const FPS: u64 = 6;
pub const FRAME_MS: u64 = 1000 / FPS;
const W: usize = 9;
const H: usize = 9;

// a antenna · b body · e eye · h hand · k key · l lit key · q "!" · x mark (error face)
const TYPE_L: [&str; H] = ["....a....", "....b....", ".bbbbbbb.", ".bebbbeb.", ".bbbbbbb.", "...bbb...", ".hbbbbb..", ".......h.", "kkkkkkklk"];
const TYPE_R: [&str; H] = ["....a....", "....b....", ".bbbbbbb.", ".bebbbeb.", ".bbbbbbb.", "...bbb...", "..bbbbbh.", ".h.......", "klkkkkkkk"];
const TYPE_R_BLINK: [&str; H] = ["....a....", "....b....", ".bbbbbbb.", ".bbbbbbb.", ".bbbbbbb.", "...bbb...", "..bbbbbh.", ".h.......", "klkkkkkkk"];
const REST: [&str; H] = ["....a....", "....b....", ".bbbbbbb.", ".bebbbeb.", ".bbbbbbb.", "...bbb...", "..bbbbb..", ".h.....h.", "kkkkkkkkk"];
const WORK: [&[&str; H]; 4] = [&TYPE_L, &TYPE_R, &TYPE_L, &TYPE_R_BLINK];
// a raised hand, waving, with an "!" beside it
const RAISE_UP: [&str; H] = ["q...a..h.", "q...b..h.", "qbbbbbbb.", ".bebbbeb.", "qbbbbbbb.", "...bbb...", ".hbbbbb..", ".........", "kkkkkkkkk"];
const RAISE_OUT: [&str; H] = ["q...a...h", "q...b..h.", "qbbbbbbb.", ".bebbbeb.", "qbbbbbbb.", "...bbb...", ".hbbbbb..", ".........", "kkkkkkkkk"];
const RAISE: [&[&str; H]; 2] = [&RAISE_UP, &RAISE_OUT];
const BROKEN: [&str; H] = ["....a....", "....b....", ".bbbbbbb.", ".bxbbbxb.", ".bbxxxbb.", "...bbb...", "..bbbbb..", ".h.....h.", "kkkkkkkkk"];

/// Which sprite frame to show `ms` after start: the typing loop while working, a waving hand
/// while it needs you (both still when motion is reduced), a broken face on error, else the
/// resting pose.
pub fn frame_at(ms: u64, state: AgentState, reduce_motion: bool) -> &'static [&'static str; H] {
    let frame = ms / FRAME_MS;
    match state {
        AgentState::Working if reduce_motion => &TYPE_L,
        AgentState::Working => WORK[(frame % WORK.len() as u64) as usize],
        AgentState::NeedsYou if reduce_motion => &RAISE_UP,
        // a slower wave: two frames each
        AgentState::NeedsYou => RAISE[((frame / 2) % RAISE.len() as u64) as usize],
        AgentState::Error => &BROKEN,
        AgentState::Idle | AgentState::Waiting => &REST,
    }
}

/// States whose sprite moves.
pub fn animated(state: AgentState) -> bool {
    matches!(state, AgentState::Working | AgentState::NeedsYou)
}

/// The robot at `cell` pixels per sprite pixel.
pub fn robot(t: &Tokens, state: AgentState, ms: u64, reduce_motion: bool, cell: f32) -> Canvas<()> {
    let frame = frame_at(ms, state, reduce_motion);
    let body = match state {
        AgentState::Idle => t.ink_muted,
        AgentState::Error => t.danger,
        _ => t.accent,
    };
    let eye = t.surface;
    let antenna = match state {
        AgentState::Working => t.added,
        AgentState::Waiting | AgentState::NeedsYou => t.tag,
        AgentState::Error => t.danger,
        AgentState::Idle => t.border_strong,
    };
    let (key, lit, hand, alert) = (t.border_strong, t.added, t.ink_muted, t.tag);
    let color = move |c: u8| match c {
        b'a' => Some(antenna),
        b'b' => Some(body),
        b'e' => Some(eye),
        b'h' => Some(hand),
        b'k' => Some(key),
        b'l' => Some(lit),
        b'q' => Some(alert),
        b'x' => Some(eye),
        _ => None,
    };
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            for (y, row) in frame.iter().enumerate() {
                for (x, c) in row.bytes().enumerate() {
                    if let Some(col) = color(c) {
                        let o = point(b.origin.x + px(x as f32 * cell), b.origin.y + px(y as f32 * cell));
                        window.paint_quad(fill(Bounds::new(o, size(px(cell), px(cell))), col));
                    }
                }
            }
        },
    )
    .w(px(W as f32 * cell))
    .h(px(H as f32 * cell))
    .flex_none()
}

#[cfg(test)]
mod tests {
    // not `super::*`: gpui's `test` macro would shadow the built-in one
    use super::{AgentState, BROKEN, FRAME_MS, RAISE_OUT, RAISE_UP, REST, TYPE_L, TYPE_R, TYPE_R_BLINK, W, WORK, animated, frame_at};

    #[test]
    fn frames() {
        for f in WORK.iter().copied().chain([&REST, &RAISE_UP, &RAISE_OUT, &BROKEN]) {
            assert!(f.iter().all(|r| r.len() == W), "every row is {W} wide");
        }
        let w = AgentState::Working;
        assert_eq!(frame_at(0, w, false), &TYPE_L);
        assert_eq!(frame_at(FRAME_MS - 1, w, false), &TYPE_L);
        assert_eq!(frame_at(FRAME_MS, w, false), &TYPE_R);
        assert_eq!(frame_at(3 * FRAME_MS, w, false), &TYPE_R_BLINK);
        assert_eq!(frame_at(4 * FRAME_MS, w, false), &TYPE_L, "loops every 4 frames");
        // about a second for the whole loop at ~6 fps
        assert!((600..=800).contains(&(FRAME_MS * 4)));
        for ms in [0, FRAME_MS, 5 * FRAME_MS] {
            assert_eq!(frame_at(ms, w, true), &TYPE_L, "reduce motion: still");
            assert_eq!(frame_at(ms, AgentState::Waiting, false), &REST);
            assert_eq!(frame_at(ms, AgentState::Idle, false), &REST);
            assert_eq!(frame_at(ms, AgentState::Error, false), &BROKEN);
            assert_eq!(frame_at(ms, AgentState::NeedsYou, true), &RAISE_UP, "reduce motion: the hand stays up");
        }
        let n = AgentState::NeedsYou;
        assert_eq!(frame_at(0, n, false), &RAISE_UP);
        assert_eq!(frame_at(FRAME_MS, n, false), &RAISE_UP);
        assert_eq!(frame_at(2 * FRAME_MS, n, false), &RAISE_OUT);
        assert_eq!(frame_at(4 * FRAME_MS, n, false), &RAISE_UP);
        assert!(animated(n) && animated(w) && !animated(AgentState::Error) && !animated(AgentState::Waiting));
    }
}
