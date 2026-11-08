//! Where a language server would plug in. gitem doesn't start one: this is the shape of what
//! the editor tells one and asks of it, so that adding a real one (over `lsp-types` and a
//! child process) changes this file and not the editor.
//!
//! Positions are 0-based lines and char columns, as the editor keeps them. A real server
//! speaks UTF-16 columns; converting is that implementation's job.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Hint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub start: Position,
    pub end: Position,
    pub severity: Severity,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub insert: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// Relative to the repository root.
    pub path: String,
    pub at: Position,
}

/// What the editor tells a language server and asks of it. Every call is made on the UI
/// thread, so an implementation hands the work to its own thread and answers from what it
/// has; `diagnostics` is read every frame.
pub trait LanguageServer: Send {
    fn did_open(&mut self, path: &str, text: &str);
    /// The whole text, with the buffer's version. Incremental changes are an optimisation an
    /// implementation can add by diffing.
    fn did_change(&mut self, path: &str, version: u64, text: &str);
    fn did_save(&mut self, path: &str);
    fn did_close(&mut self, path: &str);
    fn diagnostics(&self, path: &str) -> Vec<Diagnostic>;
    fn completions(&mut self, path: &str, at: Position) -> Vec<Completion>;
    fn hover(&mut self, path: &str, at: Position) -> Option<String>;
    fn definition(&mut self, path: &str, at: Position) -> Option<Location>;
}

/// The parts of each line a diagnostic covers, for underlining: `(line, from col, to col,
/// severity)`. A diagnostic over several lines underlines each, to its end or from its start.
pub fn underlines(diags: &[Diagnostic], line_len: impl Fn(usize) -> usize) -> Vec<(usize, usize, usize, Severity)> {
    let mut out = Vec::new();
    for d in diags {
        for line in d.start.line..=d.end.line {
            let from = if line == d.start.line { d.start.col } else { 0 };
            let to = if line == d.end.line { d.end.col } else { line_len(line) };
            // An empty range still shows, as a mark one char wide.
            out.push((line, from, to.max(from + 1), d.severity));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that only remembers what it was told, standing in for a real one.
    #[derive(Default)]
    struct Recorder {
        calls: Vec<String>,
        diags: Vec<Diagnostic>,
    }

    impl LanguageServer for Recorder {
        fn did_open(&mut self, path: &str, _: &str) {
            self.calls.push(format!("open {path}"));
        }
        fn did_change(&mut self, path: &str, v: u64, text: &str) {
            self.calls.push(format!("change {path} {v}"));
            // Pretend every `todo` is a warning.
            self.diags = text.lines().enumerate().filter_map(|(i, l)| l.find("todo").map(|c| Diagnostic { start: Position { line: i, col: c }, end: Position { line: i, col: c + 4 }, severity: Severity::Warning, message: "todo".into() })).collect();
        }
        fn did_save(&mut self, path: &str) {
            self.calls.push(format!("save {path}"));
        }
        fn did_close(&mut self, path: &str) {
            self.calls.push(format!("close {path}"));
        }
        fn diagnostics(&self, _: &str) -> Vec<Diagnostic> {
            self.diags.clone()
        }
        fn completions(&mut self, _: &str, _: Position) -> Vec<Completion> {
            Vec::new()
        }
        fn hover(&mut self, _: &str, _: Position) -> Option<String> {
            None
        }
        fn definition(&mut self, _: &str, _: Position) -> Option<Location> {
            None
        }
    }

    #[test]
    fn a_server_behind_the_trait_reports_and_the_editor_underlines() {
        let mut s: Box<dyn LanguageServer> = Box::new(Recorder::default());
        s.did_open("a.rs", "x");
        s.did_change("a.rs", 2, "ok\n  todo here\n");
        let d = s.diagnostics("a.rs");
        assert_eq!(underlines(&d, |_| 20), vec![(1, 2, 6, Severity::Warning)]);
        let multi = Diagnostic { start: Position { line: 0, col: 3 }, end: Position { line: 2, col: 1 }, severity: Severity::Error, message: String::new() };
        assert_eq!(underlines(&[multi], |_| 9), vec![(0, 3, 9, Severity::Error), (1, 0, 9, Severity::Error), (2, 0, 1, Severity::Error)]);
    }
}
