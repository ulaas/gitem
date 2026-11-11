//! Asking whether a newer release exists, and installing it when the user says so. Asking only
//! reads. Nothing is downloaded until the user asks for the update, and what's downloaded is
//! installed only if it's the file the release's checksums describe. No token goes with any of it.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// Where releases are listed, in GitHub's release API format: the latest release on GitHub.
pub const DEFAULT_RELEASE_URL: &str = "https://api.github.com/repos/ulaas/gitem/releases/latest";

/// Where a release's files are. Only a file from here is installed: an address someone typed or
/// imported into Settings can say a release exists and link to its page, and no more than that.
pub const DOWNLOAD_PREFIX: &str = "https://github.com/ulaas/gitem/releases/download/";

/// The biggest file taken as an update. The largest so far is under 40 MB.
const MAX_DOWNLOAD: u64 = 400 * 1024 * 1024;
const MAX_SUMS: u64 = 1024 * 1024;

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// What GitHub says its SHA-256 is, in lowercase hex, when it says.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// Its page, to read and download from.
    pub page: String,
    pub notes: String,
    pub assets: Vec<Asset>,
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

fn is_local(url: &str) -> bool {
    ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"].iter().any(|p| url.starts_with(p))
}

/// Only an encrypted address, or this machine for a test. A check that an attacker on the network
/// could answer would offer them a link the user is told to trust.
pub fn url_problem(url: &str) -> Option<String> {
    (!url.starts_with("https://") && !is_local(url)).then(|| format!("The address to check for updates has to start with https://: {url}"))
}

fn sha256_hex(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase();
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then_some(s)
}

fn read_assets(reply: &Value) -> Vec<Asset> {
    let list = reply.get("assets").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    list.iter()
        .filter_map(|a| {
            let name = a.get("name").and_then(Value::as_str)?;
            let url = a.get("browser_download_url").and_then(Value::as_str)?;
            // A name is used as a file name: nothing that could leave the folder it's put in.
            if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\', ':']) {
                return None;
            }
            let sha256 = a.get("digest").and_then(Value::as_str).and_then(|d| d.strip_prefix("sha256:")).and_then(sha256_hex);
            Some(Asset { name: name.to_string(), url: url.to_string(), size: a.get("size").and_then(Value::as_u64).unwrap_or(0), sha256 })
        })
        .collect()
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
    Ok(Some(Release { version: tag.trim_start_matches(['v', 'V']).to_string(), page: page.to_string(), notes, assets: read_assets(reply) }))
}

fn agent(current: &str, whole_call: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(whole_call))
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .user_agent(format!("gitem/{current}"))
        .tls_config(crate::forge::tls())
        .build()
        .into()
}

/// Asks `url` for the latest release. Nothing about this machine or what's on it goes along: the
/// request names gitem and its version, as any browser names itself.
pub fn check(url: &str, current: &str) -> Result<Option<Release>> {
    if let Some(why) = url_problem(url) {
        return Err(Error::Other(why));
    }
    let mut resp = agent(current, Duration::from_secs(20)).get(url).header("accept", "application/vnd.github+json").call().map_err(|e| Error::Other(format!("{url}: {e}")))?;
    let text = resp.body_mut().read_to_string().map_err(|e| Error::Other(format!("reading the reply from {url}: {e}")))?;
    let reply: Value = serde_json::from_str(&text).map_err(|e| Error::Other(format!("bad response from {url}: {e}")))?;
    read_release(&reply, current)
}

/// Whether a scheduled check is due: on, and at least a day since the last.
pub fn due(enabled: bool, url: &str, last: i64, now: i64) -> bool {
    enabled && !url.trim().is_empty() && now - last >= 24 * 60 * 60
}

// ---- how this copy got here, which decides how it's replaced

/// How the running copy was put on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// By gitem's Windows installer: the new version's installer is run, without its pages.
    Installer,
    /// A program file in a folder this user can write to, from the zip or the tar.gz.
    Program,
    /// An AppImage, which is one file to replace.
    AppImage(PathBuf),
    /// Something else looks after it, or the folder can't be written to. With what to run to
    /// update it, when that's known.
    Managed(Option<&'static str>),
}

