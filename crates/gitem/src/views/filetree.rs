//! The sidebar's Files view: the working tree as an explorer, with changes marked on
//! files and on the folders that hold them. Only visible rows are drawn, so it stays
//! quick in repositories with many thousands of files.
use std::collections::{BTreeMap, HashMap, HashSet};

use egui::{Align2, Rect, Sense, Stroke, Ui, WidgetInfo, WidgetType, pos2, vec2};
use gitem_core::model::{FileStatus, StatusEntry};
use gitem_core::repo::DiffSource;
use gitem_core::util::fuzzy_score;

use crate::action::{Action, Cx};
use crate::chrome::{self, icon};
use crate::menus;
use crate::task::Task;
use crate::theme::Palette;
use crate::views::Tab;
use crate::widgets::*;

const ROW_H: f32 = 24.0;
const INDENT: f32 = 14.0;
const MAX_FILTERED: usize = 500;

/// What git says about a file, reduced to one mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub letter: char,
    pub status: FileStatus,
}

/// Worktree changes win, since that's what's on disk; conflicts win over everything.
pub fn mark_for(e: &StatusEntry) -> Option<Mark> {
    if e.conflict.is_some() {
        return Some(Mark { letter: '!', status: FileStatus::Unmerged });
    }
    let status = if e.worktree != FileStatus::Unmodified { e.worktree } else { e.index };
    match status {
        FileStatus::Unmodified | FileStatus::Ignored => None,
        s => Some(Mark { letter: s.letter(), status: s }),
    }
}

#[derive(Default, Debug)]
struct Node {
    /// By lowercase name, so they sort the way people read them.
    dirs: BTreeMap<String, (String, Node)>,
    files: Vec<(String, String, Option<Mark>)>,
    changed: usize,
}

impl Node {
    fn insert(&mut self, path: &str, mark: Option<Mark>) {
        let mut cur = self;
        let mut rest = path;
        while let Some((d, r)) = rest.split_once('/') {
            cur.changed += mark.is_some() as usize;
            cur = &mut cur.dirs.entry(d.to_lowercase()).or_insert_with(|| (d.to_string(), Node::default())).1;
            rest = r;
        }
        cur.changed += mark.is_some() as usize;
        cur.files.push((rest.to_string(), path.to_string(), mark));
    }

    fn sort(&mut self) {
        self.files.sort_by_cached_key(|f| f.0.to_lowercase());
        for (_, d) in self.dirs.values_mut() {
            d.sort();
        }
    }
}

/// One line of the view.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub depth: usize,
    pub name: String,
    pub path: String,
    /// For a filter result: the folder it's in.
    pub folder: String,
    pub dir: Option<DirRow>,
    pub mark: Option<Mark>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirRow {
    pub open: bool,
    pub changed: usize,
}

#[derive(Default)]
pub struct FileTree {
    root: Node,
    all: Vec<(String, Option<Mark>)>,
}

