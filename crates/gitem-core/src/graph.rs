//! Commit-graph lane layout. Output is per-row drawing instructions; rendering
//! is up to the UI, which only ever draws the visible rows.
use std::collections::HashMap;

use crate::model::Commit;

/// One line segment within a row. Top segments run from the row's top edge to
/// its middle, bottom segments from the middle to the bottom edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: u16,
    pub to: u16,
    pub color: u16,
}

#[derive(Debug, Clone, Default)]
pub struct GraphRow {
    pub column: u16,
    pub color: u16,
    pub top: Vec<Edge>,
    pub bottom: Vec<Edge>,
    /// Number of lanes occupied in this row; used for column width.
    pub width: u16,
}

#[derive(Default)]
struct Lanes {
    /// Each lane awaits a given commit, carrying a stable colour.
    slots: Vec<Option<(String, u16)>>,
    next_color: u16,
}

impl Lanes {
    fn alloc(&mut self, sha: String, color: u16) -> usize {
        if let Some(i) = self.slots.iter().position(|s| s.is_none()) {
            self.slots[i] = Some((sha, color));
            i
        } else {
            self.slots.push(Some((sha, color)));
            self.slots.len() - 1
        }
    }
    fn new_color(&mut self) -> u16 {
        let c = self.next_color;
        self.next_color = self.next_color.wrapping_add(1);
        c
    }
}

/// Compute rows for commits ordered children-before-parents.
pub fn layout(commits: &[Commit]) -> Vec<GraphRow> {
    let mut lanes = Lanes::default();
    let mut rows = Vec::with_capacity(commits.len());
    for c in commits {
        let mut row = GraphRow::default();

        // All lanes waiting for this commit converge on its node.
        let waiting: Vec<usize> = lanes
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_ref().is_some_and(|(sha, _)| *sha == c.sha))
            .map(|(i, _)| i)
            .collect();
        let (col, color) = match waiting.first() {
            Some(&i) => (i, lanes.slots[i].as_ref().map(|s| s.1).unwrap_or(0)),
            None => {
                let color = lanes.new_color();
                (lanes.alloc(c.sha.clone(), color), color)
            }
        };
        row.column = col as u16;
        row.color = color;

        // Top half: passing lanes run straight, waiting lanes bend into the node.
        for (i, s) in lanes.slots.iter().enumerate() {
            if let Some((sha, lane_color)) = s {
                if *sha == c.sha {
                    if waiting.contains(&i) {
                        row.top.push(Edge { from: i as u16, to: col as u16, color: *lane_color });
                    }
                } else {
                    row.top.push(Edge { from: i as u16, to: i as u16, color: *lane_color });
                }
            }
        }
        for &i in &waiting {
            lanes.slots[i] = None;
        }
        // A commit nobody was waiting for got a lane just for its node.
        lanes.slots[col] = None;

        // Bottom half: passing lanes continue, node connects to its parents.
        for (i, s) in lanes.slots.iter().enumerate() {
            if let Some((_, lane_color)) = s {
                row.bottom.push(Edge { from: i as u16, to: i as u16, color: *lane_color });
            }
        }
        for (pi, parent) in c.parents.iter().enumerate() {
            let existing = lanes.slots.iter().position(|s| s.as_ref().is_some_and(|(sha, _)| sha == parent));
            match existing {
                Some(l) => {
                    let lc = lanes.slots[l].as_ref().map(|s| s.1).unwrap_or(color);
                    row.bottom.push(Edge { from: col as u16, to: l as u16, color: if pi == 0 { color } else { lc } });
                }
                None => {
                    let (lane, lc) = if pi == 0 {
                        // First parent keeps the node's lane and colour.
                        lanes.slots[col] = Some((parent.clone(), color));
                        (col, color)
                    } else {
                        let nc = lanes.new_color();
                        (lanes.alloc(parent.clone(), nc), nc)
                    };
                    row.bottom.push(Edge { from: col as u16, to: lane as u16, color: lc });
                }
            }
        }

        // Trim empty trailing lanes so the graph narrows again.
        while lanes.slots.last().is_some_and(|s| s.is_none()) {
            lanes.slots.pop();
        }
        let top_max = row.top.iter().map(|e| e.from.max(e.to)).max().unwrap_or(0);
        let bot_max = row.bottom.iter().map(|e| e.from.max(e.to)).max().unwrap_or(0);
        row.width = top_max.max(bot_max).max(row.column) + 1;
        rows.push(row);
    }
    rows
}

