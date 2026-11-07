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
    ("Ctrl+click", "Sidebar: pick several branches, to delete together"),
    ("J, K", "Graph: next and previous commit"),
    ("Space", "Changes: stage or unstage the current file"),
    ("Enter", "Changes: open the current file's diff in a tab"),
    ("/", "Graph: find a ref"),
    ("Ctrl+F", "Find in a file or a diff, or search the graph"),
    ("F3, Shift+F3", "Next and previous match"),
    ("Alt+↑, Alt+↓", "Diff: previous and next change"),
    ("Alt+Up, Alt+Down", "Rebase: move the step"),
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

/// Commands whose keys can be changed: `(name, default keys, what it does)`. See also `CONTEXT`.
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

/// A key that means something in one kind of view, which can be changed too.
pub struct ContextKey {
    pub name: &'static str,
    pub default: &'static str,
    pub what: &'static str,
    /// Where it acts. Two keys only get in each other's way if they share one of these.
    pub places: &'static [&'static str],
    /// It only goes off with nothing being typed, so a plain letter is safe for it.
    pub plain_ok: bool,
}

const fn ck(name: &'static str, default: &'static str, what: &'static str, places: &'static [&'static str], plain_ok: bool) -> ContextKey {
    ContextKey { name, default, what, places, plain_ok }
}

pub const CONTEXT: &[ContextKey] = &[
    ck("graph_next", "J", "Graph: next commit", &["Graph"], true),
    ck("graph_prev", "K", "Graph: previous commit", &["Graph"], true),
    ck("graph_find_ref", "/", "Graph: find a ref", &["Graph"], true),
    ck("changes_stage", "Space", "Changes: stage or unstage the current file", &["Changes"], true),
    ck("row_menu", "Shift+F10", "Changes and Graph: menu of the current row", &["Changes", "Graph"], true),
    ck("find", "Ctrl+F", "Find in a file or a diff, or search the graph", &["Graph", "Diff", "File"], false),
    ck("find_next", "F3", "Next match", &["Graph", "Diff", "File"], false),
    ck("find_prev", "Shift+F3", "Previous match", &["Graph", "Diff", "File"], false),
    ck("diff_prev", "Alt+Up", "Diff: previous change", &["Diff"], false),
    ck("diff_next", "Alt+Down", "Diff: next change", &["Diff"], false),
    ck("file_blame", "Alt+B", "File: show or hide blame", &["File"], false),
    ck("rebase_pick", "P", "Rebase: pick", &["Rebase"], true),
    ck("rebase_reword", "R", "Rebase: reword", &["Rebase"], true),
    ck("rebase_edit", "E", "Rebase: edit", &["Rebase"], true),
    ck("rebase_squash", "S", "Rebase: squash", &["Rebase"], true),
    ck("rebase_fixup", "F", "Rebase: fixup", &["Rebase"], true),
    ck("rebase_drop", "D", "Rebase: drop", &["Rebase"], true),
    ck("rebase_up", "Alt+Up", "Rebase: move the step up", &["Rebase"], false),
    ck("rebase_down", "Alt+Down", "Rebase: move the step down", &["Rebase"], false),
];

/// What F1's rows are made of, for showing them as they're bound now.
const CONTEXT_ROWS: &[(&str, &[&str])] = &[
    ("J, K", &["graph_next", "graph_prev"]),
    ("/", &["graph_find_ref"]),
    ("Space", &["changes_stage"]),
    ("Shift+F10", &["row_menu"]),
    ("Ctrl+F", &["find"]),
    ("F3, Shift+F3", &["find_next", "find_prev"]),
    ("Alt+↑, Alt+↓", &["diff_prev", "diff_next"]),
    ("Alt+Up, Alt+Down", &["rebase_up", "rebase_down"]),
    ("Alt+B", &["file_blame"]),
    ("P, R, E, S, F, D", &["rebase_pick", "rebase_reword", "rebase_edit", "rebase_squash", "rebase_fixup", "rebase_drop"]),
];

/// Said beside the settings, and why: these are how you get around at all, in every list and
/// dialog and in egui's own focus handling, so a change could leave a way in with no way out.
pub const FIXED_KEYS: &str = "These stay as they are: F1, Esc, Enter, Tab, the arrow keys, Ctrl+Enter, Ctrl+Tab, Ctrl+1 to Ctrl+9, and Ctrl+Shift+Z.";

/// Whether these keys belong to what's in `FIXED_KEYS`.
fn is_fixed((mods, key): (egui::Modifiers, egui::Key)) -> bool {
    use egui::{Key, Modifiers as M};
    const CS: M = M::COMMAND.plus(M::SHIFT);
    let numbers = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
    (mods.is_none() && matches!(key, Key::F1 | Key::Escape | Key::Enter | Key::Tab | Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight))
        || (mods == M::SHIFT && key == Key::Tab)
        || (mods == M::COMMAND && (matches!(key, Key::Enter | Key::Tab) || numbers.contains(&key)))
        || (mods == CS && matches!(key, Key::Tab | Key::Z))
}

fn context_key(name: &str) -> Option<&'static ContextKey> {
    CONTEXT.iter().find(|c| c.name == name)
}

