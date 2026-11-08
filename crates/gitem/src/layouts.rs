//! Arranging tabs: the two built-in layouts, one pane filling the window for a while, and
//! moving between panes from the keyboard.
use egui_dock::{DockState, Node, NodePath, Split, TabIndex};

use crate::views::Tab;

/// The arrangement a repository opens with: one pane of its overview, graph and changes.
pub fn git_layout() -> DockState<Tab> {
    let mut d = DockState::new(vec![Tab::home(), Tab::graph(), Tab::changes()]);
    if let Some(path) = d.find_tab_from(|t| matches!(t, Tab::Graph(_))) {
        let _ = d.set_active_tab(path);
    }
    d
}

/// For working on code: the editor takes most of the window, Changes has a full-height pane
/// to its right where the commit box has room, and the Console sits under the editor. `editor`
/// is the file to show, or the welcome page.
pub fn ide_layout(editor: Tab) -> DockState<Tab> {
    use egui_dock::NodeIndex;
    let mut d = DockState::new(vec![editor]);
    let tree = d.main_surface_mut();
    let [left, _] = tree.split_right(NodeIndex::root(), 0.72, vec![Tab::changes()]);
    // The split makes `left` a parent: the editor is now in the first of its children.
    let [editor_pane, _] = tree.split_below(left, 0.76, vec![Tab::Console(Box::default())]);
    d.set_focused_node_and_surface(NodePath { surface: egui_dock::SurfaceIndex::main(), node: editor_pane });
    d
}

/// The rest of the layout, while one pane fills the window. The pane's tabs were taken out of
/// `home`, which stays in place, empty, to put them back into.
pub struct Zoom {
    pub full: DockState<Tab>,
    pub home: NodePath,
}

/// One pane's tabs, alone in the window. `None` when there's nothing else to hide.
pub fn zoom(dock: &mut DockState<Tab>, pane: NodePath) -> Option<Zoom> {
    if dock.iter_leaves().count() < 2 {
        return None;
    }
    let leaf = dock.leaf_mut(pane).ok()?;
    let active = leaf.active;
    let tabs = std::mem::take(&mut leaf.tabs);
    if tabs.is_empty() {
        return None;
    }
    let mut alone = DockState::new(tabs);
    let path = alone.iter_all_tabs().nth(active.0).map(|(p, _)| p);
    if let Some(path) = path {
        let _ = alone.set_active_tab(path);
    }
    let full = std::mem::replace(dock, alone);
    Some(Zoom { full, home: pane })
}

/// Everything back as it was, with any tab opened meanwhile in the pane that was zoomed.
pub fn unzoom(dock: &mut DockState<Tab>, z: Zoom) {
    let mut alone = std::mem::replace(dock, z.full);
    let leaves: Vec<NodePath> = alone.iter_leaves().map(|(p, _)| p).collect();
    let mut tabs = Vec::new();
    let mut active = 0;
    for (i, p) in leaves.into_iter().enumerate() {
        if let Ok(l) = alone.leaf_mut(p) {
            if i == 0 {
                active = l.active.0;
            }
            tabs.append(&mut l.tabs);
        }
    }
    if tabs.is_empty() {
        // Every tab of it was closed while it filled the window.
        dock.remove_leaf(z.home);
        return;
    }
    match dock.leaf_mut(z.home) {
        Ok(home) => {
            home.active = TabIndex(active.min(tabs.len() - 1));
            home.tabs = tabs;
            dock.set_focused_node_and_surface(z.home);
        }
        Err(_) => {
            for t in tabs {
                dock.push_to_focused_leaf(t);
            }
        }
    }
}

/// Which tabs of a pane a "close" command means, by key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closing {
    Others,
    ToTheRight,
    All,
}

/// The keys of the tabs to close in the pane that holds `key`.
pub fn keys_to_close(dock: &DockState<Tab>, key: &str, which: Closing) -> Vec<String> {
    for (_, leaf) in dock.iter_leaves() {
        let keys: Vec<String> = leaf.tabs.iter().map(Tab::key).collect();
        if let Some(i) = keys.iter().position(|k| k == key) {
            return match which {
                Closing::Others => keys.into_iter().enumerate().filter(|(j, _)| *j != i).map(|(_, k)| k).collect(),
                Closing::ToTheRight => keys.into_iter().skip(i + 1).collect(),
                Closing::All => keys,
            };
        }
    }
    Vec::new()
}

