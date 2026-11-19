//! Commit graph layout, independent of any git library.
//!
//! Commits are plain indices. [`topo_order`] orders them newest first with every child before its
//! parents (like `git log --date-order`); [`layout`] then assigns each row a lane and the edges to
//! draw in its upper half (into the commit dot) and lower half (out towards its parents).
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// One commit as the layout sees it: parents as indices into the same slice (only loaded ones).
#[derive(Debug, Clone)]
pub struct Node {
    pub parents: Vec<usize>,
    /// Committer time, seconds since the epoch; breaks ties between independent branches.
    pub time: i64,
}

/// Indices of `nodes`, children before parents, newer first where history allows.
pub fn topo_order(nodes: &[Node]) -> Vec<usize> {
    let mut children = vec![0u32; nodes.len()];
    for n in nodes {
        for &p in &n.parents {
            children[p] += 1;
        }
    }
    #[derive(PartialEq, Eq)]
    struct Item(i64, std::cmp::Reverse<usize>);
    impl Ord for Item {
        fn cmp(&self, o: &Self) -> Ordering {
            self.0.cmp(&o.0).then(self.1.cmp(&o.1))
        }
    }
    impl PartialOrd for Item {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    let mut ready: BinaryHeap<Item> = (0..nodes.len()).filter(|&i| children[i] == 0).map(|i| Item(nodes[i].time, std::cmp::Reverse(i))).collect();
    let mut out = Vec::with_capacity(nodes.len());
    while let Some(Item(_, std::cmp::Reverse(i))) = ready.pop() {
        out.push(i);
        for &p in &nodes[i].parents {
            children[p] -= 1;
            if children[p] == 0 {
                ready.push(Item(nodes[p].time, std::cmp::Reverse(p)));
            }
        }
    }
    // a cycle cannot happen in git; if input is broken, keep the leftovers rather than drop them
    if out.len() < nodes.len() {
        let mut seen = vec![false; nodes.len()];
        out.iter().for_each(|&i| seen[i] = true);
        out.extend((0..nodes.len()).filter(|&i| !seen[i]));
    }
    out
}

/// A line inside one row, between two lanes. In [`Row::up`] it runs from the row's top edge
/// (`from`) to the commit's height (`to`); in [`Row::down`] from the commit's height to the
/// bottom edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: u16,
    pub to: u16,
    /// Palette index of the branch the line belongs to.
    pub color: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub lane: u16,
    pub color: u16,
    pub up: Vec<Edge>,
    pub down: Vec<Edge>,
    /// Lanes in use in this row (for sizing the graph column).
    pub width: u16,
}

