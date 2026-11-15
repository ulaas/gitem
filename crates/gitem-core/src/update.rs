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
    /// gitem.app, signed with a Developer ID, in a folder this user can write to. It's replaced
    /// as a whole by the app in the release's disk image, once that's signed by the same team
    /// (`team`) and macOS takes it as notarized. Never only its program: that would break the
    /// app's signature, and the program in the release's tar.gz isn't the app's.
    AppBundle { app: PathBuf, team: String },
    /// gitem.app where it isn't replaced in place, and why, with what to do instead.
    AppBundleByHand(String),
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
    if cfg!(target_os = "macos") && path.contains(".app/contents/macos/") {
        return match exe.ancestors().nth(3) {
            Some(app) if app.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) => {
                let folder = app.parent().map_or(Folder::NotWritable, folder_state);
                // Only asked of codesign when the answer could matter.
                let team = if folder == Folder::Writable && !is_translocated(app) { team_of(app) } else { None };
                app_bundle_kind(app, folder, team)
            }
            _ => Install::AppBundleByHand(APP_BUNDLE.to_string()),
        };
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

/// What to do when gitem.app isn't updated in place.
pub const APP_BUNDLE: &str = "Download the release's gitem-<version>-macos-arm64.dmg and drag gitem to Applications again.";

/// Whether a folder can be written to, and if not, whether that's because its disk can't be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    Writable,
    /// A disk image, or another disk mounted read-only.
    ReadOnlyVolume,
    NotWritable,
}

fn folder_state(dir: &Path) -> Folder {
    let probe = dir.join(format!(".gitem-write-test-{}", std::process::id()));
    let made = std::fs::File::create(&probe);
    let _ = std::fs::remove_file(&probe);
    match made {
        Ok(_) => Folder::Writable,
        Err(e) if e.kind() == std::io::ErrorKind::ReadOnlyFilesystem => Folder::ReadOnlyVolume,
        Err(_) => Folder::NotWritable,
    }
}

/// Run straight from Downloads with the quarantine on it, macOS starts the app from a
/// read-only copy somewhere else, and replacing that would change nothing.
fn is_translocated(app: &Path) -> bool {
    app.to_string_lossy().to_lowercase().contains("/apptranslocation/")
}

/// How gitem.app at `app` is updated, given its folder and the team its signature names: in
/// place, or by hand and why.
pub fn app_bundle_kind(app: &Path, folder: Folder, team: Option<String>) -> Install {
    let by_hand = |why: String| Install::AppBundleByHand(format!("{why} {APP_BUNDLE}"));
    if is_translocated(app) {
        return Install::AppBundleByHand("macOS is running gitem.app from where it was downloaded, so it can't update itself. Move gitem to Applications and open it from there.".to_string());
    }
    let dir = app.parent().unwrap_or(Path::new("/"));
    match (folder, team) {
        (Folder::ReadOnlyVolume, _) => Install::AppBundleByHand("gitem.app is running from its disk image or another read-only disk. Drag gitem to Applications and open it from there.".to_string()),
        (Folder::NotWritable, _) => by_hand(format!("This user can't write in {}, and gitem doesn't ask for an administrator's password.", dir.display())),
        (Folder::Writable, None) => by_hand("This gitem.app isn't signed with a Developer ID, so gitem can't tell that an update comes from the same developer.".to_string()),
        (Folder::Writable, Some(team)) => Install::AppBundle { app: app.to_path_buf(), team },
    }
}

/// The team a Developer ID signature names, from what `codesign -dv` says. Nothing for an ad hoc
/// signature, or for none: `TeamIdentifier=not set`, or no such line.
pub fn team_in(codesign_says: &str) -> Option<String> {
    let team = codesign_says.lines().find_map(|l| l.trim().strip_prefix("TeamIdentifier="))?.trim();
    (!team.is_empty() && team.len() <= 20 && team.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())).then(|| team.to_string())
}

