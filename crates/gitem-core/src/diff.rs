//! Unified-diff model: parsing git output, laying out rows for the visual diff,
//! word-level highlighting, and building partial patches for hunk/line staging.
use similar::{Algorithm, DiffOp, capture_diff_slices};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
    /// `\ No newline at end of file`
    NoNewline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
    /// The line ended in CR. Display drops it; patches must restore it to apply.
    pub cr: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub section: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    pub old_path: String,
    pub new_path: String,
    pub is_new: bool,
    pub is_deleted: bool,
    pub is_rename: bool,
    pub is_binary: bool,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub similarity: Option<u32>,
    /// Header lines up to the first hunk; needed to rebuild patches.
    pub header: Vec<String>,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn additions(&self) -> usize {
        self.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == LineKind::Add).count()
    }
    pub fn deletions(&self) -> usize {
        self.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == LineKind::Del).count()
    }
    pub fn display_path(&self) -> &str {
        if self.new_path.is_empty() { &self.old_path } else { &self.new_path }
    }
}

pub(crate) fn unquote_path(p: &str) -> String {
    let p = p.trim_end_matches('\t');
    let p = p.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(p);
    p.replace("\\\"", "\"").replace("\\\\", "\\").replace("\\t", "\t")
}

fn strip_ab(p: &str) -> String {
    let u = unquote_path(p);
    if u == "/dev/null" {
        return String::new();
    }
    u.strip_prefix("a/").or_else(|| u.strip_prefix("b/")).map(String::from).unwrap_or(u)
}

fn parse_hunk_header(l: &str) -> Option<Hunk> {
    // @@ -a,b +c,d @@ section
    let rest = l.strip_prefix("@@ -")?;
    let (ranges, section) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let pair = |s: &str| -> Option<(u32, u32)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (old_start, old_count) = pair(old)?;
    let (new_start, new_count) = pair(new)?;
    Some(Hunk { old_start, old_count, new_start, new_count, section: section.trim().to_string(), lines: Vec::new() })
}

/// Parse the output of `git diff` / `git show`. Tolerates any leading text.
/// The `(old start, old count, new start, new count)` of each hunk in a `-U0` diff.
pub fn zero_context_hunks(text: &str) -> Vec<(u32, u32, u32, u32)> {
    text.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("@@ -")?;
            let (old, rest) = rest.split_once(" +")?;
            let (new, _) = rest.split_once(" @@")?;
            let part = |s: &str| -> Option<(u32, u32)> {
                match s.split_once(',') {
                    Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
                    None => Some((s.parse().ok()?, 1)),
                }
            };
            let (a, b) = (part(old)?, part(new)?);
            Some((a.0, a.1, b.0, b.1))
        })
        .collect()
}

/// Which line of the old file a line of the new one was, given a file's `-U0` hunks. `None` for
/// a line those changes added.
pub fn line_before_changes(hunks: &[(u32, u32, u32, u32)], line: u32) -> Option<u32> {
    let mut shift: i64 = 0;
    for (old_start, old_count, new_start, new_count) in hunks {
        // With no lines in it, a hunk's start is the line before where it goes.
        let new_first = if *new_count == 0 { new_start + 1 } else { *new_start };
        if line < new_first {
            break;
        }
        if line < new_first + new_count {
            return None;
        }
        shift = *old_start as i64 + *old_count as i64 - if *old_count == 0 { -1 } else { 0 } - (new_first as i64 + *new_count as i64);
    }
    u32::try_from(line as i64 + shift).ok()
}

#[cfg(test)]
#[test]
fn a_line_is_followed_back_through_uncommitted_edits() {
    // Line 2 replaced, and two lines put in after line 5.
    let hunks = zero_context_hunks("@@ -2 +2 @@\n-a\n+b\n@@ -5,0 +6,2 @@\n+c\n+d\n");
    assert_eq!(hunks, [(2, 1, 2, 1), (5, 0, 6, 2)]);
    assert_eq!(line_before_changes(&hunks, 1), Some(1));
    assert_eq!(line_before_changes(&hunks, 2), None);
    assert_eq!(line_before_changes(&hunks, 5), Some(5));
    assert_eq!(line_before_changes(&hunks, 6), None);
    assert_eq!(line_before_changes(&hunks, 8), Some(6));
    // A line taken out moves what's after it down by one.
    let gone = zero_context_hunks("@@ -3 +2,0 @@\n-x\n");
    assert_eq!(line_before_changes(&gone, 2), Some(2));
    assert_eq!(line_before_changes(&gone, 3), Some(4));
}

