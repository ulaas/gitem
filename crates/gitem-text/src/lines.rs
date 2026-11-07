//! Commands that work on whole lines: comment, duplicate, move, join, trim.
use crate::{Buffer, Edit, Transaction};

/// The line-comment marker for a file, by its extension. `None` for files that have none.
pub fn comment_prefix(path: &str) -> Option<&'static str> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_ascii_lowercase();
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    Some(match ext {
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "cs" | "java" | "js" | "jsx" | "ts" | "tsx" | "go" | "swift" | "kt" | "kts" | "scala" | "dart" | "zig" | "php" | "groovy" | "gradle" | "proto" | "jsonc" | "glsl" | "wgsl" => "//",
        "py" | "rb" | "sh" | "bash" | "zsh" | "fish" | "pl" | "toml" | "yaml" | "yml" | "r" | "ps1" | "nix" | "cmake" | "conf" | "ini" | "tf" | "jl" | "ex" | "exs" => "#",
        "sql" | "lua" | "hs" | "elm" | "ada" => "--",
        "lisp" | "clj" | "el" | "scm" | "asm" | "s" => ";",
        "bat" | "cmd" => "REM",
        "tex" | "erl" => "%",
        "vim" => "\"",
        _ => match name.as_str() {
            "makefile" | "dockerfile" | ".gitignore" | ".gitattributes" | ".editorconfig" | "cmakelists.txt" => "#",
            _ => return None,
        },
    })
}

/// Comments every line, or uncomments them if all already are. Blank lines are left alone.
/// The marker goes at the shallowest indent, so a block stays lined up.
pub fn toggle_comment(b: &Buffer, lines: &[usize], prefix: &str) -> Transaction {
    let texts: Vec<(usize, String)> = lines.iter().map(|&l| (l, b.line(l))).filter(|(_, t)| !t.trim().is_empty()).collect();
    if texts.is_empty() {
        return Transaction::default();
    }
    let all = texts.iter().all(|(_, t)| t.trim_start().starts_with(prefix));
    let indent = texts.iter().map(|(_, t)| t.chars().take_while(|c| c.is_whitespace()).count()).min().unwrap_or(0);
    let edits = texts
        .iter()
        .rev()
        .map(|(l, t)| {
            let start = b.line_start(*l);
            if all {
                let at = t.chars().take_while(|c| c.is_whitespace()).count();
                let rest: String = t.chars().skip(at + prefix.chars().count()).collect();
                let n = prefix.chars().count() + usize::from(rest.starts_with(' '));
                Edit::delete(start + at, start + at + n)
            } else {
                Edit::insert(start + indent, format!("{prefix} "))
            }
        })
        .collect();
    Transaction::new(edits)
}

/// A copy of the lines `first..=last` below them.
pub fn duplicate(b: &Buffer, first: usize, last: usize) -> Transaction {
    let start = b.line_start(first);
    let end = b.line_end(last);
    let text = b.rope.slice(start..end).to_string();
    Transaction::new(vec![Edit::insert(end, format!("{}{text}", b.eol.as_str()))])
}

/// Lines `first..=last` swapped with the line above (`up`) or below. Nothing at the edge.
pub fn move_lines(b: &Buffer, first: usize, last: usize, up: bool) -> Option<Transaction> {
    let n = b.line_count();
    if (up && first == 0) || (!up && last + 1 >= n) {
        return None;
    }
    let (a, z) = if up { (first - 1, last) } else { (first, last + 1) };
    let block: Vec<String> = (a..=z).map(|l| b.line(l)).collect();
    let mut moved = block.clone();
    if up {
        moved.rotate_left(1);
    } else {
        moved.rotate_right(1);
    }
    let eol = b.eol.as_str();
    Some(Transaction::new(vec![Edit::replace(b.line_start(a), b.line_end(z), moved.join(eol))]))
}

/// Each line after the first joined to the one above with a space, leading space dropped.
pub fn join(b: &Buffer, first: usize, last: usize) -> Transaction {
    let last = if last == first { (first + 1).min(b.line_count() - 1) } else { last };
    let edits = (first + 1..=last)
        .rev()
        .map(|l| {
            let from = b.line_end(l - 1);
            let text = b.line(l);
            let lead = text.chars().take_while(|c| c.is_whitespace()).count();
            Edit::replace(from, b.line_start(l) + lead, if text.trim().is_empty() { "" } else { " " })
        })
        .collect();
    Transaction::new(edits)
}

/// Spaces and tabs at the ends of lines removed.
pub fn trim_trailing(b: &Buffer) -> Transaction {
    let edits = (0..b.line_count())
        .rev()
        .filter_map(|l| {
            let t = b.line(l);
            let kept = t.trim_end_matches([' ', '\t']).chars().count();
            let len = t.chars().count();
            (kept < len).then(|| Edit::delete(b.line_start(l) + kept, b.line_start(l) + len))
        })
        .collect();
    Transaction::new(edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, f: impl Fn(&Buffer) -> Transaction) -> String {
        let mut b = Buffer::from_text(text);
        let t = f(&b);
        b.apply(&t);
        b.text()
    }

    #[test]
    fn comments_toggle_at_the_shallowest_indent() {
        let on = run("fn a() {\n    x\n\n        y\n}", |b| toggle_comment(b, &[1, 2, 3], "//"));
        assert_eq!(on, "fn a() {\n    // x\n\n    //     y\n}");
        let off = run(&on, |b| toggle_comment(b, &[1, 2, 3], "//"));
        assert_eq!(off, "fn a() {\n    x\n\n        y\n}");
        assert_eq!(comment_prefix("src/Main.PY"), Some("#"));
        assert_eq!(comment_prefix("Makefile"), Some("#"));
        assert_eq!(comment_prefix("notes.md"), None);
    }

    #[test]
    fn duplicate_move_join_trim() {
        assert_eq!(run("a\r\nb", |b| duplicate(b, 0, 0)), "a\r\na\r\nb");
        assert_eq!(run("a\nb\nc", |b| move_lines(b, 1, 1, true).unwrap()), "b\na\nc");
        assert_eq!(run("a\nb\nc", |b| move_lines(b, 0, 1, false).unwrap()), "c\na\nb");
        assert!(move_lines(&Buffer::from_text("a\nb"), 1, 1, false).is_none());
        assert_eq!(run("a\n   b\n\nc", |b| join(b, 0, 3)), "a b c");
        assert_eq!(run("a  \nb\t\n c", trim_trailing), "a\nb\n c");
    }
}