fn read_context(c: &ContextKey, text: &str) -> Option<(egui::Modifiers, egui::Key)> {
    if c.plain_ok { parse_keys(text) } else { parse_chord(text) }
}

/// What's been asked for, before asking whether it can have it.
fn context_wanted(bindings: &std::collections::HashMap<String, String>, c: &ContextKey) -> Option<(egui::Modifiers, egui::Key)> {
    bindings.get(c.name).and_then(|t| read_context(c, t)).or_else(|| parse_keys(c.default))
}

/// Why this key can't have what's been asked for, if it can't. Then it keeps its default, so
/// that one key never does two things in a view.
fn context_clash(bindings: &std::collections::HashMap<String, String>, c: &ContextKey, wanted: (egui::Modifiers, egui::Key)) -> Option<String> {
    if is_fixed(wanted) {
        return Some("is one of the keys that stay as they are".to_string());
    }
    if let Some((_, _, what)) = BINDABLE.iter().find(|(name, _, _)| chord_for(bindings, name) == Some(wanted)) {
        return Some(format!("is already {what}"));
    }
    let other = CONTEXT.iter().find(|o| o.name != c.name && o.places.iter().any(|p| c.places.contains(p)) && context_wanted(bindings, o) == Some(wanted))?;
    Some(format!("is already {}", other.what))
}

/// What a context key is bound to now.
pub fn context_chord(bindings: &std::collections::HashMap<String, String>, name: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let c = context_key(name)?;
    let default = parse_keys(c.default);
    let Some(wanted) = bindings.get(name).and_then(|t| read_context(c, t)) else { return default };
    if context_clash(bindings, c, wanted).is_some() { default } else { Some(wanted) }
}

/// Whether these keys were pressed just now, with no others held.
pub fn context_pressed(i: &egui::InputState, bindings: &std::collections::HashMap<String, String>, name: &str) -> bool {
    context_chord(bindings, name).is_some_and(|(m, k)| i.modifiers.matches_exact(m) && i.key_pressed(k))
}

/// The same, keeping them from anything else that would act on them.
pub fn context_take(i: &mut egui::InputState, bindings: &std::collections::HashMap<String, String>, name: &str) -> bool {
    context_chord(bindings, name).is_some_and(|(m, k)| i.consume_key(m, k))
}

/// How they read now, for a tooltip.
pub fn context_label(bindings: &std::collections::HashMap<String, String>, name: &str) -> String {
    let default = context_key(name).map_or("", |c| c.default);
    match bindings.get(name) {
        Some(t) if context_chord(bindings, name) != parse_keys(default) => t.trim().to_string(),
        _ => default.to_string(),
    }
}

