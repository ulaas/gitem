//! Three-way merge engine (diff3) driving the visual merge tool.
use similar::{Algorithm, DiffOp, capture_diff_slices};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Unresolved,
    Ours,
    Theirs,
    OursThenTheirs,
    TheirsThenOurs,
    Base,
    Custom(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Region {
    /// Identical in all three versions.
    Stable(Vec<String>),
    /// Only one side changed; taken automatically. `from_theirs` tells which.
    Auto { lines: Vec<String>, base: Vec<String>, from_theirs: bool },
    /// Both sides made the same change.
    Same { lines: Vec<String>, base: Vec<String> },
    Conflict { base: Vec<String>, ours: Vec<String>, theirs: Vec<String>, resolution: Resolution },
}

/// Whether each version ends in a newline. `None` for an empty version, which has no say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FinalNewlines {
    pub base: Option<bool>,
    pub ours: Option<bool>,
    pub theirs: Option<bool>,
}

impl FinalNewlines {
    /// A three-way merge of this one fact: whichever side changed it wins.
    fn merged(&self) -> bool {
        match (self.ours, self.theirs) {
            (Some(o), Some(t)) if o == t => o,
            (Some(o), Some(t)) => {
                if self.base == Some(o) {
                    t
                } else {
                    o
                }
            }
            (Some(x), None) | (None, Some(x)) => x,
            (None, None) => true,
        }
    }
}

/// Lines keep a `\r` of their own, so a mixed file stays as it was. Anything that shows
/// or compares a line goes through [`plain`].
#[derive(Debug, Clone, Default)]
pub struct MergeDoc {
    pub regions: Vec<Region>,
    /// Line ending for text with none of its own: conflict markers and typed lines.
    pub eol_crlf: bool,
    pub final_newline: bool,
    /// Each version's own final newline, for when a conflict at the end is resolved.
    pub final_newlines: FinalNewlines,
    /// Our side began with a UTF-8 byte-order mark, which saving must keep.
    pub bom: bool,
}

/// A line without its carriage return.
pub fn plain(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

/// Lines as text for showing or editing, with one kind of line end.
pub fn plain_text(lines: &[String]) -> String {
    lines.iter().map(|l| plain(l)).collect::<Vec<_>>().join("\n")
}

/// Split into lines that keep their own `\r`. See [`crate::util::split_lines`].
fn split_keeping_cr(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s.split('\n').map(str::to_string).collect();
    if v.last().is_some_and(String::is_empty) {
        v.pop();
    }
    v
}

fn same_lines(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| plain(x) == plain(y))
}

/// The line to keep where only the line end can differ: whichever side changed it.
fn merged_line(base: &str, ours: &str, theirs: &str) -> String {
    if ours == base { theirs.to_string() } else { ours.to_string() }
}

/// base-line-index -> side-line-index for lines the diff kept equal.
fn matches(base: &[String], side: &[String]) -> Vec<Option<usize>> {
    let mut m = vec![None; base.len()];
    let strip = |v: &'_ [String]| -> Vec<String> { v.iter().map(|l| plain(l).to_string()).collect() };
    let (base, side) = (strip(base), strip(side));
    for op in capture_diff_slices(Algorithm::Patience, &base, &side) {
        if let DiffOp::Equal { old_index, new_index, len } = op {
            for k in 0..len {
                m[old_index + k] = Some(new_index + k);
            }
        }
    }
    m
}

