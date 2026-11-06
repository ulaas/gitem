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

#[derive(Debug, Clone, Default)]
pub struct MergeDoc {
    pub regions: Vec<Region>,
    pub eol_crlf: bool,
    pub final_newline: bool,
    /// Our side began with a UTF-8 byte-order mark, which saving must keep.
    pub bom: bool,
}

/// base-line-index -> side-line-index for lines the diff kept equal.
fn matches(base: &[String], side: &[String]) -> Vec<Option<usize>> {
    let mut m = vec![None; base.len()];
    for op in capture_diff_slices(Algorithm::Patience, base, side) {
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
        let o_same = o == b;
        let t_same = t == b;
        let region = if o_same && t_same {
            Region::Stable(b.to_vec())
        } else if o_same {
            Region::Auto { lines: t.to_vec(), base: b.to_vec(), from_theirs: true }
        } else if t_same {
            Region::Auto { lines: o.to_vec(), base: b.to_vec(), from_theirs: false }
        } else if o == t {
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
            match regions.last_mut() {
                Some(Region::Stable(v)) => v.push(base[b].clone()),
                _ => regions.push(Region::Stable(vec![base[b].clone()])),
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
        let b = crate::util::split_lines(&base);
        let o = crate::util::split_lines(&ours);
        let t = crate::util::split_lines(&theirs);
        Self {
            regions: merge3(&b, &o, &t),
            eol_crlf: ours.contains("\r\n"),
            final_newline: ours.is_empty() || ours.ends_with('\n'),
            bom,
        }
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
        let mut out: Vec<String> = Vec::new();
        for r in &self.regions {
            match r {
                Region::Stable(l) => out.extend_from_slice(l),
                Region::Auto { lines, .. } | Region::Same { lines, .. } => out.extend_from_slice(lines),
                Region::Conflict { base, ours, theirs, resolution } => match resolution {
                    Resolution::Unresolved => {
                        out.push(format!("<<<<<<< {ours_label}"));
                        out.extend_from_slice(ours);
                        out.push("||||||| base".to_string());
                        out.extend_from_slice(base);
                        out.push("=======".to_string());
                        out.extend_from_slice(theirs);
                        out.push(format!(">>>>>>> {theirs_label}"));
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
                    Resolution::Custom(c) => out.extend_from_slice(c),
                },
            }
        }
        let eol = if self.eol_crlf { "\r\n" } else { "\n" };
        let mut s = out.join(eol);
        if self.final_newline && !out.is_empty() {
            s.push_str(eol);
        }
        if self.bom {
            s.insert(0, '\u{feff}');
        }
        s
    }
}

/// True when text still contains conflict markers at line starts.
pub fn has_conflict_markers(text: &str) -> bool {
    let mut open = false;
    for l in text.lines() {
        if l.starts_with("<<<<<<< ") {
            open = true;
        } else if open && l.starts_with(">>>>>>> ") {
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