/// The team that signed the app at `app`, if it was signed with a Developer ID.
fn team_of(app: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/codesign").arg("-dv").arg("--verbose=2").arg(app).stdin(std::process::Stdio::null()).output().ok()?;
        out.status.success().then(|| team_in(&String::from_utf8_lossy(&out.stderr))).flatten()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        None
    }
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
        (Install::AppBundle { .. }, "macos") if arch == "arm64" => format!("gitem-{version}-macos-arm64.dmg"),
        (Install::AppImage(_), "linux") => format!("gitem-{version}-linux-{arch}.AppImage"),
        _ => return None,
    })
}

/// The file of `release` that updates this copy, or why there's none to install.
pub fn asset_for<'a>(release: &'a Release, kind: &Install) -> std::result::Result<&'a Asset, String> {
    if let Install::AppBundleByHand(why) = kind {
        return Err(why.clone());
    }
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
    #[cfg(target_os = "macos")]
    if let Some(app) = exe.ancestors().nth(3).filter(|a| a.extension().is_some_and(|e| e == "app")) {
        let app = app.to_path_buf();
        std::thread::spawn(move || mac::clean_up_leftovers(&app));
        return;
    }
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

/// Installs the checked download `file`, of release `version`, over the copy at `exe`. On
/// failure that copy still starts.
pub fn install(kind: &Install, file: &Path, exe: &Path, version: &str) -> std::result::Result<(), String> {
    match kind {
        Install::Managed(_) | Install::AppBundleByHand(_) => Err("This copy isn't gitem's to replace.".to_string()),
        #[cfg(target_os = "macos")]
        Install::AppBundle { app, team } => mac::install_app(file, app, team, version),
        #[cfg(not(target_os = "macos"))]
        Install::AppBundle { .. } => {
            let _ = version;
            Err("This copy isn't gitem's to replace.".to_string())
        }
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
    let stage = match kind {
        Install::AppImage(image) => staging_dir(image),
        // Nothing is written inside an app: that would break its signature.
        Install::AppBundle { .. } => std::env::temp_dir().join(format!("gitem-update-{}", std::process::id())),
        _ => staging_dir(exe),
    };
    let done = fetch(release, asset, from, &stage, current, progress).and_then(|file| {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err("Stopped.".to_string());
        }
        progress.installing.store(true, Ordering::Relaxed);
        install(kind, &file, exe, &release.version)
    });
    let _ = std::fs::remove_dir_all(&stage);
    done
}

/// What an update starts once this process has ended and everything's saved: the program it
/// installed, or on macOS the app, through `open` so it starts as any app does. The app's started
/// once this process is gone, by a small shell that waits for it, without a terminal.
pub fn start_after_exit(start: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    if start.extension().is_some_and(|e| e == "app") {
        use std::os::unix::process::CommandExt;
        let mut c = std::process::Command::new("/bin/sh");
        c.arg("-c").arg(mac::RELAUNCH).arg("sh").arg(std::process::id().to_string()).arg(start);
        // Where this copy keeps its settings, for a copy started with its own.
        if let Some(dir) = crate::settings::config_dir_override() {
            let mut env = std::ffi::OsString::from("GITEM_CONFIG_DIR=");
            env.push(dir);
            c.arg("--env").arg(env);
        }
        let null = std::process::Stdio::null;
        c.stdin(null()).stdout(null()).stderr(null()).process_group(0);
        return c.spawn().map(|_| ());
    }
    std::process::Command::new(start).spawn().map(|_| ())
}

/// gitem.app replaced by the app in a release's disk image, with Apple's own tools, by their
/// full paths: an app started from Finder has little on its `PATH`.
#[cfg(target_os = "macos")]
mod mac {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    /// Waits for the process `$1` to end, then opens the app `$2`, with any options after it.
    pub const RELAUNCH: &str = r#"pid=$1 app=$2; shift 2; n=0; while kill -0 "$pid" 2>/dev/null; do [ $n -ge 3000 ] && exit 0; n=$((n+1)); /bin/sleep 0.2; done; exec /usr/bin/open -n "$@" "$app""#;