/// Classic diff3: base lines matched on *both* sides are sync points; each gap
/// between sync points is classified by which sides diverge from base.
pub fn merge3(base: &[String], ours: &[String], theirs: &[String]) -> Vec<Region> {
    let mo = matches(base, ours);
    let mt = matches(base, theirs);
    let mut regions: Vec<Region> = Vec::new();
    let (mut bi, mut oi, mut ti) = (0usize, 0usize, 0usize);

    let emit_gap = |regions: &mut Vec<Region>, b: &[String], o: &[String], t: &[String]| {
        if b.is_empty() && o.is_empty() && t.is_empty() {
            return;
        }
        let o_same = same_lines(o, b);
        let t_same = same_lines(t, b);
        let region = if o_same && t_same {
            Region::Stable(b.iter().zip(o).zip(t).map(|((b, o), t)| merged_line(b, o, t)).collect())
        } else if o_same {
            Region::Auto { lines: t.to_vec(), base: b.to_vec(), from_theirs: true }
        } else if t_same {
            Region::Auto { lines: o.to_vec(), base: b.to_vec(), from_theirs: false }
        } else if same_lines(o, t) {
            Region::Same { lines: o.to_vec(), base: b.to_vec() }
        } else {
            Region::Conflict { base: b.to_vec(), ours: o.to_vec(), theirs: t.to_vec(), resolution: Resolution::Unresolved }
        };
        regions.push(region);
    };

    for b in 0..=base.len() {
        let sync = if b == base.len() {
            Some((ours.len(), theirs.len()))
        } else {
            match (mo[b], mt[b]) {
                (Some(o), Some(t)) if o >= oi && t >= ti => Some((o, t)),
                _ => None,
            }
        };
        let Some((o, t)) = sync else { continue };
        emit_gap(&mut regions, &base[bi..b], &ours[oi..o], &theirs[ti..t]);
        if b < base.len() {
            let line = merged_line(&base[b], &ours[o], &theirs[t]);
            match regions.last_mut() {
                Some(Region::Stable(v)) => v.push(line),
                _ => regions.push(Region::Stable(vec![line])),
            }
        }
        bi = b + 1;
        oi = o + 1;
        ti = t + 1;
    }
    regions
}

impl MergeDoc {
    pub fn new(base: &str, ours: &str, theirs: &str) -> Self {
        let strip = |s: &'_ str| -> String { s.strip_prefix('\u{feff}').unwrap_or(s).to_string() };
        let bom = ours.starts_with('\u{feff}');
        let (base, ours, theirs) = (strip(base), strip(ours), strip(theirs));
        let (b, o, t) = (split_keeping_cr(&base), split_keeping_cr(&ours), split_keeping_cr(&theirs));
        let bit = |s: &str| (!s.is_empty()).then(|| s.ends_with('\n'));
        let final_newlines = FinalNewlines { base: bit(&base), ours: bit(&ours), theirs: bit(&theirs) };
        Self { regions: merge3(&b, &o, &t), eol_crlf: mostly_crlf(&ours), final_newline: final_newlines.merged(), final_newlines, bom }
    }

    /// Like [`Self::new`], with git as a second opinion. The two can line up repeated
    /// lines differently, and when only git finds a conflict, saying "no conflicts"
    /// would offer to save a merge git refused. The disputed part becomes one conflict.
    pub fn checked_with_git(git: &crate::GitExe, base: &str, ours: &str, theirs: &str) -> Self {
        let mut doc = Self::new(base, ours, theirs);
        if doc.conflict_count() == 0 && git_finds_conflict(git, base, ours, theirs) {
            doc.regions = disputed_regions(base, ours, theirs);
        }
        doc
    }

    /// The doc for a file as it is on disk: its conflict markers become conflicts and
    /// everything else, hand edits included, stays as written. `None` if the markers
    /// aren't well formed. `fresh` supplies base text, which default-style markers leave out.
    pub fn from_marked_text(text: &str, fresh: &MergeDoc) -> Option<Self> {
        let bom = text.starts_with('\u{feff}');
        let body = text.strip_prefix('\u{feff}').unwrap_or(text);
        let regions = parse_markers(&split_keeping_cr(body), fresh)?;
        Some(Self { regions, eol_crlf: mostly_crlf(body), final_newline: body.is_empty() || body.ends_with('\n'), final_newlines: fresh.final_newlines, bom })
    }

