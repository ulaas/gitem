//! Cuts a test binary off from the machine it runs on, before any test starts.
//!
//! Put `gitem_testenv::isolate!();` at the top of each integration test file, and
//! under `#[cfg(test)]` in each library root. Results then don't depend on the
//! developer's git config, and tests can't reach their settings, their stored
//! secrets or their home folder. Child processes inherit all of it, so the same
//! goes for git and for the gitem binary when a test runs them.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Everything a test needs from global git config. Without `pull.rebase`, git
/// refuses to pull a diverged branch.
const GITCONFIG: &str = "[pull]\n\trebase = false\n";

static SCRATCH: OnceLock<PathBuf> = OnceLock::new();

/// Runs `setup` when the process starts. It has to be at process start: edition
/// 2024 rightly calls `set_var` unsafe once other threads exist, and a test's own
/// fixture would be too late for any test that forgot to call it.
///
/// Expands in each test binary because the linker would drop it from this crate,
/// which nothing else in the binary refers to.
#[macro_export]
macro_rules! isolate {
    () => {
        #[used]
        #[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__mod_init_func"))]
        #[cfg_attr(not(any(windows, target_os = "macos")), unsafe(link_section = ".init_array"))]
        static GITEM_TESTENV_ISOLATE: extern "C" fn() = {
            extern "C" fn isolate() {
                $crate::setup();
            }
            isolate
        };
    };
}

/// Variables git or gitem would obey. A hook or an outer test run can leave these set.
fn inherited(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.starts_with("GIT_") || name.starts_with("GITEM_") || name.starts_with("GCM_") || name.starts_with("SSH_ASKPASS") || name == "COPILOT_HOME"
}

/// What the process's environment becomes, given its scratch folder.
fn scratch_env(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    let home = dir.join("home");
    vec![
        ("GIT_CONFIG_GLOBAL", dir.join("gitconfig")),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_TERMINAL_PROMPT", "0".into()),
        ("GCM_INTERACTIVE", "never".into()),
        ("GITEM_CONFIG_DIR", dir.join("config")),
        ("HOME", home.clone()),
        ("USERPROFILE", home.clone()),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local/share")),
    ]
}

/// Not for calling directly: `isolate!` runs this. A failure stops the process,
/// since running on without isolation could touch the real profile.
pub fn setup() {
    let dir = std::env::temp_dir().join(format!("gitem-test-{}", std::process::id()));
    // Only there if a killed run had this process id.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).expect("create the test config folder");
    std::fs::create_dir_all(dir.join("home/.config")).expect("create the test home folder");
    std::fs::write(dir.join("gitconfig"), GITCONFIG).expect("write the test git config");
    let inherited: Vec<_> = std::env::vars_os().map(|(k, _)| k).filter(|k| inherited(&k.to_string_lossy())).collect();
    // SAFETY: this runs before main, so no other thread can be reading the environment.
    unsafe {
        for name in inherited {
            std::env::remove_var(name);
        }
        for (name, value) in scratch_env(&dir) {
            std::env::set_var(name, value);
        }
    }
    let _ = SCRATCH.set(dir);
    // SAFETY: `remove_scratch` is a plain function that stays valid until exit.
    unsafe {
        atexit(remove_scratch);
    }
}

unsafe extern "C" {
    fn atexit(callback: extern "C" fn()) -> std::ffi::c_int;
}

extern "C" fn remove_scratch() {
    if let Some(dir) = SCRATCH.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
crate::isolate!();

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    fn workspace() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn rust_files_in(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "rs")).collect()
    }

    /// A new test file that forgot the line would quietly run against the real profile.
    #[test]
    fn every_test_binary_is_isolated() {
        let crates = workspace().join("crates");
        let mut binaries = vec![crates.join("gitem/src/lib.rs"), crates.join("gitem-core/src/lib.rs")];
        binaries.extend(rust_files_in(&crates.join("gitem/tests")));
        binaries.extend(rust_files_in(&crates.join("gitem-core/tests")));
        assert!(binaries.len() >= 10, "{binaries:?}");
        for b in binaries {
            let text = std::fs::read_to_string(&b).unwrap();
            assert!(text.contains("gitem_testenv::isolate!();"), "{} doesn't call gitem_testenv::isolate!()", b.display());
        }
    }

    /// `main.rs` has no tests. If it gets some, it needs the line too.
    #[test]
    fn the_binary_has_no_tests_of_its_own() {
        let text = std::fs::read_to_string(workspace().join("crates/gitem/src/main.rs")).unwrap();
        assert!(!text.contains("#[cfg(test)]") && !text.contains("#[test]"));
    }

    #[test]
    fn the_environment_points_into_the_scratch_folder() {
        let scratch = std::env::temp_dir().join(format!("gitem-test-{}", std::process::id()));
        for name in ["GIT_CONFIG_GLOBAL", "GITEM_CONFIG_DIR", "HOME", "USERPROFILE", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
            let value = PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} isn't set")));
            assert!(value.starts_with(&scratch), "{name} is {}", value.display());
        }
        assert_eq!(std::env::var("GIT_CONFIG_NOSYSTEM").unwrap(), "1");
        assert!(scratch.join("gitconfig").exists());
    }
}
