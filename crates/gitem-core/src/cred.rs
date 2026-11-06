//! Credential manager.
//!
//! Secrets go to the OS vault: Windows Credential Manager via the Win32 API.
//! On other platforms a 0600 file under the config dir is used (no vault
//! dependency); the UI labels which backend is active.
//!
//! Three kinds of secrets, distinguished by target-name prefix:
//!   `gitem:forge:<host>`   API tokens for GitHub, GitLab, ...
//!   `gitem:git:<url>`      username/password served to git as a credential helper
//!   `gitem:ai:<provider>`  AI provider API keys
//!
//! `gitem --credential-helper get|store|erase` implements git's helper protocol.
use std::io::Write as _;
use std::path::Path;

use crate::runner::{GitExe, RunOpts};
use crate::{Error, Result};

pub const PREFIX_FORGE: &str = "gitem:forge:";
pub const PREFIX_GIT: &str = "gitem:git:";
pub const PREFIX_AI: &str = "gitem:ai:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCred {
    pub target: String,
    pub username: String,
    pub secret: String,
}

pub fn backend_name() -> &'static str {
    if cfg!(windows) { "Windows Credential Manager" } else { "Private file (0600) in the config directory" }
}

#[cfg(windows)]
mod backend {
    use super::*;
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredEnumerateW, CredFree, CredReadW, CredWriteW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe fn from_wide(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        unsafe {
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
        }
    }

    pub fn write(target: &str, username: &str, secret: &str) -> Result<()> {
        let mut t = wide(target);
        let mut u = wide(username);
        let mut blob: Vec<u8> = secret.as_bytes().to_vec();
        let mut c: CREDENTIALW = unsafe { std::mem::zeroed() };
        c.Type = CRED_TYPE_GENERIC;
        c.TargetName = t.as_mut_ptr();
        c.UserName = u.as_mut_ptr();
        c.CredentialBlobSize = blob.len() as u32;
        c.CredentialBlob = blob.as_mut_ptr();
        c.Persist = CRED_PERSIST_LOCAL_MACHINE;
        if unsafe { CredWriteW(&c, 0) } == 0 {
            return Err(Error::Other(format!("CredWrite failed: {}", std::io::Error::last_os_error())));
        }
        Ok(())
    }

    pub fn read(target: &str) -> Result<Option<StoredCred>> {
        let t = wide(target);
        let mut p: *mut CREDENTIALW = std::ptr::null_mut();
        if unsafe { CredReadW(t.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) } == 0 {
            if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                return Ok(None);
            }
            return Err(Error::Other(format!("CredRead failed: {}", std::io::Error::last_os_error())));
        }
        let cred = unsafe {
            let c = &*p;
            // An empty secret has a null blob, which `from_raw_parts` must not see.
            let blob: &[u8] = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 { &[] } else { std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize) };
            let out = StoredCred { target: from_wide(c.TargetName), username: from_wide(c.UserName), secret: decode_blob(blob) };
            CredFree(p as *const _);
            out
        };
        Ok(Some(cred))
    }

    /// Git Credential Manager stores UTF-16 blobs, we store UTF-8. Accept both.
    fn decode_blob(b: &[u8]) -> String {
        if b.len() >= 2 && b.len().is_multiple_of(2) && b.iter().skip(1).step_by(2).all(|&x| x == 0) {
            let w: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            return String::from_utf16_lossy(&w);
        }
        String::from_utf8_lossy(b).into_owned()
    }

    pub fn delete(target: &str) -> Result<()> {
        let t = wide(target);
        if unsafe { CredDeleteW(t.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 && unsafe { GetLastError() } != ERROR_NOT_FOUND {
            return Err(Error::Other(format!("CredDelete failed: {}", std::io::Error::last_os_error())));
        }
        Ok(())
    }

    /// (target, username) of all generic credentials whose target has a prefix.
    pub fn list(prefix: &str) -> Vec<(String, String)> {
        let mut count = 0u32;
        let mut arr: *mut *mut CREDENTIALW = std::ptr::null_mut();
        let filter = wide(&format!("{prefix}*"));
        let ok = unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut arr) };
        if ok == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        unsafe {
            for i in 0..count as usize {
                let c = &**arr.add(i);
                if c.Type != CRED_TYPE_GENERIC {
                    continue;
                }
                let t = from_wide(c.TargetName);
                if t.starts_with(prefix) {
                    out.push((t, from_wide(c.UserName)));
                }
            }
            CredFree(arr as *const _);
        }
        out
    }
}

#[cfg(not(windows))]
mod backend {
    use super::*;
    use std::collections::BTreeMap;
    type Store = BTreeMap<String, (String, String)>;

