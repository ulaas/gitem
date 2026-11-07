//! Reads the installer script and checks what it promises. Nothing is built or installed, so this
//! can't show that Inno Setup accepts it: that's only known once someone runs it.
gitem_testenv::isolate!();

use std::path::Path;

fn iss() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/windows/gitem.iss")).unwrap()
}

#[test]
fn it_installs_for_this_user_only() {
    let s = iss();
    assert!(s.contains("PrivilegesRequired=lowest"));
    assert!(s.contains(r"DefaultDirName={localappdata}\Programs\gitem"));
    assert!(s.contains("{userprograms}"));
    for line in s.lines().filter(|l| l.starts_with("Root:")) {
        assert!(line.starts_with("Root: HKCU;"), "machine-wide: {line}");
    }
    for bad in ["{commonpf", "{autopf", "{pf}", "{commonprograms}", "HKLM", "{sys}"] {
        assert!(!s.contains(bad), "{bad}");
    }
}

#[test]
fn what_changes_the_shell_is_opt_in() {
    let s = iss();
    for task in ["Name: \"path\"", "Name: \"explorer\""] {
        let line = s.lines().find(|l| l.starts_with(task)).expect(task);
        assert!(line.contains("Flags: unchecked"), "{line}");
    }
    assert!(!s.contains("\\Run\""), "nothing starts with Windows");
}

#[test]
fn the_version_comes_from_the_build_not_the_script() {
    let s = iss();
    assert!(s.contains("AppVersion={#AppVersion}"));
    let build = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/windows/build.ps1")).unwrap();
    assert!(build.contains("Cargo.toml") && build.contains("/DAppVersion=$version"));
    assert!(!build.contains("Start-Process"), "it builds and doesn't run the result");
}

#[test]
fn an_uninstall_takes_back_what_was_added_to_path() {
    let s = iss();
    assert!(s.contains("CurUninstallStepChanged"));
    assert!(s.contains("uninsdeletekey"));
}
