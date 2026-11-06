//! Finding text editors on this machine, for the External editor setting. Looks only
//! at whether files exist: nothing is run.
use std::path::{Path, PathBuf};

/// An editor found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub name: &'static str,
    pub path: PathBuf,
}

struct Known {
    name: &'static str,
    /// Names on PATH, without an extension.
    commands: &'static [&'static str],
    /// Relative to %LOCALAPPDATA% and the Program Files folders.
    windows: &'static [&'static str],
    /// A command-line launcher inside a macOS app, which takes a file argument.
    mac: &'static [&'static str],
    /// A JetBrains-style `<folder>/bin/<exe>`, in folders named with a version.
    versioned: Option<(&'static str, &'static str)>,
}

const fn editor(name: &'static str, commands: &'static [&'static str], windows: &'static [&'static str], mac: &'static [&'static str]) -> Known {
    Known { name, commands, windows, mac, versioned: None }
}

const fn jetbrains(name: &'static str, command: &'static [&'static str], exe: &'static str) -> Known {
    Known { name, commands: command, windows: &[], mac: &[], versioned: Some((name, exe)) }
}

/// Only editors with a window: one started from gitem has no terminal to run in.
const KNOWN: &[Known] = &[
    editor("Visual Studio Code", &["code"], &["Programs/Microsoft VS Code/Code.exe", "Microsoft VS Code/Code.exe"], &["/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"]),
    editor("Visual Studio Code Insiders", &["code-insiders"], &["Programs/Microsoft VS Code Insiders/Code - Insiders.exe", "Microsoft VS Code Insiders/Code - Insiders.exe"], &["/Applications/Visual Studio Code - Insiders.app/Contents/Resources/app/bin/code"]),
    editor("VSCodium", &["codium"], &["Programs/VSCodium/VSCodium.exe", "VSCodium/VSCodium.exe"], &["/Applications/VSCodium.app/Contents/Resources/app/bin/codium"]),
    editor("Cursor", &["cursor"], &["Programs/cursor/Cursor.exe"], &["/Applications/Cursor.app/Contents/Resources/app/bin/cursor"]),
    editor("Windsurf", &["windsurf"], &["Programs/Windsurf/Windsurf.exe"], &["/Applications/Windsurf.app/Contents/Resources/app/bin/windsurf"]),
    editor("Zed", &["zed"], &["Programs/Zed/Zed.exe"], &["/Applications/Zed.app/Contents/MacOS/cli"]),
    editor("Sublime Text", &["subl"], &["Sublime Text/sublime_text.exe", "Sublime Text 3/sublime_text.exe"], &["/Applications/Sublime Text.app/Contents/SharedSupport/bin/subl"]),
    editor("Notepad++", &["notepad++"], &["Notepad++/notepad++.exe"], &[]),
    editor("Visual Studio 2022", &["devenv"], &["Microsoft Visual Studio/2022/Community/Common7/IDE/devenv.exe", "Microsoft Visual Studio/2022/Professional/Common7/IDE/devenv.exe", "Microsoft Visual Studio/2022/Enterprise/Common7/IDE/devenv.exe"], &[]),
    jetbrains("IntelliJ IDEA", &["idea"], "idea64.exe"),
    jetbrains("PyCharm", &["pycharm"], "pycharm64.exe"),
    jetbrains("WebStorm", &["webstorm"], "webstorm64.exe"),
    jetbrains("CLion", &["clion"], "clion64.exe"),
    jetbrains("Rider", &["rider"], "rider64.exe"),
    jetbrains("GoLand", &["goland"], "goland64.exe"),
    jetbrains("RustRover", &["rustrover"], "rustrover64.exe"),
    jetbrains("PhpStorm", &["phpstorm"], "phpstorm64.exe"),
    editor("Neovim Qt", &["nvim-qt"], &["Neovim/bin/nvim-qt.exe"], &[]),
    editor("Kate", &["kate"], &["Kate/bin/kate.exe"], &[]),
    editor("gedit", &["gedit"], &[], &[]),
    editor("GNOME Text Editor", &["gnome-text-editor"], &[], &[]),
    editor("Notepad", &["notepad"], &[], &[]),
];

/// Extensions to try on PATH, best first. A real program beats a launcher script.
fn path_extensions() -> &'static [&'static str] {
    if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] }
}

fn on_path(command: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    path_extensions().iter().find_map(|ext| dirs.iter().map(|d| d.join(format!("{command}{ext}"))).find(|p| p.is_file()))
}

/// The newest `<root>/<name>*/bin/<exe>`, going by folder name.
fn versioned(roots: &[PathBuf], name: &str, exe: &str) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for root in roots {
        for base in [root.clone(), root.join("Programs"), root.join("JetBrains")] {
            let Ok(entries) = std::fs::read_dir(&base) else { continue };
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with(name) {
                    let candidate = e.path().join("bin").join(exe);
                    if candidate.is_file() {
                        found.push(candidate);
                    }
                }
            }
        }
    }
    found.sort();
    found.pop()
}

