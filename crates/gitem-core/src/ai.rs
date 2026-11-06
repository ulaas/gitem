//! AI features: commit messages, explanations, PR text, and commit composition.
//! Providers are called over plain HTTPS; keys live in the credential store.
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum AiProvider {
    #[default]
    Anthropic,
    OpenAi,
    /// Any server with an OpenAI-style chat endpoint (Ollama, LM Studio, ...).
    OpenAiCompatible,
    Gemini,
    Mistral,
    OpenRouter,
}

impl AiProvider {
    pub const ALL: [AiProvider; 6] = [Self::Anthropic, Self::OpenAi, Self::OpenAiCompatible, Self::Gemini, Self::Mistral, Self::OpenRouter];

    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic Claude",
            Self::OpenAi => "OpenAI",
            Self::OpenAiCompatible => "OpenAI-compatible (Ollama, LM Studio, ...)",
            Self::Gemini => "Google Gemini",
            Self::Mistral => "Mistral",
            Self::OpenRouter => "OpenRouter",
        }
    }
    pub fn key_name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::OpenAiCompatible => "openai-compatible",
            Self::Gemini => "gemini",
            Self::Mistral => "mistral",
            Self::OpenRouter => "openrouter",
        }
    }
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Anthropic => "claude-opus-5-5",
            Self::OpenAi => "gpt-4o-mini",
            Self::OpenAiCompatible => "llama3",
            Self::Gemini => "gemini-2.0-flash",
            Self::Mistral => "mistral-large-latest",
            Self::OpenRouter => "anthropic/claude-opus-5-5",
        }
    }
    pub fn needs_key(self) -> bool {
        !matches!(self, Self::OpenAiCompatible)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AiConfig {
    pub provider: AiProvider,
    pub model: String,
    pub base_url: String,
    pub commit_style: String,
    /// Extra guidance added to reviews and conflict resolution.
    pub custom_instructions: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self { provider: AiProvider::Anthropic, model: String::new(), base_url: String::new(), commit_style: String::new(), custom_instructions: String::new() }
    }
}

impl AiConfig {
    pub fn effective_model(&self) -> String {
        if self.model.trim().is_empty() { self.provider.default_model().to_string() } else { self.model.trim().to_string() }
    }
}

/// Requests above this size are refused rather than silently cut, so the result
/// never describes only part of a diff without the user knowing.
pub const MAX_PROMPT_CHARS: usize = 600_000;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(300))).build().into()
}