impl FileTree {
    /// `files` are what git lists; `entries` add what it doesn't, such as a file that's
    /// been deleted and staged, and mark what changed.
    pub fn build(files: Vec<String>, entries: &[StatusEntry]) -> Self {
        let marks: HashMap<&str, Mark> = entries.iter().filter_map(|e| mark_for(e).map(|m| (e.path.as_str(), m))).collect();
        let mut root = Node::default();
        let mut all: Vec<(String, Option<Mark>)> = Vec::with_capacity(files.len());
        let listed: HashSet<&str> = files.iter().map(String::as_str).collect();
        for f in &files {
            let mark = marks.get(f.as_str()).copied();
            root.insert(f, mark);
            all.push((f.clone(), mark));
        }
        for (path, mark) in &marks {
            if !listed.contains(path) {
                root.insert(path, Some(*mark));
                all.push((path.to_string(), Some(*mark)));
            }
        }
        root.sort();
        Self { root, all }
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    /// The rows to show. Folders with one folder in them are joined, as `src/views`, so
    /// deep paths take fewer clicks. With `changed_only`, everything that holds a change
    /// is open.
    pub fn rows(&self, expanded: &HashSet<String>, changed_only: bool) -> Vec<Row> {
        let mut out = Vec::new();
        Self::walk(&self.root, "", 0, expanded, changed_only, &mut out);
        out
    }

    fn walk(node: &Node, prefix: &str, depth: usize, expanded: &HashSet<String>, changed_only: bool, out: &mut Vec<Row>) {
        for (name, child) in node.dirs.values() {
            if changed_only && child.changed == 0 {
                continue;
            }
            let (mut label, mut path, mut cur) = (name.clone(), format!("{prefix}{name}"), child);
            while cur.files.is_empty() && cur.dirs.len() == 1 {
                let Some((next_name, next)) = cur.dirs.values().next() else { break };
                label = format!("{label}/{next_name}");
                path = format!("{path}/{next_name}");
                cur = next;
            }
            let open = changed_only || expanded.contains(&path);
            out.push(Row { depth, name: label, path: path.clone(), folder: String::new(), dir: Some(DirRow { open, changed: cur.changed }), mark: None });
            if open {
                Self::walk(cur, &format!("{path}/"), depth + 1, expanded, changed_only, out);
            }
        }
        for (name, path, mark) in &node.files {
            if !changed_only || mark.is_some() {
                out.push(Row { depth, name: name.clone(), path: path.clone(), folder: String::new(), dir: None, mark: *mark });
            }
        }
    }

    /// Files matching `query`, best first, as a flat list.
    pub fn filtered(&self, query: &str, changed_only: bool) -> Vec<Row> {
        let mut hits: Vec<(i32, &String, Option<Mark>)> = self.all.iter().filter(|(_, m)| !changed_only || m.is_some()).filter_map(|(p, m)| fuzzy_score(query, p).map(|s| (s, p, *m))).collect();
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        hits.into_iter()
            .take(MAX_FILTERED)
            .map(|(_, path, mark)| {
                let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
                Row { depth: 0, name: name.to_string(), path: path.clone(), folder: folder.to_string(), dir: None, mark }
            })
            .collect()
    }
}

fn file_glyph(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map_or("", |x| x.1).to_lowercase();
    match ext.as_str() {
        "rs" | "py" | "js" | "jsx" | "ts" | "tsx" | "c" | "h" | "cc" | "cpp" | "hpp" | "cs" | "go" | "java" | "kt" | "swift" | "rb" | "php" | "sh" | "ps1" | "bat" | "cmd" | "lua" | "html" | "css" | "scss" | "json" | "toml" | "yaml" | "yml" | "xml" | "sql" => icon::FILE_CODE,
        "md" | "txt" | "rst" | "adoc" | "log" => icon::FILE_TEXT,
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" | "ico" => icon::FILE_IMAGE,
        _ => icon::FILE,
    }
}

#[derive(Default)]
pub struct FilesView {
    tree: Task<FileTree>,
    built_for: Option<u64>,
    expanded: HashSet<String>,
    filter: String,
    changed_only: bool,
    selected: Option<String>,
}

impl FilesView {
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.tree.poll();
        if self.built_for != Some(cx.data.status_gen) {
            self.built_for = Some(cx.data.status_gen);
            let repo = cx.repo.clone();
            let entries = cx.status().map(|s| s.entries.clone()).unwrap_or_default();
            self.tree.spawn(ui.ctx(), move || -> gitem_core::Result<FileTree> { Ok(FileTree::build(repo.tracked_files(None)?, &entries)) });
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if chrome::icon_button(ui, icon::ARROWS_IN_LINE_VERTICAL, "Collapse folders", "", false, &p).clicked() {
                    self.expanded.clear();
                }
                if chrome::icon_button(ui, icon::GIT_DIFF, "Changed files only", "Hide everything without changes", self.changed_only, &p).clicked() {
                    self.changed_only = !self.changed_only;
                }
                ui.add(input(&mut self.filter).hint_text("Filter files…").desired_width(f32::INFINITY));
            });
        });
        ui.add_space(4.0);

        let query = self.filter.trim().to_string();
        let Some(tree) = self.tree.get() else {
            spinner_row(ui, "Loading files...", &p);
            return;
        };
        let rows = if query.is_empty() { tree.rows(&self.expanded, self.changed_only) } else { tree.filtered(&query, self.changed_only) };
        if rows.is_empty() {
            let (title, hint) = if !query.is_empty() {
                ("No matching files", "Check the spelling, or try fewer letters.")
            } else if self.changed_only {
                ("No changes", "Files you edit appear here.")
            } else {
                ("No files yet", "Files you add to this folder appear here.")
            };
            empty_state(ui, title, hint, &p);
            return;
        }

        let mut toggled = None;
        let selected = self.selected.clone();
        let mut picked = None;
        egui::ScrollArea::vertical().id_salt("sidebar-files").auto_shrink([false, false]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in &rows[range] {
                let is_selected = selected.as_deref() == Some(row.path.as_str());
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, row.name.as_str()));
                paint_row(ui, row, rect, is_selected, resp.hovered(), &p);
                if resp.clicked() {
                    if row.dir.is_some() {
                        toggled = Some(row.path.clone());
                    } else {
                        picked = Some(row.path.clone());
                        open_file(cx, row);
                    }
                }
                resp.context_menu(|ui| {
                    if row.dir.is_some() {
                        folder_menu(ui, cx, &row.path);
                    } else {
                        menus::file_menu(ui, cx, &row.path, None);
                    }
                });
            }
        });
        if let Some(path) = toggled {
            if !self.expanded.remove(&path) {
                self.expanded.insert(path);
            }
        }
        if picked.is_some() {
            self.selected = picked;
        }
    }
}