/// Moves the tab with `key` into a new pane beside its own. Its pane must have another tab
/// to keep, or there'd be nothing left where it was.
pub fn split_off(dock: &mut DockState<Tab>, key: &str, split: Split) -> bool {
    let Some(path) = dock.find_tab_from(|t| t.key() == key) else { return false };
    let pane = NodePath { surface: path.surface, node: path.node };
    if dock.leaf(pane).map_or(0, |l| l.tabs.len()) < 2 {
        return false;
    }
    let Some(tab) = dock.remove_tab(path) else { return false };
    let [_, new] = dock.split(pane, split, 0.5, Node::leaf(tab));
    dock.set_focused_node_and_surface(NodePath { surface: pane.surface, node: new });
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The pane nearest the focused one in a direction, by where they are on screen.
pub fn neighbour(dock: &DockState<Tab>, dir: Direction) -> Option<NodePath> {
    let leaves: Vec<(NodePath, egui::Rect)> = dock.iter_leaves().map(|(p, l)| (p, l.rect)).collect();
    let here = dock.focused_leaf().or_else(|| leaves.first().map(|l| l.0))?;
    let from = leaves.iter().find(|l| l.0 == here)?.1.center();
    leaves
        .iter()
        .filter(|(p, _)| *p != here)
        .filter(|(_, r)| {
            let c = r.center();
            match dir {
                Direction::Left => c.x < from.x - 1.0,
                Direction::Right => c.x > from.x + 1.0,
                Direction::Up => c.y < from.y - 1.0,
                Direction::Down => c.y > from.y + 1.0,
            }
        })
        .min_by(|a, b| a.1.center().distance(from).total_cmp(&b.1.center().distance(from)))
        .map(|l| l.0)
}

/// The next or previous pane in reading order, wrapping round.
pub fn next_pane(dock: &DockState<Tab>, back: bool) -> Option<NodePath> {
    let mut leaves: Vec<(NodePath, egui::Rect)> = dock.iter_leaves().map(|(p, l)| (p, l.rect)).collect();
    if leaves.len() < 2 {
        return None;
    }
    leaves.sort_by(|a, b| (a.1.top(), a.1.left()).partial_cmp(&(b.1.top(), b.1.left())).unwrap_or(std::cmp::Ordering::Equal));
    let here = dock.focused_leaf().or_else(|| leaves.first().map(|l| l.0))?;
    let i = leaves.iter().position(|l| l.0 == here)?;
    let n = leaves.len();
    Some(leaves[if back { (i + n - 1) % n } else { (i + 1) % n }].0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(d: &DockState<Tab>) -> Vec<String> {
        d.iter_all_tabs().map(|(_, t)| t.key()).collect()
    }

    #[test]
    fn a_zoomed_pane_goes_back_with_what_was_opened_in_it() {
        let mut d = git_layout();
        assert!(split_off(&mut d, "changes", Split::Right));
        assert_eq!(d.iter_leaves().count(), 2);
        let pane = d.focused_leaf().unwrap();
        let z = zoom(&mut d, pane).unwrap();
        assert_eq!(keys(&d), vec!["changes"]);
        d.push_to_focused_leaf(Tab::Console(Box::default()));
        unzoom(&mut d, z);
        assert_eq!(d.iter_leaves().count(), 2);
        let mut all = keys(&d);
        all.sort();
        assert_eq!(all, vec!["changes", "console", "graph", "home"]);
        assert_eq!(d.leaf(pane).unwrap().tabs.len(), 2, "the console went where the zoomed pane was");
        assert!(zoom(&mut git_layout(), d.focused_leaf().unwrap()).is_none(), "one pane has nothing to hide");
    }

    #[test]
    fn closing_others_and_to_the_right() {
        let d = git_layout();
        assert_eq!(keys_to_close(&d, "graph", Closing::Others), vec!["home", "changes"]);
        assert_eq!(keys_to_close(&d, "graph", Closing::ToTheRight), vec!["changes"]);
        assert_eq!(keys_to_close(&d, "changes", Closing::ToTheRight), Vec::<String>::new());
        assert_eq!(keys_to_close(&d, "home", Closing::All).len(), 3);
    }

    #[test]
    fn the_last_tab_of_a_pane_stays_put() {
        let mut d = DockState::new(vec![Tab::home()]);
        assert!(!split_off(&mut d, "home", Split::Right));
        assert_eq!(d.iter_leaves().count(), 1);
    }

    #[test]
    fn the_ide_layout_has_the_editor_with_changes_beside_and_the_console_below() {
        let d = ide_layout(Tab::Editor(Box::default()));
        let mut leaves: Vec<Vec<String>> = d.iter_leaves().map(|(_, l)| l.tabs.iter().map(Tab::key).collect()).collect();
        leaves.sort();
        assert_eq!(leaves, vec![vec!["changes".to_string()], vec!["console".to_string()], vec!["edit:".to_string()]]);
        let focused = d.focused_leaf().unwrap();
        assert_eq!(d.leaf(focused).unwrap().tabs[0].key(), "edit:", "new files open beside the editor");
    }
}

/// What a tab's menu or a key asks of the layout, for the tab with a given key.
#[derive(Clone, Debug, PartialEq)]
pub enum DockOp {
    Close,
    CloseMany(Closing),
    Split(Split),
    /// Fill the window with this tab's pane, or bring the layout back.
    Zoom,
    /// Keep a preview tab open rather than reusing it for the next file.
    Keep,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preset {
    Git,
    Ide,
    Saved(String),
}

/// Every tab taken out of a layout, in order, leaving it empty.
pub fn take_all(dock: &mut DockState<Tab>) -> Vec<Tab> {
    let leaves: Vec<NodePath> = dock.iter_leaves().map(|(p, _)| p).collect();
    let mut out = Vec::new();
    for p in leaves {
        if let Ok(l) = dock.leaf_mut(p) {
            out.append(&mut l.tabs);
        }
    }
    out
}

/// A new arrangement holding the tabs that were open: each takes the place of a tab of the same
/// kind in `into`, keeping what it was showing and any unsaved text, and the rest go to its
/// focused pane. Nothing open is lost by choosing a layout.
pub fn arrange(mut into: DockState<Tab>, open: Vec<Tab>) -> DockState<Tab> {
    // Where the preset wants new things: its focused pane, else the first.
    let first = into.focused_leaf().or_else(|| into.iter_leaves().next().map(|(p, _)| p));
    let has_file = open.iter().any(|t| matches!(t, Tab::Editor(e) if !e.path.is_empty()));
    for tab in open {
        let key = tab.key();
        if let Some((_, slot)) = into.iter_all_tabs_mut().find(|(_, t)| t.key() == key) {
            *slot = tab;
            continue;
        }
        if let Some(p) = first {
            into.set_focused_node_and_surface(p);
        }
        into.push_to_focused_leaf(tab);
    }
    // The editor's welcome page is only for when no file is open.
    if has_file && let Some(path) = into.find_tab_from(|t| matches!(t, Tab::Editor(e) if e.path.is_empty())) {
        into.remove_tab(path);
    }
    if let Some(p) = first {
        into.set_focused_node_and_surface(p);
    }
    into
}

#[cfg(test)]
mod arrange_tests {
    use super::*;

    #[test]
    fn choosing_a_layout_keeps_every_open_tab() {
        let mut d = git_layout();
        d.push_to_focused_leaf(Tab::editor("src/a.rs", None, true));
        d.push_to_focused_leaf(Tab::Reflog(Box::default()));
        let open = take_all(&mut d);
        let ide = arrange(ide_layout(Tab::Editor(Box::default())), open);
        let pane_of = |k: &str| ide.iter_leaves().find(|(_, l)| l.tabs.iter().any(|t| t.key() == k)).map(|(p, _)| p);
        let editor = pane_of("edit:src/a.rs").unwrap();
        for k in ["home", "graph", "reflog"] {
            assert_eq!(pane_of(k), Some(editor), "{k} goes with the editor");
        }
        assert_ne!(pane_of("changes"), Some(editor));
        assert!(pane_of("edit:").is_none(), "no welcome page beside a file");
    }
}
