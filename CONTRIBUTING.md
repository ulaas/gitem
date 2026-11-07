# Contributing

## Running it

```
cargo run -p gitem -- path/to/a/repository
cargo test --workspace
cargo clippy --workspace --all-targets
```

Tests make their own scratch git config, settings, secrets and home folder, so they don't touch yours. A new test binary has to start with `gitem_testenv::isolate!();`, and a test checks that it does. A test that changes the global git config goes in `crates/gitem-core/tests/global_config_writes.rs`.

To run the app without touching your own settings, set `GITEM_CONFIG_DIR` to a scratch folder and `GIT_CONFIG_GLOBAL` to a scratch file.

`crates/gitem/tests/screens.rs` draws dialogs, menus and views with fake data into a folder, for looking at:

```
SCREENS_OUT=some/folder cargo test -p gitem --test screens -- --ignored --test-threads=1
```

## How the code goes

- All git goes through the `git` executable. Writes are data in `ops.rs`, so they can be shown, confirmed and queued.
- Views don't change shared state: they push `Action`s, and the app applies them between frames.
- Nothing slow on the UI thread. Use `Task`, or `Action::Later`.
- Repository content is untrusted. Pass revisions through `rev_arg`, and read `crates/gitem-core/src/trust.rs` before adding anything that runs what a config names.
- A button that can be disabled says why, and a test reads the source for it.
- For a bug, write the failing test first where you can.

## Commit messages

A plain first line saying what changed, and a body for why, if it isn't obvious.
