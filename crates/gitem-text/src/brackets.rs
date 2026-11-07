//! Matching brackets, and the pairs typed together.
use crate::Buffer;

const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// How far a match is looked for. A missing bracket in a big file shouldn't cost a frame.
const BUDGET: usize = 50_000;

/// The bracket at or just before `pos` and its partner, if it has one within reach. Brackets
/// inside strings and comments aren't told apart; the highlighter's job, not this one's.
pub fn matching(b: &Buffer, pos: usize) -> Option<(usize, usize)> {
    let at = |i: usize| (i < b.len_chars()).then(|| b.rope.char(i));
    for here in [Some(pos), pos.checked_sub(1)].into_iter().flatten() {
        let Some(c) = at(here) else { continue };
        if let Some(&(open, close)) = PAIRS.iter().find(|(o, _)| *o == c) {
            return scan(b, here, open, close, true).map(|o| (here, o));
        }
        if let Some(&(open, close)) = PAIRS.iter().find(|(_, cl)| *cl == c) {
            return scan(b, here, open, close, false).map(|o| (here, o));
        }
    }
    None
}

fn scan(b: &Buffer, from: usize, open: char, close: char, forward: bool) -> Option<usize> {
    let mut depth = 0usize;
    let len = b.len_chars();
    let mut i = from;
    for _ in 0..BUDGET {
        let c = b.rope.char(i);
        if c == open {
            if forward { depth += 1 } else { depth -= 1 }
        } else if c == close {
            if forward { depth -= 1 } else { depth += 1 }
        }
        if depth == 0 {
            return Some(i);
        }
        if forward {
            i += 1;
            if i >= len {
                return None;
            }
        } else {
            i = i.checked_sub(1)?;
        }
    }
    None
}

/// The closer to type along with `c`, if it opens a pair. Quotes pair only where a word
/// doesn't touch them, so `don't` stays as typed.
pub fn auto_close(c: char, before: Option<char>, after: Option<char>) -> Option<char> {
    let word = |x: Option<char>| x.is_some_and(|x| x.is_alphanumeric() || x == '_');
    match c {
        '(' | '[' | '{' if !word(after) => PAIRS.iter().find(|(o, _)| *o == c).map(|p| p.1),
        '"' | '\'' | '`' if !word(before) && !word(after) => Some(c),
        _ => None,
    }
}

/// Typing a closer just before the same closer steps over it rather than adding another.
pub fn skips_over(c: char, after: Option<char>) -> bool {
    matches!(c, ')' | ']' | '}' | '"' | '\'' | '`') && after == Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_partner_both_ways() {
        let b = Buffer::from_text("f(a[1], {b})");
        assert_eq!(matching(&b, 1), Some((1, 11)));
        assert_eq!(matching(&b, 12), Some((11, 1)), "just after a closer counts");
        assert_eq!(matching(&b, 3), Some((3, 5)));
        assert_eq!(matching(&b, 0), None);
        assert_eq!(matching(&Buffer::from_text("(("), 0), None);
    }

    #[test]
    fn pairs_typed_together() {
        assert_eq!(auto_close('(', None, None), Some(')'));
        assert_eq!(auto_close('(', None, Some('x')), None);
        assert_eq!(auto_close('\'', Some('n'), Some('t')), None);
        assert_eq!(auto_close('"', Some(' '), None), Some('"'));
        assert!(skips_over(')', Some(')')));
    }
}
