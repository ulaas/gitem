# Packaging

`.github/workflows/release.yml` builds gitem for every system:

| File | What it is |
| --- | --- |
| `gitem-<version>-windows-x64-setup.exe` | Per-user installer (Inno Setup), from `windows/gitem.iss` |
| `gitem-<version>-windows-x64.zip`, `-windows-arm64.zip` | The program, to unpack anywhere |
| `gitem-<version>-linux-x64.tar.gz`, `-linux-arm64.tar.gz` | The program, built on Ubuntu 22.04 so it starts on that glibc or newer |
| `gitem-<version>-linux-x64.AppImage`, `-linux-arm64.AppImage` | The same, as an AppImage, from `linux/build-appimage.sh` |
| `gitem-<version>-macos-universal.tar.gz` | One binary for Apple silicon and Intel Macs |
| `SHA256SUMS` | Checksums of all of the above |

Every archive also holds `README.md`, `LICENSE` and `THIRD-PARTY-NOTICES.md`.

## Making a release

1. Set the version in `Cargo.toml` and give `CHANGELOG.md` a heading for it, `## <version> (<date>)`: the release notes are taken from that section.
2. Commit, tag `v<version>` and push the tag. The workflow refuses a tag that doesn't match `Cargo.toml`.
3. When it's done, a draft release has every file. Read it, then publish it.

To try the builds without releasing, run the workflow by hand from **Actions > Release > Run workflow**. The files are kept as the run's artifacts.

## What it doesn't do

- **Signing.** Nothing is signed, so Windows SmartScreen and macOS Gatekeeper warn before the first run. On macOS, `xattr -d com.apple.quarantine gitem` after unpacking lets it start. Signing needs certificates as repository secrets.
- **Store listings.** `winget/`, `scoop/`, `homebrew/` (a cask, for a tap of the owner's own) and `aur/` (`gitem-bin`) are templates. When a release is published, `.github/workflows/package-manifests.yml` fills them with its version and checksums and attaches them to the release as `gitem-<version>-package-manifests.tar.gz`. Submitting them to winget-pkgs, a Scoop bucket, a Homebrew tap or the AUR is left to a person.
- **Bundled libraries.** The Linux builds and the AppImage use git, GTK and the graphics libraries already on the system, and nothing registers `gitem://` links or a menu entry; Settings does that on request.

## Locally

- `windows/build.ps1` builds the binary and the installer into `target\dist`. It needs Inno Setup 6 (`iscc`) on `PATH`.
- `linux/build-appimage.sh` builds the binary and wraps it with `appimagetool`, named by `APPIMAGETOOL`.

`crates/gitem/tests/packaging.rs` reads these scripts and the workflow and checks what they promise.

The update check needs a published release to ask about: `DEFAULT_RELEASE_URL` in `crates/gitem-core/src/update.rs` stays empty until there is one, and Settings has a field for the address meanwhile.
