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

fn release_workflow() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/release.yml")).unwrap().replace("\r\n", "\n")
}

/// What `release.yml` promises: every system built, nothing released without a tag, and the
/// release published (a tag with a hyphen as a pre-release). Whether GitHub's runners agree is
/// only known from a run.
#[test]
fn the_release_workflow_builds_every_system_and_publishes_a_release_from_a_tag() {
    let y = release_workflow();
    for artefact in ["windows-${{ matrix.arch }}", "-setup.exe", "linux-${{ matrix.arch }}.AppImage", "macos-universal", "SHA256SUMS"] {
        assert!(y.contains(artefact), "{artefact}");
    }
    for runner in ["windows-latest", "windows-11-arm", "ubuntu-22.04", "ubuntu-22.04-arm", "macos-latest"] {
        assert!(y.contains(&format!("runner: {runner}")) || y.contains(&format!("runs-on: {runner}")), "{runner}");
    }
    assert!(y.contains("lipo -create"), "one binary for Apple silicon and Intel");
    assert!(y.contains("gh release create") && y.contains("--verify-tag") && y.contains("if: github.ref_type == 'tag'"), "a release, and only for a tag");
    assert!(!y.contains("--draft"), "published, not left as a draft");
    assert!(y.contains("*-*) flags+=(--prerelease)"), "a tag with a hyphen is a pre-release");
    assert!(y.contains("doesn't match Cargo.toml's version"), "the tag has to be the version");
    assert!(y.contains("build-appimage.sh") && y.contains("APPIMAGE_EXTRACT_AND_RUN"));
}

/// The one thing it downloads and runs is pinned by version and checksum, and only the job that
/// creates the release can write.
#[test]
fn the_release_workflow_runs_only_what_it_checked_and_writes_only_where_it_must() {
    let y = release_workflow();
    assert!(y.contains("APPIMAGETOOL_VERSION: \"1.9.1\"") && !y.contains("continuous"), "a pinned release");
    assert!(y.contains("sha256sum -c -"), "checked before it's run");
    assert!(y.contains("permissions:\n  contents: read"), "read-only by default");
    assert_eq!(y.matches("contents: write").count(), 1, "only the release job writes");
    for line in y.lines().filter(|l| l.trim_start().starts_with("- uses:") || l.trim_start().starts_with("uses:")) {
        let at = line.split('@').nth(1).unwrap_or("");
        assert!(at.len() >= 40 && at[..40].chars().all(|c| c.is_ascii_hexdigit()), "pinned to a commit: {line}");
    }
}

/// The updater downloads a release's files by name, so the names the workflow gives them and the
/// names it asks for have to be the same ones.
#[test]
fn the_updater_asks_for_the_files_the_release_workflow_makes() {
    use gitem_core::update::{Install, asset_name};
    let y = release_workflow();
    // The archives are named in two steps there: a folder's name, then that with its ending.
    let made = |name: &str| {
        let (stem, ending) = [".zip", ".tar.gz"].iter().find_map(|e| name.strip_suffix(e).map(|s| (s, *e))).unwrap_or((name, ""));
        let pattern = stem.replace("9.9.9", "$VERSION").replace("x64", "${{ matrix.arch }}").replace("arm64", "${{ matrix.arch }}");
        let pwsh = pattern.replace("$VERSION", "$env:VERSION");
        assert!(y.contains(&pattern) || y.contains(&pwsh), "{name}: neither {pattern} nor {pwsh} is in release.yml");
        assert!(ending.is_empty() || y.contains(&format!("$name{ending}")), "{name}: nothing is packed as {ending}");
    };
    for (kind, os, arch) in [
        (Install::Installer, "windows", "x86_64"),
        (Install::Program, "windows", "x86_64"),
        (Install::Program, "windows", "aarch64"),
        (Install::Program, "linux", "x86_64"),
        (Install::Program, "linux", "aarch64"),
        (Install::Program, "macos", "aarch64"),
        (Install::AppImage("x".into()), "linux", "x86_64"),
    ] {
        made(&asset_name("9.9.9", &kind, os, arch).expect("a file for it"));
    }
    assert!(y.contains("sha256sum * > SHA256SUMS"), "what the updater checks a download against");
    // The installer is run without its pages, so it mustn't start gitem itself or need a click.
    let s = iss();
    assert!(s.contains("skipifsilent") && s.contains("PrivilegesRequired=lowest"));
}