fn open_file(cx: &mut Cx, row: &Row) {
    // A deleted file has nothing on disk to show, but its diff does.
    match row.mark {
        Some(Mark { status: FileStatus::Deleted, .. }) => cx.diff(DiffSource::WorkingVsHead, Some(&row.path)),
        _ => cx.open(Tab::file(&row.path, None, None)),
    }
}

fn folder_menu(ui: &mut Ui, cx: &mut Cx, path: &str) {
    if ui.button("Visual history of this folder").clicked() {
        cx.open(Tab::visual(path));
        ui.close();
    }
    if ui.button("Copy path").clicked() {
        cx.copy(path.to_string());
        ui.close();
    }
    let label = if cx.settings.default_external_editor.is_empty() { "Reveal in file manager" } else { "Open in external editor" };
    if ui.button(label).clicked() {
        cx.out.push(Action::OpenFile(cx.repo.root.join(path)));
        ui.close();
    }
}

fn paint_row(ui: &Ui, row: &Row, rect: Rect, selected: bool, hovered: bool, p: &Palette) {
    if selected {
        ui.painter().rect_filled(rect, 3.0, p.selection);
    } else if hovered {
        ui.painter().rect_filled(rect, 3.0, p.hover);
    }
    // Guides show which folder a row belongs to.
    for level in 0..row.depth {
        let x = rect.left() + 12.0 + level as f32 * INDENT;
        ui.painter().vline(x, rect.y_range(), Stroke::new(1.0, p.border));
    }
    let x = rect.left() + 5.0 + row.depth as f32 * INDENT;
    let y = rect.center().y;
    let mut right = rect.right() - 6.0;
    let color = row.mark.map_or(if selected { p.text } else { p.dim }, |m| status_color(m.status, p));
    let (glyph, tint) = match row.dir {
        Some(d) => {
            let caret = if d.open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
            ui.painter().text(pos2(x + 7.0, y), Align2::CENTER_CENTER, caret, prop(10.5), p.faint);
            (if d.open { icon::FOLDER_OPEN } else { icon::FOLDER }, p.series[3])
        }
        None => (file_glyph(&row.name), row.mark.map_or(p.faint, |m| status_color(m.status, p))),
    };
    ui.painter().text(pos2(x + 23.0, y), Align2::CENTER_CENTER, glyph, prop(14.0), tint);
    if let Some(m) = row.mark {
        let g = ui.painter().text(pos2(right, y), Align2::RIGHT_CENTER, m.letter.to_string(), mono(11.0), color);
        right = g.left() - 6.0;
    } else if row.dir.is_some_and(|d| d.changed > 0) {
        ui.painter().circle_filled(pos2(right - 3.0, y), 3.0, p.series[3]);
        right -= 12.0;
    }
    let text = Rect::from_min_max(pos2(x + 33.0, rect.top()), pos2(right, rect.bottom()));
    let painter = ui.painter().with_clip_rect(text.intersect(ui.clip_rect()));
    let name = painter.text(pos2(text.left(), y), Align2::LEFT_CENTER, &row.name, prop(12.5), if row.dir.is_some() { p.text } else { color });
    if !row.folder.is_empty() {
        painter.text(pos2(name.right() + 7.0, y), Align2::LEFT_CENTER, &row.folder, prop(11.0), p.faint);
    }
}

