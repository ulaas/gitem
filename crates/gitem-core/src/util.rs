//! Small helpers shared across the crate.
use chrono::{Local, TimeZone};

/// "3 hours ago" style formatting.
pub fn relative_time(ts: i64, now: i64) -> String {
    let d = now - ts;
    let (n, unit) = match d {
        d if d < 0 => return "in the future".into(),
        d if d < 60 => return "just now".into(),
        d if d < 3600 => (d / 60, "minute"),
        d if d < 86400 => (d / 3600, "hour"),
        d if d < 86400 * 7 => (d / 86400, "day"),
        d if d < 86400 * 30 => (d / (86400 * 7), "week"),
        d if d < 86400 * 365 => (d / (86400 * 30), "month"),
        d => (d / (86400 * 365), "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn format_date(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0).single() {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

pub fn format_day(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0).single() {
        Some(t) => t.format("%Y-%m-%d").to_string(),
        None => String::new(),
    }
}

/// Lossy UTF-8 that also strips a BOM.
pub fn text(bytes: &[u8]) -> String {
    let b = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    String::from_utf8_lossy(b).into_owned()
}

/// Heuristic used by git itself: a NUL in the first 8000 bytes means binary.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// Split into lines, keeping `\r` out; a trailing newline does not create a line.
pub fn split_lines(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// At most `n` bytes of `s`, cut on a character boundary (names can be non-ASCII).
pub fn prefix(s: &str, n: usize) -> &str {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Subsequence fuzzy scoring; `None` when `needle` does not match.
pub fn fuzzy_score(needle: &str, hay: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay_l: Vec<char> = hay.to_lowercase().chars().collect();
    let mut score = 0;
    let mut pos = 0usize;
    let mut last: Option<usize> = None;
    for n in needle.to_lowercase().chars() {
        if n.is_whitespace() {
            continue;
        }
        let found = hay_l[pos..].iter().position(|&c| c == n)? + pos;
        score += 10;
        if last.is_some_and(|l| l + 1 == found) {
            score += 15;
        }
        if found == 0 || matches!(hay_l.get(found - 1), Some(' ' | '/' | '-' | '_' | ':' | '.')) {
            score += 10;
        }
        last = Some(found);
        pos = found + 1;
    }
    Some(score - (hay_l.len() as i32) / 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative() {
        assert_eq!(relative_time(0, 30), "just now");
        assert_eq!(relative_time(0, 3600), "1 hour ago");
        assert_eq!(relative_time(0, 7200), "2 hours ago");
    }
    #[test]
    fn lines() {
        assert_eq!(split_lines("a\r\nb\n"), vec!["a", "b"]);
        assert_eq!(split_lines("a\n\nb"), vec!["a", "", "b"]);
        assert!(split_lines("").is_empty());
    }
    #[test]
    fn prefix_respects_char_boundaries() {
        assert_eq!(prefix("abcdef", 3), "abc");
        assert_eq!(prefix("ab", 8), "ab");
        assert_eq!(prefix("özellik", 1), "");
        assert_eq!(prefix("özellik", 2), "ö");
    }

    #[test]
    fn fuzzy() {
        assert!(fuzzy_score("gc", "git commit").is_some());
        assert!(fuzzy_score("zz", "git commit").is_none());
        assert!(fuzzy_score("gc", "git commit") > fuzzy_score("gc", "xgxxxxcxxxxxxxxxxxxxxxxxxx"));
    }
}