    /// Whether `text` is what this merge produces, conflicts aside. It compares each
    /// side's view of the file, so a resolved conflict or a hand edit shows up, while
    /// conflict shape and line endings, which only say how git wrote it, don't.
    pub fn matches_marked_text(&self, text: &str) -> bool {
        let body = text.strip_prefix('\u{feff}').unwrap_or(text);
        let Some(regions) = parse_markers(&split_keeping_cr(body), &MergeDoc::default()) else { return false };
        let view = |regions: &[Region], theirs_side: bool| -> Vec<String> {
            let mut out = Vec::new();
            for r in regions {
                let part: &[String] = match r {
                    Region::Stable(l) | Region::Auto { lines: l, .. } | Region::Same { lines: l, .. } => l,
                    Region::Conflict { ours, theirs, .. } => {
                        if theirs_side {
                            theirs
                        } else {
                            ours
                        }
                    }
                };
                out.extend(part.iter().map(|l| plain(l).to_string()));
            }
            out
        };
        view(&self.regions, false) == view(&regions, false) && view(&self.regions, true) == view(&regions, true)
    }

    pub fn conflict_count(&self) -> usize {
        self.regions.iter().filter(|r| matches!(r, Region::Conflict { .. })).count()
    }

    pub fn unresolved_count(&self) -> usize {
        self.regions
            .iter()
            .filter(|r| matches!(r, Region::Conflict { resolution: Resolution::Unresolved, .. }))
            .count()
    }

    /// Apply `f` to all conflicts still unresolved.
    pub fn resolve_all(&mut self, res: Resolution) {
        for r in &mut self.regions {
            if let Region::Conflict { resolution, .. } = r
                && *resolution == Resolution::Unresolved {
                    *resolution = res.clone();
                }
        }
    }

    /// Resolve conflicts that are only whitespace apart, preferring ours.
    pub fn resolve_whitespace_only(&mut self) -> usize {
        let squeeze = |v: &[String]| -> String { v.iter().flat_map(|l| l.chars().filter(|c| !c.is_whitespace())).collect() };
        let mut n = 0;
        for r in &mut self.regions {
            if let Region::Conflict { ours, theirs, resolution, .. } = r
                && *resolution == Resolution::Unresolved && squeeze(ours) == squeeze(theirs) {
                    *resolution = Resolution::Ours;
                    n += 1;
                }
        }
        n
    }

    /// The merged text. Unresolved conflicts are written with standard markers
    /// so an incomplete save still leaves a valid conflicted file.
    pub fn render(&self, ours_label: &str, theirs_label: &str) -> String {
        let cr = if self.eol_crlf { "\r" } else { "" };
        let typed = |line: &str| format!("{}{cr}", plain(line));
        let mut out: Vec<String> = Vec::new();
        for r in &self.regions {
            match r {
                Region::Stable(l) => out.extend_from_slice(l),
                Region::Auto { lines, .. } | Region::Same { lines, .. } => out.extend_from_slice(lines),
                Region::Conflict { base, ours, theirs, resolution } => match resolution {
                    Resolution::Unresolved => {
                        out.push(typed(&format!("<<<<<<< {ours_label}")));
                        out.extend_from_slice(ours);
                        out.push(typed("||||||| base"));
                        out.extend_from_slice(base);
                        out.push(typed("======="));
                        out.extend_from_slice(theirs);
                        out.push(typed(&format!(">>>>>>> {theirs_label}")));
                    }
                    Resolution::Ours => out.extend_from_slice(ours),
                    Resolution::Theirs => out.extend_from_slice(theirs),
                    Resolution::OursThenTheirs => {
                        out.extend_from_slice(ours);
                        out.extend_from_slice(theirs);
                    }
                    Resolution::TheirsThenOurs => {
                        out.extend_from_slice(theirs);
                        out.extend_from_slice(ours);
                    }
                    Resolution::Base => out.extend_from_slice(base),
                    Resolution::Custom(c) => out.extend(c.iter().map(|l| typed(l))),
                },
            }
        }
        let mut s = out.join("\n");
        if self.final_newline_of_result() && !out.is_empty() {
            s.push('\n');
        }
        if self.bom {
            s.insert(0, '\u{feff}');
        }
        s
    }