fn can_write_in(dir: &Path) -> bool {
    let probe = dir.join(format!(".gitem-write-test-{}", std::process::id()));
    let made = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    made
}

/// `exe` is the running program's real path, and `appimage` what `$APPIMAGE` says.
pub fn install_kind(exe: &Path, appimage: Option<&Path>) -> Install {
    let path = exe.to_string_lossy().replace('\\', "/").to_lowercase();
    let managers: [(&str, &'static str); 5] = [
        ("/scoop/apps/", "scoop update gitem"),
        ("/microsoft/winget/", "winget upgrade ulaas.gitem"),
        ("/caskroom/", "brew upgrade --cask gitem"),
        ("/cellar/", "brew upgrade gitem"),
        ("/homebrew/", "brew upgrade --cask gitem"),
    ];
    if let Some((_, how)) = managers.iter().find(|(part, _)| path.contains(part)) {
        return Install::Managed(Some(how));
    }
    if let Some(image) = appimage {
        return match image.parent() {
            Some(dir) if image.is_file() && can_write_in(dir) => Install::AppImage(image.to_path_buf()),
            _ => Install::Managed(None),
        };
    }
    // A package's files: replacing one behind the package manager's back leaves it confused.
    if cfg!(unix) && ["/usr/", "/opt/", "/nix/", "/snap/", "/app/", "/bin/"].iter().any(|p| path.starts_with(p)) && !path.starts_with("/usr/local/") {
        return Install::Managed(None);
    }
    let Some(dir) = exe.parent() else { return Install::Managed(None) };
    if !can_write_in(dir) {
        return Install::Managed(None);
    }
    if cfg!(windows) && dir.join("unins000.exe").is_file() { Install::Installer } else { Install::Program }
}

/// The name of the release file that replaces this kind of copy on this system.
pub fn asset_name(version: &str, kind: &Install, os: &str, arch: &str) -> Option<String> {
    let arch = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return None,
    };
    Some(match (kind, os) {
        (Install::Installer, "windows") if arch == "x64" => format!("gitem-{version}-windows-x64-setup.exe"),
        (Install::Program, "windows") => format!("gitem-{version}-windows-{arch}.zip"),
        (Install::Program, "linux") => format!("gitem-{version}-linux-{arch}.tar.gz"),
        (Install::Program, "macos") => format!("gitem-{version}-macos-universal.tar.gz"),
        (Install::AppImage(_), "linux") => format!("gitem-{version}-linux-{arch}.AppImage"),
        _ => return None,
    })
}

/// The file of `release` that updates this copy, or why there's none to install.
pub fn asset_for<'a>(release: &'a Release, kind: &Install) -> std::result::Result<&'a Asset, String> {
    if let Install::Managed(how) = kind {
        return Err(match how {
            Some(how) => format!("This copy was installed by a package manager. Update it with: {how}"),
            None => "This copy is in a folder gitem can't write to, or belongs to a package. Update it the way it was installed.".to_string(),
        });
    }
    let name = asset_name(&release.version, kind, std::env::consts::OS, std::env::consts::ARCH).ok_or("There's no build of gitem for this system to update to.")?;
    release.assets.iter().find(|a| a.name == name).ok_or_else(|| format!("Release {} has no file called {name}.", release.version))
}

// ---- downloading and checking

/// How far a download is, and the way to stop it.
#[derive(Debug, Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    /// Set once the file is checked and being put in place, which can't be stopped half way.
    pub installing: AtomicBool,
    pub cancel: AtomicBool,
}

/// The checksum of `name` in a `SHA256SUMS` file: `<hex>  <name>` a line, as `sha256sum` writes.
pub fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| sha256_hex(hash)).flatten()
    })
}

/// What the file has to hash to. Every source the release has must agree, and there has to be one.
pub fn expected_sha256(asset: &Asset, sums: Option<&str>) -> std::result::Result<String, String> {
    let listed = sums.and_then(|s| sum_for(s, &asset.name));
    match (listed, asset.sha256.clone()) {
        (Some(a), Some(b)) if a != b => Err(format!("The release's checksums disagree about {}, so it wasn't installed.", asset.name)),
        (Some(a), _) | (None, Some(a)) => Ok(a),
        (None, None) => Err(format!("The release has no checksum for {}, so it wasn't installed.", asset.name)),
    }
}