/// Editors found under `roots` (install folders) and `path` (PATH's folders), in a
/// fixed order, each once. Install folders win, since they hold the real program.
pub fn detect_in(roots: &[PathBuf], path: &[PathBuf], apps: bool) -> Vec<Editor> {
    let mut out = Vec::new();
    for k in KNOWN {
        let installed = k.windows.iter().find_map(|rel| {
            // The table uses forward slashes; this path gets shown in Settings.
            let rel = rel.replace('/', std::path::MAIN_SEPARATOR_STR);
            roots.iter().map(|r| r.join(&rel)).find(|p| p.is_file())
        });
        let mac = if apps { k.mac.iter().map(PathBuf::from).find(|p| p.is_file()) } else { None };
        let jb = k.versioned.and_then(|(name, exe)| versioned(roots, name, exe));
        let found = installed.or(mac).or(jb).or_else(|| k.commands.iter().find_map(|c| on_path(c, path)));
        if let Some(path) = found {
            out.push(Editor { name: k.name, path });
        }
    }
    out
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default()
}

/// Editors on this machine.
pub fn detect() -> Vec<Editor> {
    let roots: Vec<PathBuf> = ["LOCALAPPDATA", "ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
    detect_in(&roots, &path_dirs(), cfg!(target_os = "macos"))
}

/// What to launch for a setting that may be a full path or just a command name,
/// such as `code`, which on Windows is really `code.cmd`.
pub fn resolve(editor: &str) -> PathBuf {
    resolve_in(editor, &path_dirs())
}

pub fn resolve_in(editor: &str, dirs: &[PathBuf]) -> PathBuf {
    let editor = editor.trim();
    let given = Path::new(editor);
    if given.components().count() > 1 || given.is_file() {
        return given.to_path_buf();
    }
    on_path(editor, dirs).unwrap_or_else(|| given.to_path_buf())
}

/// Whether `resolved` can be started: a file, and not one that needs interpreting.
pub fn runnable(resolved: &Path) -> bool {
    resolved.is_file()
}

fn same_path(a: &str, b: &Path) -> bool {
    let norm = |s: &str| {
        let s = s.trim().replace('\\', "/");
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(a) == norm(&b.to_string_lossy())
}

/// The detected editor a setting points at, if any.
pub fn matching<'a>(setting: &str, editors: &'a [Editor]) -> Option<&'a Editor> {
    editors.iter().find(|e| same_path(setting, &e.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }

    fn exe(name: &str) -> String {
        format!("{name}{}", if cfg!(windows) { ".exe" } else { "" })
    }

    #[test]
    fn install_folders_beat_launchers_on_path() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("local");
        let bin = d.path().join("bin");
        touch(&root.join("Programs/Microsoft VS Code/Code.exe"));
        touch(&bin.join(if cfg!(windows) { "code.cmd" } else { "code" }));
        let found = detect_in(std::slice::from_ref(&root), std::slice::from_ref(&bin), false);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Visual Studio Code");
        assert!(found[0].path.ends_with("Code.exe"));
    }

    #[test]
    fn path_finds_what_install_folders_do_not() {
        let d = tempfile::tempdir().unwrap();
        touch(&d.path().join(exe("zed")));
        let found = detect_in(&[], &[d.path().to_path_buf()], false);
        assert_eq!(found.iter().map(|e| e.name).collect::<Vec<_>>(), ["Zed"]);
    }

    #[test]
    fn nothing_installed_finds_nothing() {
        let d = tempfile::tempdir().unwrap();
        assert!(detect_in(&[d.path().to_path_buf()], &[d.path().to_path_buf()], false).is_empty());
    }

    #[test]
    fn jetbrains_uses_the_newest_versioned_folder() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_path_buf();
        touch(&root.join("JetBrains/PyCharm 2024.1/bin/pycharm64.exe"));
        touch(&root.join("JetBrains/PyCharm 2025.2/bin/pycharm64.exe"));
        let found = detect_in(std::slice::from_ref(&root), &[], false);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.to_string_lossy().contains("2025.2"));
    }

    #[cfg(windows)]
    #[test]
    fn a_bare_command_resolves_to_its_script_on_windows() {
        let d = tempfile::tempdir().unwrap();
        let script = d.path().join("myedit.cmd");
        touch(&script);
        assert_eq!(resolve_in("myedit", &[d.path().to_path_buf()]), script);
    }

    #[test]
    fn full_paths_and_unknown_commands_pass_through() {
        assert_eq!(resolve_in("  C:/x/y.exe ", &[]), PathBuf::from("C:/x/y.exe"));
        assert_eq!(resolve_in("nosuchcommand", &[]), PathBuf::from("nosuchcommand"));
        assert!(!runnable(&resolve_in("nosuchcommand", &[])));
    }

    #[test]
    fn matching_ignores_slash_style() {
        let list = vec![Editor { name: "Zed", path: PathBuf::from("C:\\Apps\\Zed.exe") }];
        if cfg!(windows) {
            assert_eq!(matching("c:/apps/zed.exe", &list).map(|e| e.name), Some("Zed"));
        }
        assert!(matching("", &list).is_none());
    }
}