    /// A conflict at the very end decides by which side's text ends the file.
    fn final_newline_of_result(&self) -> bool {
        let n = &self.final_newlines;
        let side = match self.regions.last() {
            Some(Region::Conflict { resolution, .. }) => match resolution {
                Resolution::Unresolved => return true,
                Resolution::Ours | Resolution::TheirsThenOurs => n.ours,
                Resolution::Theirs | Resolution::OursThenTheirs => n.theirs,
                Resolution::Base => n.base,
                Resolution::Custom(_) => None,
            },
            _ => None,
        };
        side.unwrap_or(self.final_newline)
    }
}

/// Whether a file on disk uses CRLF while none of git's versions do, as `core.autocrlf`
/// leaves things. Merged text should then be converted to match the file.
pub fn worktree_uses_crlf(disk: &str, versions: [&str; 3]) -> bool {
    mostly_crlf(disk) && !versions.iter().any(|v| v.contains('\r'))
}

pub fn to_crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

fn mostly_crlf(text: &str) -> bool {
    let lf = text.bytes().filter(|b| *b == b'\n').count();
    let crlf = text.matches("\r\n").count();
    crlf > 0 && crlf * 2 >= lf
}

/// `git merge-file`'s verdict on three versions. If it can't be asked, or fails, the
/// answer is yes: that means a decision for you rather than an unchecked "no conflicts".
fn git_finds_conflict(git: &crate::GitExe, base: &str, ours: &str, theirs: &str) -> bool {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("gitem-merge-{}-{n}", std::process::id()));
    let verdict = (|| -> std::io::Result<bool> {
        std::fs::create_dir_all(&dir)?;
        for (name, text) in [("base", base), ("ours", ours), ("theirs", theirs)] {
            std::fs::write(dir.join(name), text)?;
        }
        let code = git.run_raw(Some(&dir), &["merge-file", "-p", "ours", "base", "theirs"]).map_err(std::io::Error::other)?.code;
        Ok(code != 0)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    verdict.unwrap_or(true)
}

/// The three versions as one conflict, between the lines all three share at each end.
fn disputed_regions(base: &str, ours: &str, theirs: &str) -> Vec<Region> {
    let strip = |s: &'_ str| split_keeping_cr(s.strip_prefix('\u{feff}').unwrap_or(s));
    let (b, o, t) = (strip(base), strip(ours), strip(theirs));
    let same = |x: &String, y: &String, z: &String| plain(x) == plain(y) && plain(y) == plain(z);
    let min = b.len().min(o.len()).min(t.len());
    let head = (0..min).take_while(|&i| same(&b[i], &o[i], &t[i])).count();
    let tail = (0..min - head).take_while(|&i| same(&b[b.len() - 1 - i], &o[o.len() - 1 - i], &t[t.len() - 1 - i])).count();
    let mut regions = Vec::new();
    if head > 0 {
        regions.push(Region::Stable(o[..head].to_vec()));
    }
    regions.push(Region::Conflict { base: b[head..b.len() - tail].to_vec(), ours: o[head..o.len() - tail].to_vec(), theirs: t[head..t.len() - tail].to_vec(), resolution: Resolution::Unresolved });
    if tail > 0 {
        regions.push(Region::Stable(o[o.len() - tail..].to_vec()));
    }
    regions
}

fn marker(line: &str, sign: char) -> bool {
    let l = plain(line);
    let n = l.chars().take_while(|c| *c == sign).count();
    n == 7 && (l.len() == 7 || l.as_bytes()[7] == b' ')
}

/// Conflict blocks and the text between them. `None` if a block isn't well formed.
fn parse_markers(lines: &[String], fresh: &MergeDoc) -> Option<Vec<Region>> {
    let mut regions = Vec::new();
    let mut stable: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !marker(&lines[i], '<') {
            if marker(&lines[i], '>') {
                return None;
            }
            stable.push(lines[i].clone());
            i += 1;
            continue;
        }
        if !stable.is_empty() {
            regions.push(Region::Stable(std::mem::take(&mut stable)));
        }
        i += 1;
        let (mut ours, mut base, mut theirs) = (Vec::new(), None::<Vec<String>>, Vec::new());
        while i < lines.len() && !marker(&lines[i], '|') && plain(&lines[i]) != "=======" {
            if marker(&lines[i], '<') || marker(&lines[i], '>') {
                return None;
            }
            ours.push(lines[i].clone());
            i += 1;
        }
        if i < lines.len() && marker(&lines[i], '|') {
            i += 1;
            let mut b = Vec::new();
            while i < lines.len() && plain(&lines[i]) != "=======" {
                b.push(lines[i].clone());
                i += 1;
            }
            base = Some(b);
        }
        if i >= lines.len() {
            return None;
        }
        i += 1;
        while i < lines.len() && !marker(&lines[i], '>') {
            if marker(&lines[i], '<') || plain(&lines[i]) == "=======" {
                return None;
            }
            theirs.push(lines[i].clone());
            i += 1;
        }
        if i >= lines.len() {
            return None;
        }
        i += 1;
        let base = base.unwrap_or_else(|| fresh_base(fresh, &ours, &theirs));
        regions.push(Region::Conflict { base, ours, theirs, resolution: Resolution::Unresolved });
    }
    if !stable.is_empty() {
        regions.push(Region::Stable(stable));
    }
    Some(regions)
}

