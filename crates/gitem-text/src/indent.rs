//! Tabs or spaces, and how far to indent a new line.
use crate::{Buffer, Edit, Transaction};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    Tabs,
    Spaces(u8),
}

impl Indent {
    pub fn unit(self) -> String {
        match self {
            Indent::Tabs => "\t".into(),
            Indent::Spaces(n) => " ".repeat(n as usize),
        }
    }

    pub fn name(self) -> String {
        match self {
            Indent::Tabs => "Tabs".into(),
            Indent::Spaces(n) => format!("Spaces: {n}"),
        }
    }
}

/// What the file already uses, from the lines that are indented. `None` when no line is,
/// and the setting decides.
pub fn detect(b: &Buffer) -> Option<Indent> {
    let (mut tabs, mut spaces) = (0, 0);
    // How often each step between consecutive space indents occurs, 2 to 8.
    let mut steps = [0usize; 9];
    let mut last = 0usize;
    for i in 0..b.line_count().min(10_000) {
        let line = b.line(i);
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            tabs += 1;
        } else if line.starts_with(' ') {
            spaces += 1;
        }
        let width = line.chars().take_while(|c| *c == ' ').count();
        if !line.starts_with('\t') {
            let d = width.abs_diff(last);
            if (2..=8).contains(&d) {
                steps[d] += 1;
            }
            last = width;
        }
    }
    if tabs == 0 && spaces == 0 {
        return None;
    }
    if tabs > spaces {
        return Some(Indent::Tabs);
    }
    // A tie goes to the smaller step: 4-space files also have 8-space jumps.
    let best = (2..=8).max_by_key(|&n| (steps[n], std::cmp::Reverse(n))).filter(|&n| steps[n] > 0).unwrap_or(4);
    Some(Indent::Spaces(best as u8))
}

/// What goes after a line break typed at `pos`: the line's own indent, one more after an
/// opening bracket or a colon at the end of what comes before.
pub fn newline_indent(b: &Buffer, pos: usize, unit: &str) -> String {
    let (line, col) = b.point(pos);
    let text: String = b.line(line).chars().take(col).collect();
    let own: String = text.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let opens = text.trim_end().ends_with(['{', '(', '[', ':']);
    if opens { format!("{own}{unit}") } else { own }
}

/// One unit more on every line the selections touch.
pub fn indent_lines(b: &Buffer, lines: &[usize], unit: &str) -> Transaction {
    let edits = lines.iter().rev().filter(|&&l| !b.line(l).is_empty()).map(|&l| Edit::insert(b.line_start(l), unit)).collect();
    Transaction::new(edits)
}

/// One unit less: a tab, or up to a unit's width of spaces.
pub fn outdent_lines(b: &Buffer, lines: &[usize], unit: &str) -> Transaction {
    let width = unit.chars().count().max(1);
    let edits = lines
        .iter()
        .rev()
        .filter_map(|&l| {
            let text = b.line(l);
            let n = if text.starts_with('\t') { 1 } else { text.chars().take(width).take_while(|c| *c == ' ').count() };
            (n > 0).then(|| Edit::delete(b.line_start(l), b.line_start(l) + n))
        })
        .collect();
    Transaction::new(edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_what_a_file_uses() {
        assert_eq!(detect(&Buffer::from_text("fn a() {\n    if x {\n        y\n    }\n}\n")), Some(Indent::Spaces(4)));
        assert_eq!(detect(&Buffer::from_text("a:\n  b:\n    c\n  d\n")), Some(Indent::Spaces(2)));
        assert_eq!(detect(&Buffer::from_text("a {\n\tb\n\t\tc\n}\n")), Some(Indent::Tabs));
        assert_eq!(detect(&Buffer::from_text("flat\ntext\n")), None);
    }

    #[test]
    fn a_new_line_keeps_and_adds_indent() {
        let b = Buffer::from_text("    if x {\n    y");
        assert_eq!(newline_indent(&b, b.line_end(0), "    "), "        ");
        assert_eq!(newline_indent(&b, b.line_end(1), "    "), "    ");
        assert_eq!(newline_indent(&b, 2, "    "), "  ", "only what's before the caret counts");
    }

    #[test]
    fn indent_and_outdent_skip_what_they_cant_change() {
        let mut b = Buffer::from_text("a\n\n  b\n\tc");
        let t = indent_lines(&b, &[0, 1, 2], "    ");
        b.apply(&t);
        assert_eq!(b.text(), "    a\n\n      b\n\tc");
        let t = outdent_lines(&b, &[0, 1, 2, 3], "    ");
        b.apply(&t);
        assert_eq!(b.text(), "a\n\n  b\nc");
    }
}