/// Keys as typed: `Ctrl+Shift+F`, `Alt+F7`, `F5`. Ctrl means Cmd on a Mac, as egui has it.
pub fn parse_chord(text: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let (mods, key) = parse_keys(text)?;
    // A bare letter would go off while typing in nothing in particular.
    let plain_ok = matches!(key.name().strip_prefix('F').map(|n| n.parse::<u8>().is_ok()), Some(true));
    (!mods.is_none() || plain_ok).then_some((mods, key))
}

/// The same with a bare key allowed, for what only acts with nothing being typed.
fn parse_keys(text: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let mut mods = egui::Modifiers::NONE;
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "cmd" | "command" => mods = mods.plus(egui::Modifiers::COMMAND),
            "shift" => mods = mods.plus(egui::Modifiers::SHIFT),
            "alt" | "option" => mods = mods.plus(egui::Modifiers::ALT),
            "" => return None,
            _ if key.is_some() => return None,
            _ => {
                // `space` and `up`, which egui only knows capitalised.
                let mut word = part.to_ascii_lowercase();
                if let Some(first) = word.get_mut(..1) {
                    first.make_ascii_uppercase();
                }
                key = Some(egui::Key::from_name(&part.to_uppercase()).or_else(|| egui::Key::from_name(part)).or_else(|| egui::Key::from_name(&word))?);
            }
        }
    }
    Some((mods, key?))
}

/// What a command is bound to now: the user's keys if they read, otherwise its default.
pub fn chord_for(bindings: &std::collections::HashMap<String, String>, name: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let default = BINDABLE.iter().find(|b| b.0 == name)?.1;
    bindings.get(name).and_then(|t| parse_chord(t)).or_else(|| parse_chord(default))
}

