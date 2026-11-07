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
    ("Ctrl+Alt+Z", "Undo the last action"),
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
    ("Ctrl+F", "Find in a file or a diff, or search the graph"),
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

/// Commands whose keys can be changed: `(name, default keys, what it does)`. The rest of
/// `TABLE` is about where you are, such as a list or a dialog, and stays as it is.
pub const BINDABLE: &[(&str, &str, &str)] = &[
    ("palette", "Ctrl+P", "Command palette"),
    ("refresh", "F5", "Reload this repository"),
    ("sidebar", "Ctrl+B", "Show or hide the sidebar"),
    ("changes", "Ctrl+Shift+G", "Changes"),
    ("graph", "Ctrl+Shift+H", "Graph"),
    ("settings", "Ctrl+,", "Settings"),
    ("open", "Ctrl+O", "Open a folder"),
    ("close_tab", "Ctrl+W", "Close this tab"),
    ("fetch", "Ctrl+Shift+F", "Fetch"),
    ("pull", "Ctrl+Shift+L", "Pull"),
    ("push", "Ctrl+Shift+K", "Push"),
    ("undo_last", "Ctrl+Alt+Z", "Undo the last action"),
];

/// Keys as typed: `Ctrl+Shift+F`, `Alt+F7`, `F5`. Ctrl means Cmd on a Mac, as egui has it.
pub fn parse_chord(text: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let mut mods = egui::Modifiers::NONE;
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "cmd" | "command" => mods = mods.plus(egui::Modifiers::COMMAND),
            "shift" => mods = mods.plus(egui::Modifiers::SHIFT),
            "alt" | "option" => mods = mods.plus(egui::Modifiers::ALT),
            "" => return None,
            _ if key.is_some() => return None,
            _ => key = Some(egui::Key::from_name(&part.to_uppercase()).or_else(|| egui::Key::from_name(part))?),
        }
    }
    let key = key?;
    // A bare letter would go off while typing in nothing in particular.
    let plain_ok = matches!(key.name().strip_prefix('F').map(|n| n.parse::<u8>().is_ok()), Some(true));
    (!mods.is_none() || plain_ok).then_some((mods, key))
}

/// What a command is bound to now: the user's keys if they read, otherwise its default.
pub fn chord_for(bindings: &std::collections::HashMap<String, String>, name: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let default = BINDABLE.iter().find(|b| b.0 == name)?.1;
    bindings.get(name).and_then(|t| parse_chord(t)).or_else(|| parse_chord(default))
}

/// How the keys read in F1's list, where they've been changed.
pub fn shown_keys(bindings: &std::collections::HashMap<String, String>, listed: &str) -> String {
    match BINDABLE.iter().find(|b| b.1 == listed) {
        Some(b) => bindings.get(b.0).filter(|t| parse_chord(t).is_some()).cloned().unwrap_or_else(|| listed.to_string()),
        None => listed.to_string(),
    }
}

/// What's wrong with these bindings, to say beside them.
pub fn binding_problems(bindings: &std::collections::HashMap<String, String>) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, _, what) in BINDABLE {
        if let Some(t) = bindings.get(*name).filter(|t| !t.trim().is_empty())
            && parse_chord(t).is_none()
        {
            problems.push(format!("\"{t}\" for {what} can't be read, so its default is in use."));
        }
    }
    for (i, (a, _, what_a)) in BINDABLE.iter().enumerate() {
        for (b, _, what_b) in &BINDABLE[i + 1..] {
            if chord_for(bindings, a) == chord_for(bindings, b) {
                problems.push(format!("{what_a} and {what_b} have the same keys."));
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_read_as_typed_and_a_bare_letter_is_refused() {
        use egui::{Key, Modifiers};
        assert_eq!(parse_chord("Ctrl+Shift+F"), Some((Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::F)));
        assert_eq!(parse_chord("alt + f7"), Some((Modifiers::ALT, Key::F7)));
        assert_eq!(parse_chord("F5"), Some((Modifiers::NONE, Key::F5)));
        assert_eq!(parse_chord("Ctrl+,"), Some((Modifiers::COMMAND, Key::Comma)));
        for bad in ["F", "x", "", "Ctrl+", "Ctrl+A+B", "Hyper+Q", "Ctrl+Nonsense"] {
            assert_eq!(parse_chord(bad), None, "{bad}");
        }
    }

    #[test]
    fn every_default_reads_and_none_collide() {
        for (name, keys, _) in BINDABLE {
            assert!(parse_chord(keys).is_some(), "{name}: {keys}");
        }
        assert_eq!(binding_problems(&Default::default()), Vec::<String>::new());
    }

    #[test]
    fn a_change_wins_a_bad_one_falls_back_and_a_clash_is_said() {
        let mut b = std::collections::HashMap::new();
        b.insert("fetch".to_string(), "Ctrl+Alt+F".to_string());
        assert_eq!(chord_for(&b, "fetch"), parse_chord("Ctrl+Alt+F"));
        assert_eq!(shown_keys(&b, "Ctrl+Shift+F"), "Ctrl+Alt+F");
        assert_eq!(shown_keys(&b, "Ctrl+Shift+L"), "Ctrl+Shift+L");
        b.insert("fetch".to_string(), "nonsense".to_string());
        assert_eq!(chord_for(&b, "fetch"), parse_chord("Ctrl+Shift+F"));
        assert_eq!(binding_problems(&b).len(), 1);
        b.insert("fetch".to_string(), "Ctrl+Shift+L".to_string());
        assert!(binding_problems(&b)[0].contains("same keys"));
    }

    #[test]
    fn every_bindable_default_is_in_the_table_f1_shows() {
        for (_, keys, _) in BINDABLE.iter().filter(|b| b.0 != "undo_last") {
            assert!(TABLE.iter().any(|t| t.0 == *keys), "{keys}");
        }
    }

    #[test]
    fn the_readme_lists_every_key() {
        let readme = include_str!("../../../README.md");
        for (keys, _) in super::TABLE {
            let first = keys.split(", ").next().unwrap().split(" to ").next().unwrap().trim();
            assert!(readme.contains(first), "{keys} isn't in the README");
        }
    }
}