fn download_problem(url: &str, from: &str) -> Option<String> {
    (!url.starts_with(from) || url_problem(url).is_some()).then(|| format!("The update isn't at an address gitem installs from: {url}"))
}

/// Downloads `asset` into `dir` and checks it. `from` is what its address has to start with. The
/// file is only left there if it's the one the release's checksums describe.
pub fn fetch(release: &Release, asset: &Asset, from: &str, dir: &Path, current: &str, progress: &Progress) -> std::result::Result<PathBuf, String> {
    if let Some(why) = download_problem(&asset.url, from) {
        return Err(why);
    }
    let agent = agent(current, Duration::from_secs(30 * 60));
    let sums = match release.assets.iter().find(|a| a.name == "SHA256SUMS" || a.name == "SHA256SUMS.txt") {
        Some(s) => {
            if let Some(why) = download_problem(&s.url, from) {
                return Err(why);
            }
            let resp = agent.get(&s.url).call().map_err(|e| format!("{}: {e}", s.url))?;
            Some(resp.into_body().into_with_config().limit(MAX_SUMS).read_to_string().map_err(|e| format!("reading {}: {e}", s.url))?)
        }
        None => None,
    };
    let want = expected_sha256(asset, sums.as_deref())?;

    let resp = agent.get(&asset.url).call().map_err(|e| format!("{}: {e}", asset.url))?;
    let total = resp.body().content_length().unwrap_or(asset.size);
    if total > MAX_DOWNLOAD {
        return Err(format!("{} is larger than an update should be.", asset.name));
    }
    progress.total.store(total, Ordering::Relaxed);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(&asset.name);
    let written = (|| -> std::result::Result<String, String> {
        let mut reader = resp.into_body().into_with_config().limit(MAX_DOWNLOAD).reader();
        let mut file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut hash = Sha256::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err("Stopped.".to_string());
            }
            let n = reader.read(&mut chunk).map_err(|e| format!("downloading {}: {e}", asset.name))?;
            if n == 0 {
                break;
            }
            hash.update(&chunk[..n]);
            file.write_all(&chunk[..n]).map_err(|e| format!("{}: {e}", path.display()))?;
            progress.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        file.sync_all().map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
    })();
    match written {
        Ok(got) if got == want => Ok(path),
        Ok(_) => {
            let _ = std::fs::remove_file(&path);
            Err(format!("{} isn't the file the release's checksums describe, so it wasn't installed.", asset.name))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(e)
        }
    }
}

// ---- putting it in place

/// Where a download is kept until it's installed: beside the program, so moving it into place
/// never crosses to another drive.
pub fn staging_dir(target: &Path) -> PathBuf {
    target.parent().unwrap_or(Path::new(".")).join(".gitem-update")
}

/// The running program, moved aside on Windows while a new one takes its name.
pub fn aside(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    exe.with_file_name(name)
}

/// Takes away what an earlier update left. The program it replaced may still be closing, so this
/// tries for a few seconds, from a thread of its own.
pub fn clean_up_after_update(exe: &Path) {
    let (old, stage) = (aside(exe), staging_dir(exe));
    if !old.exists() && !stage.exists() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..10 {
            let _ = std::fs::remove_dir_all(&stage);
            if std::fs::remove_file(&old).is_ok() || !old.exists() {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

fn io(what: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", what.display())
}

#[cfg(unix)]
fn make_runnable(path: &Path) -> std::result::Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| io(path, e))
}

#[cfg(not(unix))]
fn make_runnable(_: &Path) -> std::result::Result<(), String> {
    Ok(())
}

/// Puts `new` where `target` is. Either it's all done or `target` is as it was. Windows won't
/// replace a running program but lets it be renamed, so there the old one is moved aside first and
/// deleted at the next start.
pub fn swap(new: &Path, target: &Path) -> std::result::Result<(), String> {
    make_runnable(new)?;
    if cfg!(windows) {
        let old = aside(target);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(target, &old).map_err(|e| io(target, e))?;
        if let Err(e) = std::fs::rename(new, target) {
            let _ = std::fs::rename(&old, target);
            return Err(io(new, e));
        }
        Ok(())
    } else {
        std::fs::rename(new, target).map_err(|e| io(new, e))
    }
}

/// The system's own `tar`, which on Windows reads zip files too. By its full path there: one
/// found through `PATH` could be any program of that name.
fn tar() -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let mut c = std::process::Command::new(root.join("System32").join("tar.exe"));
        c.creation_flags(0x0800_0000);
        c
    }
    #[cfg(not(windows))]
    std::process::Command::new("tar")
}

