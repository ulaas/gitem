//! Random edits, carets and bytes: the model must never lose text, panic, or let carets
//! overlap or leave the text.
use gitem_text::history::Kind;
use gitem_text::selection::Move;
use gitem_text::{Buffer, Edit, History, Selection, SelectionSet, Transaction};
use proptest::prelude::*;

fn text() -> impl Strategy<Value = String> {
    proptest::collection::vec(prop_oneof!["[a-z ]{0,6}", Just("\n".to_string()), Just("\r\n".to_string()), Just("é".to_string()), Just("🦀".to_string()), Just("e\u{301}".to_string())], 0..20).prop_map(|v| v.concat())
}

/// An edit somewhere in a text of `len` chars, given as fractions so it always fits.
fn edit_in(len: usize, a: f64, b: f64, insert: String) -> Edit {
    let x = (a * len as f64) as usize;
    let y = (b * len as f64) as usize;
    Edit::replace(x.min(y), x.max(y), insert)
}

proptest! {
    #[test]
    fn utf8_and_utf16_files_come_back_byte_for_byte(t in text(), bom in any::<bool>(), wide in any::<bool>()) {
        let enc = if wide { gitem_text::Encoding::Utf16Le } else { gitem_text::Encoding::Utf8 };
        let bytes = gitem_text::encoding::encode(&t, enc, bom || wide);
        let b = Buffer::from_bytes(&bytes).unwrap();
        prop_assert_eq!(b.to_bytes(), bytes);
    }

    #[test]
    fn undoing_every_step_gives_back_the_original(start in text(), steps in proptest::collection::vec((0.0..1.0f64, 0.0..1.0f64, text(), any::<bool>(), 0u64..2000), 1..25)) {
        let mut b = Buffer::from_text(&start);
        let mut h = History::default();
        let mut seen = vec![b.text()];
        let mut now = 0;
        for (x, y, ins, typing, gap) in steps {
            let e = edit_in(b.len_chars(), x, y, ins);
            now += gap;
            let kind = if typing { Kind::Typing } else { Kind::Other };
            h.apply(&mut b, Transaction::new(vec![e]), SelectionSet::default(), SelectionSet::default(), kind, now);
            seen.push(b.text());
        }
        let end = b.text();
        while h.undo(&mut b).is_some() {
            prop_assert!(seen.contains(&b.text()), "undo reached a text that never existed");
        }
        prop_assert_eq!(b.text(), start);
        prop_assert!(!h.is_dirty());
        while h.redo(&mut b).is_some() {}
        prop_assert_eq!(b.text(), end);
    }

    #[test]
    fn carets_stay_sorted_apart_and_inside_the_text(t in text(), carets in proptest::collection::vec((0.0..1.0f64, 0.0..1.0f64), 1..8), moves in proptest::collection::vec((0usize..10, any::<bool>()), 0..30)) {
        let b = Buffer::from_text(&t);
        let len = b.len_chars();
        let sels = carets.iter().map(|&(a, h)| Selection::new(b.snap((a * len as f64) as usize), b.snap((h * len as f64) as usize))).collect();
        let mut set = SelectionSet::from_vec(sels, 0);
        let all = [Move::Left, Move::Right, Move::Up, Move::Down, Move::WordLeft, Move::WordRight, Move::Home, Move::End, Move::DocStart, Move::Lines(3)];
        for (m, extend) in moves {
            set.motion(&b, all[m], extend);
            let s = set.as_slice();
            prop_assert!(!s.is_empty());
            for w in s.windows(2) {
                prop_assert!(w[0].end() <= w[1].start() && w[0] != w[1], "{:?}", s);
            }
            for x in s {
                prop_assert!(x.end() <= len);
                // Never inside a \r\n or a grapheme cluster.
                let (line, col) = b.point(x.head);
                prop_assert!(col <= b.line(line).chars().count());
            }
            prop_assert!(s.contains(&set.primary()));
        }
    }
}