/// How the keys read in F1's list, where they've been changed.
pub fn shown_keys(bindings: &std::collections::HashMap<String, String>, listed: &str) -> String {
    if let Some((_, names)) = CONTEXT_ROWS.iter().find(|(row, _)| *row == listed) {
        // The row's own spelling until something on it is changed: it has arrows in it.
        return if names.iter().any(|n| context_label(bindings, n) != context_key(n).map_or("", |c| c.default)) { names.iter().map(|n| context_label(bindings, n)).collect::<Vec<_>>().join(", ") } else { listed.to_string() };
    }
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
    for c in CONTEXT {
        let Some(t) = bindings.get(c.name).filter(|t| !t.trim().is_empty()) else { continue };
        match read_context(c, t) {
            None => problems.push(format!("\"{t}\" for {} can't be read, so its default is in use.", c.what)),
            Some(wanted) => {
                if let Some(why) = context_clash(bindings, c, wanted) {
                    problems.push(format!("\"{t}\" for {} {why}, so its default is in use.", c.what));
                }
            }
        }
    }
    // Commands act everywhere, so one moved onto a view's default key would take it from the view.
    for (name, _, what) in BINDABLE.iter().filter(|b| bindings.contains_key(b.0)) {
        if let Some(c) = CONTEXT.iter().find(|c| !bindings.contains_key(c.name) && parse_keys(c.default) == chord_for(bindings, name)) {
            problems.push(format!("{what} now has the keys of {}.", c.what));
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

    fn bound(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_context_key_can_be_changed_and_a_plain_key_only_where_nothing_is_being_typed() {
        use egui::{Key, Modifiers as M};
        let b = bound(&[("graph_next", "n"), ("changes_stage", "x"), ("find", "Alt+G")]);
        assert_eq!(context_chord(&b, "graph_next"), Some((M::NONE, Key::N)));
        assert_eq!(context_chord(&b, "changes_stage"), Some((M::NONE, Key::X)));
        assert_eq!(context_chord(&b, "find"), Some((M::ALT, Key::G)));
        assert_eq!(context_chord(&b, "graph_prev"), Some((M::NONE, Key::K)), "the rest stay");
        assert_eq!(context_label(&b, "graph_next"), "n");
        assert_eq!(context_label(&b, "graph_prev"), "K");
        assert_eq!(binding_problems(&b), Vec::<String>::new());
        // Find works from inside a text box, where a plain letter is someone typing.
        let plain = bound(&[("find", "x")]);
        assert_eq!(context_chord(&plain, "find"), Some((M::COMMAND, Key::F)));
        assert!(binding_problems(&plain)[0].contains("can't be read"), "{:?}", binding_problems(&plain));
    }

    #[test]
    fn a_key_already_doing_something_there_is_refused_and_said() {
        use egui::{Key, Modifiers as M};
        // Another in the same place.
        let b = bound(&[("graph_next", "K")]);
        assert_eq!(context_chord(&b, "graph_next"), Some((M::NONE, Key::J)));
        assert!(binding_problems(&b)[0].contains("already Graph: previous commit"), "{:?}", binding_problems(&b));
        // The same keys in different places are fine: Alt+Up is both of these by default, and K
        // here doesn't touch the graph's.
        let b = bound(&[("rebase_pick", "K")]);
        assert_eq!(context_chord(&b, "rebase_pick"), Some((M::NONE, Key::K)));
        assert_eq!(binding_problems(&b), Vec::<String>::new());
        // A command, which acts everywhere.
        let b = bound(&[("graph_next", "Ctrl+Shift+F")]);
        assert_eq!(context_chord(&b, "graph_next"), Some((M::NONE, Key::J)));
        assert!(binding_problems(&b)[0].contains("already Fetch"), "{:?}", binding_problems(&b));
        // What stays fixed.
        for fixed in ["Enter", "Esc", "Tab", "Up", "Down", "F1", "Ctrl+Enter", "Ctrl+Tab", "Ctrl+3", "Ctrl+Shift+Z"] {
            let b = bound(&[("graph_next", fixed)]);
            assert_eq!(context_chord(&b, "graph_next"), Some((M::NONE, Key::J)), "{fixed}");
            assert!(binding_problems(&b)[0].contains("stay as they are"), "{fixed}: {:?}", binding_problems(&b));
        }
        // Swapping two works, as neither ends up on the other.
        let b = bound(&[("graph_next", "K"), ("graph_prev", "J")]);
        assert_eq!(context_chord(&b, "graph_next"), Some((M::NONE, Key::K)));
        assert_eq!(context_chord(&b, "graph_prev"), Some((M::NONE, Key::J)));
        assert_eq!(binding_problems(&b), Vec::<String>::new());
        // A command moved onto a view's default key is said too.
        let b = bound(&[("fetch", "Ctrl+F")]);
        assert!(binding_problems(&b)[0].contains("Fetch now has the keys of Find"), "{:?}", binding_problems(&b));
    }

    #[test]
    fn f1_shows_a_row_as_it_is_bound_now() {
        assert_eq!(shown_keys(&Default::default(), "J, K"), "J, K");
        assert_eq!(shown_keys(&Default::default(), "Alt+↑, Alt+↓"), "Alt+↑, Alt+↓");
        assert_eq!(shown_keys(&bound(&[("graph_next", "N")]), "J, K"), "N, K");
        // Every row named here has to be in the table, or it would stop being shown.
        for (row, _) in CONTEXT_ROWS {
            assert!(TABLE.iter().any(|t| t.0 == *row), "{row}");
        }
        for c in CONTEXT {
            assert!(CONTEXT_ROWS.iter().any(|(_, names)| names.contains(&c.name)), "{}", c.name);
        }
    }

    #[test]
    fn no_default_context_key_clashes() {
        assert_eq!(binding_problems(&Default::default()), Vec::<String>::new());
        for c in CONTEXT {
            assert!(parse_keys(c.default).is_some(), "{}", c.name);
            assert!(!is_fixed(parse_keys(c.default).unwrap()), "{}", c.name);
            assert!(context_clash(&Default::default(), c, parse_keys(c.default).unwrap()).is_none(), "{}", c.name);
        }
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
