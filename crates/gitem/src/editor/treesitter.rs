//! With the `tree-sitter` feature: the outline and folding from the file's syntax tree, for
//! Rust, Python, JavaScript and Go. Other files, and builds without the feature, go by
//! pattern and indent instead.
use tree_sitter::{Language, Node, Parser};

use super::outline::Symbol;

/// The grammar for a file, and which kinds of node are definitions, with the word to show.
fn grammar(path: &str) -> Option<(Language, &'static [(&'static str, &'static str)])> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => (
            tree_sitter_rust::LANGUAGE.into(),
            &[("function_item", "fn"), ("function_signature_item", "fn"), ("struct_item", "struct"), ("enum_item", "enum"), ("trait_item", "trait"), ("impl_item", "impl"), ("mod_item", "mod"), ("macro_definition", "macro"), ("const_item", "const"), ("static_item", "static"), ("type_item", "type")][..],
        ),
        "py" | "pyi" => (tree_sitter_python::LANGUAGE.into(), &[("function_definition", "def"), ("class_definition", "class")][..]),
        "js" | "jsx" | "mjs" | "cjs" => (
            tree_sitter_javascript::LANGUAGE.into(),
            &[("function_declaration", "function"), ("generator_function_declaration", "function"), ("class_declaration", "class"), ("method_definition", "method")][..],
        ),
        "go" => (tree_sitter_go::LANGUAGE.into(), &[("function_declaration", "func"), ("method_declaration", "func"), ("type_spec", "type")][..]),
        _ => return None,
    })
}

fn parse(path: &str, text: &str) -> Option<(tree_sitter::Tree, &'static [(&'static str, &'static str)])> {
    let (lang, kinds) = grammar(path)?;
    let mut parser = Parser::new();
    parser.set_language(&lang).ok()?;
    Some((parser.parse(text, None)?, kinds))
}

fn text_of<'a>(node: Node, text: &'a str) -> &'a str {
    text.get(node.byte_range()).unwrap_or("")
}

/// The definitions, nested as the syntax nests them. `None` for a language without a grammar.
pub fn symbols(path: &str, text: &str) -> Option<Vec<Symbol>> {
    let (tree, kinds) = parse(path, text)?;
    let mut out = Vec::new();
    walk(tree.root_node(), text, kinds, 0, &mut out);
    Some(out)
}

fn walk(node: Node, text: &str, kinds: &[(&str, &str)], depth: usize, out: &mut Vec<Symbol>) {
    let mut depth_inside = depth;
    if let Some((_, word)) = kinds.iter().find(|(k, _)| *k == node.kind()) {
        let name = match node.kind() {
            // `impl Display for Point` reads better than the type alone.
            "impl_item" => {
                let ty = node.child_by_field_name("type").map(|n| text_of(n, text)).unwrap_or("");
                match node.child_by_field_name("trait") {
                    Some(t) => format!("{} for {ty}", text_of(t, text)),
                    None => ty.to_string(),
                }
            }
            _ => node.child_by_field_name("name").map(|n| text_of(n, text).to_string()).unwrap_or_default(),
        };
        if !name.is_empty() {
            out.push(Symbol { line: node.start_position().row, name, kind: word.to_string(), depth });
            depth_inside += 1;
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, text, kinds, depth_inside, out);
    }
}

/// Blocks that can fold: each node over more than one line, by its first line, keeping the
/// largest for a line. A closing bracket on the last line stays showing, as with indent folds.
pub fn fold_ranges(path: &str, text: &str) -> Option<Vec<(usize, usize)>> {
    let (tree, _) = parse(path, text)?;
    let lines: Vec<&str> = text.lines().collect();
    let mut by_start: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let (s, e) = (node.start_position().row, node.end_position().row);
        if e > s && node.parent().is_some() && node.is_named() {
            let closer = lines.get(e).is_some_and(|l| l.trim_start().starts_with(['}', ')', ']']));
            let end = if closer { e - 1 } else { e };
            if end > s {
                let slot = by_start.entry(s).or_insert(end);
                *slot = (*slot).max(end);
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    Some(by_start.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_symbols_nest_and_impls_name_their_trait() {
        let src = "struct Point { x: i32 }\nimpl std::fmt::Display for Point {\n    fn fmt(&self) {}\n}\nmod inner {\n    pub fn deep() {}\n}\n";
        let s: Vec<(String, String, usize)> = symbols("a.rs", src).unwrap().into_iter().map(|s| (s.name, s.kind, s.depth)).collect();
        assert_eq!(
            s,
            vec![("Point".into(), "struct".into(), 0), ("std::fmt::Display for Point".into(), "impl".into(), 0), ("fmt".into(), "fn".into(), 1), ("inner".into(), "mod".into(), 0), ("deep".into(), "fn".into(), 1)]
        );
        assert!(symbols("notes.txt", "x").is_none());
    }

    #[test]
    fn folds_follow_the_syntax_not_the_indent() {
        // A call spread over lines with no deeper indent still folds.
        let src = "fn a() {\nlet v = vec![\n1,\n2,\n];\n}\n";
        let f = fold_ranges("a.rs", src).unwrap();
        assert!(f.contains(&(0, 4)), "{f:?}");
        assert!(f.contains(&(1, 3)), "{f:?}");
    }

    #[test]
    fn python_and_go_and_javascript() {
        let names = |p: &str, t: &str| symbols(p, t).unwrap().into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names("a.py", "class A:\n    def run(self):\n        pass\n"), ["A", "run"]);
        assert_eq!(names("a.go", "package m\ntype T struct{}\nfunc (t T) Go() {}\n"), ["T", "Go"]);
        assert_eq!(names("a.js", "class S { get() {} }\nfunction f() {}\n"), ["S", "get", "f"]);
    }
}
