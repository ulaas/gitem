//! Carets and selections, one or many, and how they move.
use unicode_segmentation::UnicodeSegmentation;

use crate::Buffer;
use crate::edit::Transaction;

/// From `anchor`, where it started, to `head`, where the caret is. Equal when nothing is selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// The column up and down try to keep, so a short line passed on the way doesn't pull
    /// the caret left for good. `None` until the caret moves up or down.
    pub goal: Option<usize>,
}

impl Selection {
    pub fn caret(pos: usize) -> Self {
        Self { anchor: pos, head: pos, goal: None }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head, goal: None }
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Moved to `pos`, keeping the anchor if `extend`, as Shift does.
    pub fn to(self, pos: usize, extend: bool) -> Self {
        Self { anchor: if extend { self.anchor } else { pos }, head: pos, goal: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    /// First non-blank char, then column 0 on a second press, as Zed and VS Code do.
    Home,
    End,
    DocStart,
    DocEnd,
    /// By this many lines, for Page Up and Page Down.
    Lines(isize),
}

/// Every caret, sorted and never overlapping. There is always at least one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionSet {
    sels: Vec<Selection>,
    /// The one the view follows and Escape keeps.
    primary: usize,
}

impl Default for SelectionSet {
    fn default() -> Self {
        Self::single(Selection::caret(0))
    }
}

impl SelectionSet {
    pub fn single(s: Selection) -> Self {
        Self { sels: vec![s], primary: 0 }
    }

    pub fn from_vec(sels: Vec<Selection>, primary: usize) -> Self {
        let keep = sels.get(primary).copied();
        let mut set = Self { sels, primary: 0 };
        set.normalize(keep);
        set
    }

    pub fn iter(&self) -> impl Iterator<Item = &Selection> {
        self.sels.iter()
    }