    fn run(tool: &str, args: &[&std::ffi::OsStr]) -> Result<String, String> {
        let out = Command::new(tool).args(args).stdin(Stdio::null()).output().map_err(|e| format!("Couldn't run {tool}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let said = String::from_utf8_lossy(&out.stderr);
            let name = Path::new(tool).file_name().unwrap_or_default().to_string_lossy();
            Err(format!("{name} said: {}", said.trim()))
        }
    }

    /// What a signature has to satisfy: Apple's Developer ID chain, ending in a certificate of
    /// `team`. The running app's own team, so a copy built and signed by someone else updates
    /// from their releases and not from gitem's.
    pub fn requirement(team: &str) -> String {
        format!(r#"anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "{team}""#)
    }

    fn unescape(s: &str) -> String {
        s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
    }

    /// Each string that follows `<key>key</key>` in an XML property list.
    pub fn plist_strings(xml: &str, key: &str) -> Vec<String> {
        let tag = format!("<key>{key}</key>");
        xml.match_indices(&tag)
            .filter_map(|(at, _)| {
                let rest = xml[at + tag.len()..].trim_start();
                let rest = rest.strip_prefix("<string>")?;
                Some(unescape(&rest[..rest.find("</string>")?]))
            })
            .collect()
    }

    /// A value from an app's Info.plist, which may be stored in binary.
    fn info(app: &Path, key: &str) -> Result<String, String> {
        let plist = app.join("Contents/Info.plist");
        let xml = run("/usr/bin/plutil", &["-convert".as_ref(), "xml1".as_ref(), "-o".as_ref(), "-".as_ref(), plist.as_os_str()])?;
        plist_strings(&xml, key).into_iter().next().ok_or_else(|| format!("{} doesn't say its {key}.", app.display()))
    }

    /// Whether the app at `new` is the release `version` of the app at `app`.
    pub fn same_app(new: &Path, app: &Path, version: &str) -> Result<(), String> {
        let (id, ours) = (info(new, "CFBundleIdentifier")?, info(app, "CFBundleIdentifier")?);
        if id != ours {
            return Err(format!("The app in the update is {id}, not {ours}."));
        }
        let says = info(new, "CFBundleShortVersionString")?;
        if says != version {
            return Err(format!("The app in the update says it's version {says}, not {version}."));
        }
        Ok(())
    }

    /// Whether the app at `new` is whole, signed by `team` with a Developer ID, and notarized.
    fn trusted(new: &Path, team: &str) -> Result<(), String> {
        let app = new.as_os_str();
        run("/usr/bin/codesign", &["--verify".as_ref(), "--deep".as_ref(), "--strict".as_ref(), app]).map_err(|e| format!("The update's signature doesn't hold: {e}"))?;
        let req = format!("-R={}", requirement(team));
        run("/usr/bin/codesign", &["--verify".as_ref(), req.as_ref(), app]).map_err(|e| format!("The update isn't signed by the team that signed this copy ({team}): {e}"))?;
        run("/usr/sbin/spctl", &["--assess".as_ref(), "--type".as_ref(), "execute".as_ref(), app]).map_err(|e| format!("macOS doesn't accept the update as notarized: {e}"))?;
        Ok(())
    }

    /// A hidden name beside `app`, in the same folder, so moving it is a rename.
    pub fn beside(app: &Path, what: &str) -> PathBuf {
        let name = app.file_name().unwrap_or_default().to_string_lossy();
        app.with_file_name(format!(".{name}.{what}-{}", std::process::id()))
    }

    /// Exchanges what's at `a` and `b` in one step, so neither name is ever missing.
    pub fn exchange(a: &Path, b: &Path) -> std::io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(std::io::Error::other);
        let (a, b) = (c(a)?, c(b)?);
        // SAFETY: both are NUL-terminated paths that live through the call.
        if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }

    /// Puts the app at `staged` where `app` is and takes the old one away. Swapped in one step
    /// where the disk can; elsewhere by two renames, the old app put back if the second fails.
    pub fn swap_app(staged: &Path, app: &Path) -> Result<(), String> {
        swap_app_with(staged, app, exchange)
    }

    pub fn swap_app_with(staged: &Path, app: &Path, exchange: impl FnOnce(&Path, &Path) -> std::io::Result<()>) -> Result<(), String> {
        if exchange(staged, app).is_ok() {
            // The old app is where the copy was.
            let _ = std::fs::remove_dir_all(staged);
            return Ok(());
        }
        swap_by_renames(staged, app)
    }

    fn swap_by_renames(staged: &Path, app: &Path) -> Result<(), String> {
        let old = beside(app, "old");
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(app, &old).map_err(|e| super::io(app, e))?;
        if let Err(e) = std::fs::rename(staged, app) {
            let _ = std::fs::rename(&old, app);
            return Err(super::io(staged, e));
        }
        let _ = std::fs::remove_dir_all(&old);
        Ok(())
    }

    /// Whether process `pid` is still running: only "no such process" says it isn't.
    pub fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 sends nothing; it only asks whether the process is there.
        pid > 0 && (unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH))
    }

    /// Takes away the copies and old apps an update left beside `app` when it was stopped half
    /// way: `.gitem.app.update-<pid>` and `.gitem.app.old-<pid>`, unless that process still runs.
    pub fn clean_up_leftovers(app: &Path) {
        let (Some(dir), Some(name)) = (app.parent(), app.file_name()) else { return };
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let name = name.to_string_lossy();
        for e in entries.flatten() {
            let file = e.file_name();
            let file = file.to_string_lossy();
            let pid = ["update", "old"].iter().find_map(|what| file.strip_prefix(&format!(".{name}.{what}-"))).and_then(|p| p.parse::<i32>().ok());
            if let Some(pid) = pid
                && !alive(pid)
                && e.file_type().is_ok_and(|t| t.is_dir())
            {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    /// Copies `new` beside `app`, has `check` look at the copy, and swaps it in. Whatever fails,
    /// `app` is as it was and the copy is gone.
    pub fn place(new: &Path, app: &Path, check: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
        let staged = beside(app, "update");
        let _ = std::fs::remove_dir_all(&staged);
        let done = run("/usr/bin/ditto", &[new.as_os_str(), staged.as_os_str()]).map_err(|e| format!("Couldn't copy the new app beside this one: {e}")).and_then(|_| check(&staged)).and_then(|_| swap_app(&staged, app));
        if done.is_err() {
            let _ = std::fs::remove_dir_all(&staged);
        }
        done
    }

    /// The one app at the top of a mounted disk image.
    pub fn app_in(mount: &Path) -> Result<PathBuf, String> {
        let apps: Vec<PathBuf> = std::fs::read_dir(mount)
            .map_err(|e| super::io(mount, e))?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "app"))
            .collect();
        match apps.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err("The update's disk image has no app in it.".to_string()),
            _ => Err("The update's disk image has more than one app in it.".to_string()),
        }
    }

    /// Mounts `dmg` read-only, out of sight, somewhere of its own under `dir`.
    fn attach(dmg: &Path, dir: &Path) -> Result<PathBuf, String> {
        let plist = run("/usr/bin/hdiutil", &["attach".as_ref(), "-nobrowse".as_ref(), "-readonly".as_ref(), "-noautoopen".as_ref(), "-plist".as_ref(), "-mountrandom".as_ref(), dir.as_os_str(), dmg.as_os_str()])
            .map_err(|e| format!("Couldn't open the update's disk image: {e}"))?;
        plist_strings(&plist, "mount-point").into_iter().next().map(PathBuf::from).ok_or_else(|| "The update's disk image mounted nowhere.".to_string())
    }

    fn detach(mount: &Path) {
        if run("/usr/bin/hdiutil", &["detach".as_ref(), "-quiet".as_ref(), mount.as_os_str()]).is_err() {
            let _ = run("/usr/bin/hdiutil", &["detach".as_ref(), "-force".as_ref(), "-quiet".as_ref(), mount.as_os_str()]);
        }
    }

    /// Replaces gitem.app at `app` with the one in the checked disk image `dmg`, if that's
    /// release `version` of the same app, signed by `team` and notarized.
    pub fn install_app(dmg: &Path, app: &Path, team: &str, version: &str) -> Result<(), String> {
        let mounts = dmg.with_file_name("mount");
        std::fs::create_dir_all(&mounts).map_err(|e| super::io(&mounts, e))?;
        let done = attach(dmg, &mounts).and_then(|mount| {
            let done = app_in(&mount).and_then(|new| {
                same_app(&new, app, version)?;
                place(&new, app, |staged| trusted(staged, team))
            });
            detach(&mount);
            done
        });
        // Only empty folders by now: a mount that's still there is left alone.
        let _ = std::fs::remove_dir(&mounts);
        done
    }
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
            assert!(install(&kind, &dir.join("x"), &dir.join("gitem.exe"), "9.0.0").is_err());
        }
        assert_eq!(install_kind(&d.path().join("missing").join("gitem"), None), Install::Managed(None), "a folder that can't be written to");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_unsigned_app_bundle_is_left_to_its_disk_image() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("gitem.app/Contents/MacOS");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gitem"), "x").unwrap();
        let kind = install_kind(&dir.join("gitem"), None);
        let Install::AppBundleByHand(why) = &kind else { panic!("{kind:?}") };
        assert!(why.contains("isn't signed with a Developer ID") && why.contains(".dmg"), "{why}");
        let release = Release { version: "9.0.0".into(), page: String::new(), notes: String::new(), assets: vec![] };
        assert_eq!(asset_for(&release, &kind).unwrap_err(), *why);
        assert!(install(&kind, &dir.join("x"), &dir.join("gitem"), "9.0.0").is_err());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "the test for writing leaves nothing");
        // Not the program of an app, if it isn't three folders down in a .app.
        let odd = d.path().join("x.app/Contents/MacOS/sub/gitem");
        assert_eq!(install_kind(&odd, None), Install::AppBundleByHand(APP_BUNDLE.to_string()));
    }

    #[test]
    fn which_app_bundle_is_updated_in_place_and_which_by_hand() {
        let team = || Some("N6FA79GV97".to_string());
        let app = Path::new("/Users/me/Applications/gitem.app");
        assert_eq!(app_bundle_kind(app, Folder::Writable, team()), Install::AppBundle { app: app.to_path_buf(), team: "N6FA79GV97".into() });
        let by_hand = |app: &Path, folder, team| match app_bundle_kind(app, folder, team) {
            Install::AppBundleByHand(why) => why,
            other => panic!("{other:?}"),
        };
        assert!(by_hand(app, Folder::Writable, None).contains("isn't signed with a Developer ID"), "ad hoc or unsigned");
        assert!(by_hand(app, Folder::NotWritable, team()).contains("can't write in /Users/me/Applications"));
        assert!(by_hand(app, Folder::ReadOnlyVolume, team()).contains("read-only disk"));
        let moved = Path::new("/private/var/folders/xy/abc/T/AppTranslocation/1234-ABCD/d/gitem.app");
        assert!(by_hand(moved, Folder::ReadOnlyVolume, team()).contains("Move gitem to Applications"));
        assert!(by_hand(moved, Folder::Writable, team()).contains("Move gitem to Applications"));
        // Homebrew's own, linked from Applications or not, is Homebrew's to update.
        assert_eq!(install_kind(Path::new("/opt/homebrew/Caskroom/gitem/1.0/gitem.app/Contents/MacOS/gitem"), None), Install::Managed(Some("brew upgrade --cask gitem")));
        assert_eq!(install_kind(Path::new("/usr/local/Caskroom/gitem/1.0/gitem.app/Contents/MacOS/gitem"), None), Install::Managed(Some("brew upgrade --cask gitem")));
    }

    #[test]
    fn the_team_comes_from_a_developer_id_signature_only() {
        let says = "Executable=/A/gitem.app/Contents/MacOS/gitem\nIdentifier=dev.gitem.gitem\nAuthority=Developer ID Application: A (N6FA79GV97)\nTeamIdentifier=N6FA79GV97\n";
        assert_eq!(team_in(says).as_deref(), Some("N6FA79GV97"));
        assert_eq!(team_in("Signature=adhoc\nTeamIdentifier=not set\n"), None);
        assert_eq!(team_in("gitem.app: code object is not signed at all\n"), None);
        assert_eq!(team_in("TeamIdentifier=AB\" or anchor apple\n"), None, "nothing that could change the requirement");
    }

    #[test]
    fn an_app_bundle_takes_the_disk_image_on_apple_silicon_only() {
        let kind = Install::AppBundle { app: "/A/gitem.app".into(), team: "T".into() };
        assert_eq!(asset_name("1.2.3", &kind, "macos", "aarch64").as_deref(), Some("gitem-1.2.3-macos-arm64.dmg"));
        assert_eq!(asset_name("1.2.3", &kind, "macos", "x86_64"), None);
        assert_eq!(asset_name("1.2.3", &Install::AppBundleByHand(String::new()), "macos", "aarch64"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_signature_has_to_be_a_developer_ids_of_the_same_team() {
        assert_eq!(
            mac::requirement("N6FA79GV97"),
            r#"anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "N6FA79GV97""#
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_property_list_is_read_for_its_strings() {
        let xml = "<dict>\n\t<key>CFBundleIdentifier</key>\n\t<string>dev.gitem.gitem</string>\n\t<key>system-entities</key><array><dict><key>mount-point</key>\n<string>/private/tmp/x/dmg.a &amp; b</string></dict><dict><key>mount-point</key><string>/b</string></dict></array>\n\t<key>CFBundleVersion</key><integer>3</integer></dict>";
        assert_eq!(mac::plist_strings(xml, "CFBundleIdentifier"), ["dev.gitem.gitem"]);
        assert_eq!(mac::plist_strings(xml, "mount-point"), ["/private/tmp/x/dmg.a & b", "/b"]);
        assert!(mac::plist_strings(xml, "CFBundleVersion").is_empty(), "not a string");
        assert!(mac::plist_strings(xml, "missing").is_empty());
    }

    #[cfg(target_os = "macos")]
    fn fake_app(at: &Path, id: &str, version: &str, program: &str) {
        std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
        std::fs::write(at.join("Contents/MacOS/gitem"), program).unwrap();
        let plist = format!(r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{id}</string><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"#);
        std::fs::write(at.join("Contents/Info.plist"), plist).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_app_in_the_image_has_to_be_this_app_at_the_releases_version() {
        let d = tempfile::tempdir().unwrap();
        let (app, new) = (d.path().join("gitem.app"), d.path().join("image/gitem.app"));
        fake_app(&app, "dev.gitem.gitem", "1.0.0", "old");
        fake_app(&new, "dev.gitem.gitem", "2.0.0", "new");
        mac::same_app(&new, &app, "2.0.0").unwrap();
        assert!(mac::same_app(&new, &app, "2.0.1").unwrap_err().contains("version 2.0.0, not 2.0.1"));
        fake_app(&new, "dev.other.app", "2.0.0", "new");
        assert!(mac::same_app(&new, &app, "2.0.0").unwrap_err().contains("dev.other.app"));
        assert_eq!(mac::app_in(&d.path().join("image")).unwrap(), new);
        std::os::unix::fs::symlink("/Applications", d.path().join("image/Applications")).unwrap();
        assert_eq!(mac::app_in(&d.path().join("image")).unwrap(), new, "the link to Applications isn't an app");
        fake_app(&d.path().join("image/second.app"), "x", "1", "x");
        assert!(mac::app_in(&d.path().join("image")).unwrap_err().contains("more than one"));
        assert!(mac::app_in(&d.path().join("gitem.app/Contents/MacOS")).unwrap_err().contains("no app"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_app_is_swapped_whole_or_left_as_it_was() {
        let d = tempfile::tempdir().unwrap();
        let (app, new) = (d.path().join("Apps/gitem.app"), d.path().join("image/gitem.app"));
        fake_app(&app, "dev.gitem.gitem", "1.0.0", "old");
        fake_app(&new, "dev.gitem.gitem", "2.0.0", "new");
        let program = app.join("Contents/MacOS/gitem");
        let beside = || std::fs::read_dir(app.parent().unwrap()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect::<Vec<_>>();

        // A check that fails: nothing changes, and the copy is gone.
        let mut seen = None;
        let e = mac::place(&new, &app, |staged| {
            assert_eq!(staged.parent(), app.parent(), "staged on the same disk, beside it");
            assert!(staged.file_name().unwrap().to_string_lossy().starts_with(".gitem.app.update-"));
            assert_eq!(std::fs::read_to_string(staged.join("Contents/MacOS/gitem")).unwrap(), "new");
            seen = Some(staged.to_path_buf());
            Err("not signed".to_string())
        })
        .unwrap_err();
        assert_eq!(e, "not signed");
        assert!(!seen.unwrap().exists());
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "old");
        assert_eq!(beside(), ["gitem.app"]);

        // Nothing to copy: the same.
        assert!(mac::place(&d.path().join("missing.app"), &app, |_| Ok(())).is_err());
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "old");
        assert_eq!(beside(), ["gitem.app"]);

        // The second rename failing puts the old app back.
        let refuse = |_: &Path, _: &Path| Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
        assert!(mac::swap_app_with(&d.path().join("gone.app"), &app, refuse).is_err());
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "old");
        assert_eq!(beside(), ["gitem.app"]);

        mac::place(&new, &app, |_| Ok(())).unwrap();
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "new");
        assert_eq!(beside(), ["gitem.app"], "neither the copy nor the old app is left");
        assert!(new.exists(), "the image is only read");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_swap_is_one_step_and_two_renames_where_it_cant_be() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("gitem.app");
        let program = app.join("Contents/MacOS/gitem");
        let beside = || {
            let mut v: Vec<String> = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        fake_app(&app, "dev.gitem.gitem", "1.0.0", "old");
        let staged = mac::beside(&app, "update");
        fake_app(&staged, "dev.gitem.gitem", "2.0.0", "new");
        mac::exchange(&staged, &app).unwrap();
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(staged.join("Contents/MacOS/gitem")).unwrap(), "old", "swapped, not moved");
        std::fs::remove_dir_all(&staged).unwrap();

        // Through swap_app: the old app is gone after.
        fake_app(&staged, "dev.gitem.gitem", "3.0.0", "newer");
        mac::swap_app(&staged, &app).unwrap();
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "newer");
        assert_eq!(beside(), ["gitem.app"]);

        // A disk that can't swap: two renames, with the same end.
        fake_app(&staged, "dev.gitem.gitem", "4.0.0", "newest");
        let refuse = |_: &Path, _: &Path| Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
        mac::swap_app_with(&staged, &app, refuse).unwrap();
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "newest");
        assert_eq!(beside(), ["gitem.app"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn what_a_stopped_update_left_goes_unless_its_process_still_runs() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("gitem.app");
        fake_app(&app, "dev.gitem.gitem", "1.0.0", "x");
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let dead = child.id() as i32;
        child.wait().unwrap();
        assert!(!mac::alive(dead) && !mac::alive(i32::MAX) && mac::alive(std::process::id() as i32));
        let me = std::process::id();
        for name in [format!(".gitem.app.update-{dead}"), format!(".gitem.app.old-{}", i32::MAX), format!(".gitem.app.update-{me}"), format!(".gitem.app.old-{me}"), ".other.app.old-1".to_string(), ".gitem.app.old-x".to_string()] {
            std::fs::create_dir_all(d.path().join(name)).unwrap();
        }
        mac::clean_up_leftovers(&app);
        let mut left: Vec<String> = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        let mut want = vec![".gitem.app.old-x".to_string(), format!(".gitem.app.old-{me}"), format!(".gitem.app.update-{me}"), ".other.app.old-1".to_string(), "gitem.app".to_string()];
        want.sort();
        assert_eq!(left, want, "a live process's, and what isn't an update's, stay");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_disk_image_that_isnt_one_installs_nothing() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("gitem.app");
        fake_app(&app, "dev.gitem.gitem", "1.0.0", "old");
        let dmg = d.path().join("dl/gitem-2.0.0-macos-arm64.dmg");
        std::fs::create_dir_all(dmg.parent().unwrap()).unwrap();
        std::fs::write(&dmg, "not a disk image").unwrap();
        let kind = Install::AppBundle { app: app.clone(), team: "N6FA79GV97".into() };
        let e = install(&kind, &dmg, &app.join("Contents/MacOS/gitem"), "2.0.0").unwrap_err();
        assert!(e.contains("disk image"), "{e}");
        assert_eq!(std::fs::read_to_string(app.join("Contents/MacOS/gitem")).unwrap(), "old");
        assert!(!d.path().join("dl/mount").exists());
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
