//! Changes to a rope, each of which knows how to undo itself.
use ropey::Rope;

/// Replace the chars in `start..end` with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl Edit {
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self { start: at, end: at, text: text.into() }
    }

    pub fn delete(start: usize, end: usize) -> Self {
        Self { start, end, text: String::new() }
    }

    pub fn replace(start: usize, end: usize, text: impl Into<String>) -> Self {
        Self { start, end, text: text.into() }
    }

    /// Where this edit's text ends once it's in.
    pub fn new_end(&self) -> usize {
        self.start + self.text.chars().count()
    }

    /// Makes the change and returns the edit that undoes it.
    pub fn apply(&self, rope: &mut Rope) -> Edit {
        let end = self.end.min(rope.len_chars());
        let start = self.start.min(end);
        let removed = rope.slice(start..end).to_string();
        rope.remove(start..end);
        rope.insert(start, &self.text);
        Edit { start, end: start + self.text.chars().count(), text: removed }
    }

    /// Where `pos` ends up once this edit is made. A position inside the replaced range goes
    /// to the end of the new text, which is where a caret typing there should be.
    pub fn map(&self, pos: usize) -> usize {
        if pos <= self.start {
            pos
        } else if pos >= self.end {
            pos - (self.end - self.start) + self.text.chars().count()
        } else {
            self.new_end()
        }
    }
}

/// Edits made together and undone together. Each edit's positions are in the text as it is
/// after the ones before it, so callers with several carets list them from the last one up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transaction {
    pub edits: Vec<Edit>,
}

impl Transaction {
    pub fn new(edits: Vec<Edit>) -> Self {
        Self { edits }
    }

    pub fn is_empty(&self) -> bool {
        self.edits.iter().all(|e| e.start == e.end && e.text.is_empty())
    }

    pub fn apply(&self, rope: &mut Rope) -> Transaction {
        let mut undo: Vec<Edit> = self.edits.iter().map(|e| e.apply(rope)).collect();
        undo.reverse();
        Transaction { edits: undo }
    }

    pub fn map(&self, pos: usize) -> usize {
        self.edits.iter().fold(pos, |p, e| e.map(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_and_its_undo() {
        let mut r = Rope::from_str("hello world");
        let undo = Edit::replace(6, 11, "there").apply(&mut r);
        assert_eq!(r.to_string(), "hello there");
        undo.apply(&mut r);
        assert_eq!(r.to_string(), "hello world");
    }

    #[test]
    fn positions_follow_an_edit() {
        let e = Edit::replace(2, 4, "xyz");
        assert_eq!((e.map(1), e.map(2), e.map(3), e.map(4), e.map(9)), (1, 2, 5, 5, 10));
    }

    #[test]
    fn a_transaction_undoes_in_reverse() {
        let mut r = Rope::from_str("a\nb\nc");
        let t = Transaction::new(vec![Edit::insert(4, "// "), Edit::insert(2, "// "), Edit::insert(0, "// ")]);
        let undo = t.apply(&mut r);
        assert_eq!(r.to_string(), "// a\n// b\n// c");
        undo.apply(&mut r);
        assert_eq!(r.to_string(), "a\nb\nc");
    }
}
