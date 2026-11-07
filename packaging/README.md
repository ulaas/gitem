# Packaging

`windows/gitem.iss` is an Inno Setup script for a per-user install, and `windows/build.ps1` builds the binary and runs it. `.github/workflows/release.yml` does the same on a version tag and attaches the result to a draft release.

None of these has been run: Inno Setup isn't installed where this was written, installing would write to the machine, and a workflow can't be run from here. `crates/gitem/tests/packaging.rs` reads the script and checks what it says, which isn't the same thing. Expect the first real build to find something.

Not here, because they need the owner's accounts or certificate:

- Code signing. Until the binary and installer are signed, SmartScreen warns about them.
- winget, Homebrew and other store listings, which need a public release to point at.

The update check also needs a public release to ask about. `DEFAULT_RELEASE_URL` in `crates/gitem-core/src/update.rs` is empty until there is one, and Settings has a field for the address meanwhile.
