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

/// The parents to draw, when a filter leaves commits out of the list. Git still names the real
/// parents, which would each start a lane that never ends. With `first_parent`, only the first
/// is followed. `hidden_merges` maps each merge left out to its first parent, so a child is
/// drawn down to the next commit that's in the list.
pub fn parents_for_filters(commits: &[Commit], first_parent: bool, hidden_merges: Option<&std::collections::HashMap<String, String>>) -> Vec<Commit> {
    commits
        .iter()
        .map(|c| {
            let mut c = c.clone();
            if first_parent {
                c.parents.truncate(1);
            }
            if let Some(map) = hidden_merges {
                let mut seen = Vec::with_capacity(c.parents.len());
                for p in &c.parents {
                    let mut p = p;
                    // Merges in a row, and never for ever whatever the map says.
                    for _ in 0..map.len() {
                        match map.get(p) {
                            Some(next) => p = next,
                            None => break,
                        }
                    }
                    if !seen.contains(p) {
                        seen.push(p.clone());
                    }
                }
                c.parents = seen;
            }
            c
        })
        .collect()
}

/// One lane held by one commit-to-be, straight from the row after `from` to the row before `to`.
#[derive(Debug, Clone, Copy)]
struct Stretch {
    lane: u16,
    color: u16,
    from: u32,
    to: u32,
}

/// How often the lanes running are written down in full. Finding a row's costs at most this many
/// rows of changes beyond that.
const CHECKPOINT_EVERY: usize = 64;

/// A history's rows, holding only what changes. On a wide history nearly every edge is a lane going
/// straight past, the same as in the row above, and storing those per row cost 172 MB on one with
/// 1,261 lanes. Here each stretch is stored once, and a row's are found when it's drawn.
#[derive(Debug, Clone, Default)]
pub struct LaneRows {
    /// With only the edges that bend, or start or end at a node, in `top` and `bottom`.
    rows: Vec<GraphRow>,
    /// In order of `from`.
    stretches: Vec<Stretch>,
    /// For every `CHECKPOINT_EVERY`th row, which stretches are running through it.
    checkpoints: Vec<Box<[u32]>>,
}

impl LaneRows {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Its node, colour, width, and the edges that aren't lanes going straight past.
    pub fn row(&self, i: usize) -> &GraphRow {
        &self.rows[i]
    }

    pub fn max_width(&self) -> Option<u16> {
        self.rows.iter().map(|r| r.width).max()
    }

    /// The lanes going straight past this row, top and bottom alike, in no particular order.
    pub fn for_each_passing(&self, i: usize, mut f: impl FnMut(u16, u16)) {
        let i32_row = i as u32;
        let at = i / CHECKPOINT_EVERY;
        let Some(start) = self.checkpoints.get(at) else { return };
        let running = |s: &Stretch| s.from < i32_row && i32_row < s.to;
        for id in start.iter() {
            let s = &self.stretches[*id as usize];
            if running(s) {
                f(s.lane, s.color);
            }
        }
        // What's started since. A stretch starting at the checkpoint's own row isn't listed there.
        let first = self.stretches.partition_point(|s| (s.from as usize) < at * CHECKPOINT_EVERY);
        for s in self.stretches[first..].iter().take_while(|s| s.from < i32_row) {
            if running(s) {
                f(s.lane, s.color);
            }
        }
    }

    /// Everything in the top half, in order of lane.
    pub fn top(&self, i: usize) -> Vec<Edge> {
        let mut all = self.rows[i].top.clone();
        self.for_each_passing(i, |lane, color| all.push(Edge { from: lane, to: lane, color }));
        all.sort_by_key(|e| e.from);
        all
    }

    /// Everything in the bottom half: lanes going past in order, then the node's edges.
    pub fn bottom(&self, i: usize) -> Vec<Edge> {
        let mut all = Vec::new();
        self.for_each_passing(i, |lane, color| all.push(Edge { from: lane, to: lane, color }));
        all.sort_by_key(|e| e.from);
        all.extend_from_slice(&self.rows[i].bottom);
        all
    }
}

/// No stretch: a lane that's only held for a node nobody was waiting for, in its own row.
const NO_STRETCH: u32 = u32::MAX;