/// The base text a fresh merge has for this conflict, if it found the same one.
fn fresh_base(fresh: &MergeDoc, ours: &[String], theirs: &[String]) -> Vec<String> {
    fresh.regions.iter().find_map(|r| match r {
        Region::Conflict { base, ours: o, theirs: t, .. } if same_lines(o, ours) && same_lines(t, theirs) => Some(base.clone()),
        _ => None,
    }).unwrap_or_default()
}

/// True when text still contains conflict markers at line starts.
pub fn has_conflict_markers(text: &str) -> bool {
    let mut open = false;
    for l in text.lines() {
        // With or without a label: a model's answer may well leave it off.
        if l == "<<<<<<<" || l.starts_with("<<<<<<< ") {
            open = true;
        } else if open && (l == ">>>>>>>" || l.starts_with(">>>>>>> ")) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(b: &str, o: &str, t: &str) -> MergeDoc {
        MergeDoc::new(b, o, t)
    }

    #[test]
    fn identical() {
        let d = doc("a\nb\n", "a\nb\n", "a\nb\n");
        assert_eq!(d.conflict_count(), 0);
        assert_eq!(d.render("o", "t"), "a\nb\n");
    }

    #[test]
    fn non_overlapping_changes_merge_cleanly() {
        let d = doc("1\n2\n3\n4\n5\n", "1\nTWO\n3\n4\n5\n", "1\n2\n3\n4\nFIVE\n");
        assert_eq!(d.conflict_count(), 0);
        assert_eq!(d.render("o", "t"), "1\nTWO\n3\n4\nFIVE\n");
    }

    #[test]
    fn same_change_both_sides() {
        let d = doc("a\nb\nc\n", "a\nX\nc\n", "a\nX\nc\n");
        assert_eq!(d.conflict_count(), 0);
        assert_eq!(d.render("o", "t"), "a\nX\nc\n");
    }

    #[test]
    fn conflict_and_resolutions() {
        let mut d = doc("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
        assert_eq!(d.conflict_count(), 1);
        assert_eq!(d.unresolved_count(), 1);
        let r = d.render("mine", "other");
        assert!(r.contains("<<<<<<< mine\nOURS\n||||||| base\nb\n=======\nTHEIRS\n>>>>>>> other\n"), "{r}");
        assert!(has_conflict_markers(&r));

        d.resolve_all(Resolution::Theirs);
        assert_eq!(d.unresolved_count(), 0);
        assert_eq!(d.render("o", "t"), "a\nTHEIRS\nc\n");
        for reg in &mut d.regions {
            if let Region::Conflict { resolution, .. } = reg {
                *resolution = Resolution::OursThenTheirs;
            }
        }
        assert_eq!(d.render("o", "t"), "a\nOURS\nTHEIRS\nc\n");
        for reg in &mut d.regions {
            if let Region::Conflict { resolution, .. } = reg {
                *resolution = Resolution::Custom(vec!["z".into()]);
            }
        }
        assert_eq!(d.render("o", "t"), "a\nz\nc\n");
    }

    #[test]
    fn insert_at_ends_and_empty_base() {
        let d = doc("m\n", "o\nm\n", "m\nt\n");
        assert_eq!(d.conflict_count(), 0);
        assert_eq!(d.render("o", "t"), "o\nm\nt\n");
        let d = doc("", "x\n", "y\n");
        assert_eq!(d.conflict_count(), 1);
        let d = doc("", "x\n", "x\n");
        assert_eq!(d.render("o", "t"), "x\n");
    }

    #[test]
    fn delete_vs_edit_conflicts() {
        let d = doc("a\nb\nc\n", "a\nc\n", "a\nB!\nc\n");
        assert_eq!(d.conflict_count(), 1);
    }

    #[test]
    fn crlf_preserved() {
        let d = doc("a\r\nb\r\n", "a\r\nX\r\n", "a\r\nb\r\n");
        assert_eq!(d.render("o", "t"), "a\r\nX\r\n");
    }

    #[test]
    fn byte_order_mark_survives_a_merge() {
        let d = doc("\u{feff}a\nb\n", "\u{feff}a\nX\n", "\u{feff}a\nb\n");
        assert_eq!(d.conflict_count(), 0);
        assert_eq!(d.render("o", "t"), "\u{feff}a\nX\n");
    }

    #[test]
    fn whitespace_conflict_autoresolve() {
        let mut d = doc("a\nb\nc\n", "a\n  b1\nc\n", "a\nb1\nc\n");
        assert_eq!(d.conflict_count(), 1);
        assert_eq!(d.resolve_whitespace_only(), 1);
        assert_eq!(d.unresolved_count(), 0);
    }

    #[test]
    fn a_mixed_file_keeps_each_lines_own_ending() {
        let d = doc("a\r\nb\nc\nd\n", "a\r\nb\nc\nD\n", "a\r\nb\nc\nd\n");
        assert_eq!(d.render("o", "t"), "a\r\nb\nc\nD\n");
    }

    #[test]
    fn a_line_end_changed_by_one_side_survives_an_edit_by_the_other() {
        let d = doc("a\nb\nc\n", "a\r\nb\r\nc\n", "a\nb\nC\n");
        assert_eq!(d.conflict_count(), 0);
        assert_eq!(d.render("o", "t"), "a\r\nb\r\nC\n");
    }

    #[test]
    fn typed_text_and_markers_take_the_files_usual_ending() {
        let mut d = doc("a\r\nb\r\n", "a\r\nX\r\n", "a\r\nY\r\n");
        assert!(d.render("o", "t").contains("<<<<<<< o\r\nX\r\n"));
        d.resolve_all(Resolution::Custom(vec!["typed".to_string()]));
        assert_eq!(d.render("o", "t"), "a\r\ntyped\r\n");
    }

    #[test]
    fn final_newline_follows_the_side_that_changed_it() {
        assert_eq!(doc("a\nb\n", "a\nb\n", "a\nb").render("o", "t"), "a\nb");
        assert_eq!(doc("a\nb", "a\nb\n", "a\nb").render("o", "t"), "a\nb\n");
        assert_eq!(doc("a\nb\n", "a\nb\n", "a\nb\n").render("o", "t"), "a\nb\n");
    }

    #[test]
    fn final_newline_follows_the_side_taken_at_the_end() {
        let mut d = doc("a\n", "b", "c\n");
        d.resolve_all(Resolution::Theirs);
        assert_eq!(d.render("o", "t"), "c\n");
        let mut d = doc("a\n", "b\n", "c");
        d.resolve_all(Resolution::Theirs);
        assert_eq!(d.render("o", "t"), "c");
        let mut d = doc("a\n", "b\n", "c");
        d.resolve_all(Resolution::Ours);
        assert_eq!(d.render("o", "t"), "b\n");
    }

    #[test]
    fn a_file_with_a_hand_edit_is_read_as_it_stands() {
        let fresh = doc("1\n2\n3\n4\n5\n", "1\nA\n3\nB\n5\n", "1\nC\n3\nD\n5\n");
        let on_disk = "1\nHAND\n3\n<<<<<<< HEAD\nB\n=======\nD\n>>>>>>> side\n5\n";
        assert!(!fresh.matches_marked_text(on_disk));
        let mut d = MergeDoc::from_marked_text(on_disk, &fresh).unwrap();
        assert_eq!(d.conflict_count(), 1);
        d.resolve_all(Resolution::Theirs);
        assert_eq!(d.render("o", "t"), "1\nHAND\n3\nD\n5\n");
    }

    #[test]
    fn a_base_section_or_fresh_merge_supplies_the_conflicts_base() {
        let fresh = doc("x\n", "o\n", "t\n");
        for text in ["<<<<<<< a\no\n=======\nt\n>>>>>>> b\n", "<<<<<<< a\no\n||||||| b\nx\n=======\nt\n>>>>>>> c\n"] {
            let mut d = MergeDoc::from_marked_text(text, &fresh).unwrap();
            d.resolve_all(Resolution::Base);
            assert_eq!(d.render("o", "t"), "x\n", "{text}");
        }
    }

    #[test]
    fn an_untouched_file_matches_its_merge_whatever_the_marker_style_or_line_end() {
        let fresh = doc("1\n2\n3\n", "1\nA\n3\n", "1\nB\n3\n");
        assert!(fresh.matches_marked_text(&fresh.render("x", "y")));
        assert!(fresh.matches_marked_text("1\n<<<<<<< HEAD\nA\n=======\nB\n>>>>>>> side\n3\n"));
        assert!(fresh.matches_marked_text("1\r\n<<<<<<< HEAD\r\nA\r\n=======\r\nB\r\n>>>>>>> side\r\n3\r\n"));
    }

    #[test]
    fn a_partly_resolved_file_does_not_match_its_merge() {
        let fresh = doc("1\n2\n3\n", "1\nA\n3\n", "1\nB\n3\n");
        assert!(!fresh.matches_marked_text("1\nA\n3\n"));
    }

    #[test]
    fn broken_markers_are_not_guessed_at() {
        let fresh = MergeDoc::default();
        for text in ["<<<<<<< a\nx\n", "<<<<<<< a\nx\n=======\ny\n", ">>>>>>> a\n", "<<<<<<< a\n<<<<<<< b\n=======\n>>>>>>> c\n"] {
            assert!(MergeDoc::from_marked_text(text, &fresh).is_none(), "{text}");
        }
    }

    #[test]
    fn multiple_conflicts_ordering() {
        let b = "1\n2\n3\n4\n5\n6\n7\n";
        let o = "1\nA\n3\n4\n5\nC\n7\n";
        let t = "1\nB\n3\n4\n5\nD\n7\n";
        let mut d = doc(b, o, t);
        assert_eq!(d.conflict_count(), 2);
        d.resolve_all(Resolution::Ours);
        assert_eq!(d.render("o", "t"), o);
    }
}
