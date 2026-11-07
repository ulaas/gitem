//! A file's text, held so that saving gives back exactly the bytes that weren't edited.
use ropey::Rope;

use crate::edit::Transaction;
use crate::encoding::{self, Encoding, Unsupported};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
        }
    }
}

pub struct Buffer {
    pub rope: Rope,
    /// Goes up with every change, so views can tell their caches are stale.
    pub version: u64,
    /// What a new line gets. Lines already there keep their own.
    pub eol: LineEnding,
    pub encoding: Encoding,
    pub bom: bool,
}

impl Buffer {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Unsupported> {
        let (text, encoding, bom) = encoding::decode(bytes)?;
        let eol = dominant_eol(&text).unwrap_or(LineEnding::Lf);
        Ok(Self { rope: Rope::from_str(&text), version: 0, eol, encoding, bom })
    }

    pub fn from_text(text: &str) -> Self {
        Self { rope: Rope::from_str(text), version: 0, eol: dominant_eol(text).unwrap_or(LineEnding::Lf), encoding: Encoding::Utf8, bom: false }
    }

    /// Whether the text had any line breaks to judge by. An empty or one-line file doesn't,
    /// and the caller may know better, from `.gitattributes`.
    pub fn eol_was_found(&self) -> bool {
        self.rope.len_lines() > 1
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        encoding::encode(&self.text(), self.encoding, self.bom)
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// A line's text without its line break.
    pub fn line(&self, i: usize) -> String {
        let mut s = self.rope.line(i).to_string();
        while s.ends_with(['\n', '\r']) {
            s.pop();
        }
        s
    }

    /// The char index where line `i` starts.
    pub fn line_start(&self, i: usize) -> usize {
        self.rope.line_to_char(i.min(self.rope.len_lines()))
    }

    /// The char index where line `i`'s text ends, before its line break.
    pub fn line_end(&self, i: usize) -> usize {
        self.line_start(i) + self.line(i).chars().count()
    }

    pub fn line_of(&self, pos: usize) -> usize {
        self.rope.char_to_line(pos.min(self.rope.len_chars()))
    }

    /// Line and column (in chars) of a position.
    pub fn point(&self, pos: usize) -> (usize, usize) {
        let pos = pos.min(self.rope.len_chars());
        let line = self.rope.char_to_line(pos);
        (line, pos - self.rope.line_to_char(line))
    }

    /// The position of a line and column, kept inside the line's text.
    pub fn pos(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        self.line_start(line) + col.min(self.line(line).chars().count())
    }

    /// A position moved off the middle of a `\r\n` to the end of its line's text. The rope
    /// counts the gap between the two as a place; no caret should be there.
    pub fn snap(&self, pos: usize) -> usize {
        let (line, col) = self.point(pos);
        self.pos(line, col)
    }

    pub fn apply(&mut self, t: &Transaction) -> Transaction {
        self.version += 1;
        t.apply(&mut self.rope)
    }

    /// Every line ending changed to `eol`, as one transaction so it can be undone.
    pub fn convert_eol(&self, eol: LineEnding) -> Transaction {
        let mut edits = Vec::new();
        for i in (0..self.line_count()).rev() {
            let start = self.line_end(i);
            let end = if i + 1 < self.line_count() { self.line_start(i + 1) } else { self.len_chars() };
            if end > start && self.rope.slice(start..end) != eol.as_str() {
                edits.push(crate::Edit::replace(start, end, eol.as_str()));
            }
        }
        Transaction::new(edits)
    }
}

/// The line ending most lines have, or none if there are no line breaks.
pub fn dominant_eol(text: &str) -> Option<LineEnding> {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    match (lf, crlf) {
        (0, 0) => None,
        (lf, crlf) if crlf > lf => Some(LineEnding::CrLf),
        _ => Some(LineEnding::Lf),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Edit;

    #[test]
    fn mixed_line_endings_survive_an_edit_elsewhere() {
        let bytes = b"one\r\ntwo\nthree\r\n";
        let mut b = Buffer::from_bytes(bytes).unwrap();
        assert_eq!(b.eol, LineEnding::CrLf);
        let at = b.pos(1, 3);
        b.apply(&Transaction::new(vec![Edit::insert(at, "!")]));
        assert_eq!(b.to_bytes(), b"one\r\ntwo!\nthree\r\n");
    }

    #[test]
    fn lines_and_points() {
        let b = Buffer::from_text("ab\r\ncd\n\nlast");
        assert_eq!(b.line_count(), 4);
        assert_eq!((b.line(0), b.line(2), b.line(3)), ("ab".into(), "".into(), "last".into()));
        assert_eq!(b.line_end(0), 2);
        assert_eq!(b.point(b.pos(1, 1)), (1, 1));
        assert_eq!(b.pos(1, 99), b.line_end(1), "a column past the end stays on the line");
        assert_eq!(b.pos(99, 0), b.line_start(3));
    }

    #[test]
    fn converting_line_endings_is_one_undoable_step() {
        let mut b = Buffer::from_text("a\r\nb\nc");
        let t = b.convert_eol(LineEnding::Lf);
        let undo = b.apply(&t);
        assert_eq!(b.text(), "a\nb\nc");
        b.apply(&undo);
        assert_eq!(b.text(), "a\r\nb\nc");
    }
}