/// Compute rows for commits ordered children-before-parents.
pub fn layout_rows(commits: &[Commit]) -> LaneRows {
    let mut lanes = Lanes::default();
    // What each lane holds, beside it.
    let mut held: Vec<u32> = Vec::new();
    let mut out = LaneRows { rows: Vec::with_capacity(commits.len()), ..Default::default() };
    let mut open: Vec<u32> = Vec::new();
    let open_stretch = |lanes: &Lanes, held: &mut Vec<u32>, out: &mut LaneRows, open: &mut Vec<u32>, lane: usize, row: usize| {
        let color = lanes.slots[lane].as_ref().map_or(0, |s| s.1);
        let id = out.stretches.len() as u32;
        out.stretches.push(Stretch { lane: lane as u16, color, from: row as u32, to: u32::MAX });
        held.resize(held.len().max(lane + 1), NO_STRETCH);
        held[lane] = id;
        open.push(id);
    };
    for (at, c) in commits.iter().enumerate() {
        if at.is_multiple_of(CHECKPOINT_EVERY) {
            // A stretch that started in the row above is running from here on.
            open.retain(|id| out.stretches[*id as usize].to == u32::MAX);
            out.checkpoints.push(open.clone().into_boxed_slice());
        }
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
        let mut widest = col;
        for (i, s) in lanes.slots.iter().enumerate() {
            if let Some((sha, lane_color)) = s {
                if *sha == c.sha {
                    if waiting.contains(&i) {
                        row.top.push(Edge { from: i as u16, to: col as u16, color: *lane_color });
                        widest = widest.max(i);
                    }
                } else {
                    widest = widest.max(i);
                }
            }
        }
        for &i in &waiting {
            lanes.slots[i] = None;
            if let Some(id) = held.get(i).copied().filter(|id| *id != NO_STRETCH) {
                out.stretches[id as usize].to = at as u32;
                held[i] = NO_STRETCH;
            }
        }
        // A commit nobody was waiting for got a lane just for its node.
        lanes.slots[col] = None;

        // Bottom half: passing lanes continue, node connects to its parents. What's still held
        // here was counted above, so only the node's edges can make it wider.
        for (pi, parent) in c.parents.iter().enumerate() {
            let existing = lanes.slots.iter().position(|s| s.as_ref().is_some_and(|(sha, _)| sha == parent));
            match existing {
                Some(l) => {
                    let lc = lanes.slots[l].as_ref().map(|s| s.1).unwrap_or(color);
                    row.bottom.push(Edge { from: col as u16, to: l as u16, color: if pi == 0 { color } else { lc } });
                    widest = widest.max(l);
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
                    open_stretch(&lanes, &mut held, &mut out, &mut open, lane, at);
                    widest = widest.max(lane);
                }
            }
        }

        // Trim empty trailing lanes so the graph narrows again.
        while lanes.slots.last().is_some_and(|s| s.is_none()) {
            lanes.slots.pop();
        }
        row.width = widest as u16 + 1;
        out.rows.push(row);
    }
    out.rows.shrink_to_fit();
    out.stretches.shrink_to_fit();
    out
}

/// The same with every edge of every row written out, as `LaneRows::top` and `bottom` give them.
pub fn layout(commits: &[Commit]) -> Vec<GraphRow> {
    let lanes = layout_rows(commits);
    (0..lanes.len()).map(|i| GraphRow { top: lanes.top(i), bottom: lanes.bottom(i), ..lanes.rows[i].clone() }).collect()
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

    /// How this was done when each row held every edge, which it's held to.
    fn every_edge_per_row(commits: &[Commit]) -> Vec<GraphRow> {
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

    fn rows_equal(commits: &[Commit]) {
        let was = every_edge_per_row(commits);
        let now = layout(commits);
        assert_eq!(was.len(), now.len());
        for (i, (a, b)) in was.iter().zip(&now).enumerate() {
            assert_eq!((a.column, a.color, a.width), (b.column, b.color, b.width), "row {i}");
            assert_eq!(a.top, b.top, "row {i}'s top");
            assert_eq!(a.bottom, b.bottom, "row {i}'s bottom");
        }
    }

    /// Random histories, with merges, octopus merges, parents outside the list, roots, and lanes
    /// freed and reused, each long enough to cross a checkpoint or several.
    #[test]
    fn it_draws_exactly_what_storing_every_edge_drew() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n.max(1)
        };
        for round in 0..60 {
            let n = 1 + next(if round < 40 { 90 } else { 700 }) as usize;
            let mut commits = Vec::with_capacity(n);
            for i in 0..n {
                let mut parents = Vec::new();
                for _ in 0..[1, 1, 1, 1, 2, 2, 0, 3][next(8) as usize] {
                    // Mostly close by, so that lanes end, sometimes far, sometimes not listed at all.
                    let to = match next(10) {
                        0 => i + 1 + next(300) as usize,
                        1 => n + next(5) as usize,
                        _ => i + 1 + next(4) as usize,
                    };
                    let name = format!("c{to}");
                    if !parents.contains(&name) {
                        parents.push(name);
                    }
                }
                commits.push(Commit { sha: format!("c{i}"), parents, ..Default::default() });
            }
            rows_equal(&commits);
        }
    }

    #[test]
    fn it_draws_the_same_across_a_checkpoint_on_each_side_of_one() {
        // A lane starting exactly at a checkpoint's row, ending exactly at one, and one going past.
        let e = CHECKPOINT_EVERY;
        let mut commits: Vec<Commit> = (0..3 * e).map(|i| Commit { sha: format!("m{i}"), parents: vec![format!("m{}", i + 1)], ..Default::default() }).collect();
        commits[e].parents.push(format!("m{}", 2 * e));
        commits[0].parents.push(format!("m{}", 3 * e - 1));
        commits[e - 1].parents.push(format!("m{}", e + 1));
        rows_equal(&commits);
    }

    #[test]
    fn only_lane_changes_are_held() {
        let commits: Vec<Commit> = (0..200).flat_map(|i| [Commit { sha: format!("a{i}"), parents: vec![format!("a{}", i + 1)], ..Default::default() }, Commit { sha: format!("b{i}"), parents: vec![format!("b{}", i + 1)], ..Default::default() }]).collect();
        let held = layout_rows(&commits);
        assert_eq!(held.stretches.len() as u64, 400, "one for each commit's parent");
        assert!(held.rows.iter().all(|r| r.bottom.len() == 1 && r.top.len() <= 1), "a lane going past isn't stored");
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