/// Remove stash "index on ..." / "untracked files on ..." helper commits, which
/// would otherwise clutter the graph, re-pointing the stash at its base commit.
pub fn hide_stash_internals(commits: Vec<Commit>, stash_shas: &[String]) -> Vec<Commit> {
    let by_sha: HashMap<&str, usize> = commits.iter().enumerate().map(|(i, c)| (c.sha.as_str(), i)).collect();
    let mut hide: Vec<bool> = vec![false; commits.len()];
    for s in stash_shas {
        if let Some(&i) = by_sha.get(s.as_str()) {
            for p in commits[i].parents.iter().skip(1) {
                if let Some(&k) = by_sha.get(p.as_str()) {
                    hide[k] = true;
                }
            }
        }
    }
    let mut out = Vec::with_capacity(commits.len());
    for (i, mut c) in commits.into_iter().enumerate() {
        if hide[i] {
            continue;
        }
        if stash_shas.contains(&c.sha) {
            c.parents.truncate(1);
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(sha: &str, parents: &[&str]) -> Commit {
        Commit { sha: sha.into(), parents: parents.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn linear_stays_in_one_lane() {
        let r = layout(&[c("a", &["b"]), c("b", &["c"]), c("c", &[])]);
        assert!(r.iter().all(|x| x.column == 0));
        assert!(r[0].top.is_empty());
        assert_eq!(r[0].bottom, vec![Edge { from: 0, to: 0, color: r[0].color }]);
        assert!(r[2].bottom.is_empty());
        assert_eq!(r[0].color, r[2].color);
    }

    #[test]
    fn branch_and_merge() {
        // m merges x and y, both descending from b.
        let r = layout(&[c("m", &["x", "y"]), c("x", &["b"]), c("y", &["b"]), c("b", &[])]);
        assert_eq!(r[0].column, 0);
        assert_eq!(r[1].column, 0);
        assert_eq!(r[2].column, 1);
        // Row for x: lane 1 passes straight through.
        assert!(r[1].top.iter().any(|e| e.from == 1 && e.to == 1));
        // Row for y: its parent b is already awaited on lane 0, so it bends across.
        assert!(r[2].bottom.iter().any(|e| e.from == 1 && e.to == 0));
        // b converges into the first lane only.
        assert_eq!(r[3].column, 0);
        assert_eq!(r[3].width, 1);
    }

    #[test]
    fn two_children_converge() {
        let r = layout(&[c("t1", &["b"]), c("t2", &["b"]), c("b", &[])]);
        assert_eq!(r[1].column, 1);
        assert_eq!(r[1].bottom.iter().filter(|e| e.to == 0).count(), 2, "{:?}", r[1].bottom);
    }

    #[test]
    fn unrelated_roots_reuse_lanes() {
        let r = layout(&[c("a", &[]), c("b", &[])]);
        assert_eq!(r[0].column, 0);
        assert_eq!(r[1].column, 0);
        assert_ne!(r[0].color, r[1].color);
    }

    #[test]
    fn octopus() {
        let r = layout(&[c("m", &["a", "b", "d"]), c("a", &[]), c("b", &[]), c("d", &[])]);
        assert_eq!(r[0].bottom.len(), 3);
        assert_eq!(r[0].width, 3);
    }

    #[test]
    fn stash_internals_hidden() {
        let v = vec![c("s", &["b", "i"]), c("i", &["b"]), c("b", &[])];
        let out = hide_stash_internals(v, &["s".to_string()]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].parents, vec!["b"]);
    }

    #[test]
    fn large_history_is_fast() {
        let v: Vec<Commit> = (0..50_000).map(|i| c(&format!("{i}"), &[&format!("{}", i + 1)])).collect();
        let t = std::time::Instant::now();
        let r = layout(&v);
        assert_eq!(r.len(), 50_000);
        assert!(t.elapsed().as_secs() < 5);
    }
}
