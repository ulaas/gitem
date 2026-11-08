//! The code editor: a buffer, its carets and undo, and the commands that change them. Drawing
//! and input are in `view`, so everything here can be tested without a window.
pub mod highlight;
pub mod view;

use gitem_text::brackets;
use gitem_text::history::Kind;
use gitem_text::indent::{self, Indent};
use gitem_text::lines;
use gitem_text::search::{self, Query};
use gitem_text::selection::{self, Move};
use gitem_text::{Buffer, Edit, History, Selection, SelectionSet, Transaction};

use highlight::Highlight;

/// Something the editor can be asked to do, by a key, a menu or the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd {
    Type(String),
    Paste(String),
    Newline,
    /// A new line below this one, wherever the caret is in it.
    NewlineBelow,
    Backspace { word: bool },
    Delete { word: bool },
    Tab,
    Outdent,
    Move(Move, bool),
    SelectAll,
    SelectLine,
    Undo,
    Redo,
    ToggleComment,
    DuplicateLines,
    MoveLines { up: bool },
    DeleteLines,
    JoinLines,
    /// The word at the caret, or the next place the selected text appears, as another selection.
    SelectNext,
    SelectAllMatches,
    /// Back to one caret.
    Collapse,
    /// Carets above or below each one, for column editing from the keyboard.
    AddCaret { up: bool },
    /// Folds the block the caret's line opens, or unfolds the fold it's in.
    Fold,
    Unfold,
    FoldAll,
    UnfoldAll,
}

/// What the palette or a menu asks of the editor in front, beyond a plain command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Run(Cmd),
    Find,
    Replace,
    GoToLine,
    Save,
    TrimTrailing,
    /// Find in files, starting from the word or selection at the caret.
    FindInFiles,
}

/// Find and replace, and the matches for the text as it is.
#[derive(Default)]
pub struct Find {
    pub open: bool,
    pub replacing: bool,
    pub query: Query,
    pub with: String,
    pub matches: Vec<(usize, usize)>,
    pub error: Option<String>,
    /// What the matches were found for, so they're only found again when one of them changes.
    found_for: Option<(u64, Query)>,
    /// The query box takes focus the frame after it opens.
    pub focus_query: bool,
}

/// Over this many, matches stop being counted. Highlighting a million `e`s helps nobody.
const MAX_MATCHES: usize = 10_000;

pub struct Editor {
    pub buf: Buffer,
    pub sels: SelectionSet,
    pub hist: History,
    pub hl: Highlight,
    pub indent: Indent,
    pub path: String,
    /// Shown but not changed, as for a file in an encoding that can't be written back.
    pub read_only: bool,
    pub find: Find,
    /// Bring the primary caret into view on the next frame.
    pub reveal: bool,
    /// The widest line seen, in chars, for the horizontal scroll. Only grows while editing.
    pub max_cols: usize,
    pub goto: Option<String>,
    /// Take the keyboard on the next frame, as after the go-to box closes.
    pub focus: bool,
    /// Folded blocks: the line that stays, and the last line hidden under it.
    pub folds: Vec<(usize, usize)>,
    /// Goes up when the folds change, so the view lays rows out again.
    pub folds_gen: u64,
    /// The comment marker for this kind of file.
    comment: Option<&'static str>,
    view: view::ViewState,
}

