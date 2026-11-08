//! Asking whether a newer release exists. It only reads: nothing here downloads or runs anything,
//! and the app opens the release's page in the browser if the user asks. No token goes with it.
use serde_json::Value;

use crate::{Error, Result};

/// Where releases are listed, in GitHub's release API format: the latest release on GitHub. Only
/// asked when the user turns update checks on.
pub const DEFAULT_RELEASE_URL: &str = "https://api.github.com/repos/ulaas/gitem/releases/latest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// Its page, to read and download from.
    pub page: String,
    pub notes: String,
}

/// `1.2.3` from `v1.2.3` or `gitem-1.2.3`. A pre-release's suffix doesn't count towards order.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let at = s.find(|c: char| c.is_ascii_digit())?;
    let core = s[at..].split(['-', '+']).next().unwrap_or("");
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    Some((major, minor, patch))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

/// Only an encrypted address, or this machine for a test. A check that an attacker on the network
/// could answer would offer them a link the user is told to trust.
pub fn url_problem(url: &str) -> Option<String> {
    let local = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"].iter().any(|p| url.starts_with(p));
    (!url.starts_with("https://") && !local).then(|| format!("The address to check for updates has to start with https://: {url}"))
}

/// What the reply says about a release, if it's one worth mentioning. A draft or pre-release isn't,
/// and neither is anything that would send the browser somewhere other than a web page.
pub fn read_release(reply: &Value, current: &str) -> Result<Option<Release>> {
    let tag = reply.get("tag_name").and_then(Value::as_str).ok_or_else(|| Error::Other("The reply about updates named no release.".to_string()))?;
    let flagged = |k: &str| reply.get(k).and_then(Value::as_bool) == Some(true);
    if flagged("draft") || flagged("prerelease") || !is_newer(tag, current) {
        return Ok(None);
    }
    let page = reply.get("html_url").and_then(Value::as_str).unwrap_or("");
    if !page.starts_with("https://") && !page.starts_with("http://127.0.0.1:") {
        return Err(Error::Other(format!("The reply about updates gave an unusable address: {page}")));
    }
    let notes = reply.get("body").and_then(Value::as_str).unwrap_or("").chars().take(2000).collect();
    Ok(Some(Release { version: tag.trim_start_matches(['v', 'V']).to_string(), page: page.to_string(), notes }))
}

/// Asks `url` for the latest release. Run only when the user asked, or has turned the check on.
pub fn check(url: &str, current: &str) -> Result<Option<Release>> {
    if let Some(why) = url_problem(url) {
        return Err(Error::Other(why));
    }
    let config = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(20))).user_agent(format!("gitem/{current}")).build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent.get(url).header("accept", "application/vnd.github+json").call().map_err(|e| Error::Other(format!("{url}: {e}")))?;
    let text = resp.body_mut().read_to_string().map_err(|e| Error::Other(format!("reading the reply from {url}: {e}")))?;
    let reply: Value = serde_json::from_str(&text).map_err(|e| Error::Other(format!("bad response from {url}: {e}")))?;
    read_release(&reply, current)
}

/// Whether a scheduled check is due: on, and at least a day since the last.
pub fn due(enabled: bool, url: &str, last: i64, now: i64) -> bool {
    enabled && !url.trim().is_empty() && now - last >= 24 * 60 * 60
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(is_newer("v0.10.0", "0.5.2"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("v0.5.2", "0.5.2"));
        assert!(!is_newer("0.5.1", "0.5.2"));
        assert!(!is_newer("not-a-version", "0.5.2"));
        assert!(is_newer("gitem-0.6.0-rc1", "0.5.2"));
    }

    #[test]
    fn only_a_finished_newer_release_with_a_web_page_counts() {
        let r = json!({"tag_name":"v0.6.0","html_url":"https://example.test/r","body":"Fixes"});
        assert_eq!(read_release(&r, "0.5.2").unwrap().unwrap(), Release { version: "0.6.0".into(), page: "https://example.test/r".into(), notes: "Fixes".into() });
        assert_eq!(read_release(&r, "0.6.0").unwrap(), None);
        for flag in ["draft", "prerelease"] {
            let mut x = r.clone();
            x[flag] = json!(true);
            assert_eq!(read_release(&x, "0.5.2").unwrap(), None, "{flag}");
        }
        let bad = json!({"tag_name":"v9.0.0","html_url":"file:///c:/evil.exe"});
        assert!(read_release(&bad, "0.5.2").is_err());
        assert!(read_release(&json!({}), "0.5.2").is_err());
    }

    #[test]
    fn a_plain_web_address_is_refused_but_this_machine_is_not() {
        assert!(url_problem("http://example.test/latest").is_some());
        assert!(url_problem("https://example.test/latest").is_none());
        assert!(url_problem("http://127.0.0.1:9/latest").is_none());
        assert!(check("http://example.test/latest", "0.5.2").unwrap_err().to_string().contains("https://"));
    }

    #[test]
    fn a_scheduled_check_waits_for_the_setting_an_address_and_a_day() {
        let day = 24 * 60 * 60;
        assert!(due(true, "https://x", 0, day));
        assert!(!due(false, "https://x", 0, day), "off");
        assert!(!due(true, "", 0, day), "nowhere to ask");
        assert!(!due(true, "https://x", 1, day), "asked less than a day ago");
    }
}
