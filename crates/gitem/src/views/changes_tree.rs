//! The Changes list as folders. Built once per status, so a large list isn't sorted
//! again every frame; walking it only visits folders that are open.
use std::collections::{BTreeMap, HashSet};

#[derive(Default, Debug)]
pub struct Node {
    /// By lowercase name for reading order, then exact name, so that folders differing
    /// only in case stay apart: an action on one mustn't reach the other.
    dirs: BTreeMap<(String, String), Node>,
    /// Name, and the file's position in the status list.
    files: Vec<(String, usize)>,
    count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Dir { open: bool, count: usize },
    File(usize),
}

/// One line of the tree. `path` is a folder's full path, and empty for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub depth: usize,
    pub name: String,
    pub path: String,
    pub kind: RowKind,
}

impl Node {
    pub fn build<'a>(files: impl IntoIterator<Item = (usize, &'a str)>) -> Self {
        let mut root = Node::default();
        for (index, path) in files {
            root.insert(path, index);
        }
        root.sort();
        root
    }

    fn insert(&mut self, path: &str, index: usize) {
        let mut cur = self;
        let mut rest = path;
        while let Some((dir, tail)) = rest.split_once('/') {
            cur.count += 1;
            cur = cur.dirs.entry((dir.to_lowercase(), dir.to_string())).or_default();
            rest = tail;
        }
        cur.count += 1;
        cur.files.push((rest.to_string(), index));
    }

    fn sort(&mut self) {
        self.files.sort_by_cached_key(|f| (f.0.to_lowercase(), f.0.clone()));
        for d in self.dirs.values_mut() {
            d.sort();
        }
    }

    /// Folders first. A folder holding only one folder is joined with it, as `src/views`.
    /// With no `collapsed` set, everything is open.
    pub fn rows(&self, collapsed: Option<&HashSet<String>>) -> Vec<Row> {
        let mut out = Vec::new();
        self.walk("", 0, collapsed, &mut out);
        out
    }

    fn walk(&self, prefix: &str, depth: usize, collapsed: Option<&HashSet<String>>, out: &mut Vec<Row>) {
        for ((_, name), child) in &self.dirs {
            let (mut label, mut path, mut cur) = (name.clone(), format!("{prefix}{name}"), child);
            while cur.files.is_empty() && cur.dirs.len() == 1 {
                let Some(((_, next_name), next)) = cur.dirs.iter().next() else { break };
                label = format!("{label}/{next_name}");
                path = format!("{path}/{next_name}");
                cur = next;
            }
            let open = !collapsed.is_some_and(|c| c.contains(&path));
            out.push(Row { depth, name: label, path: path.clone(), kind: RowKind::Dir { open, count: cur.count } });
            if open {
                cur.walk(&format!("{path}/"), depth + 1, collapsed, out);
            }
        }
        for (name, index) in &self.files {
            out.push(Row { depth, name: name.clone(), path: String::new(), kind: RowKind::File(*index) });
        }
    }

    /// Every folder that has a row of its own.
    pub fn folders(&self) -> Vec<String> {
        self.rows(None).into_iter().filter(|r| matches!(r.kind, RowKind::Dir { .. })).map(|r| r.path).collect()
    }
}

/// The files in the folder at `rows[at]`. Needs rows with that folder open.
pub fn files_in(rows: &[Row], at: usize) -> Vec<usize> {
    let depth = rows[at].depth;
    rows[at + 1..]
        .iter()
        .take_while(|r| r.depth > depth)
        .filter_map(|r| match r.kind {
            RowKind::File(i) => Some(i),
            RowKind::Dir { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str]) -> Node {
        Node::build(paths.iter().enumerate().map(|(i, p)| (i, *p)))
    }

    fn lines(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.name)).collect()
    }

    fn closed(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn folders_come_first_then_files_ignoring_case() {
        let t = tree(&["Zed.txt", "src/main.rs", "apple.txt", "docs/a.md", "README.md"]);
        assert_eq!(lines(&t.rows(None)), ["docs", "  a.md", "src", "  main.rs", "apple.txt", "README.md", "Zed.txt"]);
    }

    #[test]
    fn single_child_folders_are_joined() {
        let t = tree(&["a/b/c/deep.txt", "src/views/x.rs", "src/main.rs"]);
        assert_eq!(lines(&t.rows(None)), ["a/b/c", "  deep.txt", "src", "  views", "    x.rs", "  main.rs"]);
        assert_eq!(t.rows(None)[0].path, "a/b/c");
        assert_eq!(t.rows(None)[3].path, "src/views");
    }

    #[test]
    fn collapsing_hides_only_what_is_inside() {
        let t = tree(&["src/views/x.rs", "src/main.rs", "top.txt"]);
        assert_eq!(lines(&t.rows(Some(&closed(&["src/views"])))), ["src", "  views", "  main.rs", "top.txt"]);
        assert_eq!(lines(&t.rows(Some(&closed(&["src"])))), ["src", "top.txt"]);
    }

    #[test]
    fn folders_count_every_file_below_them() {
        let t = tree(&["src/views/x.rs", "src/views/y.rs", "src/main.rs"]);
        let rows = t.rows(Some(&closed(&["src"])));
        assert_eq!(rows[0].kind, RowKind::Dir { open: false, count: 3 });
    }

    #[test]
    fn folders_differing_only_in_case_stay_apart() {
        let t = tree(&["Src/a.txt", "src/b.txt"]);
        let paths = t.folders();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"Src".to_string()) && paths.contains(&"src".to_string()));
    }

    #[test]
    fn rows_lead_back_to_the_status_entry() {
        let t = tree(&["b/two.txt", "a/one.txt"]);
        let files: Vec<(String, RowKind)> = t.rows(None).into_iter().filter(|r| matches!(r.kind, RowKind::File(_))).map(|r| (r.name, r.kind)).collect();
        assert_eq!(files, [("one.txt".to_string(), RowKind::File(1)), ("two.txt".to_string(), RowKind::File(0))]);
    }

    #[test]
    fn files_in_stops_at_the_end_of_the_folder() {
        let t = tree(&["src/views/x.rs", "src/main.rs", "z/other.txt", "top.txt"]);
        let rows = t.rows(None);
        let at = |name: &str| rows.iter().position(|r| r.name == name).unwrap();
        assert_eq!(files_in(&rows, at("src")), [0, 1]);
        assert_eq!(files_in(&rows, at("views")), [0]);
        assert_eq!(files_in(&rows, at("z")), [2]);
    }

    #[test]
    fn folders_lists_each_joined_folder_once() {
        let t = tree(&["a/b/one.txt", "c/d/two.txt", "c/three.txt"]);
        assert_eq!(t.folders(), ["a/b", "c", "c/d"]);
    }

    #[test]
    fn nothing_changed_means_no_rows() {
        assert!(tree(&[]).rows(None).is_empty());
    }
}