    fn path() -> std::path::PathBuf {
        crate::settings::config_dir().join("credentials.json")
    }
    fn load() -> Store {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    fn save(s: &Store) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let p = path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p)?;
        f.write_all(&serde_json::to_vec(s).map_err(|e| Error::Other(e.to_string()))?)?;
        Ok(())
    }
    pub fn write(target: &str, username: &str, secret: &str) -> Result<()> {
        let mut s = load();
        s.insert(target.to_string(), (username.to_string(), secret.to_string()));
        save(&s)
    }
    pub fn read(target: &str) -> Result<Option<StoredCred>> {
        Ok(load().get(target).map(|(u, p)| StoredCred { target: target.to_string(), username: u.clone(), secret: p.clone() }))
    }
    pub fn delete(target: &str) -> Result<()> {
        let mut s = load();
        s.remove(target);
        save(&s)
    }
    pub fn list(prefix: &str) -> Vec<(String, String)> {
        load().into_iter().filter(|(k, _)| k.starts_with(prefix)).map(|(k, (u, _))| (k, u)).collect()
    }
}

pub fn store(target: &str, username: &str, secret: &str) -> Result<()> {
    backend::write(target, username, secret)
}
pub fn get(target: &str) -> Result<Option<StoredCred>> {
    backend::read(target)
}
pub fn delete(target: &str) -> Result<()> {
    backend::delete(target)
}
pub fn list(prefix: &str) -> Vec<(String, String)> {
    backend::list(prefix)
}

/// `gitem:*` entries plus those from Git Credential Manager (`git:*`), which the
/// manager view can display and remove.
pub fn list_all() -> Vec<(String, String)> {
    let mut v = list("gitem:");
    v.extend(list("git:"));
    v.sort();
    v
}

pub fn forge_token(host: &str) -> Option<String> {
    get(&format!("{PREFIX_FORGE}{host}")).ok().flatten().map(|c| c.secret).filter(|s| !s.is_empty())
}

pub fn set_forge_token(host: &str, user: &str, token: &str) -> Result<()> {
    store(&format!("{PREFIX_FORGE}{host}"), user, token)
}

pub fn ai_key(provider: &str) -> Option<String> {
    get(&format!("{PREFIX_AI}{provider}")).ok().flatten().map(|c| c.secret).filter(|s| !s.is_empty())
}

pub fn set_ai_key(provider: &str, key: &str) -> Result<()> {
    store(&format!("{PREFIX_AI}{provider}"), "api-key", key)
}

// ---- git's credential helper protocol ------------------------------------------

/// Parse `key=value` lines from git; stops at the first blank line.
pub fn parse_helper_input(s: &str) -> Vec<(String, String)> {
    s.lines().take_while(|l| !l.is_empty()).filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn helper_target(fields: &[(String, String)]) -> String {
    let get = |k: &str| fields.iter().find(|(a, _)| a == k).map(|x| x.1.as_str()).unwrap_or("");
    let mut t = format!("{PREFIX_GIT}{}://{}", get("protocol"), get("host"));
    let path = get("path");
    if !path.is_empty() {
        t.push('/');
        t.push_str(path);
    }
    t
}

/// Implements `get`, `store` and `erase`. Returns what to print on stdout.
pub fn helper(op: &str, input: &str) -> Result<String> {
    let fields = parse_helper_input(input);
    let target = helper_target(&fields);
    let field = |k: &str| fields.iter().find(|(a, _)| a == k).map(|x| x.1.clone());
    let host_only = helper_target(&fields.iter().filter(|(k, _)| k != "path").cloned().collect::<Vec<_>>());
    match op {
        "get" => {
            // Prefer a path-specific entry, then fall back to the host entry.
            let found = match get(&target)? {
                Some(c) => Some(c),
                None => get(&host_only)?,
            };
            if let Some(c) = found {
                if field("username").is_some_and(|u| !c.username.is_empty() && u != c.username) {
                    return Ok(String::new());
                }
                return Ok(format!("username={}\npassword={}\n", c.username, c.secret));
            }
            Ok(String::new())
        }
        "store" => {
            if let (Some(u), Some(p)) = (field("username"), field("password")) {
                store(&target, &u, &p)?;
            }
            Ok(String::new())
        }
        "erase" => {
            // `get` may have answered from the host entry, so that's the one to drop.
            delete(if get(&target)?.is_some() { &target } else { &host_only })?;
            Ok(String::new())
        }
        // Git ignores unknown operations; do the same.
        _ => Ok(String::new()),
    }
}

/// Ask whichever credential helpers git is configured with. Never prompts.
pub fn git_credential_fill(git: &GitExe, url: &str) -> Option<(String, String)> {
    let opts = RunOpts {
        stdin: Some(format!("url={url}\n\n").into_bytes()),
        env: vec![("GIT_TERMINAL_PROMPT".into(), "0".into()), ("GIT_ASKPASS".into(), "".into()), ("SSH_ASKPASS".into(), "".into()), ("GCM_INTERACTIVE".into(), "never".into())],
        ..Default::default()
    };
    let o = git.run_opts(None, &["credential".to_string(), "fill".to_string()], &opts, &mut |_| {}).ok()?;
    if !o.ok() {
        return None;
    }
    let text = o.text();
    let fields = parse_helper_input(&text);
    let f = |k: &str| fields.iter().find(|(a, _)| a == k).map(|x| x.1.clone());
    Some((f("username").unwrap_or_default(), f("password")?))
}

/// Remove an entry from git's configured helpers (`git credential reject`).
pub fn git_credential_reject(git: &GitExe, url: &str) -> Result<()> {
    let opts = RunOpts { stdin: Some(format!("url={url}\n\n").into_bytes()), is_write: true, ..Default::default() };
    git.run_opts(None, &["credential".to_string(), "reject".to_string()], &opts, &mut |_| {})?.into_result()?;
    Ok(())
}

// ---- SSH keys ------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct SshKey {
    pub name: String,
    pub private_path: String,
    pub public_path: String,
    pub public: String,
    pub kind: String,
    pub comment: String,
}