    pub fn len(&self) -> usize {
        self.sels.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn primary(&self) -> Selection {
        self.sels[self.primary]
    }

    pub fn as_slice(&self) -> &[Selection] {
        &self.sels
    }

    /// Adds a caret or selection, merging it with any it touches.
    pub fn add(&mut self, s: Selection) {
        self.sels.push(s);
        self.normalize(Some(s));
    }

    /// Back to the primary caret alone.
    pub fn collapse_to_primary(&mut self) {
        *self = Self::single(self.primary());
    }

    pub fn map(&mut self, f: impl Fn(Selection) -> Selection) {
        let keep = f(self.primary());
        self.sels = self.sels.iter().map(|s| f(*s)).collect();
        self.normalize(Some(keep));
    }

    /// Each selection moved, or extended with Shift. A selection with no Shift collapses to
    /// its near edge on Left and Right, as every editor does.
    pub fn motion(&mut self, b: &Buffer, m: Move, extend: bool) {
        self.map(|s| {
            if !extend && !s.is_empty() && matches!(m, Move::Left | Move::Right) {
                let pos = if m == Move::Left { s.start() } else { s.end() };
                return Selection::caret(pos);
            }
            let (pos, goal) = moved(b, s, m);
            Selection { anchor: if extend { s.anchor } else { pos }, head: pos, goal }
        });
    }

    /// Where the carets go after a transaction someone else made, such as a reload or the
    /// other side of an undo.
    pub fn after(&mut self, t: &Transaction) {
        self.map(|s| Selection { anchor: t.map(s.anchor), head: t.map(s.head), goal: None });
    }

    /// Clamped to the text, after it got shorter.
    pub fn clamp(&mut self, len: usize) {
        self.map(|s| Selection { anchor: s.anchor.min(len), head: s.head.min(len), goal: s.goal });
    }

    fn normalize(&mut self, primary: Option<Selection>) {
        self.sels.sort_by_key(|s| (s.start(), s.end()));
        let mut out: Vec<Selection> = Vec::with_capacity(self.sels.len());
        for s in self.sels.drain(..) {
            match out.last_mut() {
                // Carets at the same spot, or selections that overlap, become one.
                Some(last) if s.start() < last.end() || (s.start() == last.start() && s.end() == last.end()) => {
                    let (start, end) = (last.start(), last.end().max(s.end()));
                    *last = if last.head < last.anchor { Selection::new(end, start) } else { Selection::new(start, end) };
                }
                _ => out.push(s),
            }
        }
        if out.is_empty() {
            out.push(Selection::caret(0));
        }
        self.primary = primary.and_then(|p| out.iter().position(|s| s.start() <= p.head && p.head <= s.end())).unwrap_or(out.len() - 1);
        self.sels = out;
    }
}

fn moved(b: &Buffer, s: Selection, m: Move) -> (usize, Option<usize>) {
    let pos = s.head.min(b.len_chars());
    let (line, col) = b.point(pos);
    let vertical = |delta: isize| {
        let goal = s.goal.unwrap_or(col);
        let last = b.line_count().saturating_sub(1);
        let target = line as isize + delta;
        if target < 0 {
            return (0, Some(goal));
        }
        if target as usize > last {
            return (b.len_chars(), Some(goal));
        }
        (b.pos(target as usize, goal), Some(goal))
    };
    match m {
        Move::Left => (prev_grapheme(b, pos), None),
        Move::Right => (next_grapheme(b, pos), None),
        Move::Up => vertical(-1),
        Move::Down => vertical(1),
        Move::Lines(n) => vertical(n),
        Move::WordLeft => (word_left(b, pos), None),
        Move::WordRight => (word_right(b, pos), None),
        Move::Home => {
            let text = b.line(line);
            let indent = text.chars().take_while(|c| c.is_whitespace()).count();
            let to = if col == indent { 0 } else { indent };
            (b.line_start(line) + to, None)
        }
        Move::End => (b.line_end(line), None),
        Move::DocStart => (0, None),
        Move::DocEnd => (b.len_chars(), None),
    }
}

/// A line break counts as one step, \r\n included.
pub fn prev_grapheme(b: &Buffer, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let (line, col) = b.point(pos);
    if col == 0 {
        return b.line_end(line - 1);
    }
    let text = b.line(line);
    let byte = char_to_byte(&text, col);
    let start = text[..byte].grapheme_indices(true).next_back().map_or(0, |(i, _)| i);
    b.line_start(line) + text[..start].chars().count()
}

pub fn next_grapheme(b: &Buffer, pos: usize) -> usize {
    let (line, col) = b.point(pos);
    let text = b.line(line);
    let len = text.chars().count();
    if col >= len {
        return if line + 1 < b.line_count() { b.line_start(line + 1) } else { b.len_chars() };
    }
    let byte = char_to_byte(&text, col);
    let g = text[byte..].graphemes(true).next().map_or(1, |g| g.chars().count());
    pos + g
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// Past spaces, then past a run of word chars or of punctuation. At a line's start, to the
/// end of the line above.
pub fn word_left(b: &Buffer, pos: usize) -> usize {
    let (line, col) = b.point(pos);
    if col == 0 {
        return prev_grapheme(b, pos);
    }
    let chars: Vec<char> = b.line(line).chars().collect();
    let mut i = col.min(chars.len());
    while i > 0 && class(chars[i - 1]) == Class::Space {
        i -= 1;
    }
    if i > 0 {
        let c = class(chars[i - 1]);
        while i > 0 && class(chars[i - 1]) == c {
            i -= 1;
        }
    }
    b.line_start(line) + i
}

pub fn word_right(b: &Buffer, pos: usize) -> usize {
    let (line, col) = b.point(pos);
    let chars: Vec<char> = b.line(line).chars().collect();
    if col >= chars.len() {
        return next_grapheme(b, pos);
    }
    let mut i = col;
    while i < chars.len() && class(chars[i]) == Class::Space {
        i += 1;
    }
    if i < chars.len() {
        let c = class(chars[i]);
        while i < chars.len() && class(chars[i]) == c {
            i += 1;
        }
    }
    b.line_start(line) + i
}

/// The word around `pos`, for a double click. Spaces or punctuation select their own run.
pub fn word_at(b: &Buffer, pos: usize) -> (usize, usize) {
    let (line, col) = b.point(pos);
    let chars: Vec<char> = b.line(line).chars().collect();
    if chars.is_empty() {
        return (pos, pos);
    }
    // Just after a word, as a click at its end lands, still means that word.
    let after_word = col > 0 && class(chars[col - 1]) == Class::Word && chars.get(col).is_none_or(|c| class(*c) != Class::Word);
    let at = if after_word { col - 1 } else { col.min(chars.len() - 1) };
    let c = class(chars[at]);
    let (mut s, mut e) = (at, at + 1);
    while s > 0 && class(chars[s - 1]) == c {
        s -= 1;
    }
    while e < chars.len() && class(chars[e]) == c {
        e += 1;
    }
    let base = b.line_start(line);
    (base + s, base + e)
}

fn char_to_byte(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret_after(b: &Buffer, start: usize, moves: &[Move]) -> usize {
        let mut s = SelectionSet::single(Selection::caret(start));
        for m in moves {
            s.motion(b, *m, false);
        }
        s.primary().head
    }

    #[test]
    fn up_and_down_keep_their_column_past_a_short_line() {
        let b = Buffer::from_text("long line here\nab\nanother long one");
        let start = b.pos(0, 10);
        assert_eq!(b.point(caret_after(&b, start, &[Move::Down])), (1, 2));
        assert_eq!(b.point(caret_after(&b, start, &[Move::Down, Move::Down])), (2, 10));
        assert_eq!(caret_after(&b, start, &[Move::Up]), 0, "up from the first line goes to its start");
    }

    #[test]
    fn left_and_right_step_over_a_whole_grapheme_and_a_crlf() {
        let b = Buffer::from_text("e\u{301}x\r\ny");
        assert_eq!(caret_after(&b, 0, &[Move::Right]), 2);
        assert_eq!(caret_after(&b, 3, &[Move::Right]), 5, "\\r\\n is one step");
        assert_eq!(caret_after(&b, 5, &[Move::Left]), 3);
        assert_eq!(caret_after(&b, 2, &[Move::Left]), 0);
    }

    #[test]
    fn word_moves() {
        let b = Buffer::from_text("let foo_bar = baz();");
        assert_eq!(caret_after(&b, 0, &[Move::WordRight, Move::WordRight]), 11);
        assert_eq!(caret_after(&b, 20, &[Move::WordLeft]), 17, "past the run of punctuation");
        assert_eq!(word_at(&b, 6), (4, 11));
        assert_eq!(word_at(&b, 11), (4, 11), "a click just after a word");
    }

    #[test]
    fn home_toggles_between_indent_and_column_zero() {
        let b = Buffer::from_text("    code");
        assert_eq!(caret_after(&b, 8, &[Move::Home]), 4);
        assert_eq!(caret_after(&b, 8, &[Move::Home, Move::Home]), 0);
    }

    #[test]
    fn carets_that_meet_become_one() {
        let b = Buffer::from_text("abc");
        let mut s = SelectionSet::from_vec(vec![Selection::caret(1), Selection::caret(2)], 1);
        s.motion(&b, Move::DocStart, false);
        assert_eq!(s.len(), 1);
        let mut s = SelectionSet::from_vec(vec![Selection::new(0, 2), Selection::new(1, 3)], 0);
        assert_eq!(s.as_slice(), &[Selection::new(0, 3)]);
        s.add(Selection::caret(3));
        assert_eq!(s.len(), 2, "a caret just past a selection is its own");
    }

    #[test]
    fn shift_extends_and_a_plain_arrow_collapses() {
        let b = Buffer::from_text("abcdef");
        let mut s = SelectionSet::single(Selection::caret(2));
        s.motion(&b, Move::Right, true);
        s.motion(&b, Move::Right, true);
        assert_eq!(s.primary(), Selection::new(2, 4));
        s.motion(&b, Move::Left, false);
        assert_eq!(s.primary(), Selection::caret(2));
    }
}
