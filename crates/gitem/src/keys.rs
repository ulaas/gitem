//! Every keyboard shortcut, in one table: what F1 shows, and what the README lists.

/// `(keys, what they do)`, the most used first.
pub const TABLE: &[(&str, &str)] = &[
    ("F1", "These shortcuts"),
    ("Ctrl+P", "Command palette: any command, branch or file"),
    ("Ctrl+Shift+G", "Changes"),
    ("Ctrl+Shift+H", "Graph"),
    ("Ctrl+B", "Show or hide the sidebar"),
    ("F5", "Reload this repository"),
    ("Ctrl+O", "Open a folder"),
    ("Ctrl+,", "Settings"),
    ("Ctrl+W", "Close this tab"),
    ("Ctrl+Tab", "Next tab in this pane"),
    ("Ctrl+Shift+Tab", "Previous tab in this pane"),
    ("Ctrl+1 to Ctrl+9", "Go to that repository"),
    ("Ctrl+Shift+F", "Fetch"),
    ("Ctrl+Shift+L", "Pull"),
    ("Ctrl+Shift+K", "Push"),
    ("Ctrl+Shift+Z", "Zen mode, outside a text box"),
    ("Enter", "Submit a dialog, from a one-line field. Answers a question that loses nothing"),
    ("Ctrl+Enter", "Submit the open form. With none open, or from the commit box, commit"),
    ("Esc", "Close a dialog or menu"),
    ("Tab", "Move between controls"),
    ("Shift+F10", "Open the menu of the current row, in Changes and the Graph"),
    ("↑ ↓", "Changes: move through the files. Graph: move through the commits"),
    ("J, K", "Graph: next and previous commit"),
    ("Space", "Changes: stage or unstage the current file"),
    ("Enter", "Changes: open the current file's diff in a tab"),
    ("/", "Graph: find a ref"),
    ("Ctrl+F", "Find in a file, or search the graph"),
    ("F3, Shift+F3", "Next and previous match"),
    ("Alt+↑, Alt+↓", "Diff: previous and next change. Rebase: move the step"),
    ("Alt+B", "File: show or hide blame"),
    ("P, R, E, S, F, D", "Rebase: pick, reword, edit, squash, fixup, drop"),
];

/// The words gitem uses for things, and git's where it has one: what F1 shows and what the README
/// lists, so there's one place to look and to change.
pub const WORDS: &[(&str, &str)] = &[
    ("Check out", "Make a branch or commit the one you're on. Git's `switch`"),
    ("Changes", "What you've edited and not committed. Unstaged until you stage it"),
    ("Stage", "Add to what the next commit holds"),
    ("Graph", "The history, with its branches drawn as lanes"),
    ("Details", "The panel about the selected commit"),
    ("Launchpad", "Pull requests and issues on the repository's host"),
    ("Review", "Your own notes on a diff. Review with AI adds to them"),
    ("Compose", "Split uncommitted changes into several commits, in the Composer"),
    ("Recompose", "The same for commits you've already made"),
    ("Rebase", "Replay commits on another base, or edit them. Git's `rebase -i`"),
    ("Upstream", "The remote branch a branch follows, for its counts, Pull and Push"),
    ("Patches", "A local library of saved changes"),
];

#[cfg(test)]
mod tests {
    #[test]
    fn the_readme_lists_every_key() {
        let readme = include_str!("../../../README.md");
        for (keys, _) in super::TABLE {
            let first = keys.split(", ").next().unwrap().split(" to ").next().unwrap().trim();
            assert!(readme.contains(first), "{keys} isn't in the README");
        }
    }
}