pub fn ssh_dir() -> std::path::PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
    Path::new(&home).join(".ssh")
}

pub fn list_ssh_keys() -> Vec<SshKey> {
    let dir = ssh_dir();
    let mut v = Vec::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "pub") {
            let public = std::fs::read_to_string(&p).unwrap_or_default().trim().to_string();
            let mut parts = public.split_whitespace();
            let kind = parts.next().unwrap_or("").to_string();
            let _key = parts.next();
            let comment = parts.collect::<Vec<_>>().join(" ");
            let private = p.with_extension("");
            v.push(SshKey {
                name: private.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                private_path: if private.exists() { private.display().to_string() } else { String::new() },
                public_path: p.display().to_string(),
                public,
                kind,
                comment,
            });
        }
    }
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

fn run_tool(tool: &str, args: &[&str]) -> Result<String> {
    let mut c = std::process::Command::new(tool);
    c.args(args).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let o = c.output().map_err(|e| Error::Other(format!("{tool}: {e}")))?;
    let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&o.stderr));
    if o.status.success() { Ok(s) } else { Err(Error::Other(s.trim().to_string())) }
}

/// Generate an ed25519 key with `ssh-keygen`; an empty passphrase means none.
pub fn ssh_generate(name: &str, comment: &str, passphrase: &str) -> Result<String> {
    let dir = ssh_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    if path.exists() {
        return Err(Error::Other(format!("{} already exists", path.display())));
    }
    let p = path.to_string_lossy().into_owned();
    run_tool("ssh-keygen", &["-t", "ed25519", "-C", comment, "-N", passphrase, "-f", &p])?;
    Ok(p)
}

pub fn ssh_agent_list() -> Result<String> {
    // `ssh-add -l` exits 1 with "no identities"; that is a state, not an error.
    match run_tool("ssh-add", &["-l"]) {
        Ok(s) => Ok(s),
        Err(Error::Other(m)) if m.contains("no identities") => Ok("The agent has no identities.".to_string()),
        Err(e) => Err(e),
    }
}

pub fn ssh_agent_add(path: &str) -> Result<String> {
    run_tool("ssh-add", &[path])
}

/// `ssh -T git@host`: forges answer with a greeting and exit 1, so report output either way.
pub fn ssh_test(host: &str) -> String {
    match run_tool("ssh", &["-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new", "-o", "ConnectTimeout=10", &format!("git@{host}")]) {
        Ok(s) => s,
        Err(e) => e.to_string(),
    }
}

/// Register gitem as credential helper in git config (`global` or `local`).
pub fn helper_config_value(exe: &Path) -> String {
    format!("!\"{}\" --credential-helper", exe.to_string_lossy().replace('\\', "/"))
}

/// For the credential-helper CLI mode.
pub fn helper_main(op: &str) -> i32 {
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    match helper(op, &input) {
        Ok(s) => {
            let _ = std::io::stdout().write_all(s.as_bytes());
            0
        }
        Err(e) => {
            eprintln!("gitem credential helper: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_input() {
        let f = parse_helper_input("protocol=https\nhost=example.com\n\nignored=1\n");
        assert_eq!(f.len(), 2);
        assert_eq!(helper_target(&f), "gitem:git:https://example.com");
    }

    /// Real round trip through the OS vault, under a unique name, always cleaned up.
    #[test]
    fn vault_roundtrip_and_helper_protocol() {
        let host = format!("gitem-test-{}.invalid", std::process::id());
        let input = format!("protocol=https\nhost={host}\nusername=me\npassword=s3cret\n\n");
        let target = format!("{PREFIX_GIT}https://{host}");
        let result = std::panic::catch_unwind(|| {
            helper("store", &input).unwrap();
            let got = helper("get", &format!("protocol=https\nhost={host}\n\n")).unwrap();
            assert_eq!(got, "username=me\npassword=s3cret\n");
            assert!(list(PREFIX_GIT).iter().any(|(t, _)| *t == target));
            helper("erase", &format!("protocol=https\nhost={host}\n\n")).unwrap();
            assert_eq!(helper("get", &format!("protocol=https\nhost={host}\n\n")).unwrap(), "");
        });
        let _ = delete(&target);
        result.unwrap();
    }
}