fn post(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value> {
    let mut r = agent().post(url);
    for (k, v) in headers {
        r = r.header(*k, v);
    }
    let mut resp = r.header("content-type", "application/json").send(body.to_string()).map_err(|e| Error::Other(format!("request failed: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| Error::Other(format!("reading response: {e}")))?;
    let v: Value = serde_json::from_str(&text).map_err(|_| Error::Other(format!("HTTP {status}: {}", text.chars().take(400).collect::<String>())))?;
    if status >= 400 {
        let msg = v.pointer("/error/message").and_then(Value::as_str).unwrap_or(&text);
        return Err(Error::Other(format!("HTTP {status}: {msg}")));
    }
    Ok(v)
}

/// Pull the text out of a Messages API response, checking why it stopped first.
pub fn parse_anthropic(v: &Value) -> Result<String> {
    if v.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        return Err(Error::Other("The model declined this request.".into()));
    }
    let text: String = v
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("text")).filter_map(|b| b.get("text").and_then(Value::as_str)).collect())
        .unwrap_or_default();
    if text.is_empty() {
        return Err(Error::Other("The model returned no text.".into()));
    }
    Ok(text)
}

pub fn anthropic_body(model: &str, system: &str, user: &str) -> Value {
    let mut b = json!({
        "model": model,
        "max_tokens": 16000,
        "system": system,
        "messages": [{"role": "user", "content": user}],
    });
    // Short, well-scoped tasks: low effort is plenty, and it is set explicitly
    // because defaults vary across models.
    if model.starts_with("claude-opus-5") || model.starts_with("claude-sonnet-5") || model.starts_with("claude-fable-5") || model.starts_with("claude-haiku-5") {
        b["output_config"] = json!({"effort": "low"});
    }
    if uses_server_fallback(model) {
        b["fallbacks"] = json!("default");
    }
    b
}

/// Models that may decline on a safety classifier and accept server-side fallback routing.
fn uses_server_fallback(model: &str) -> bool {
    matches!(model, "claude-fable-5-1" | "claude-opus-5-5" | "claude-opus-5" | "claude-sonnet-5-5")
}

/// Send one system+user exchange to the configured provider.
pub fn complete(cfg: &AiConfig, key: Option<&str>, system: &str, user: &str) -> Result<String> {
    if system.len() + user.len() > MAX_PROMPT_CHARS {
        return Err(Error::Other(format!(
            "This change is too large to send ({} characters; the limit is {MAX_PROMPT_CHARS}). Select fewer files and try again.",
            user.len()
        )));
    }
    let model = cfg.effective_model();
    let need_key = || key.filter(|k| !k.is_empty()).ok_or_else(|| Error::Other(format!("No API key for {}. Add one in Settings > AI.", cfg.provider.label())));
    match cfg.provider {
        AiProvider::Anthropic => {
            let base = if cfg.base_url.is_empty() { "https://api.anthropic.com" } else { cfg.base_url.trim_end_matches('/') };
            let mut h = vec![("x-api-key", need_key()?.to_string()), ("anthropic-version", "2026-10-21".to_string())];
            if uses_server_fallback(&model) {
                h.push(("anthropic-beta", "server-side-fallback-2026-07-01".to_string()));
            }
            parse_anthropic(&post(&format!("{base}/v1/messages"), &h, &anthropic_body(&model, system, user))?)
        }
        AiProvider::OpenAi | AiProvider::OpenAiCompatible | AiProvider::Mistral | AiProvider::OpenRouter => {
            let base = if !cfg.base_url.is_empty() {
                cfg.base_url.trim_end_matches('/').to_string()
            } else if cfg.provider == AiProvider::OpenAi {
                "https://api.openai.com/v1".to_string()
            } else if cfg.provider == AiProvider::Mistral {
                "https://api.mistral.ai/v1".to_string()
            } else if cfg.provider == AiProvider::OpenRouter {
                "https://openrouter.ai/api/v1".to_string()
            } else {
                "http://localhost:11434/v1".to_string()
            };
            let mut h = Vec::new();
            if let Some(k) = key.filter(|k| !k.is_empty()) {
                h.push(("authorization", format!("Bearer {k}")));
            } else if cfg.provider != AiProvider::OpenAiCompatible {
                need_key()?;
            }
            let body = json!({"model": model, "messages": [{"role":"system","content":system},{"role":"user","content":user}]});
            let v = post(&format!("{base}/chat/completions"), &h, &body)?;
            v.pointer("/choices/0/message/content").and_then(Value::as_str).map(String::from).ok_or_else(|| Error::Other("The model returned no text.".into()))
        }
        AiProvider::Gemini => {
            let base = if cfg.base_url.is_empty() { "https://generativelanguage.googleapis.com/v1beta" } else { cfg.base_url.trim_end_matches('/') };
            let h = vec![("x-goog-api-key", need_key()?.to_string())];
            let body = json!({"systemInstruction":{"parts":[{"text":system}]},"contents":[{"role":"user","parts":[{"text":user}]}]});
            let v = post(&format!("{base}/models/{model}:generateContent"), &h, &body)?;
            v.pointer("/candidates/0/content/parts/0/text").and_then(Value::as_str).map(String::from).ok_or_else(|| Error::Other("The model returned no text.".into()))
        }
    }
}

// ---- prompts --------------------------------------------------------------------

pub fn commit_message_prompt(style: &str, diff: &str) -> (String, String) {
    let mut sys = String::from(
        "You write git commit messages. Read the diff and reply with only the commit message: a subject line in the imperative mood, under 72 characters, and, if the change needs explaining, a blank line followed by a short body saying why. No code fences and no preamble.",
    );
    if !style.trim().is_empty() {
        sys.push_str("\nFollow these project conventions: ");
        sys.push_str(style.trim());
    }
    (sys, format!("Diff:\n\n{diff}"))
}

pub fn explain_prompt(what: &str, message: &str, diff: &str) -> (String, String) {
    (
        format!("You explain {what} to a developer reviewing a repository. Say what changed and why it matters, and call out risks or follow-ups if you see any. Be concise and use plain text with short paragraphs."),
        format!("Message:\n{message}\n\nDiff:\n\n{diff}"),
    )
}

pub fn pr_prompt(commits: &str, diff: &str) -> (String, String) {
    (
        "You write pull request descriptions. Reply with the title on the first line, a blank line, and a Markdown body summarising the change and how to test it. No preamble.".to_string(),
        format!("Commits:\n{commits}\n\nDiff:\n\n{diff}"),
    )
}

pub fn stash_prompt(diff: &str) -> (String, String) {
    ("Reply with only a short stash description of under 60 characters summarising this diff.".to_string(), format!("Diff:\n\n{diff}"))
}

pub fn changelog_prompt(commits: &str) -> (String, String) {
    (
        "You write changelogs. Group the commits under Added, Changed, Fixed and Removed headings, omitting empty groups. Reply with Markdown only.".to_string(),
        format!("Commits:\n{commits}"),
    )
}

pub fn review_prompt(extra: &str, diff: &str) -> (String, String) {
    let mut s = "You review code changes. List concrete problems, most severe first, each tagged [high], [medium], or [low] with the file and a one-line fix. If you find none, say so. Don't praise or restate the diff.".to_string();
    if !extra.trim().is_empty() {
        s.push_str("\nAlso follow: ");
        s.push_str(extra.trim());
    }
    (s, format!("Diff:\n\n{diff}"))
}

/// What the model replies when it can't resolve a conflict confidently.
pub const UNSURE: &str = "UNSURE";

/// Findings from a review reply: `[high] path: message` becomes (path, text).
pub fn parse_review_findings(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim().trim_start_matches(['-', '*', ' ']);
        let Some(rest) = ["[high]", "[medium]", "[low]"].iter().find_map(|t| l.strip_prefix(t).map(|r| (*t, r))) else { continue };
        let body = rest.1.trim().replace('`', "");
        let (path, msg) = match body.split_once(": ") {
            Some((p, m)) if !p.contains(' ') => (p.to_string(), m.to_string()),
            _ => (String::new(), body.clone()),
        };
        out.push((path, format!("{} {msg}", rest.0)));
    }
    out
}

