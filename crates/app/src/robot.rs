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

// a antenna · b body · e eye · h hand · k key · l lit key
const TYPE_L: [&str; H] = ["....a....", "....b....", ".bbbbbbb.", ".bebbbeb.", ".bbbbbbb.", "...bbb...", ".hbbbbb..", ".......h.", "kkkkkkklk"];
const TYPE_R: [&str; H] = ["....a....", "....b....", ".bbbbbbb.", ".bebbbeb.", ".bbbbbbb.", "...bbb...", "..bbbbbh.", ".h.......", "klkkkkkkk"];
const TYPE_R_BLINK: [&str; H] = ["....a....", "....b....", ".bbbbbbb.", ".bbbbbbb.", ".bbbbbbb.", "...bbb...", "..bbbbbh.", ".h.......", "klkkkkkkk"];
const REST: [&str; H] = ["....a....", "....b....", ".bbbbbbb.", ".bebbbeb.", ".bbbbbbb.", "...bbb...", "..bbbbb..", ".h.....h.", "kkkkkkkkk"];
const WORK: [&[&str; H]; 4] = [&TYPE_L, &TYPE_R, &TYPE_L, &TYPE_R_BLINK];

/// Which sprite frame to show `ms` after start: the typing loop while working (unless motion
/// is reduced), else the resting pose.
pub fn frame_at(ms: u64, state: AgentState, reduce_motion: bool) -> &'static [&'static str; H] {
    if state != AgentState::Working {
        return &REST;
    }
    if reduce_motion {
        return &TYPE_L;
    }
    WORK[((ms / FRAME_MS) % WORK.len() as u64) as usize]
}

/// The robot at `cell` pixels per sprite pixel.
pub fn robot(t: &Tokens, state: AgentState, ms: u64, reduce_motion: bool, cell: f32) -> Canvas<()> {
    let frame = frame_at(ms, state, reduce_motion);
    let dim = state == AgentState::Idle;
    let body = if dim { t.ink_muted } else { t.accent };
    let eye = t.surface;
    let antenna = match state {
        AgentState::Working => t.added,
        AgentState::Waiting => t.tag,
        AgentState::Idle => t.border_strong,
    };
    let (key, lit, hand) = (t.border_strong, t.added, t.ink_muted);
    let color = move |c: u8| match c {
        b'a' => Some(antenna),
        b'b' => Some(body),
        b'e' => Some(eye),
        b'h' => Some(hand),
        b'k' => Some(key),
        b'l' => Some(lit),
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
    use super::{AgentState, FRAME_MS, REST, TYPE_L, TYPE_R, TYPE_R_BLINK, W, WORK, frame_at};

    #[test]
    fn frames() {
        for f in WORK.iter().copied().chain([&REST]) {
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
        }
    }
}
