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

/// Replace a file in one step, so a crash or a full disk leaves the old one whole. The
/// temp name is unique: two copies of the app saving at once mustn't share it.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("{}.{}-{n}.tmp", path.file_name().unwrap_or_default().to_string_lossy(), std::process::id());
    let tmp = path.with_file_name(name);
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        // On Windows a rename fails while anything has the target open for a moment.
        let mut tries = 0;
        loop {
            match std::fs::rename(&tmp, path) {
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && tries < 5 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
                other => return other,
            }
        }
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// At most `n` bytes of `s`, cut on a character boundary (names can be non-ASCII).
pub fn prefix(s: &str, n: usize) -> &str {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A full object name shortened for a title. Anything else is a ref name, which can be
/// 40 bytes long and non-ASCII too, and stays whole.
pub fn short_rev(s: &str) -> &str {
    if s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()) { &s[..7] } else { s }
}

/// Replaces the credentials in any URL in `s` with `***`, for showing it on screen.
/// Git itself still gets the real URL.
pub fn mask_url_credentials(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        let (head, after) = rest.split_at(i + 3);
        out.push_str(head);
        let authority = &after[..after.find(['/', '?', '#', '"', '\'']).unwrap_or(after.len())];
        let authority = authority.split_whitespace().next().unwrap_or("");
        rest = after;
        if let Some(at) = authority.rfind('@') {
            let scheme = head[..i].rsplit(|c: char| !c.is_ascii_alphanumeric()).next().unwrap_or("");
            // `ssh://git@host` names an account and holds no secret. Elsewhere a lone
            // name is often the token itself, as in `https://TOKEN@github.com`.
            if !scheme.eq_ignore_ascii_case("ssh") || authority[..at].contains(':') {
                out.push_str("***");
                rest = &after[at..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Subsequence fuzzy scoring; `None` when `needle` does not match.
pub fn fuzzy_score(needle: &str, hay: &str) -> Option<i32> {
    fuzzy_match(needle, hay).map(|m| m.0)
}

/// A score and the matched character positions. A run of the typed text beats
/// scattered letters, and one starting at a word beats one inside a word.
pub fn fuzzy_match(needle: &str, hay: &str) -> Option<(i32, Vec<u32>)> {
    let wanted: Vec<char> = needle.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if wanted.is_empty() {
        return Some((0, Vec::new()));
    }
    let hay_l: Vec<char> = hay.to_lowercase().chars().collect();
    let at_word = |i: usize| i == 0 || matches!(hay_l.get(i - 1), Some(' ' | '/' | '-' | '_' | ':' | '.'));
    let length_cost = (hay_l.len() as i32) / 8;
    let runs: Vec<usize> = (0..hay_l.len().saturating_sub(wanted.len() - 1)).filter(|&i| hay_l[i..i + wanted.len()] == wanted[..]).collect();
    if let Some(&start) = runs.iter().find(|&&i| at_word(i)).or(runs.first()) {
        let bonus = if start == 0 { 60 } else if at_word(start) { 40 } else { 0 };
        let score = 25 * wanted.len() as i32 + bonus - length_cost;
        return Some((score, (start..start + wanted.len()).map(|i| i as u32).collect()));
    }
    let mut score = 0;
    let mut pos = 0usize;
    let mut last: Option<usize> = None;
    let mut positions = Vec::with_capacity(wanted.len());
    for n in wanted {
        let found = hay_l[pos..].iter().position(|&c| c == n)? + pos;
        score += 10;
        if last.is_some_and(|l| l + 1 == found) {
            score += 15;
        }
        if at_word(found) {
            score += 10;
        }
        positions.push(found as u32);
        last = Some(found);
        pos = found + 1;
    }
    Some((score - length_cost, positions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_rev_only_shortens_a_full_sha() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(short_rev(sha), "0123456");
        assert_eq!(short_rev("main"), "main");
        // Git allows a branch name of any shape, including 40 bytes with a multibyte
        // character across byte 7.
        for name in [format!("fix-ab\u{f6}{}", "x".repeat(32)), format!("abcde\u{20ac}{}", "x".repeat(32)), "x".repeat(40)] {
            assert_eq!(name.len(), 40);
            assert_eq!(short_rev(&name), name);
        }
    }

    #[test]
    fn url_credentials_are_masked_for_display() {
        for (given, shown) in [
            ("https://user:FAKETOKEN@host.invalid/o/r.git", "https://***@host.invalid/o/r.git"),
            ("https://FAKETOKEN@host.invalid/o/r.git", "https://***@host.invalid/o/r.git"),
            ("http://x-access-token:FAKE@127.0.0.1:1/x.git", "http://***@127.0.0.1:1/x.git"),
            ("ssh://u:FAKE@host.invalid/x", "ssh://***@host.invalid/x"),
            ("Clone https://a:FAKE@h.invalid/x into https://b:FAKE@i.invalid/y", "Clone https://***@h.invalid/x into https://***@i.invalid/y"),
            ("--foo=https://a:FAKE@h.invalid/x", "--foo=https://***@h.invalid/x"),
            // A user name alone in SSH isn't a secret, and these have no credentials.
            ("ssh://git@host.invalid/x.git", "ssh://git@host.invalid/x.git"),
            ("git@host.invalid:o/r.git", "git@host.invalid:o/r.git"),
            ("https://host.invalid/o/r@v1", "https://host.invalid/o/r@v1"),
            ("file:///c:/a@b/c", "file:///c:/a@b/c"),
            ("plain text with an @ sign", "plain text with an @ sign"),
        ] {
            assert_eq!(mask_url_credentials(given), shown);
        }
    }
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
        assert_eq!(fuzzy_match("gc", "git commit").unwrap().1, vec![0, 4]);
        assert_eq!(fuzzy_match("comm", "git commit").unwrap().1, vec![4, 5, 6, 7]);
        // A word start beats the same letters inside a word, and a run beats scattered.
        assert_eq!(fuzzy_match("log", "catalog log").unwrap().1, vec![8, 9, 10]);
        assert!(fuzzy_score("pull", "Pull") > fuzzy_score("pull", "a pretty unusual lull"));
        assert_eq!(fuzzy_match("", "anything"), Some((0, Vec::new())));
    }
}