/// Ask for the merged text of one conflict.
pub fn conflict_prompt(path: &str, base: &str, ours: &str, theirs: &str, extra: &str) -> (String, String) {
    let mut s = "You resolve git merge conflicts. Combine both sides so neither side's intent is lost. Reply with only the resolved lines: no code fences, no explanation, no conflict markers. If you can't tell what's right, reply with exactly UNSURE.".to_string();
    if !extra.trim().is_empty() {
        s.push_str("\nAlso follow: ");
        s.push_str(extra.trim());
    }
    (s, format!("File: {path}\n\nCommon ancestor:\n{base}\n\nOurs:\n{ours}\n\nTheirs:\n{theirs}"))
}

/// Turn a plain-language request into gitem's search syntax.
pub fn search_prompt(question: &str, today: &str) -> (String, String) {
    (
        format!("Convert the request into a commit search query. Today is {today}. Use only these filters, separated by spaces: =:text (message), -message:text, @:name (author), committer:name, @me (the current user), file:path, change:text (code added or removed), after:YYYY-MM-DD, before:YYYY-MM-DD, is:merge, -is:merge, ref:branch. Quote values containing spaces. Reply with only the query."),
        question.to_string(),
    )
}

/// Remove files matching ignore rules from a diff, so they're never sent.
pub fn filter_diff(diff: &str, patterns: &[String]) -> String {
    if patterns.is_empty() {
        return diff.to_string();
    }
    let mut out = String::new();
    let mut keep = true;
    for line in diff.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = rest.trim_end().rsplit(" b/").next().unwrap_or("");
            keep = !patterns.iter().any(|p| ignore_match(p, path));
        }
        if keep {
            out.push_str(line);
        }
    }
    out
}

