//! Find and replace over the whole buffer.
use regex::{Regex, RegexBuilder};

use crate::{Buffer, Edit, Transaction};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl Query {
    /// The query as a regex, or why it isn't one.
    pub fn compile(&self) -> Result<Option<Regex>, String> {
        if self.text.is_empty() {
            return Ok(None);
        }
        let mut pattern = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        if self.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        RegexBuilder::new(&pattern).case_insensitive(!self.case_sensitive).multi_line(true).build().map(Some).map_err(|e| e.to_string())
    }
}

/// Every match, as char ranges, in order. Empty matches are skipped: `^` would match every line.
pub fn find_all(b: &Buffer, re: &Regex, limit: usize) -> Vec<(usize, usize)> {
    let text = b.text();
    let mut out = Vec::new();
    // Byte to char offsets, worked out going forward so it's linear.
    let (mut byte, mut chars) = (0usize, 0usize);
    let mut to_chars = |target: usize| {
        chars += text[byte..target].chars().count();
        byte = target;
        chars
    };
    for m in re.find_iter(&text) {
        if m.start() == m.end() {
            continue;
        }
        let s = to_chars(m.start());
        let e = to_chars(m.end());
        out.push((s, e));
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// The first match at or after `from`, wrapping round to the start.
pub fn next(matches: &[(usize, usize)], from: usize) -> Option<(usize, usize)> {
    matches.iter().find(|m| m.0 >= from).or_else(|| matches.first()).copied()
}

/// The last match before `from`, wrapping round to the end.
pub fn prev(matches: &[(usize, usize)], from: usize) -> Option<(usize, usize)> {
    matches.iter().rev().find(|m| m.0 < from).or_else(|| matches.last()).copied()
}

/// The replacement for one match, with `$1` and the like filled in for a regex.
pub fn replacement(b: &Buffer, re: &Regex, m: (usize, usize), with: &str, is_regex: bool) -> String {
    if !is_regex {
        return with.to_string();
    }
    let found = b.rope.slice(m.0..m.1).to_string();
    match re.captures(&found) {
        Some(c) => {
            let mut out = String::new();
            c.expand(with, &mut out);
            out
        }
        None => with.to_string(),
    }
}

/// Every match replaced, as one transaction.
pub fn replace_all(b: &Buffer, re: &Regex, with: &str, is_regex: bool) -> Transaction {
    let matches = find_all(b, re, usize::MAX);
    let edits = matches.iter().rev().map(|&m| Edit::replace(m.0, m.1, replacement(b, re, m, with, is_regex))).collect();
    Transaction::new(edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> Query {
        Query { text: text.into(), ..Default::default() }
    }

    #[test]
    fn finds_in_chars_not_bytes() {
        let b = Buffer::from_text("çay Foo foo\nfoo");
        let re = q("foo").compile().unwrap().unwrap();
        assert_eq!(find_all(&b, &re, 100), vec![(4, 7), (8, 11), (12, 15)]);
        let re = Query { case_sensitive: true, ..q("foo") }.compile().unwrap().unwrap();
        assert_eq!(find_all(&b, &re, 100).len(), 2);
        assert_eq!(next(&find_all(&b, &re, 100), 13), Some((8, 11)), "wraps round");
    }

    #[test]
    fn literal_text_is_not_a_pattern_and_whole_words_stand_alone() {
        let b = Buffer::from_text("a.b axb a.bc");
        assert_eq!(find_all(&b, &q("a.b").compile().unwrap().unwrap(), 9).len(), 2);
        let re = Query { whole_word: true, ..q("a.b") }.compile().unwrap().unwrap();
        assert_eq!(find_all(&b, &re, 9), vec![(0, 3)]);
        assert!(Query { regex: true, ..q("(") }.compile().is_err());
    }

    #[test]
    fn replace_all_with_groups_is_one_step() {
        let mut b = Buffer::from_text("x=1, y=2");
        let re = Query { regex: true, ..q(r"(\w)=(\d)") }.compile().unwrap().unwrap();
        let t = replace_all(&b, &re, "$2=$1", true);
        let undo = b.apply(&t);
        assert_eq!(b.text(), "1=x, 2=y");
        b.apply(&undo);
        assert_eq!(b.text(), "x=1, y=2");
    }
}