pub fn parse_unified(input: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur: Option<FileDiff> = None;
    let mut hunk: Option<Hunk> = None;
    let (mut old_no, mut new_no) = (0u32, 0u32);

    let flush_hunk = |cur: &mut Option<FileDiff>, hunk: &mut Option<Hunk>| {
        if let (Some(f), Some(h)) = (cur.as_mut(), hunk.take()) {
            f.hunks.push(h);
        }
    };

    for raw in input.split('\n') {
        let line = raw;
        if line.starts_with("diff --git ") || line.starts_with("diff --cc ") || line.starts_with("diff --combined ") {
            flush_hunk(&mut cur, &mut hunk);
            if let Some(f) = cur.take() {
                files.push(f);
            }
            let mut f = FileDiff { header: vec![line.to_string()], ..Default::default() };
            if let Some(rest) = line.strip_prefix("diff --git ") {
                let (a, b) = split_git_paths(rest);
                f.old_path = a;
                f.new_path = b;
            } else if let Some(rest) = line.strip_prefix("diff --cc ").or_else(|| line.strip_prefix("diff --combined ")) {
                f.old_path = unquote_path(rest);
                f.new_path = f.old_path.clone();
            }
            cur = Some(f);
            continue;
        }
        if hunk.is_some() {
            let first = line.as_bytes().first().copied();
            let h = hunk.as_mut().unwrap();
            let t = line.get(1..).unwrap_or("");
            let cr = t.ends_with('\r');
            let t = t.strip_suffix('\r').unwrap_or(t);
            match first {
                Some(b'+') => {
                    h.lines.push(DiffLine { kind: LineKind::Add, old_no: None, new_no: Some(new_no), text: t.to_string(), cr });
                    new_no += 1;
                    continue;
                }
                Some(b'-') => {
                    h.lines.push(DiffLine { kind: LineKind::Del, old_no: Some(old_no), new_no: None, text: t.to_string(), cr });
                    old_no += 1;
                    continue;
                }
                Some(b' ') => {
                    h.lines.push(DiffLine { kind: LineKind::Context, old_no: Some(old_no), new_no: Some(new_no), text: t.to_string(), cr });
                    old_no += 1;
                    new_no += 1;
                    continue;
                }
                Some(b'\\') => {
                    h.lines.push(DiffLine { kind: LineKind::NoNewline, old_no: None, new_no: None, text: t.trim().to_string(), cr: false });
                    continue;
                }
                _ => flush_hunk(&mut cur, &mut hunk),
            }
        }
        if line.starts_with("@@ ") {
            flush_hunk(&mut cur, &mut hunk);
            if let Some(h) = parse_hunk_header(line) {
                old_no = h.old_start;
                new_no = h.new_start;
                hunk = Some(h);
            }
            continue;
        }
        let Some(f) = cur.as_mut() else { continue };
        if line.is_empty() {
            continue;
        }
        f.header.push(line.to_string());
        if let Some(p) = line.strip_prefix("--- ") {
            let s = strip_ab(p);
            if !s.is_empty() {
                f.old_path = s;
            }
        } else if let Some(p) = line.strip_prefix("+++ ") {
            let s = strip_ab(p);
            if !s.is_empty() {
                f.new_path = s;
            }
        } else if line.starts_with("new file mode") {
            f.is_new = true;
            f.new_mode = line.rsplit(' ').next().map(String::from);
        } else if line.starts_with("deleted file mode") {
            f.is_deleted = true;
            f.old_mode = line.rsplit(' ').next().map(String::from);
        } else if let Some(m) = line.strip_prefix("old mode ") {
            f.old_mode = Some(m.to_string());
        } else if let Some(m) = line.strip_prefix("new mode ") {
            f.new_mode = Some(m.to_string());
        } else if let Some(p) = line.strip_prefix("rename from ") {
            f.is_rename = true;
            f.old_path = unquote_path(p);
        } else if let Some(p) = line.strip_prefix("rename to ") {
            f.new_path = unquote_path(p);
        } else if let Some(p) = line.strip_prefix("similarity index ") {
            f.similarity = p.trim_end_matches('%').parse().ok();
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            f.is_binary = true;
        }
    }
    flush_hunk(&mut cur, &mut hunk);
    if let Some(f) = cur.take() {
        files.push(f);
    }
    files
}