/// The commonly used subset of gitignore rules: `*`, `?`, folders, anchored paths.
fn ignore_match(pattern: &str, path: &str) -> bool {
    let p = pattern.trim_end_matches('/');
    let anchored = p.starts_with('/') || p.trim_start_matches('/').contains('/');
    let p = p.trim_start_matches('/').trim_start_matches("**/");
    let glob = |pat: &str, text: &str| -> bool {
        fn m(p: &[char], t: &[char]) -> bool {
            match p.first() {
                None => t.is_empty(),
                Some('*') => (0..=t.len()).any(|i| m(&p[1..], &t[i..])),
                Some('?') => !t.is_empty() && m(&p[1..], &t[1..]),
                Some(c) => t.first() == Some(c) && m(&p[1..], &t[1..]),
            }
        }
        m(&pat.chars().collect::<Vec<_>>(), &text.chars().collect::<Vec<_>>())
    };
    if anchored {
        return glob(p, path) || path.starts_with(&format!("{p}/"));
    }
    path.split('/').any(|seg| glob(p, seg))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposedCommit {
    pub message: String,
    pub files: Vec<String>,
}

pub fn composer_prompt(diff: &str) -> (String, String) {
    (
        "You organise uncommitted work into a clean series of commits. Group the changed files into logical commits, ordered so each builds on the previous, and place every file in exactly one commit. Reply with only JSON of the form {\"commits\":[{\"message\":\"subject\\n\\nbody\",\"files\":[\"path\"]}]}.".to_string(),
        format!("Diff:\n\n{diff}"),
    )
}

/// Tolerates prose or code fences around the JSON.
pub fn parse_composer(text: &str) -> Result<Vec<ComposedCommit>> {
    let start = text.find('{').ok_or_else(|| Error::Other("No JSON in the model's reply.".into()))?;
    let end = text.rfind('}').ok_or_else(|| Error::Other("No JSON in the model's reply.".into()))?;
    #[derive(Deserialize)]
    struct W {
        commits: Vec<ComposedCommit>,
    }
    let w: W = serde_json::from_str(&text[start..=end]).map_err(|e| Error::Other(format!("Could not read the model's reply: {e}")))?;
    Ok(w.commits)
}

/// Models sometimes wrap a message in a code fence despite instructions.
pub fn clean_message(s: &str) -> String {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("```") {
        let rest = rest.split_once('\n').map(|x| x.1).unwrap_or(rest);
        return rest.strip_suffix("```").unwrap_or(rest).trim().to_string();
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_parsing() {
        let ok = json!({"stop_reason":"end_turn","content":[{"type":"thinking","thinking":""},{"type":"text","text":"Fix bug"}]});
        assert_eq!(parse_anthropic(&ok).unwrap(), "Fix bug");
        let no = json!({"stop_reason":"refusal","content":[]});
        assert!(parse_anthropic(&no).unwrap_err().to_string().contains("declined"));
    }

    #[test]
    fn anthropic_request_shape() {
        let b = anthropic_body("claude-opus-5-5", "sys", "hi");
        assert_eq!(b["model"], "claude-opus-5-5");
        assert_eq!(b["output_config"]["effort"], "low");
        assert_eq!(b["fallbacks"], "default");
        assert!(b.get("thinking").is_none());
        assert!(b.get("temperature").is_none());
        let old = anthropic_body("claude-3-haiku", "s", "u");
        assert!(old.get("output_config").is_none() && old.get("fallbacks").is_none());
    }

    #[test]
    fn oversize_prompt_is_refused_before_any_network() {
        let cfg = AiConfig::default();
        let huge = "x".repeat(MAX_PROMPT_CHARS + 1);
        assert!(complete(&cfg, Some("k"), "s", &huge).unwrap_err().to_string().contains("too large"));
    }

    #[test]
    fn missing_key_reported() {
        let cfg = AiConfig::default();
        assert!(complete(&cfg, None, "s", "u").unwrap_err().to_string().contains("No API key"));
    }

    #[test]
    fn review_findings() {
        let f = parse_review_findings("Here you go:\n- [high] `src/a.rs`: null check\n[low] style nit in general\nno tag here\n");
        assert_eq!(f, vec![("src/a.rs".to_string(), "[high] null check".to_string()), (String::new(), "[low] style nit in general".to_string())]);
    }

    #[test]
    fn ignored_files_never_reach_the_prompt() {
        let d = "diff --git a/src/a.rs b/src/a.rs\n+ok\ndiff --git a/secrets/k.env b/secrets/k.env\n+TOKEN\ndiff --git a/x/y.pem b/x/y.pem\n+KEY\n";
        let f = filter_diff(d, &["secrets/".to_string(), "*.pem".to_string()]);
        assert!(f.contains("+ok"));
        assert!(!f.contains("TOKEN"));
        assert!(!f.contains("KEY"));
        assert_eq!(filter_diff(d, &[]), d);
        assert!(!ignore_match("/build", "src/build"));
        assert!(ignore_match("build", "src/build/out.o"));
    }

    #[test]
    fn composer_reply() {
        let t = "Sure:\n```json\n{\"commits\":[{\"message\":\"a\",\"files\":[\"x\"]}]}\n```";
        assert_eq!(parse_composer(t).unwrap()[0].files, vec!["x"]);
        assert!(parse_composer("nope").is_err());
    }

    #[test]
    fn fence_removal() {
        assert_eq!(clean_message("```\nFix it\n```"), "Fix it");
        assert_eq!(clean_message("  Fix it\n"), "Fix it");
    }
}
