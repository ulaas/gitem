//! Syntax colours for the editor, worked out as far down as is shown and kept between frames.
//! The parser's state is saved every `EVERY` lines, so an edit only re-colours from the save
//! point above it, and only as far as the view reaches.
use std::time::{Duration, Instant};

use egui::Color32;
use gitem_text::Buffer;
use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter};
use syntect::parsing::{ParseState, ScopeStack};

use crate::syntax::{self, Spans};

const EVERY: usize = 256;

/// Past this many bytes a line is shown uncoloured. Colouring is per character class and a
/// minified file's one line would take seconds.
const LONGEST_COLOURED: usize = 20_000;

pub struct Highlight {
    engine: Option<syntax::Engine>,
    /// The parser's state at the start of line `i * EVERY`.
    checkpoints: Vec<(ParseState, HighlightState)>,
    /// Byte ranges and colours per line. Lines from `valid` on may be stale, shown until
    /// they're done again.
    spans: Vec<Spans>,
    valid: usize,
}

impl Highlight {
    pub fn new(path: &str, dark: bool) -> Self {
        let engine = syntax::engine(path, dark);
        let checkpoints = engine.as_ref().map(|e| vec![(ParseState::new(e.syntax), HighlightState::new(&Highlighter::new(e.theme), ScopeStack::new()))]).unwrap_or_default();
        Self { engine, checkpoints, spans: Vec::new(), valid: 0 }
    }

    /// A change starting at `line`: everything from there down has to be done again.
    pub fn edited(&mut self, line: usize) {
        self.valid = self.valid.min(line);
        self.checkpoints.truncate(line / EVERY + 1);
    }

    /// Lines `..upto` coloured, as far as `budget` allows. `true` when done, else the caller
    /// asks for another frame.
    pub fn ensure(&mut self, buf: &Buffer, upto: usize, budget: Duration) -> bool {
        let Some(e) = &self.engine else { return true };
        let upto = upto.min(buf.line_count());
        if self.valid >= upto {
            return true;
        }
        let started = Instant::now();
        let highlighter = Highlighter::new(e.theme);
        let k = (self.valid / EVERY).min(self.checkpoints.len() - 1);
        let (mut ps, mut hs) = self.checkpoints[k].clone();
        let mut line = k * EVERY;
        if self.spans.len() < buf.line_count() {
            self.spans.resize(buf.line_count(), Vec::new());
        }
        self.spans.truncate(buf.line_count());
        while line < upto {
            if line.is_multiple_of(EVERY) && line / EVERY == self.checkpoints.len() {
                self.checkpoints.push((ps.clone(), hs.clone()));
            }
            let text = buf.line(line);
            let shown = gitem_core::util::prefix(&text, LONGEST_COLOURED);
            let with_nl = format!("{shown}\n");
            let mut spans = Vec::new();
            if let Ok(ops) = ps.parse_line(&with_nl, e.set) {
                let mut pos = 0;
                for (style, piece) in HighlightIterator::new(&mut hs, &ops, &with_nl, &highlighter) {
                    let end = (pos + piece.len()).min(shown.len());
                    if end > pos {
                        let c = style.foreground;
                        spans.push((pos, end, Color32::from_rgb(c.r, c.g, c.b)));
                    }
                    pos += piece.len();
                }
            }
            self.spans[line] = spans;
            line += 1;
            self.valid = self.valid.max(line);
            if line.is_multiple_of(64) && started.elapsed() > budget {
                return line >= upto;
            }
        }
        true
    }

    pub fn line(&self, i: usize) -> Option<&Spans> {
        self.spans.get(i)
    }

    pub fn is_plain(&self) -> bool {
        self.engine.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_recolours_from_the_save_point_above_it() {
        let text: String = (0..1000).map(|i| format!("let x{i} = \"s\"; // c\n")).collect();
        let buf = Buffer::from_text(&text);
        let mut h = Highlight::new("a.rs", true);
        assert!(h.ensure(&buf, 600, Duration::from_secs(5)));
        assert_eq!(h.valid, 600);
        assert_eq!(h.checkpoints.len(), 3);
        let before = h.line(500).cloned();
        h.edited(520);
        assert_eq!((h.valid, h.checkpoints.len()), (520, 3));
        h.edited(300);
        assert_eq!((h.valid, h.checkpoints.len()), (300, 2));
        assert!(h.ensure(&buf, 600, Duration::from_secs(5)));
        assert_eq!(h.line(500).cloned(), before, "the same text gets the same colours");
        assert!(h.line(500).unwrap().len() > 2);
        assert!(Highlight::new("notes.txt", true).is_plain());
    }

    #[test]
    fn a_string_opened_above_colours_what_is_below() {
        let mut buf = Buffer::from_text("let a = 1;\nlet b = 2;\n");
        let mut h = Highlight::new("a.rs", true);
        h.ensure(&buf, 2, Duration::from_secs(5));
        let plain = h.line(1).cloned();
        let at = buf.pos(0, 8);
        buf.apply(&gitem_text::Transaction::new(vec![gitem_text::Edit::insert(at, "\"")]));
        h.edited(0);
        h.ensure(&buf, 2, Duration::from_secs(5));
        assert_ne!(h.line(1).cloned(), plain, "line 2 is inside the string now");
    }
}