/// Split `a/x b/x` from a `diff --git` line; handles quotes and spaces in names.
fn split_git_paths(rest: &str) -> (String, String) {
    if rest.starts_with('"')
        && let Some(end) = rest[1..].find("\" ") {
            let a = &rest[..end + 2];
            let b = &rest[end + 3..];
            return (strip_ab(a), strip_ab(b));
        }
    // Without renames both names are identical: find the split where they match.
    let bytes = rest.as_bytes();
    for (i, w) in bytes.windows(3).enumerate() {
        if w == b" b/" {
            let (a, b) = (&rest[..i], &rest[i + 1..]);
            if a.strip_prefix("a/") == b.strip_prefix("b/") {
                return (strip_ab(a), strip_ab(b));
            }
        }
    }
    match rest.find(" b/") {
        Some(i) => (strip_ab(&rest[..i]), strip_ab(&rest[i + 1..])),
        None => {
            // --no-index / custom prefixes: split in the middle space.
            match rest.split_once(' ') {
                Some((a, b)) => (strip_ab(a), strip_ab(b)),
                None => (rest.to_string(), rest.to_string()),
            }
        }
    }
}

/// A row of the side-by-side view. `hunk`/`line` index into the source `FileDiff`
/// so a row can be mapped back for line staging.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SideRow {
    pub left: Option<(u32, String)>,
    pub right: Option<(u32, String)>,
    pub left_kind: Option<LineKind>,
    pub right_kind: Option<LineKind>,
    pub hunk: usize,
    pub left_idx: Option<usize>,
    pub right_idx: Option<usize>,
    pub is_hunk_header: bool,
    pub header: String,
}

