//! The definitions in a file, for going to one by name. Found by the shape of the line, which
//! works for most languages without knowing any of them; the `tree-sitter` build reads the
//! syntax instead, for the languages it has.
use std::sync::OnceLock;

use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub line: usize,
    pub name: String,
    /// What the line called it: `fn`, `class`, `#` for a heading, and so on.
    pub kind: String,
    /// How deeply it's nested, for indenting the list.
    pub depth: usize,
}

fn definition() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^(\s*)(?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?(?:export\s+)?(?:default\s+)?(?:(?:public|private|protected|internal|static|abstract|final|unsafe|const)\s+)*(fn|func|function|def|class|struct|enum|trait|impl|interface|module|mod|namespace|type|sub|macro_rules!)\s+([A-Za-z_][\w:<>, ]*)").expect("valid regex")
    })
}

fn heading() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(#{1,6})\s+(.+?)\s*#*$").expect("valid regex"))
}

/// The definitions in `lines`, in order. Markdown files list their headings.
pub fn by_pattern(path: &str, lines: impl Iterator<Item = String>) -> Vec<Symbol> {
    let markdown = path.to_ascii_lowercase().ends_with(".md") || path.to_ascii_lowercase().ends_with(".markdown");
    let mut out = Vec::new();
    let mut fenced = false;
    for (i, text) in lines.enumerate() {
        if markdown {
            if text.trim_start().starts_with("```") {
                fenced = !fenced;
            }
            if let Some(c) = heading().captures(&text).filter(|_| !fenced) {
                out.push(Symbol { line: i, name: c[2].to_string(), kind: c[1].to_string(), depth: c[1].len() - 1 });
            }
            continue;
        }
        if let Some(c) = definition().captures(&text) {
            let indent = c[1].chars().map(|ch| if ch == '\t' { 4 } else { 1 }).sum::<usize>();
            let name = c[3].trim().trim_end_matches(['<', ',', ' ', ':']).to_string();
            out.push(Symbol { line: i, name, kind: c[2].to_string(), depth: indent / 4 });
        }
    }
    out
}

/// The symbols for a file: from its syntax with the `tree-sitter` build when the language is
/// known, otherwise by pattern.
pub fn symbols(path: &str, buf: &gitem_text::Buffer) -> Vec<Symbol> {
    #[cfg(feature = "tree-sitter")]
    if let Some(s) = super::treesitter::symbols(path, &buf.text()) {
        return s;
    }
    by_pattern(path, (0..buf.line_count()).map(|i| buf.line(i)))
}

/// Symbols whose name has the query's letters in order, best first.
pub fn filter<'a>(all: &'a [Symbol], query: &str) -> Vec<&'a Symbol> {
    if query.trim().is_empty() {
        return all.iter().collect();
    }
    let mut scored: Vec<(i32, &Symbol)> = all.iter().filter_map(|s| gitem_core::util::fuzzy_match(query, &s.name).map(|(score, _)| (score, s))).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.line.cmp(&b.1.line)));
    scored.into_iter().map(|(_, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(path: &str, text: &str) -> Vec<(usize, String, String, usize)> {
        by_pattern(path, text.lines().map(String::from)).into_iter().map(|s| (s.line, s.name, s.kind, s.depth)).collect()
    }

    #[test]
    fn definitions_in_several_languages() {
        let rust = "use x;\npub struct Point {\n}\nimpl Point {\n    pub fn new() -> Self {}\n    async fn go(&self) {}\n}\n";
        assert_eq!(
            of("a.rs", rust),
            vec![(1, "Point".into(), "struct".into(), 0), (3, "Point".into(), "impl".into(), 0), (4, "new".into(), "fn".into(), 1), (5, "go".into(), "fn".into(), 1)]
        );
        let py = "class A:\n    def run(self):\n        pass\ndef main():\n    pass\n";
        assert_eq!(of("a.py", py).iter().map(|s| s.1.as_str()).collect::<Vec<_>>(), ["A", "run", "main"]);
        let js = "export default function app() {}\nexport class Store {}\n";
        assert_eq!(of("a.js", js).iter().map(|s| s.1.as_str()).collect::<Vec<_>>(), ["app", "Store"]);
    }

    #[test]
    fn markdown_lists_its_headings_but_not_inside_code() {
        let md = "# Title\n\ntext\n## Part one\n```\n# not a heading\n```\n### Deeper ###\n";
        assert_eq!(of("README.md", md), vec![(0, "Title".into(), "#".into(), 0), (3, "Part one".into(), "##".into(), 1), (7, "Deeper".into(), "###".into(), 2)]);
    }

    #[test]
    fn the_filter_finds_letters_in_order() {
        let all = by_pattern("a.rs", ["fn parse_header() {}", "fn render() {}", "fn print_help() {}"].into_iter().map(String::from));
        let names: Vec<&str> = filter(&all, "ph").iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"parse_header") && names.contains(&"print_help") && !names.contains(&"render"), "{names:?}");
    }
}