/// Switches the sidebar between its two views. Returns the choice if it changed.
pub fn view_switch(ui: &mut Ui, files: bool, p: &Palette) -> Option<bool> {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, p.border));
    let half = rect.width() / 2.0;
    let mut choice = None;
    for (i, (value, glyph, name)) in [(false, icon::GIT_BRANCH, "Repository"), (true, icon::FOLDER, "Files")].into_iter().enumerate() {
        let r = Rect::from_min_size(rect.min + vec2(half * i as f32, 0.0), vec2(half, rect.height()));
        let on = files == value;
        let resp = ui.interact(r, ui.id().with(("sidebar-view", name)), Sense::click());
        resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, on, name));
        if resp.hovered() && !on {
            ui.painter().rect_filled(r.shrink2(vec2(2.0, 2.0)), 4.0, p.hover);
        }
        let color = if on || resp.hovered() { p.text } else { p.dim };
        let g = ui.painter().layout_no_wrap(name.to_string(), prop(12.5), color);
        let total = 20.0 + g.size().x;
        let left = r.center().x - total / 2.0;
        ui.painter().text(pos2(left + 7.0, r.center().y), Align2::CENTER_CENTER, glyph, prop(14.0), if on { p.accent } else { color });
        ui.painter().galley(pos2(left + 20.0, r.center().y - g.size().y / 2.0), g, color);
        if on {
            let line = Rect::from_min_size(pos2(r.left() + 6.0, r.bottom() - 2.0), vec2(r.width() - 12.0, 2.0));
            ui.painter().rect_filled(line, 1.0, p.accent);
        }
        if resp.clicked() && !on {
            choice = Some(value);
        }
    }
    choice
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, index: FileStatus, worktree: FileStatus) -> StatusEntry {
        StatusEntry { path: path.into(), orig_path: None, index, worktree, conflict: None, submodule: false }
    }

    fn sample() -> FileTree {
        let files = ["README.md", "src/main.rs", "src/views/graph.rs", "src/views/home.rs", "a/b/c/deep.txt", "Zed.txt", "apple.txt"];
        FileTree::build(files.iter().map(|s| s.to_string()).collect(), &[entry("src/views/graph.rs", FileStatus::Unmodified, FileStatus::Modified), entry("gone/old.rs", FileStatus::Deleted, FileStatus::Unmodified)])
    }

    fn names(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    fn open(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn folders_come_first_and_names_ignore_case() {
        let t = sample();
        assert_eq!(names(&t.rows(&HashSet::new(), false)), ["a/b/c", "gone", "src", "apple.txt", "README.md", "Zed.txt"]);
    }

    #[test]
    fn single_child_folders_are_joined_and_open_by_their_full_path() {
        let t = sample();
        let rows = t.rows(&open(&["a/b/c"]), false);
        assert_eq!(rows[0].path, "a/b/c");
        assert_eq!(rows[1].name, "deep.txt");
        assert_eq!((rows[1].depth, rows[1].path.as_str()), (1, "a/b/c/deep.txt"));
    }

    #[test]
    fn opening_reveals_children_at_the_next_depth() {
        let t = sample();
        let rows = t.rows(&open(&["src", "src/views"]), false);
        let graph = rows.iter().find(|r| r.name == "graph.rs").unwrap();
        assert_eq!(graph.depth, 2);
        assert_eq!(graph.mark.unwrap().letter, 'M');
        assert!(!t.rows(&open(&["src"]), false).iter().any(|r| r.name == "graph.rs"), "its parent is closed");
    }

    #[test]
    fn folders_count_the_changes_inside_them() {
        let t = sample();
        let rows = t.rows(&open(&["src"]), false);
        let changed = |n: &str| rows.iter().find(|r| r.name == n).unwrap().dir.unwrap().changed;
        assert_eq!(changed("src"), 1);
        assert_eq!(changed("views"), 1);
        assert_eq!(changed("a/b/c"), 0);
    }

    #[test]
    fn files_only_in_status_still_appear() {
        let t = sample();
        let rows = t.rows(&open(&["gone"]), false);
        let old = rows.iter().find(|r| r.name == "old.rs").unwrap();
        assert_eq!(old.mark.unwrap().status, FileStatus::Deleted);
    }

    #[test]
    fn changed_only_opens_what_holds_changes_and_hides_the_rest() {
        let rows = sample().rows(&HashSet::new(), true);
        assert_eq!(names(&rows), ["gone", "old.rs", "src", "views", "graph.rs"]);
    }

    #[test]
    fn filtering_is_flat_best_first_and_shows_the_folder() {
        let t = sample();
        let rows = t.filtered("graph", false);
        assert_eq!(names(&rows), ["graph.rs"]);
        assert_eq!(rows[0].folder, "src/views");
        assert_eq!(rows[0].depth, 0);
        assert!(t.filtered("zzzzzz", false).is_empty());
        let mut changed = t.filtered(".rs", true);
        changed.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(names(&changed), ["graph.rs", "old.rs"], "only changed files");
    }

    #[test]
    fn marks_prefer_the_worktree_and_conflicts_win() {
        assert_eq!(mark_for(&entry("x", FileStatus::Added, FileStatus::Modified)).unwrap().letter, 'M');
        assert_eq!(mark_for(&entry("x", FileStatus::Added, FileStatus::Unmodified)).unwrap().letter, 'A');
        assert_eq!(mark_for(&entry("x", FileStatus::Unmodified, FileStatus::Unmodified)), None);
        assert_eq!(mark_for(&entry("x", FileStatus::Unmodified, FileStatus::Ignored)), None);
        let conflict = StatusEntry { conflict: Some("UU".into()), ..entry("x", FileStatus::Modified, FileStatus::Modified) };
        assert_eq!(mark_for(&conflict).unwrap().status, FileStatus::Unmerged);
    }

    #[test]
    fn an_empty_list_is_empty() {
        assert!(FileTree::build(Vec::new(), &[]).is_empty());
    }
}
