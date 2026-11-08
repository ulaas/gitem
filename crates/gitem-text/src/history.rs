//! Undo and redo, and whether the text is as it was last saved.
use crate::Buffer;
use crate::edit::Transaction;
use crate::selection::SelectionSet;

/// Typing within this long of the last key, carrying on where it left off, is undone as one.
const COALESCE_MS: u64 = 800;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Plain typing and Backspace, which group into runs.
    Typing,
    /// Everything else: paste, replace, a line moved. Each is its own step.
    Other,
}

struct Entry {
    id: u64,
    redo: Transaction,
    undo: Transaction,
    /// The carets before the change, which undo brings back, and after, which redo does.
    before: SelectionSet,
    after: SelectionSet,
    kind: Kind,
    at_ms: u64,
}

#[derive(Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    next_id: u64,
    /// The step the file on disk matches. 0 is before any.
    saved: u64,
}

impl History {
    /// Applies a change and remembers how to undo it. `now_ms` is any clock in milliseconds.
    pub fn apply(&mut self, buf: &mut Buffer, t: Transaction, before: SelectionSet, after: SelectionSet, kind: Kind, now_ms: u64) {
        if t.is_empty() {
            return;
        }
        let undo = buf.apply(&t);
        self.record(t, undo, before, after, kind, now_ms);
    }

    /// Remembers a change already made to the buffer, with the transaction that undoes it.
    /// For callers that need to see the text partway through, as when lines move one block
    /// at a time.
    pub fn record(&mut self, t: Transaction, undo: Transaction, before: SelectionSet, after: SelectionSet, kind: Kind, now_ms: u64) {
        if t.is_empty() {
            return;
        }
        self.redo.clear();
        if let Some(top) = self.undo.last_mut()
            && kind == Kind::Typing
            && top.kind == Kind::Typing
            && top.id != self.saved
            && now_ms.saturating_sub(top.at_ms) < COALESCE_MS
            && top.after == before
        {
            top.redo.edits.extend(t.edits);
            let mut edits = undo.edits;
            edits.append(&mut top.undo.edits);
            top.undo.edits = edits;
            top.after = after;
            top.at_ms = now_ms;
            return;
        }
        self.next_id += 1;
        self.undo.push(Entry { id: self.next_id, redo: t, undo, before, after, kind, at_ms: now_ms });
    }

    /// Undoes the last step and returns the carets as they were before it.
    pub fn undo(&mut self, buf: &mut Buffer) -> Option<SelectionSet> {
        let e = self.undo.pop()?;
        buf.apply(&e.undo);
        let sels = e.before.clone();
        self.redo.push(e);
        Some(sels)
    }

    pub fn redo(&mut self, buf: &mut Buffer) -> Option<SelectionSet> {
        let e = self.redo.pop()?;
        buf.apply(&e.redo);
        let sels = e.after.clone();
        self.undo.push(e);
        Some(sels)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The next keystroke starts a step of its own, as after a click elsewhere.
    pub fn break_run(&mut self) {
        if let Some(top) = self.undo.last_mut() {
            top.kind = Kind::Other;
        }
    }

    pub fn mark_saved(&mut self) {
        self.saved = self.undo.last().map_or(0, |e| e.id);
    }

    /// Whether the text differs from what was saved, by the steps taken. Typing a char and
    /// deleting it counts as a change; undoing back to the save doesn't.
    pub fn is_dirty(&self) -> bool {
        self.undo.last().map_or(0, |e| e.id) != self.saved
    }

    /// Forgets everything, as after a reload from disk.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.saved = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Edit, Selection};

    fn typed(h: &mut History, b: &mut Buffer, at: usize, s: &str, ms: u64) {
        let before = SelectionSet::single(Selection::caret(at));
        let after = SelectionSet::single(Selection::caret(at + s.chars().count()));
        h.apply(b, Transaction::new(vec![Edit::insert(at, s)]), before, after, Kind::Typing, ms);
    }

    #[test]
    fn a_run_of_typing_undoes_at_once_and_a_pause_splits_it() {
        let mut b = Buffer::from_text("");
        let mut h = History::default();
        typed(&mut h, &mut b, 0, "a", 0);
        typed(&mut h, &mut b, 1, "b", 100);
        typed(&mut h, &mut b, 2, "c", 5000);
        assert_eq!(b.text(), "abc");
        assert_eq!(h.undo(&mut b).unwrap().primary(), Selection::caret(2));
        assert_eq!(b.text(), "ab");
        h.undo(&mut b);
        assert_eq!(b.text(), "");
        assert_eq!(h.redo(&mut b).unwrap().primary(), Selection::caret(2));
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn dirty_follows_the_save_point_both_ways() {
        let mut b = Buffer::from_text("x");
        let mut h = History::default();
        assert!(!h.is_dirty());
        typed(&mut h, &mut b, 1, "y", 0);
        assert!(h.is_dirty());
        h.mark_saved();
        assert!(!h.is_dirty());
        typed(&mut h, &mut b, 2, "z", 10);
        assert!(h.is_dirty(), "typing right after a save isn't folded into the saved step");
        h.undo(&mut b);
        assert!(!h.is_dirty());
        h.undo(&mut b);
        assert!(h.is_dirty(), "before the save is a change too");
        typed(&mut h, &mut b, 1, "q", 20);
        assert!(h.is_dirty());
        assert!(!h.can_redo(), "a new change drops what could be redone");
    }
}