/// Lanes for commits already in display order: `parents[r]` are the display rows of row `r`'s
/// parents (first parent first), each greater than `r`.
pub fn layout(parents: &[Vec<usize>]) -> Vec<Row> {
    // lanes[j] = (row this lane is heading to, colour)
    let mut lanes: Vec<Option<(usize, u16)>> = Vec::new();
    let mut next_color: u16 = 0;
    let mut new_color = || {
        let c = next_color;
        next_color = next_color.wrapping_add(1);
        c
    };
    fn free_slot(lanes: &mut Vec<Option<(usize, u16)>>) -> usize {
        match lanes.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                lanes.push(None);
                lanes.len() - 1
            }
        }
    }
    let mut rows = Vec::with_capacity(parents.len());
    for (r, ps) in parents.iter().enumerate() {
        let hits: Vec<usize> = (0..lanes.len()).filter(|&j| matches!(lanes[j], Some((t, _)) if t == r)).collect();
        let (lane, color) = match hits.first() {
            Some(&j) => (j, lanes[j].unwrap().1),
            None => {
                let j = free_slot(&mut lanes);
                (j, new_color())
            }
        };
        let mut up = Vec::new();
        for (j, l) in lanes.iter().enumerate() {
            if let Some((t, c)) = *l {
                up.push(Edge { from: j as u16, to: if t == r { lane as u16 } else { j as u16 }, color: c });
            }
        }
        let width_before = lanes.len();
        for &j in &hits {
            lanes[j] = None;
        }

        let mut down = Vec::new();
        let mut fresh = vec![lane];
        if let Some(&p0) = ps.first() {
            if lanes[lane].is_some() {
                // `lane` was taken by nothing (we just freed it) — guard anyway
                lanes.push(None);
            }
            lanes[lane] = Some((p0, color));
            down.push(Edge { from: lane as u16, to: lane as u16, color });
        }
        for &p in ps.iter().skip(1) {
            match (0..lanes.len()).find(|&k| k != lane && matches!(lanes[k], Some((t, _)) if t == p)) {
                Some(k) => down.push(Edge { from: lane as u16, to: k as u16, color: lanes[k].unwrap().1 }),
                None => {
                    let k = free_slot(&mut lanes);
                    let c = new_color();
                    lanes[k] = Some((p, c));
                    fresh.push(k);
                    down.push(Edge { from: lane as u16, to: k as u16, color: c });
                }
            }
        }
        for (j, l) in lanes.iter().enumerate() {
            if let Some((_, c)) = *l
                && !fresh.contains(&j)
            {
                down.push(Edge { from: j as u16, to: j as u16, color: c });
            }
        }
        let width = width_before.max(lanes.len()).max(lane + 1) as u16;
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        rows.push(Row { lane: lane as u16, color, up, down, width });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(from: u16, to: u16, color: u16) -> Edge {
        Edge { from, to, color }
    }

    #[test]
    fn linear_history_stays_in_one_lane() {
        let rows = layout(&[vec![1], vec![2], vec![]]);
        assert!(rows.iter().all(|r| r.lane == 0 && r.color == 0 && r.width == 1));
        assert_eq!(rows[0].up, vec![]);
        assert_eq!(rows[0].down, vec![e(0, 0, 0)]);
        assert_eq!(rows[2].down, vec![]);
    }

    #[test]
    fn branch_and_merge() {
        // 0 = merge(2, 1); 1 = feature on 2; 2 = base
        let rows = layout(&[vec![2, 1], vec![2], vec![]]);
        assert_eq!((rows[0].lane, rows[1].lane, rows[2].lane), (0, 1, 0));
        assert_eq!(rows[0].down, vec![e(0, 0, 0), e(0, 1, 1)]);
        assert_eq!(rows[1].up, vec![e(0, 0, 0), e(1, 1, 1)]);
        assert_eq!(rows[1].down, vec![e(1, 1, 1), e(0, 0, 0)]);
        // both lanes converge into the base commit
        assert_eq!(rows[2].up, vec![e(0, 0, 0), e(1, 0, 1)]);
        assert_eq!(rows[2].width, 2);
    }

    #[test]
    fn two_tips_fork_from_one_commit_and_lanes_are_reused() {
        // 0 and 1 are tips whose parent is 2; 3 is a later root tip in a free lane
        let rows = layout(&[vec![2], vec![2], vec![], vec![]]);
        assert_eq!((rows[0].lane, rows[1].lane, rows[2].lane), (0, 1, 0));
        assert_eq!(rows[2].up, vec![e(0, 0, 0), e(1, 0, 1)]);
        assert_eq!(rows[3].lane, 0, "lane 0 is free again");
        assert_ne!(rows[3].color, rows[0].color);
    }

    #[test]
    fn merge_into_lane_already_heading_there() {
        // 0 tip -> 3; 1 merge(2, 3); 2 -> 3
        let rows = layout(&[vec![3], vec![2, 3], vec![3], vec![]]);
        assert_eq!(rows[1].lane, 1);
        // second parent joins lane 0, which already heads to row 3: no new lane
        assert!(rows[1].down.contains(&e(1, 0, 0)));
        assert_eq!(rows[1].width, 2);
        assert_eq!(rows[3].lane, 0);
    }

    #[test]
    fn topo_order_puts_children_first_even_with_skewed_clocks() {
        // 0 is the parent of 1, but 1 claims to be older
        let nodes = vec![Node { parents: vec![], time: 100 }, Node { parents: vec![0], time: 50 }, Node { parents: vec![0], time: 200 }];
        let order = topo_order(&nodes);
        assert_eq!(order, vec![2, 1, 0]);
        let pos = |i| order.iter().position(|&x| x == i).unwrap();
        assert!(pos(1) < pos(0) && pos(2) < pos(0));
    }
}
