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

fn linux(file: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux").join(file)).unwrap()
}

/// Not whether appimagetool accepts it, which is only known once someone runs it on Linux.
#[test]
fn the_appimage_takes_its_version_from_the_build_and_runs_nothing_it_makes() {
    let s = linux("build-appimage.sh");
    assert!(s.contains("Cargo.toml") && s.contains("gitem-$version-"), "{s}");
    assert!(s.contains("--profile dist"));
    assert!(s.starts_with("#!/usr/bin/env bash") && s.contains("set -euo pipefail"));
    assert!(!s.contains("sudo") && !s.contains("/usr/local"), "it installs nothing");
    assert!(!s.contains("curl"), "it fetches nothing itself");
    for file in ["AppRun", "gitem.desktop", "gitem.png"] {
        assert!(s.contains(&format!("\"$here/{file}\"")), "{file}");
    }
}

/// A child process that loaded bundled libraries could be handed the wrong ones, and git is one.
#[test]
fn the_appimage_leaves_the_library_path_alone() {
    let run = linux("AppRun");
    assert!(run.starts_with("#!/bin/sh"));
    assert!(run.contains("usr/bin/gitem\" \"$@\""));
    assert!(!run.contains("LD_LIBRARY_PATH"));
    assert!(!run.contains('\r') && !linux("build-appimage.sh").contains('\r'), "a shell can't run CRLF");
}

#[test]
fn the_desktop_entry_is_what_appimagetool_asks_for_and_claims_nothing() {
    let d = linux("gitem.desktop");
    for line in ["[Desktop Entry]", "Type=Application", "Name=gitem", "Icon=gitem", "Terminal=false"] {
        assert!(d.lines().any(|l| l == line), "{line}");
    }
    assert!(d.lines().any(|l| l.starts_with("Exec=gitem")));
    assert!(d.lines().any(|l| l.starts_with("Categories=") && l.ends_with(';')));
    assert!(!d.contains("MimeType"), "gitem:// links are registered from Settings, on request");
}

#[test]
fn the_appimage_icon_is_the_logo() {
    let bytes = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux/gitem.png")).unwrap();
    let got = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert_eq!(got.dimensions(), (256, 256));
    assert_eq!(got.as_raw(), &gitem::logo::rgba(256));
}

#[test]
fn the_release_workflow_builds_an_appimage_as_a_draft() {
    let y = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/release.yml")).unwrap();
    let (_, job) = y.split_once("  appimage:\n").or_else(|| y.split_once("  appimage:\r\n")).expect("a job for it");
    let job = job.split("  unix:").next().unwrap();
    assert!(job.contains("runs-on: ubuntu-22.04"), "{job}");
    assert!(job.contains("build-appimage.sh") && job.contains("gitem-*.AppImage") && job.contains("draft: true"), "{job}");
    assert!(job.contains("/releases/download/13/"), "a pinned release, not `continuous`");
    assert!(job.contains("APPIMAGE_EXTRACT_AND_RUN"));
}