impl Editor {
    pub fn new(path: &str, buf: Buffer, dark: bool, default_indent: Indent) -> Self {
        let indent = indent::detect(&buf).unwrap_or(default_indent);
        let max_cols = (0..buf.line_count()).map(|i| buf.rope.line(i).len_chars()).max().unwrap_or(0);
        Self {
            hl: Highlight::new(path, dark),
            buf,
            sels: SelectionSet::default(),
            hist: History::default(),
            indent,
            path: path.to_string(),
            read_only: false,
            find: Find::default(),
            reveal: false,
            max_cols,
            goto: None,
            focus: false,
            folds: Vec::new(),
            folds_gen: 0,
            comment: lines::comment_prefix(path),
            view: view::ViewState::default(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.hist.is_dirty()
    }

    /// Puts the primary caret at a line, 0-based, and shows it.
    pub fn go_to_line(&mut self, line: usize) {
        let pos = self.buf.pos(line, 0);
        self.sels = SelectionSet::single(Selection::caret(pos));
        self.open_folds_at_carets();
        self.reveal = true;
    }

    /// Lines any selection touches, each once, in order. A selection ending at a line's
    /// start doesn't count that line, as when whole lines are selected with Shift+Down.
    pub fn selected_lines(&self) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for s in self.sels.iter() {
            let first = self.buf.line_of(s.start());
            let mut last = self.buf.line_of(s.end());
            if last > first && self.buf.point(s.end()).1 == 0 {
                last -= 1;
            }
            for l in first..=last {
                if out.last() != Some(&l) && !out.contains(&l) {
                    out.push(l);
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// The selected text, each selection on its own line. With nothing selected, the lines
    /// the carets are on, as editors copy.
    pub fn copied(&self) -> String {
        let eol = self.buf.eol.as_str();
        if self.sels.iter().all(|s| s.is_empty()) {
            return self.selected_lines().iter().map(|&l| format!("{}{eol}", self.buf.line(l))).collect();
        }
        self.sels.iter().filter(|s| !s.is_empty()).map(|s| self.buf.rope.slice(s.start()..s.end()).to_string()).collect::<Vec<_>>().join(eol)
    }

    /// Runs a command. `now_ms` groups typing for undo.
    pub fn run(&mut self, cmd: Cmd, now_ms: u64) {
        let editing = !matches!(cmd, Cmd::Move(..) | Cmd::SelectAll | Cmd::SelectLine | Cmd::SelectNext | Cmd::SelectAllMatches | Cmd::Collapse | Cmd::AddCaret { .. } | Cmd::Fold | Cmd::Unfold | Cmd::FoldAll | Cmd::UnfoldAll);
        if editing && self.read_only {
            return;
        }
        match cmd {
            Cmd::Type(text) => self.type_text(&text, now_ms),
            Cmd::Paste(text) => self.paste(&text, now_ms),
            Cmd::Newline => self.newline(now_ms),
            Cmd::NewlineBelow => {
                self.sels.motion(&self.buf, Move::End, false);
                self.newline(now_ms);
            }
            Cmd::Backspace { word } => self.backspace(word, now_ms),
            Cmd::Delete { word } => self.delete(word, now_ms),
            Cmd::Tab => self.tab(now_ms),
            Cmd::Outdent => {
                let t = indent::outdent_lines(&self.buf, &self.selected_lines(), &self.indent.unit());
                self.apply_mapped(t, now_ms);
            }
            Cmd::Move(m, extend) => {
                self.sels.motion(&self.buf, m, extend);
                // Up and down step over a folded block; anything else landing in one opens it.
                let up = matches!(m, Move::Up) || matches!(m, Move::Lines(n) if n < 0);
                let down = matches!(m, Move::Down) || matches!(m, Move::Lines(n) if n > 0);
                if (up || down) && !self.folds.is_empty() {
                    let folds = self.folds.clone();
                    let b = &self.buf;
                    self.sels.map(|s| {
                        let (line, col) = b.point(s.head);
                        match folds.iter().find(|f| line > f.0 && line <= f.1) {
                            Some(f) => {
                                let to = if up { f.0 } else { (f.1 + 1).min(b.line_count() - 1) };
                                let head = b.pos(to, s.goal.unwrap_or(col));
                                Selection { anchor: if extend { s.anchor } else { head }, head, goal: s.goal }
                            }
                            None => s,
                        }
                    });
                } else {
                    self.open_folds_at_carets();
                }
                self.hist.break_run();
                self.reveal = true;
            }
            Cmd::Fold => {
                let line = self.buf.line_of(self.sels.primary().head);
                if !self.folds.iter().any(|f| f.0 == line) {
                    self.toggle_fold(line);
                }
            }
            Cmd::Unfold => {
                let line = self.buf.line_of(self.sels.primary().head);
                let before = self.folds.len();
                self.folds.retain(|f| !(f.0 <= line && line <= f.1));
                if self.folds.len() != before {
                    self.folds_gen += 1;
                }
            }
            Cmd::FoldAll => {
                // Outermost blocks only: folding what's inside a fold changes nothing seen.
                self.folds.clear();
                let mut l = 0;
                while l < self.buf.line_count() {
                    match self.fold_end(l) {
                        Some(end) => {
                            self.folds.push((l, end));
                            l = end + 1;
                        }
                        None => l += 1,
                    }
                }
                self.folds_gen += 1;
                self.open_folds_at_carets();
            }
            Cmd::UnfoldAll => {
                self.folds.clear();
                self.folds_gen += 1;
            }
            Cmd::SelectAll => self.sels = SelectionSet::single(Selection::new(0, self.buf.len_chars())),
            Cmd::SelectLine => {
                let b = &self.buf;
                self.sels.map(|s| {
                    let (first, last) = (b.line_of(s.start()), b.line_of(s.end()));
                    let end = if last + 1 < b.line_count() { b.line_start(last + 1) } else { b.len_chars() };
                    Selection::new(b.line_start(first), end)
                });
            }
            Cmd::Undo => {
                if let Some(s) = self.hist.undo(&mut self.buf) {
                    self.sels = s;
                    self.after_edit();
                }
            }
            Cmd::Redo => {
                if let Some(s) = self.hist.redo(&mut self.buf) {
                    self.sels = s;
                    self.after_edit();
                }
            }
            Cmd::ToggleComment => {
                if let Some(prefix) = self.comment {
                    let t = lines::toggle_comment(&self.buf, &self.selected_lines(), prefix);
                    self.apply_mapped(t, now_ms);
                }
            }
            Cmd::DuplicateLines => self.duplicate(now_ms),
            Cmd::MoveLines { up } => self.move_lines(up, now_ms),
            Cmd::DeleteLines => self.delete_lines(now_ms),
            Cmd::JoinLines => {
                let lines = self.selected_lines();
                let (first, last) = (lines[0], *lines.last().unwrap_or(&lines[0]));
                let t = lines::join(&self.buf, first, last);
                self.apply_mapped(t, now_ms);
            }
            Cmd::SelectNext => self.select_next(),
            Cmd::SelectAllMatches => self.select_all_matches(),
            Cmd::Collapse => self.sels.collapse_to_primary(),
            Cmd::AddCaret { up } => {
                let p = self.sels.primary();
                let (line, col) = self.buf.point(p.head);
                let target = if up { line.checked_sub(1) } else { (line + 1 < self.buf.line_count()).then_some(line + 1) };
                if let Some(t) = target {
                    self.sels.add(Selection::caret(self.buf.pos(t, col)));
                    self.reveal = true;
                }
            }
        }
    }

    /// Replaces each selection with what `f` gives for it, as one undo step. `f` returns the
    /// range to replace, the new text, and the new selection as offsets into that text. A
    /// range that overlaps the one before is skipped, as two carets backspacing into each other.
    fn replace_each(&mut self, kind: Kind, now_ms: u64, f: impl Fn(&Buffer, &Selection) -> Option<(usize, usize, String, usize, usize)>) {
        let before = self.sels.clone();
        let mut edits = Vec::new();
        let mut after = Vec::new();
        let mut delta: isize = 0;
        let mut last_end = 0;
        let shift = |p: usize, d: isize| (p as isize + d).max(0) as usize;
        for s in self.sels.iter() {
            match f(&self.buf, s) {
                Some((start, end, text, a, h)) if start >= last_end || edits.is_empty() => {
                    let ns = shift(start, delta);
                    after.push(Selection::new(ns + a, ns + h));
                    delta += text.chars().count() as isize - (end - start) as isize;
                    last_end = end;
                    edits.push(Edit::replace(start, end, text));
                }
                _ => after.push(Selection { anchor: shift(s.anchor, delta), head: shift(s.head, delta), goal: s.goal }),
            }
        }
        edits.reverse();
        let after = SelectionSet::from_vec(after, before.primary_index());
        let t = Transaction::new(edits);
        if t.is_empty() {
            self.sels = after;
        } else {
            self.hist.apply(&mut self.buf, t, before, after.clone(), kind, now_ms);
            self.sels = after;
        }
        self.after_edit();
    }

    /// A whole-text transaction, with the carets carried through it.
    fn apply_mapped(&mut self, t: Transaction, now_ms: u64) {
        if t.is_empty() {
            return;
        }
        let before = self.sels.clone();
        let mut after = self.sels.clone();
        after.after(&t);
        self.hist.apply(&mut self.buf, t, before, after.clone(), Kind::Other, now_ms);
        self.sels = after;
        self.after_edit();
    }

    fn after_edit(&mut self) {
        if let Some(line) = self.buf.take_changed() {
            self.hl.edited(line);
            // Folds from the change down no longer match their lines.
            let before = self.folds.len();
            self.folds.retain(|f| f.1 < line);
            if self.folds.len() != before {
                self.folds_gen += 1;
            }
            let len = self.buf.line(line).chars().count();
            self.max_cols = self.max_cols.max(len);
        }
        let len = self.buf.len_chars();
        self.sels.clamp(len);
        self.reveal = true;
    }

    fn char_at(b: &Buffer, pos: usize) -> Option<char> {
        (pos < b.len_chars()).then(|| b.rope.char(pos))
    }

    fn type_text(&mut self, text: &str, now_ms: u64) {
        let mut chars = text.chars();
        let single = match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        };
        let kind = if single.is_some_and(|c| !c.is_whitespace()) { Kind::Typing } else { Kind::Other };
        let text = text.to_string();
        self.replace_each(kind, now_ms, |b, s| {
            let (start, end) = (s.start(), s.end());
            let n = text.chars().count();
            if let Some(c) = single {
                let after = Self::char_at(b, end);
                let before = start.checked_sub(1).and_then(|p| Self::char_at(b, p));
                // Typing a closer just before the same one steps over it.
                if s.is_empty() && brackets::skips_over(c, after) && Self::pairs_before(b, start, c) {
                    return Some((start, start, String::new(), 1, 1));
                }
                if let Some(close) = brackets::auto_close(c, before, after) {
                    if s.is_empty() {
                        return Some((start, end, format!("{c}{close}"), 1, 1));
                    }
                    // Wraps what's selected, and keeps it selected.
                    let inner = b.rope.slice(start..end).to_string();
                    let len = inner.chars().count();
                    return Some((start, end, format!("{c}{inner}{close}"), 1, 1 + len));
                }
            }
            Some((start, end, text.clone(), n, n))
        });
    }

    /// Whether a closer typed at `pos` would close something opened earlier on the line, so
    /// stepping over the one already there is what was meant. Quotes always step over.
    fn pairs_before(b: &Buffer, pos: usize, close: char) -> bool {
        let open = match close {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => return true,
        };
        let (line, col) = b.point(pos);
        let text: Vec<char> = b.line(line).chars().collect();
        let opens = text[..col.min(text.len())].iter().filter(|c| **c == open).count();
        let closes = text[..col.min(text.len())].iter().filter(|c| **c == close).count();
        opens > closes
    }

    fn paste(&mut self, text: &str, now_ms: u64) {
        let eol = self.buf.eol.as_str();
        let text = text.replace("\r\n", "\n").replace('\n', eol);
        let parts: Vec<&str> = text.trim_end_matches(eol).split(eol).collect();
        // One line per caret, when the counts match, as copied from as many carets.
        let spread = self.sels.len() > 1 && parts.len() == self.sels.len();
        let sels: Vec<Selection> = self.sels.iter().copied().collect();
        self.replace_each(Kind::Other, now_ms, |_, s| {
            let i = sels.iter().position(|x| x == s).unwrap_or(0);
            let piece = if spread { parts[i].to_string() } else { text.clone() };
            let n = piece.chars().count();
            Some((s.start(), s.end(), piece, n, n))
        });
    }

    fn newline(&mut self, now_ms: u64) {
        let unit = self.indent.unit();
        let eol = self.buf.eol.as_str().to_string();
        self.replace_each(Kind::Other, now_ms, |b, s| {
            let (start, end) = (s.start(), s.end());
            let indent = indent::newline_indent(b, start, &unit);
            let first = format!("{eol}{indent}");
            let n = first.chars().count();
            // Between a pair of brackets, the closer goes on a line of its own below.
            let open = start.checked_sub(1).and_then(|p| Self::char_at(b, p));
            let close = Self::char_at(b, end);
            if matches!((open, close), (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']'))) {
                let own = indent.strip_suffix(unit.as_str()).unwrap_or(&indent).to_string();
                return Some((start, end, format!("{first}{eol}{own}"), n, n));
            }
            Some((start, end, first, n, n))
        });
    }

    fn backspace(&mut self, word: bool, now_ms: u64) {
        let unit_width = self.indent.unit().chars().count();
        let spaces = matches!(self.indent, Indent::Spaces(_));
        self.replace_each(Kind::Typing, now_ms, |b, s| {
            if !s.is_empty() {
                return Some((s.start(), s.end(), String::new(), 0, 0));
            }
            let pos = s.head;
            if pos == 0 {
                return None;
            }
            if word {
                return Some((selection::word_left(b, pos), pos, String::new(), 0, 0));
            }
            let before = Self::char_at(b, pos - 1);
            let after = Self::char_at(b, pos);
            // An empty pair goes together.
            if let (Some(o), Some(c)) = (before, after)
                && brackets::auto_close(o, None, None) == Some(c)
            {
                return Some((pos - 1, pos + 1, String::new(), 0, 0));
            }
            // In the indent, back to the previous stop rather than one space.
            let (line, col) = b.point(pos);
            let lead: String = b.line(line).chars().take(col).collect();
            if spaces && col > 0 && lead.chars().all(|c| c == ' ') {
                let back = match col % unit_width {
                    0 => unit_width,
                    n => n,
                };
                return Some((pos - back, pos, String::new(), 0, 0));
            }
            Some((selection::prev_grapheme(b, pos), pos, String::new(), 0, 0))
        });
    }

    fn delete(&mut self, word: bool, now_ms: u64) {
        self.replace_each(Kind::Typing, now_ms, |b, s| {
            if !s.is_empty() {
                return Some((s.start(), s.end(), String::new(), 0, 0));
            }
            let end = if word { selection::word_right(b, s.head) } else { selection::next_grapheme(b, s.head) };
            (end > s.head).then(|| (s.head, end, String::new(), 0, 0))
        });
    }

    fn tab(&mut self, now_ms: u64) {
        let multi_line = self.sels.iter().any(|s| self.buf.line_of(s.start()) != self.buf.line_of(s.end()));
        if multi_line {
            let t = indent::indent_lines(&self.buf, &self.selected_lines(), &self.indent.unit());
            self.apply_mapped(t, now_ms);
            return;
        }
        let indent = self.indent;
        self.replace_each(Kind::Other, now_ms, |b, s| {
            // Spaces up to the next stop, so columns line up.
            let text = match indent {
                Indent::Tabs => "\t".to_string(),
                Indent::Spaces(n) => {
                    let col = b.point(s.start()).1;
                    " ".repeat(n as usize - col % n.max(1) as usize)
                }
            };
            let n = text.chars().count();
            Some((s.start(), s.end(), text, n, n))
        });
    }

    /// Each selection's lines as `(first, last)`, merged where they share a line.
    fn line_blocks(&self) -> Vec<(usize, usize)> {
        let mut blocks: Vec<(usize, usize)> = Vec::new();
        for l in self.selected_lines() {
            match blocks.last_mut() {
                Some(b) if b.1 + 1 >= l => b.1 = l,
                _ => blocks.push((l, l)),
            }
        }
        blocks
    }

    fn duplicate(&mut self, now_ms: u64) {
        let blocks = self.line_blocks();
        let mut edits = Vec::new();
        for &(first, last) in blocks.iter().rev() {
            edits.extend(lines::duplicate(&self.buf, first, last).edits);
        }
        // Each caret goes down with the copy: by its own block's height plus those above it.
        let b = &self.buf;
        let positions: Vec<(usize, usize, usize, usize)> = self
            .sels
            .iter()
            .map(|s| {
                let shift: usize = blocks.iter().filter(|(f, _)| *f <= b.line_of(s.head)).map(|(f, l)| l - f + 1).sum();
                let (al, ac) = b.point(s.anchor);
                let (hl, hc) = b.point(s.head);
                (al + shift, ac, hl + shift, hc)
            })
            .collect();
        self.apply_at_points(Transaction::new(edits), &positions, now_ms);
    }

    /// A transaction, then the carets put at these (anchor line, column, head line, column)
    /// in the text it leaves.
    fn apply_at_points(&mut self, t: Transaction, points: &[(usize, usize, usize, usize)], now_ms: u64) {
        let undo = self.buf.apply(&t);
        self.record_at_points(t, undo, points, now_ms);
    }

    fn record_at_points(&mut self, t: Transaction, undo: Transaction, points: &[(usize, usize, usize, usize)], now_ms: u64) {
        let primary = self.sels.primary_index();
        let before = self.sels.clone();
        let sels = points.iter().map(|&(al, ac, hl, hc)| Selection::new(self.buf.pos(al, ac), self.buf.pos(hl, hc))).collect();
        let after = SelectionSet::from_vec(sels, primary);
        self.hist.record(t, undo, before, after.clone(), Kind::Other, now_ms);
        self.sels = after;
        self.after_edit();
    }

    fn move_lines(&mut self, up: bool, now_ms: u64) {
        let blocks = self.line_blocks();
        // Nothing moves if any block is already at the edge, so blocks keep their spacing.
        if (up && blocks.first().is_some_and(|b| b.0 == 0)) || (!up && blocks.last().is_some_and(|b| b.1 + 1 >= self.buf.line_count())) {
            return;
        }
        let positions: Vec<(usize, usize, usize, usize)> = self
            .sels
            .iter()
            .map(|s| {
                let (al, ac) = self.buf.point(s.anchor);
                let (hl, hc) = self.buf.point(s.head);
                let d = |l: usize| if up { l - 1 } else { l + 1 };
                (d(al), ac, d(hl), hc)
            })
            .collect();
        // One block at a time, each in the text the ones before left, which is why moving up
        // goes top first and moving down bottom first.
        let order: Vec<(usize, usize)> = if up { blocks } else { blocks.into_iter().rev().collect() };
        let (mut forward, mut undos) = (Vec::new(), Vec::new());
        for (first, last) in order {
            if let Some(t) = lines::move_lines(&self.buf, first, last, up) {
                undos.push(self.buf.apply(&t));
                forward.extend(t.edits);
            }
        }
        let undo = Transaction::new(undos.into_iter().rev().flat_map(|u| u.edits).collect());
        self.record_at_points(Transaction::new(forward), undo, &positions, now_ms);
    }

    fn delete_lines(&mut self, now_ms: u64) {
        let blocks = self.line_blocks();
        let b = &self.buf;
        let edits = blocks
            .iter()
            .rev()
            .map(|&(first, last)| {
                let start = b.line_start(first);
                let end = if last + 1 < b.line_count() { b.line_start(last + 1) } else { b.len_chars() };
                // The last line takes the line break before it instead.
                let start = if end == b.len_chars() && first > 0 { b.line_end(first - 1) } else { start };
                Edit::delete(start, end)
            })
            .collect();
        let firsts: Vec<(usize, usize)> = self.sels.iter().map(|s| b.point(s.head)).collect();
        let removed_above = |line: usize| blocks.iter().filter(|(_, l)| *l < line).map(|(f, l)| l - f + 1).sum::<usize>();
        let positions: Vec<(usize, usize, usize, usize)> = firsts.iter().map(|&(l, c)| (l - removed_above(l).min(l), c)).map(|(l, c)| (l, c, l, c)).collect();
        self.apply_at_points(Transaction::new(edits), &positions, now_ms);
    }

    fn select_next(&mut self) {
        let p = self.sels.primary();
        if p.is_empty() {
            let (s, e) = selection::word_at(&self.buf, p.head);
            if s < e {
                self.sels.map(|x| if x == p { Selection::new(s, e) } else { x });
            }
            return;
        }
        let needle = self.buf.rope.slice(p.start()..p.end()).to_string();
        let Ok(Some(re)) = (Query { text: needle, case_sensitive: true, ..Default::default() }).compile() else { return };
        let all = search::find_all(&self.buf, &re, MAX_MATCHES);
        let last = self.sels.as_slice().last().map_or(0, |s| s.end());
        let taken = |m: &(usize, usize)| self.sels.iter().any(|s| s.start() == m.0 && s.end() == m.1);
        let next = all.iter().find(|m| m.0 >= last && !taken(m)).or_else(|| all.iter().find(|m| !taken(m)));
        if let Some(&(s, e)) = next {
            self.sels.add(Selection::new(s, e));
            self.reveal = true;
        }
    }

    fn select_all_matches(&mut self) {
        let p = self.sels.primary();
        let (s, e) = if p.is_empty() { selection::word_at(&self.buf, p.head) } else { (p.start(), p.end()) };
        if s == e {
            return;
        }
        let needle = self.buf.rope.slice(s..e).to_string();
        let Ok(Some(re)) = (Query { text: needle, case_sensitive: true, ..Default::default() }).compile() else { return };
        let all = search::find_all(&self.buf, &re, MAX_MATCHES);
        let primary = all.iter().position(|m| m.0 == s).unwrap_or(0);
        self.sels = SelectionSet::from_vec(all.iter().map(|&(a, b)| Selection::new(a, b)).collect(), primary);
    }

    fn indent_of(&self, line: usize) -> Option<usize> {
        let text = self.buf.line(line);
        if text.trim().is_empty() {
            return None;
        }
        Some(text.chars().take_while(|c| *c == ' ' || *c == '\t').map(|c| if c == '\t' { 4 } else { 1 }).sum())
    }

    /// Whether the line opens a block: the next line with text is indented deeper. Cheap
    /// enough for every line in view.
    pub fn can_fold(&self, line: usize) -> bool {
        let Some(base) = self.indent_of(line) else { return false };
        (line + 1..self.buf.line_count().min(line + 200)).find_map(|l| self.indent_of(l)).is_some_and(|i| i > base)
    }

    /// The last line of the block `line` opens, by indent: blank lines inside it go with it,
    /// those after it don't.
    fn fold_end(&self, line: usize) -> Option<usize> {
        let base = self.indent_of(line)?;
        let mut end = None;
        for l in line + 1..self.buf.line_count() {
            match self.indent_of(l) {
                Some(i) if i > base => end = Some(l),
                Some(_) => break,
                None => {}
            }
        }
        end
    }

    pub fn toggle_fold(&mut self, line: usize) {
        if let Some(i) = self.folds.iter().position(|f| f.0 == line) {
            self.folds.remove(i);
        } else if let Some(end) = self.fold_end(line) {
            // One inside another goes: the outer one hides it anyway.
            self.folds.retain(|f| !(f.0 > line && f.1 <= end));
            self.folds.push((line, end));
            self.folds.sort_unstable();
            // A caret inside goes to the line that stays.
            let b = &self.buf;
            self.sels.map(|s| {
                let l = b.line_of(s.head);
                if l > line && l <= end { Selection::caret(b.line_end(line)) } else { s }
            });
        }
        self.folds_gen += 1;
    }

    /// The lines hidden under folds, in order.
    pub fn hidden_lines(&self) -> Vec<std::ops::RangeInclusive<usize>> {
        let mut out: Vec<std::ops::RangeInclusive<usize>> = self.folds.iter().map(|f| f.0 + 1..=f.1).collect();
        out.sort_by_key(|r| *r.start());
        out
    }

    /// Opens any fold a caret has ended up inside, as after a jump to a line or a match.
    pub fn open_folds_at_carets(&mut self) {
        let lines: Vec<usize> = self.sels.iter().map(|s| self.buf.line_of(s.head)).collect();
        let before = self.folds.len();
        self.folds.retain(|f| !lines.iter().any(|l| *l > f.0 && *l <= f.1));
        if self.folds.len() != before {
            self.folds_gen += 1;
        }
    }

    /// The matches for the find box, found again only when the text or query changed.
    pub fn refresh_matches(&mut self) {
        let key = (self.buf.version, self.find.query.clone());
        if self.find.found_for.as_ref() == Some(&key) {
            return;
        }
        self.find.found_for = Some(key);
        match self.find.query.compile() {
            Ok(Some(re)) => {
                self.find.matches = search::find_all(&self.buf, &re, MAX_MATCHES);
                self.find.error = None;
            }
            Ok(None) => {
                self.find.matches.clear();
                self.find.error = None;
            }
            Err(e) => {
                self.find.matches.clear();
                self.find.error = Some(e);
            }
        }
    }

    /// Selects the next match after the caret, or the one before with `back`.
    pub fn find_step(&mut self, back: bool) {
        self.refresh_matches();
        let from = if back { self.sels.primary().start() } else { self.sels.primary().end() };
        let m = if back { search::prev(&self.find.matches, from) } else { search::next(&self.find.matches, from) };
        if let Some((s, e)) = m {
            self.sels = SelectionSet::single(Selection::new(s, e));
            self.open_folds_at_carets();
            self.reveal = true;
        }
    }

    /// Replaces the selected match and goes to the next.
    pub fn replace_one(&mut self, now_ms: u64) {
        self.refresh_matches();
        let p = self.sels.primary();
        let Ok(Some(re)) = self.find.query.compile() else { return };
        if self.find.matches.contains(&(p.start(), p.end())) && !self.read_only {
            let with = search::replacement(&self.buf, &re, (p.start(), p.end()), &self.find.with, self.find.query.regex);
            let n = with.chars().count();
            self.sels = SelectionSet::single(p);
            self.replace_each(Kind::Other, now_ms, |_, s| Some((s.start(), s.end(), with.clone(), n, n)));
            self.refresh_matches();
        }
        self.find_step(false);
    }

    pub fn replace_all(&mut self, now_ms: u64) {
        let Ok(Some(re)) = self.find.query.compile() else { return };
        if self.read_only {
            return;
        }
        let t = search::replace_all(&self.buf, &re, &self.find.with, self.find.query.regex);
        self.apply_mapped(t, now_ms);
        self.refresh_matches();
    }

    /// Takes in text read from disk again, keeping the carets on the same lines.
    pub fn reload(&mut self, buf: Buffer) {
        let points: Vec<(usize, usize)> = self.sels.iter().map(|s| self.buf.point(s.head)).collect();
        self.buf = buf;
        self.hist.clear();
        self.hl.edited(0);
        let sels = points.iter().map(|&(l, c)| Selection::caret(self.buf.pos(l, c))).collect();
        self.sels = SelectionSet::from_vec(sels, 0);
        self.max_cols = (0..self.buf.line_count()).map(|i| self.buf.rope.line(i).len_chars()).max().unwrap_or(0);
    }

    /// The text replaced as one undoable step, as when converting line endings.
    pub fn transform(&mut self, t: Transaction, now_ms: u64) {
        if !self.read_only {
            self.apply_mapped(t, now_ms);
        }
    }

    /// The word or selection to look for elsewhere, as Find in files starts with.
    pub fn search_seed(&self) -> String {
        let p = self.sels.primary();
        let (s, e) = if p.is_empty() { selection::word_at(&self.buf, p.head) } else { (p.start(), p.end()) };
        let text = self.buf.rope.slice(s..e).to_string();
        if text.contains('\n') { String::new() } else { text }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str, caret: usize) -> Editor {
        let mut e = Editor::new("a.rs", Buffer::from_text(text), true, Indent::Spaces(4));
        e.sels = SelectionSet::single(Selection::caret(caret));
        e
    }

    fn typed(e: &mut Editor, s: &str) {
        for c in s.chars() {
            e.run(Cmd::Type(c.to_string()), 0);
        }
    }

    #[test]
    fn typing_pairs_brackets_and_steps_over_the_closer() {
        let mut e = ed("", 0);
        typed(&mut e, "f(x)");
        assert_eq!(e.buf.text(), "f(x)");
        assert_eq!(e.sels.primary().head, 4);
        let mut e = ed("ab", 0);
        e.sels = SelectionSet::single(Selection::new(0, 2));
        typed(&mut e, "[");
        assert_eq!(e.buf.text(), "[ab]");
        assert_eq!(e.sels.primary(), Selection::new(1, 3), "what was wrapped stays selected");
    }

    #[test]
    fn enter_indents_and_opens_a_block() {
        let mut e = ed("    fn a() {}", 12);
        e.run(Cmd::Newline, 0);
        assert_eq!(e.buf.text(), "    fn a() {\n        \n    }");
        assert_eq!(e.buf.point(e.sels.primary().head), (1, 8));
    }

    #[test]
    fn backspace_takes_an_empty_pair_and_an_indent_step() {
        let mut e = ed("()", 1);
        e.run(Cmd::Backspace { word: false }, 0);
        assert_eq!(e.buf.text(), "");
        let mut e = ed("    a
        x", 14);
        e.run(Cmd::Backspace { word: false }, 0);
        assert_eq!(e.buf.text(), "    a
    x", "the file's indent is 4, found from its lines");
    }

    #[test]
    fn several_carets_type_together_and_undo_as_one() {
        let mut e = ed("a\nb\nc", 1);
        e.run(Cmd::AddCaret { up: false }, 0);
        e.run(Cmd::AddCaret { up: false }, 0);
        assert_eq!(e.sels.len(), 3, "each add goes on from the one added before");
        typed(&mut e, ";");
        assert_eq!(e.buf.text(), "a;\nb;\nc;");
        e.run(Cmd::Undo, 0);
        assert_eq!(e.buf.text(), "a\nb\nc");
    }

    #[test]
    fn select_next_then_type_renames_every_one() {
        let mut e = ed("foo + foo * foo", 1);
        e.run(Cmd::SelectNext, 0);
        assert_eq!(e.sels.primary(), Selection::new(0, 3), "first the word at the caret");
        e.run(Cmd::SelectNext, 0);
        assert_eq!(e.sels.len(), 2);
        typed(&mut e, "x");
        assert_eq!(e.buf.text(), "x + x * foo");
        let mut e = ed("ab ab ab", 0);
        e.run(Cmd::SelectAllMatches, 0);
        typed(&mut e, "z");
        assert_eq!(e.buf.text(), "z z z");
    }

    #[test]
    fn lines_move_duplicate_and_go_with_their_carets() {
        let mut e = ed("one\ntwo\nthree", 5);
        e.run(Cmd::MoveLines { up: true }, 0);
        assert_eq!(e.buf.text(), "two\none\nthree");
        assert_eq!(e.buf.point(e.sels.primary().head), (0, 1));
        e.run(Cmd::DuplicateLines, 0);
        assert_eq!(e.buf.text(), "two\ntwo\none\nthree");
        assert_eq!(e.buf.point(e.sels.primary().head), (1, 1));
        e.run(Cmd::DeleteLines, 0);
        assert_eq!(e.buf.text(), "two\none\nthree");
        e.sels = SelectionSet::single(Selection::caret(e.buf.pos(2, 0)));
        e.run(Cmd::MoveLines { up: false }, 0);
        assert_eq!(e.buf.text(), "two\none\nthree", "nothing below the last line");
    }

    #[test]
    fn paste_spreads_lines_over_as_many_carets() {
        let mut e = ed("a\nb", 1);
        e.run(Cmd::AddCaret { up: false }, 0);
        e.run(Cmd::Paste("1\n2\n".into()), 0);
        assert_eq!(e.buf.text(), "a1\nb2");
        let mut e = ed("x", 1);
        e.buf.eol = gitem_text::LineEnding::CrLf;
        e.run(Cmd::Paste("p\nq".into()), 0);
        assert_eq!(e.buf.text(), "xp\r\nq", "pasted lines take the file's line ending");
    }

    #[test]
    fn copy_with_nothing_selected_takes_the_line() {
        let e = ed("one\ntwo", 5);
        assert_eq!(e.copied(), "two\n");
    }

    #[test]
    fn find_and_replace() {
        let mut e = ed("cat hat cat", 0);
        e.find.query = Query { text: "cat".into(), ..Default::default() };
        e.find.with = "dog".into();
        e.find_step(false);
        assert_eq!(e.sels.primary(), Selection::new(0, 3));
        e.replace_one(0);
        assert_eq!(e.buf.text(), "dog hat cat");
        assert_eq!(e.sels.primary(), Selection::new(8, 11));
        e.replace_all(0);
        assert_eq!(e.buf.text(), "dog hat dog");
        e.run(Cmd::Undo, 0);
        assert_eq!(e.buf.text(), "dog hat cat");
    }

    #[test]
    fn read_only_moves_but_never_changes() {
        let mut e = ed("abc", 0);
        e.read_only = true;
        typed(&mut e, "x");
        e.run(Cmd::Move(Move::Right, false), 0);
        assert_eq!((e.buf.text().as_str(), e.sels.primary().head), ("abc", 1));
    }

    #[test]
    fn folding_by_indent() {
        let text = "fn a() {\n    one\n\n    two\n}\nfn b() {\n    three\n}\n";
        let mut e = ed(text, 0);
        assert!(e.can_fold(0) && !e.can_fold(1) && !e.can_fold(4));
        e.run(Cmd::Fold, 0);
        assert_eq!(e.folds, vec![(0, 3)], "the blank line inside goes with it, the closer stays");
        e.run(Cmd::Move(Move::Down, false), 0);
        assert_eq!(e.buf.line_of(e.sels.primary().head), 4, "down steps over the fold");
        e.run(Cmd::Move(Move::Up, false), 0);
        assert_eq!(e.buf.line_of(e.sels.primary().head), 0);
        e.run(Cmd::FoldAll, 0);
        assert_eq!(e.folds, vec![(0, 3), (5, 6)]);
        e.go_to_line(6);
        assert_eq!(e.folds, vec![(0, 3)], "going to a hidden line opens its fold");
        // An edit above a fold leaves its lines wrong, so it opens.
        e.go_to_line(0);
        e.run(Cmd::Type("x".into()), 0);
        assert!(e.folds.is_empty());
    }

    #[test]
    fn comment_toggle_and_tab_indent() {
        let mut e = ed("a\nb", 0);
        e.run(Cmd::SelectAll, 0);
        e.run(Cmd::ToggleComment, 0);
        assert_eq!(e.buf.text(), "// a\n// b");
        e.run(Cmd::Tab, 0);
        assert_eq!(e.buf.text(), "    // a\n    // b");
        e.run(Cmd::Outdent, 0);
        e.run(Cmd::ToggleComment, 0);
        assert_eq!(e.buf.text(), "a\nb");
        let mut e = ed("ab", 1);
        e.run(Cmd::Tab, 0);
        assert_eq!(e.buf.text(), "a   b", "to the next stop");
    }
}