/// Lay out hunks for side-by-side display: runs of deletions pair with the
/// following run of additions so changed lines sit on the same row.
pub fn side_by_side(f: &FileDiff) -> Vec<SideRow> {
    let mut rows = Vec::new();
    for (hi, h) in f.hunks.iter().enumerate() {
        rows.push(SideRow {
            hunk: hi,
            is_hunk_header: true,
            header: format!("@@ -{},{} +{},{} @@ {}", h.old_start, h.old_count, h.new_start, h.new_count, h.section),
            ..Default::default()
        });
        let mut i = 0;
        while i < h.lines.len() {
            let l = &h.lines[i];
            match l.kind {
                LineKind::Context => {
                    rows.push(SideRow {
                        left: Some((l.old_no.unwrap_or(0), l.text.clone())),
                        right: Some((l.new_no.unwrap_or(0), l.text.clone())),
                        left_kind: Some(LineKind::Context),
                        right_kind: Some(LineKind::Context),
                        hunk: hi,
                        left_idx: Some(i),
                        right_idx: Some(i),
                        ..Default::default()
                    });
                    i += 1;
                }
                LineKind::NoNewline => i += 1,
                LineKind::Del | LineKind::Add => {
                    let start = i;
                    while i < h.lines.len() && matches!(h.lines[i].kind, LineKind::Del | LineKind::NoNewline) {
                        i += 1;
                    }
                    let dels: Vec<usize> = (start..i).filter(|&k| h.lines[k].kind == LineKind::Del).collect();
                    let add_start = i;
                    while i < h.lines.len() && matches!(h.lines[i].kind, LineKind::Add | LineKind::NoNewline) {
                        i += 1;
                    }
                    let adds: Vec<usize> = (add_start..i).filter(|&k| h.lines[k].kind == LineKind::Add).collect();
                    for k in 0..dels.len().max(adds.len()) {
                        let d = dels.get(k).map(|&x| (x, &h.lines[x]));
                        let a = adds.get(k).map(|&x| (x, &h.lines[x]));
                        // The two read the same, so the only difference has to be said.
                        let same = matches!((d, a), (Some((_, x)), Some((_, y))) if x.text == y.text);
                        let shown = |x: usize, l: &DiffLine| if same { format!("{}{}", l.text, line_end_mark(h, x)) } else { l.text.clone() };
                        rows.push(SideRow {
                            left: d.map(|(x, l)| (l.old_no.unwrap_or(0), shown(x, l))),
                            right: a.map(|(x, l)| (l.new_no.unwrap_or(0), shown(x, l))),
                            left_kind: d.map(|_| LineKind::Del),
                            right_kind: a.map(|_| LineKind::Add),
                            hunk: hi,
                            left_idx: d.map(|x| x.0),
                            right_idx: a.map(|x| x.0),
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }
    rows
}

/// How a line ends, for lines whose text alone wouldn't show what changed.
fn line_end_mark(h: &Hunk, at: usize) -> &'static str {
    let l = &h.lines[at];
    let no_newline = h.lines.get(at + 1).is_some_and(|n| n.kind == LineKind::NoNewline);
    match (l.cr, no_newline) {
        (true, true) => " [CR, no newline]",
        (true, false) => " [CR]",
        (false, true) => " [no newline]",
        (false, false) => " [LF]",
    }
}

/// Byte ranges that differ between two lines, for intra-line highlighting.
pub fn word_diff(old: &str, new: &str) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let mut o = Vec::new();
    let mut n = Vec::new();
    if old.len() + new.len() > 20_000 {
        return (o, n);
    }
    let a = tokens(old);
    let b = tokens(new);
    let offset = |t: &[&str], i: usize| -> usize { t[..i].iter().map(|s| s.len()).sum() };
    for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { old_index, old_len, .. } => push_range(&mut o, offset(&a, old_index), offset(&a, old_index + old_len)),
            DiffOp::Insert { new_index, new_len, .. } => push_range(&mut n, offset(&b, new_index), offset(&b, new_index + new_len)),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                push_range(&mut o, offset(&a, old_index), offset(&a, old_index + old_len));
                push_range(&mut n, offset(&b, new_index), offset(&b, new_index + new_len));
            }
        }
    }
    (o, n)
}

/// Identifier runs, whitespace runs, and individual punctuation characters.
fn tokens(s: &str) -> Vec<&str> {
    let mut v = Vec::new();
    let mut start = 0;
    let mut class = 0u8;
    for (i, c) in s.char_indices() {
        let k = if c.is_alphanumeric() || c == '_' {
            1
        } else if c.is_whitespace() {
            2
        } else {
            3
        };
        if i > start && (k != class || k == 3) {
            v.push(&s[start..i]);
            start = i;
        }
        class = k;
    }
    if start < s.len() {
        v.push(&s[start..]);
    }
    v
}

fn push_range(v: &mut Vec<(usize, usize)>, a: usize, b: usize) {
    if let Some(last) = v.last_mut()
        && last.1 == a {
            last.1 = b;
            return;
        }
    v.push((a, b));
}

/// Whether this hunk's text can be written back exactly. A replacement character means
/// the bytes weren't UTF-8 (or, rarely, are that character, which gets the same answer).
pub fn hunk_is_exact(h: &Hunk) -> bool {
    !h.lines.iter().any(|l| l.text.contains(char::REPLACEMENT_CHARACTER))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchMode {
    /// Stage: applied forward with `git apply --cached`.
    Forward,
    /// Unstage or discard: applied with `git apply --reverse`.
    Reverse,
}

/// Build a patch from selected lines of one hunk.
///
/// `selected` holds indices into `hunk.lines`; `None` selects the whole hunk.
/// Unselected changes are neutralised so they stay untouched: in forward mode an
/// unselected `-` becomes context and an unselected `+` vanishes; in reverse mode
/// the roles swap. Use with `git apply --recount`, which fixes the line counts.
/// `None` when nothing is selected, or when the hunk's text can't be rebuilt exactly.
pub fn build_patch(f: &FileDiff, hunk_idx: usize, selected: Option<&[usize]>, mode: PatchMode) -> Option<String> {
    let h = f.hunks.get(hunk_idx)?;
    // The text was decoded lossily, so a replacement character may stand for bytes that
    // aren't UTF-8. Written back, it would corrupt the file, so such a hunk has no patch.
    if !hunk_is_exact(h) {
        return None;
    }
    let is_sel = |i: usize| selected.is_none_or(|s| s.contains(&i));
    // (sign, line, whether a "no newline" marker follows it)
    let mut kept: Vec<(char, &DiffLine, bool)> = Vec::new();
    let mut any = false;
    let mut last_kept = false;
    for (i, l) in h.lines.iter().enumerate() {
        let sign = match l.kind {
            LineKind::Context => Some(' '),
            LineKind::Add if is_sel(i) => {
                any = true;
                Some('+')
            }
            // Already in the target when reversing: keep it as context.
            LineKind::Add => (mode == PatchMode::Reverse).then_some(' '),
            LineKind::Del if is_sel(i) => {
                any = true;
                Some('-')
            }
            LineKind::Del => (mode == PatchMode::Forward).then_some(' '),
            LineKind::NoNewline => {
                if last_kept && let Some(k) = kept.last_mut() {
                    k.2 = true;
                }
                continue;
            }
        };
        last_kept = sign.is_some();
        if let Some(sign) = sign {
            kept.push((sign, l, false));
        }
    }
    if !any {
        return None;
    }
    // The sign of lines that exist only in the file being patched, and only in the result.
    let (source_only, result_only) = if mode == PatchMode::Forward { ('-', '+') } else { ('+', '-') };
    let last_in_result = kept.iter().rposition(|(sign, ..)| *sign != source_only);
    let mut body = String::new();
    let (mut old_n, mut new_n) = (0u32, 0u32);
    let mut put = |sign: char, l: &DiffLine, no_newline: bool| {
        body.push(sign);
        body.push_str(&l.text);
        if l.cr {
            body.push('\r');
        }
        body.push('\n');
        if no_newline {
            body.push_str("\\ No newline at end of file\n");
        }
        old_n += u32::from(sign != '+');
        new_n += u32::from(sign != '-');
    };
    for (i, (sign, l, no_newline)) in kept.iter().copied().enumerate() {
        // Something from the result follows a line with no newline: it needs one, or
        // git would run the two together. The file being patched still has it without.
        let followed = no_newline && last_in_result != Some(i) && sign != source_only;
        match (followed, sign) {
            (true, ' ') => {
                put(source_only, l, true);
                put(result_only, l, false);
            }
            (true, _) => put(sign, l, false),
            (false, _) => put(sign, l, no_newline),
        }
    }
    let mut out = patch_header(f, mode, old_n, new_n);
    // `git apply` starts searching at the position the patch is applied to. That
    // file isn't the one this hunk's other side was numbered against, and other
    // unstaged hunks shift those numbers, so a patch in a file of repeated blocks
    // could land on the wrong copy. Use the numbering of the file being patched:
    // the old side going forward, and the new side when reversing.
    let at = if mode == PatchMode::Forward { h.old_start } else { h.new_start };
    let (old_at, new_at) = match (mode, old_n, new_n) {
        // With an empty side, its start names the line before the change.
        (PatchMode::Forward, 0, _) => (at, at + 1),
        (PatchMode::Forward, _, 0) => (at, at.saturating_sub(1)),
        (PatchMode::Reverse, 0, _) => (at.saturating_sub(1), at),
        (PatchMode::Reverse, _, 0) => (at + 1, at),
        _ => (at, at),
    };
    out.push_str(&format!("@@ -{old_at},{old_n} +{new_at},{new_n} @@\n"));
    out.push_str(&body);
    Some(out)
}

/// The lines before the hunk. The diff's own header says what happens to the whole file,
/// which is wrong for a patch doing only part of that, and `git apply` holds it to its word.
fn patch_header(f: &FileDiff, mode: PatchMode, old_n: u32, new_n: u32) -> String {
    let path = f.display_path();
    // Taking back part of a staged rename: the rename stays, and this changes the new file.
    let keeps_rename = f.is_rename && mode == PatchMode::Reverse;
    // A new file that part of stays, when taking back only some of its lines. Or a deleted
    // file that part of stays, when staging only some of its deletion.
    let partly_new = f.is_new && mode == PatchMode::Reverse && old_n > 0;
    let partly_deleted = f.is_deleted && mode == PatchMode::Forward && new_n > 0;
    if !keeps_rename && !partly_new && !partly_deleted {
        return f.header.iter().map(|l| format!("{l}\n")).collect();
    }
    format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/f.txt b/f.txt\nindex 111..222 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,4 @@ fn main\n one\n-two\n+TWO\n+2.5\n three\n@@ -10 +11 @@\n-x\n\\ No newline at end of file\n+y\n";

    #[test]
    fn parses() {
        let f = parse_unified(SAMPLE);
        assert_eq!(f.len(), 1);
        let d = &f[0];
        assert_eq!(d.new_path, "f.txt");
        assert_eq!(d.hunks.len(), 2);
        assert_eq!(d.hunks[0].section, "fn main");
        assert_eq!(d.additions(), 3);
        assert_eq!(d.deletions(), 2);
        assert_eq!(d.hunks[0].lines[3].new_no, Some(3));
        assert_eq!(d.hunks[1].lines[1].kind, LineKind::NoNewline);
    }

    #[test]
    fn parses_rename_new_binary() {
        let s = "diff --git a/o.txt b/n.txt\nsimilarity index 90%\nrename from o.txt\nrename to n.txt\ndiff --git a/new b/new\nnew file mode 100644\nindex 000..111\n--- /dev/null\n+++ b/new\n@@ -0,0 +1 @@\n+hi\ndiff --git a/b.png b/b.png\nBinary files a/b.png and b/b.png differ\n";
        let f = parse_unified(s);
        assert_eq!(f.len(), 3);
        assert!(f[0].is_rename);
        assert_eq!(f[0].old_path, "o.txt");
        assert_eq!(f[0].similarity, Some(90));
        assert!(f[1].is_new);
        assert_eq!(f[1].new_path, "new");
        assert!(f[2].is_binary);
    }

    #[test]
    fn paths_with_spaces() {
        let (a, b) = split_git_paths("a/my b/file.txt b/my b/file.txt");
        assert_eq!(a, "my b/file.txt");
        assert_eq!(b, "my b/file.txt");
    }

    #[test]
    fn side_rows_pair_changes() {
        let f = parse_unified(SAMPLE);
        let rows = side_by_side(&f[0]);
        assert!(rows[0].is_hunk_header);
        // "one" context, then two->TWO paired, then 2.5 alone on the right
        assert_eq!(rows[1].left.as_ref().unwrap().1, "one");
        assert_eq!(rows[2].left.as_ref().unwrap().1, "two");
        assert_eq!(rows[2].right.as_ref().unwrap().1, "TWO");
        assert!(rows[3].left.is_none());
        assert_eq!(rows[3].right.as_ref().unwrap().1, "2.5");
    }

    #[test]
    fn word_diff_ranges() {
        let (o, n) = word_diff("let a = 1;", "let a = 22;");
        assert_eq!(&"let a = 1;"[o[0].0..o[0].1], "1");
        assert_eq!(&"let a = 22;"[n[0].0..n[0].1], "22");
    }

    #[test]
    fn partial_forward_patch() {
        let f = parse_unified(SAMPLE);
        // lines: 0 ' one', 1 '-two', 2 '+TWO', 3 '+2.5', 4 ' three'
        let p = build_patch(&f[0], 0, Some(&[3]), PatchMode::Forward).unwrap();
        assert!(p.contains(" two\n"), "unselected del becomes context:\n{p}");
        assert!(!p.contains("+TWO"));
        assert!(p.contains("+2.5"));
        assert!(p.contains("@@ -1,3 +1,4 @@"));
    }

    #[test]
    fn partial_reverse_patch() {
        let f = parse_unified(SAMPLE);
        let p = build_patch(&f[0], 0, Some(&[2]), PatchMode::Reverse).unwrap();
        assert!(!p.contains("-two"), "unselected del dropped when reversing:\n{p}");
        assert!(p.contains("+TWO"));
        assert!(p.contains(" 2.5\n"), "unselected add is context when reversing:\n{p}");
    }

    #[test]
    fn empty_selection_is_none() {
        let f = parse_unified(SAMPLE);
        assert!(build_patch(&f[0], 0, Some(&[0]), PatchMode::Forward).is_none());
    }
}
