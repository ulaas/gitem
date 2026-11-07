# Packaging

`windows/gitem.iss` is an Inno Setup script for a per-user install, and `windows/build.ps1` builds the binary and runs it. `.github/workflows/release.yml` does the same on a version tag and attaches the result to a draft release.

None of these has been run: Inno Setup isn't installed where this was written, installing would write to the machine, and a workflow can't be run from here. `crates/gitem/tests/packaging.rs` reads the script and checks what it says, which isn't the same thing. Expect the first real build to find something.

Not here, because they need the owner's accounts or certificate:

- Code signing. Until the binary and installer are signed, SmartScreen warns about them.
- winget, Homebrew and other store listings, which need a public release to point at.

The update check also needs a public release to ask about. `DEFAULT_RELEASE_URL` in `crates/gitem-core/src/update.rs` is empty until there is one, and Settings has a field for the address meanwhile.

## AppImage

`linux/build-appimage.sh` builds the binary and wraps it with `appimagetool`, and `release.yml` runs it on the oldest supported Ubuntu runner, with appimagetool taken from its release 13. That release's checksum isn't pinned, as it couldn't be read from here.

None of this has been run on Linux. What has: `packaging.rs` reads the files, and on Windows the script was run in Git Bash up to a stand-in for appimagetool, which shows the folder it lays out and nothing about the AppImage. The AppImage doesn't carry git, GTK or graphics libraries, so it needs them from the system, and nothing registers `gitem://` links or a menu entry.