/// Unpacks a release's zip or tar.gz into `dir` and finds the program in it.
fn unpack(archive: &Path, dir: &Path, program: &std::ffi::OsStr) -> std::result::Result<PathBuf, String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let out = tar().arg("-xf").arg(archive).arg("-C").arg(dir).stdin(std::process::Stdio::null()).output().map_err(|e| format!("Couldn't run tar to unpack the update: {e}"))?;
    if !out.status.success() {
        return Err(format!("Couldn't unpack the update: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    // The archives hold one folder with the program in it.
    let mut places = vec![dir.to_path_buf()];
    places.extend(std::fs::read_dir(dir).map_err(|e| io(dir, e))?.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    places.iter().map(|p| p.join(program)).find(|p| p.is_file()).ok_or_else(|| format!("The update had no {} in it.", program.to_string_lossy()))
}

/// Runs the Windows installer over this copy, without its pages. The running program is moved
/// aside first so the installer can write the new one, and put back if it fails.
fn run_installer(setup: &Path, exe: &Path) -> std::result::Result<(), String> {
    let old = aside(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| io(exe, e))?;
    let ran = std::process::Command::new(setup).args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"]).stdin(std::process::Stdio::null()).status();
    let failed = match ran {
        Ok(s) if s.success() && exe.is_file() => return Ok(()),
        Ok(s) => format!("The installer didn't finish ({s})."),
        Err(e) => format!("Couldn't start the installer: {e}"),
    };
    if !exe.exists() {
        let _ = std::fs::rename(&old, exe);
    }
    Err(failed)
}

/// Installs the checked download `file` over the copy at `exe`. On failure that copy still starts.
pub fn install(kind: &Install, file: &Path, exe: &Path) -> std::result::Result<(), String> {
    match kind {
        Install::Managed(_) => Err("This copy isn't gitem's to replace.".to_string()),
        Install::AppImage(image) => swap(file, image),
        Install::Installer => run_installer(file, exe),
        Install::Program => {
            let program = exe.file_name().ok_or("The running program has no name.")?;
            let new = unpack(file, &staging_dir(exe).join("unpacked"), program)?;
            swap(&new, exe)
        }
    }
}

/// The whole of an update, once asked for: download, check, install. Leaves nothing behind but
/// the old program on Windows, which the next start removes.
pub fn download_and_install(release: &Release, kind: &Install, exe: &Path, from: &str, current: &str, progress: &Progress) -> std::result::Result<(), String> {
    let asset = asset_for(release, kind)?;
    let target = match kind {
        Install::AppImage(image) => image.as_path(),
        _ => exe,
    };
    let stage = staging_dir(target);
    let done = fetch(release, asset, from, &stage, current, progress).and_then(|file| {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err("Stopped.".to_string());
        }
        progress.installing.store(true, Ordering::Relaxed);
        install(kind, &file, exe)
    });
    let _ = std::fs::remove_dir_all(&stage);
    done
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
        assert_eq!(read_release(&r, "0.5.2").unwrap().unwrap(), Release { version: "0.6.0".into(), page: "https://example.test/r".into(), notes: "Fixes".into(), assets: vec![] });
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
    fn a_release_lists_its_files_and_what_github_says_they_hash_to() {
        let hash = "ab".repeat(32);
        let r = json!({"tag_name":"v0.6.0","html_url":"https://example.test/r","assets":[
            {"name":"gitem-0.6.0-windows-x64.zip","browser_download_url":"https://example.test/a.zip","size":7,"digest":format!("sha256:{}", hash.to_uppercase())},
            {"name":"SHA256SUMS","browser_download_url":"https://example.test/sums","digest":"md5:abc"},
            {"name":"../escape.exe","browser_download_url":"https://example.test/x"},
            {"name":"no-address"},
        ]});
        let got = read_release(&r, "0.5.2").unwrap().unwrap().assets;
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0], Asset { name: "gitem-0.6.0-windows-x64.zip".into(), url: "https://example.test/a.zip".into(), size: 7, sha256: Some(hash) });
        assert_eq!(got[1].sha256, None, "not a SHA-256");
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

    #[test]
    fn each_kind_of_copy_takes_its_own_file() {
        let name = |k: &Install, os, arch| asset_name("1.2.3", k, os, arch);
        assert_eq!(name(&Install::Installer, "windows", "x86_64").as_deref(), Some("gitem-1.2.3-windows-x64-setup.exe"));
        assert_eq!(name(&Install::Installer, "windows", "aarch64"), None, "no installer is built for Arm");
        assert_eq!(name(&Install::Program, "windows", "aarch64").as_deref(), Some("gitem-1.2.3-windows-arm64.zip"));
        assert_eq!(name(&Install::Program, "linux", "x86_64").as_deref(), Some("gitem-1.2.3-linux-x64.tar.gz"));
        assert_eq!(name(&Install::Program, "macos", "aarch64").as_deref(), Some("gitem-1.2.3-macos-universal.tar.gz"));
        assert_eq!(name(&Install::AppImage("x".into()), "linux", "aarch64").as_deref(), Some("gitem-1.2.3-linux-arm64.AppImage"));
        assert_eq!(name(&Install::Program, "freebsd", "x86_64"), None);
        assert_eq!(name(&Install::Program, "linux", "riscv64"), None);
    }

    #[test]
    fn a_copy_something_else_looks_after_is_left_to_it() {
        let d = tempfile::tempdir().unwrap();
        for (folder, how) in [("scoop/apps/gitem/current", "scoop update gitem"), ("Microsoft/WinGet/Packages/ulaas.gitem", "winget upgrade ulaas.gitem"), ("Caskroom/gitem/1.0", "brew upgrade --cask gitem")] {
            let dir = d.path().join(folder);
            std::fs::create_dir_all(&dir).unwrap();
            let kind = install_kind(&dir.join("gitem.exe"), None);
            assert_eq!(kind, Install::Managed(Some(how)), "{folder}");
            let release = Release { version: "9.0.0".into(), page: String::new(), notes: String::new(), assets: vec![] };
            assert!(asset_for(&release, &kind).unwrap_err().contains(how));
            assert!(install(&kind, &dir.join("x"), &dir.join("gitem.exe")).is_err());
        }
        assert_eq!(install_kind(&d.path().join("missing").join("gitem"), None), Install::Managed(None), "a folder that can't be written to");
    }

    #[test]
    fn a_plain_copy_an_installed_one_and_an_appimage_are_told_apart() {
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join("gitem.exe");
        std::fs::write(&exe, "x").unwrap();
        assert_eq!(install_kind(&exe, None), Install::Program);
        assert!(std::fs::read_dir(d.path()).unwrap().count() == 1, "the test for writing leaves nothing");
        std::fs::write(d.path().join("unins000.exe"), "x").unwrap();
        assert_eq!(install_kind(&exe, None), if cfg!(windows) { Install::Installer } else { Install::Program });
        let image = d.path().join("gitem.AppImage");
        std::fs::write(&image, "x").unwrap();
        assert_eq!(install_kind(Path::new("/tmp/.mount_gitem/usr/bin/gitem"), Some(&image)), Install::AppImage(image));
        assert_eq!(install_kind(&exe, Some(&d.path().join("gone.AppImage"))), Install::Managed(None));
    }

    #[test]
    fn the_checksum_has_to_be_there_and_every_source_has_to_agree() {
        let (a, b) = ("0a".repeat(32), "0b".repeat(32));
        let asset = |sha: Option<&String>| Asset { name: "gitem.zip".into(), url: String::new(), size: 0, sha256: sha.cloned() };
        let sums = format!("{b}  other.zip\n{}  *gitem.zip\n", a.to_uppercase());
        assert_eq!(sum_for(&sums, "gitem.zip").as_deref(), Some(a.as_str()));
        assert_eq!(sum_for(&sums, "missing.zip"), None);
        assert_eq!(sum_for("short  gitem.zip", "gitem.zip"), None);
        assert_eq!(expected_sha256(&asset(None), Some(&sums)).unwrap(), a);
        assert_eq!(expected_sha256(&asset(Some(&a)), None).unwrap(), a);
        assert_eq!(expected_sha256(&asset(Some(&a)), Some(&sums)).unwrap(), a);
        assert!(expected_sha256(&asset(Some(&b)), Some(&sums)).unwrap_err().contains("disagree"));
        assert!(expected_sha256(&asset(None), None).unwrap_err().contains("no checksum"));
        assert!(expected_sha256(&asset(None), Some("")).is_err());
    }

    #[test]
    fn nothing_is_fetched_from_anywhere_but_the_releases() {
        let d = tempfile::tempdir().unwrap();
        let asset = Asset { name: "gitem.zip".into(), url: "https://example.test/gitem.zip".into(), size: 1, sha256: Some("0a".repeat(32)) };
        let release = Release { version: "9.0.0".into(), page: String::new(), notes: String::new(), assets: vec![asset.clone()] };
        let e = fetch(&release, &asset, DOWNLOAD_PREFIX, d.path(), "0.1.0", &Progress::default()).unwrap_err();
        assert!(e.contains("isn't at an address"), "{e}");
        let plain = Asset { url: "http://github.com/ulaas/gitem/releases/download/x".into(), ..asset };
        assert!(fetch(&release, &plain, "http://github.com/", d.path(), "0.1.0", &Progress::default()).is_err(), "not encrypted");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_swap_replaces_the_program_or_leaves_it_as_it_was() {
        let d = tempfile::tempdir().unwrap();
        let (exe, new) = (d.path().join("gitem.exe"), d.path().join("new.exe"));
        std::fs::write(&exe, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        swap(&new, &exe).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert!(!new.exists());
        if cfg!(windows) {
            assert_eq!(std::fs::read_to_string(aside(&exe)).unwrap(), "old", "kept until the next start");
        }
        // Nothing to put there: the program is still the one that was.
        assert!(swap(&d.path().join("missing"), &exe).is_err());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
    }

    /// The real thing, against GitHub: `cargo test -p gitem-core live_release -- --ignored`. It
    /// replaces a stand-in program in a scratch folder with the latest release's.
    #[test]
    #[ignore = "asks GitHub and downloads a release"]
    fn live_release_replaces_a_stand_in_program() {
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join(if cfg!(windows) { "gitem.exe" } else { "gitem" });
        std::fs::write(&exe, "stand-in").unwrap();
        let release = check(DEFAULT_RELEASE_URL, "0.0.1").unwrap().expect("a release newer than 0.0.1");
        let kind = install_kind(&exe, None);
        assert_eq!(kind, Install::Program);
        let progress = Progress::default();
        download_and_install(&release, &kind, &exe, DOWNLOAD_PREFIX, "0.0.1", &progress).unwrap();
        let size = std::fs::metadata(&exe).unwrap().len();
        assert!(size > 1024 * 1024, "{size} bytes isn't a program");
        assert_eq!(progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed));
        let said = std::process::Command::new(&exe).arg("--version").output().unwrap();
        assert!(String::from_utf8_lossy(&said.stdout).contains(&release.version), "{said:?}");
        assert!(!staging_dir(&exe).exists());
    }

    #[test]
    fn what_an_update_left_is_taken_away() {
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join("gitem.exe");
        std::fs::write(&exe, "new").unwrap();
        std::fs::write(aside(&exe), "old").unwrap();
        std::fs::create_dir_all(staging_dir(&exe).join("unpacked")).unwrap();
        clean_up_after_update(&exe);
        for _ in 0..100 {
            if !aside(&exe).exists() && !staging_dir(&exe).exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!aside(&exe).exists() && !staging_dir(&exe).exists());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
    }
}
