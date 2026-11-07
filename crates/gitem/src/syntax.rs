//! Syntax highlighting via syntect. Grammars load lazily on first use, on a
//! worker thread, and results are plain colour spans the UI can draw cheaply.
use std::sync::OnceLock;

use egui::Color32;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

pub type Spans = Vec<(usize, usize, Color32)>;

/// Files above this size are shown without highlighting to stay responsive.
pub const MAX_BYTES: usize = 800_000;

struct Sets {
    syntax: SyntaxSet,
    dark: Theme,
    light: Theme,
}

fn sets() -> &'static Sets {
    static S: OnceLock<Sets> = OnceLock::new();
    S.get_or_init(|| {
        let mut themes = ThemeSet::load_defaults();
        Sets {
            syntax: SyntaxSet::load_defaults_newlines(),
            dark: themes.themes.remove("base16-ocean.dark").expect("bundled theme"),
            light: themes.themes.remove("InspiredGitHub").expect("bundled theme"),
        }
    })
}

fn syntax_for<'a>(s: &'a SyntaxSet, path: &str) -> &'a SyntaxReference {
    let ext = path.rsplit('.').next().unwrap_or("");
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    s.find_syntax_by_extension(ext).or_else(|| s.find_syntax_by_extension(name)).unwrap_or_else(|| s.find_syntax_plain_text())
}

/// Highlight consecutive lines of one logical text.
pub fn highlight(path: &str, lines: &[&str], dark: bool) -> Vec<Spans> {
    // Only what can be drawn: colouring a whole 5 MB line takes many seconds.
    let shown = |l: &&str| gitem_core::util::prefix(l, crate::widgets::MAX_SHOWN_LINE_BYTES).len();
    let total: usize = lines.iter().map(shown).sum();
    if total > MAX_BYTES {
        return vec![Vec::new(); lines.len()];
    }
    let st = sets();
    let theme = if dark { &st.dark } else { &st.light };
    let mut h = HighlightLines::new(syntax_for(&st.syntax, path), theme);
    let mut out = Vec::with_capacity(lines.len());
    for l in lines {
        let l = &gitem_core::util::prefix(l, crate::widgets::MAX_SHOWN_LINE_BYTES);
        let with_nl = format!("{l}\n");
        let mut spans = Vec::new();
        if let Ok(parts) = h.highlight_line(&with_nl, &st.syntax) {
            let mut pos = 0;
            for (style, piece) in parts {
                let end = (pos + piece.len()).min(l.len());
                if end > pos {
                    let c = style.foreground;
                    spans.push((pos, end, Color32::from_rgb(c.r, c.g, c.b)));
                }
                pos += piece.len();
            }
        }
        out.push(spans);
    }
    out
}
