# gitem

A desktop Git client built with Rust and [egui](https://github.com/emilk/egui). It covers what the GitLens extension does for VS Code, as a standalone app, with its own visual diff and merge tools. All repository access goes through the `git` executable.

## Build and run

```
cargo build --release
target/release/gitem [path-to-repository ...]
```

`cargo build --profile dist` makes a smaller binary, in `target/dist`, and takes minutes.

With no path, gitem restores the repositories from your last session. With a path, it opens that repository and leaves the saved session in place for next time.

### Which git is used

gitem needs **git 2.38 or newer**, and warns at startup if it finds an older one. Most things still work below that, but merge conflict checks (`merge-tree --write-tree`) and updating dependent branches in a rebase (`rebase --update-refs`) need 2.38.

gitem looks for git in this order:

1. The path set in **Settings > General**.
2. **A `git/` folder beside `gitem.exe`** (also `mingit/` or `PortableGit/`). To ship a self-contained copy, unpack [MinGit](https://github.com/git-for-windows/git/releases) so that `git/cmd/git.exe` sits next to the binary.
3. `PATH`.
4. Standard install folders.

## Features

| GitLens feature | In gitem |
|---|---|
| Inline and status-bar blame | Current-line blame in the file view and status bar, with a configurable format |
| File blame annotations | Blame gutter with authors, age bars, and commit cards on hover |
| Heatmap annotations | Heatmap gutter from oldest to newest line |
| Changes annotations | Change bars and deletion markers against `HEAD` |
| Git CodeLens | Recent-change and author summaries above symbols, by regex for common languages |
| Revision navigation | Older and Newer across a file's history, following renames; compare with the previous or next revision, the working tree, or any branch or tag; open any revision |
| Commit Graph | Lanes with folding, ref badges, search with highlights and match options, Current and Smart presets, favorites, focus and hide refs, worktree pills, minimap by commits or lines changed, Changes column, columns you can resize, reorder, and hide by dragging or right-clicking headers, remembered between sessions, dim merges, jump to HEAD or upstream, `/` ref finder, multi-select, drag a ref onto a commit to merge, rebase, or compare, work-in-progress row, stashes |
| Commit actions | Reword, squash, drop, Undo Commit, Push to Commit, fixup and Squash Fixups, recompose, copy as patch, compare with common base, conflict prediction |
| Inspect / Commit Details | Metadata, signatures, autolinks, notes, containing refs, files with stats |
| Commits, Branches, Remotes, Tags, Stashes, Contributors views | Sidebar trees, plus full views where useful. Local and remote branches open to their latest commits, and long lists have Load more |
| Repositories, Workspaces | Tabs for several open repositories; named local workspaces with status overview and fetch-all |
| Worktrees | Add, open, move, lock, remove, prune, repair; per-worktree change counts; copy changes or apply a stash into another worktree; open a terminal, run a task, or start an agent CLI there |
| File History, Line History | Commit lists for a file or a line range |
| Visual History | Time and author bubble chart with churn bars, zoom, and pan, for a file, a folder, a branch, or the whole repository |
| Treemaps, Repository Health | Files by size or recent commits; a verdict plus optimizations you can turn on and undo; maintenance on demand or at open |
| Search & Compare | The same search syntax (`message:`, `-message:`, `author:`, `committer:`, `@me`, `commit:`, `file:`, `change:`, `after:`, `before:`, `is:merge`, `type:merge`), and compare of any two refs |
| Interactive Rebase Editor | Drag to reorder; pick, reword, edit, squash, fixup, drop, exec, break. Also works as git's own `sequence.editor` |
| Git Command Palette | `Ctrl+P`: every command, branch, and file, with guided forms |
| Launchpad | Pull requests grouped by what they need from you, with checks, review, and conflict state; pin, snooze, stacked-PR labels and rebase; current branch's PR in the status bar |
| Start Work, Start Review | Branch or worktree from an issue; check out a PR, or open it in a worktree |
| Remote provider integrations | GitHub, GitLab, Bitbucket, Bitbucket Server/Data Center, Azure DevOps (and Server), and Gitea/Forgejo, including self-hosted: links, PRs, issues, create, merge, approve, comment. Gerrit and Google Source: links only |
| Issue tracker integrations | Jira Cloud, Jira Data Center/Server, Linear, and Trello |
| Remote commands | Open or copy links to commits, files and lines, branches, tags, comparisons, and the repository; open a file from a pasted link |
| Autolinks | Configurable prefixes, plus `#123` for the repository's host |
| AI features | Commit messages; explanations of commits, branches, stashes, and working changes; review; conflict resolution; natural-language search; PR descriptions; stash messages; changelogs; commit composition and recompose. Through Anthropic, OpenAI, Gemini, Mistral, OpenRouter, or any OpenAI-compatible server such as Ollama. `.aiignore`, `.cursorignore`, and `.aiexclude` are honored |
| Cloud Patches | A local patch library, shareable as a secret GitHub gist. No account required |
| Hovers, avatars | Commit cards everywhere; initials by default, Gravatar opt-in |
| Commit signatures | Verification status per commit, signing setup, SSH allowed signers editor |
| Agents | A board of Claude Code and Codex sessions in this repository's worktrees, with resume, archive, and a file activity treemap; start a session in any worktree |
| Review | Sessions with a file checklist, comment threads, AI findings, a reading-order suggestion, and handing open notes to an agent |
| Auto-rebase | Rebase with AI conflict resolution that pauses when the model is unsure, with an undo that keeps uncommitted changes, and stops without changing anything if they're in a file it has to change |
| Files view | A sidebar view of the working tree: folders first, change marks on files and the folders holding them, a filter, and a changed-only mode |
| Changes list | Staged, unstaged, and conflicted files as a flat list or a tree, with folders you can close, and stage or unstage as one |
| Themes | Seven dark and five light themes, with nineteen named accent colors or any custom color; named accents are tuned for dark and light, and tests check their contrast in every theme |
| Command palette | `Ctrl+P`: commands, branches, and files in one search, with recent commands first. Start with `>` for commands, `@` for branches, `/` for files, or `:` for commits |
| Window | Every window, helpers included, has the app's own titlebar, and the main one has an icon rail of views with tooltips, so it looks the same on every platform; an About box shows the commit a build came from (also `gitem --version`) |
| Modes | Zen mode (`Ctrl+Shift+Z`) hides the side panels; dockable tabs replace detachable views |
| Terminal links | Not applicable. A Git console shows every command gitem runs and lets you run your own |

### Beyond GitLens

- **Visual diff tool:** side-by-side and unified, syntax highlighting, word-level changes, stage or discard by hunk or line, whitespace and algorithm options, image comparison. Works as git's `difftool`.
- **Visual merge tool:** Ours, Result, Theirs (optionally Base) with synchronised scrolling, per-conflict resolution, inline editing, and auto-resolved regions shown in context. It starts from the file on disk, so hand edits and earlier partial saves are kept, and it asks before saving over a file changed since it was read. Works as git's `mergetool`.
- **Credential manager:** API tokens and git HTTPS credentials in Windows Credential Manager, a git credential helper, removal of stale Git Credential Manager entries, SSH key generation and agent management, connection tests, commit-signing setup, and dialogs for git's password and passphrase prompts.
- **Repository tools:** reflog recovery, guided bisect (including automated runs), submodules, hooks, `.gitignore` editing with path testing, clean with preview, Git LFS, sparse checkout, maintenance, and a config editor.
- **Docking:** drag tabs to split or rearrange the workspace.

### Differences from GitLens

This was compared against the GitLens README, Help Center, and changelog through 19.5.0 (November 2026).

**Not included, because they're GitKraken's own services:** GitKraken sign-in and AI credits, Cloud Patches links and hosted patch storage (the patch library and secret gists cover sharing), Cloud Workspaces sync (workspaces are local), and Kepler. gitem has its own equivalents for Code Suggest (GitHub review suggestions), deep links (`gitem://`), and the MCP server (`gitem --mcp`, read-only).

**Partial:**

- **Agent sessions** are read from the files Claude Code, Codex, Copilot CLI, and OpenCode write, so status is an estimate. You answer permission prompts in the agent, not in gitem. Copilot's format is undocumented and OpenCode's has changed between releases, so both may need updating. OpenCode sessions show no branch, because it doesn't record one.
- **Localization** is experimental, as in GitLens: Spanish, Turkish, and Simplified Chinese cover navigation, toolbars, and context menus. The rest is English.
- **Suggested changes** only work on GitHub.

**Not applicable:** follow-the-terminal and terminal links, since gitem has no integrated terminal.

gitem isn't an editor plugin, so the following differ by design.

- Blame and annotations appear in gitem's read-only file view, not in your editor as you type. Use **Settings > External editor** to jump to your editor.
- Patches and workspaces are stored locally; there's no GitKraken account or cloud sync.
- Code lens finds symbols with a regex rather than a language server, so it can miss some declarations.
- Bare repositories can't be opened, because gitem works from a working tree. It says so, and names any linked worktrees to open instead. A linked worktree of a bare repository opens as usual.
- The in-app rebase editor works on linear history: merge commits in the range aren't kept, and the view says so. It refuses to start if HEAD has moved since the plan was made. For `--rebase-merges`, use **Rebase onto…**, which has that option, or run the rebase from the command line with gitem as `sequence.editor`.
- Hunk and line staging need UTF-8 text. For other encodings, stage the whole file. The merge tool offers a choice of versions for such files rather than rewriting them. A valid file that really contains U+FFFD is treated the same way.

## Using gitem from the command line

Register these from **Credentials > Git integration**, or set them yourself:

```
gitem --credential-helper get|store|erase    git credential helper
gitem --difftool LEFT RIGHT                  git difftool
gitem --mergetool BASE LOCAL REMOTE MERGED   git mergetool
gitem --rebase-editor TODO-FILE              git sequence.editor
gitem --edit-file FILE                       git core.editor
gitem --mcp [PATH]                           read-only MCP server for coding agents
gitem gitem://open?repo=PATH&commit=SHA      open a link
```

## Keyboard

| Keys | Action |
|---|---|
| `Ctrl+P` | Command palette |
| `Ctrl+Shift+G` | Changes |
| `Ctrl+Shift+H` | Commit graph |
| `Ctrl+B` | Toggle sidebar |
| `F5` | Refresh |
| `Ctrl+Enter` | Submit the open form. With none open, or from the commit box, commit |
| `↑` `↓` or `J` `K` | Move through the graph |
| `Alt+↑` `Alt+↓` | Previous and next change in a diff; move a commit in the rebase editor |
| `P` `R` `E` `S` `F` `D` | Set the rebase action |
| `Ctrl+F` | Find in file; search in the graph |
| `/` | Find a ref in the graph |
| `F3` `Shift+F3` | Next and previous search match |
| `Alt+B` | Toggle blame in a file |
| `Ctrl+Shift+Z` | Zen mode |

## Safety

- Destructive commands ask first and show the exact git command, including a plain force push from the push form. You can turn this off in Settings. Deleting a stored secret asks too, and a revealed secret hides itself after 30 seconds or when you leave its page.
- Unstaging during a merge, cherry-pick or rebase keeps the operation and its conflicts. The Commit Composer waits until the operation is finished, except while a rebase is stopped at an edit step, where splitting a commit is the point.
- The Commit Composer keeps its plan until each commit has landed, leaves other staged files staged, and asks before committing a partly staged file whole.
- Closing a tab, a repository or the window asks first if that would lose unsaved merge resolutions, a written commit message, a Composer plan, typing in an open dialog, or a running or queued git command. A fetch doesn't count. Not yet covered: rebase plans, Launchpad's pull request form, and review notes being typed.
- Force with lease also uses `--force-if-includes`, so a push after a fetch is refused until you've taken in what the fetch brought.
- Writes wait up to four seconds for another program's `index.lock`. A lock that stays gets a dialog offering to remove it, only when you ask and no git process is running on the computer. It's never removed automatically. Cancelling a pull can leave one behind, along with half-updated files.
- Saved patches keep binary files, other encodings and, for all changes, untracked files. Applying tries a plain `git apply` first, which stages nothing, and falls back to a three-way merge and says so. A patch with text that isn't UTF-8 can't go on the clipboard or in a gist, so those are refused.
- A settings file gitem can't fully read is kept beside it as `settings.json.bad-<time>`, and you're told. Unknown fields are kept when saving, and two copies of gitem merge their changes by setting rather than overwriting each other.
- A command gitem starts in a terminal, such as an agent or a worktree task, comes from `PATH` on Windows, never from the repository folder. For a tool in the repository, give its path in the setting: `.\gradlew test`. "Open Terminal" itself is unchanged.
- Passwords and tokens inside remote URLs are masked in titles, the Console and tooltips. Git still gets the real URL, and Copy URL copies it.
- Very long lines are cut on screen at 5,000 bytes, with a note saying so, because drawing a whole line of a few megabytes would crash the GPU buffer. Staging, saving, copying and Find still use the whole line. Only that part is coloured.
- Files over 10 MB open without blame, which can take minutes on them. A button turns it on.
- If gitem crashes it says so in a message box, writes `crash-*.log` to its config folder, and offers it on the next launch. It holds the panic message, a backtrace and the last git commands. Nothing is sent anywhere. A commit message being written is kept in the config folder's `drafts` while you type, and comes back when you reopen that repository.
- When git can't read a repository, or stops answering, a banner says so with a way to try again, rather than a spinner forever. A write that seems stuck, such as a hook that never returns, gets a Cancel after a few seconds, which warns that stopping git can leave files half updated.
- Scheduled fetches run beside your work, not in its queue, and wait for any operation in progress. They don't disable Commit, don't report success, and only say in the status bar when they fail, going from every N minutes to at most 16 times N while it keeps failing. They can't ask for a password. A push or pull you start waits for one that's running.
- A failed command drops only what was asked for with it, unless it leaves a rebase or merge going on, when everything queued is dropped and you're told. A conflict isn't reported as a failure: the banner has what to do next. A button pressed twice within 0.6 seconds runs its command once.
- Changes to files git ignores don't refresh anything straight away, but do get a look every 30 seconds while they go on, since a tracked file can match an ignore rule. Coming back to the window always refreshes. Where file notifications don't work, as on some network drives, gitem looks every 5 seconds and says so in the status bar.
- Repository content is treated as untrusted, because a folder can arrive with its `.git` from a zip or a shared drive, and git would run programs named in that config just to read it. So before the first read of a repository, gitem looks for such settings in the repository's own config and in its submodules': filters, diff and merge drivers, `gpg.program`, `core.sshCommand`, `core.gitProxy`, `credential.helper`, a `core.fsmonitor` program and a few more. If there are any, it names them and asks, once per folder, and asks again if the list changes. Until you say yes, they're off: files using a filter can look modified when they aren't, diffs and blame show raw text, signatures aren't checked, and staging skips those filters. A banner says so. Automatic fetch and maintenance wait too. Your global config is yours and isn't touched, so ordinary repositories and global Git LFS never see this. `git lfs install --local`, done outside gitem, gets one question. Repositories gitem clones itself, and changes you make in gitem's own config editor, aren't asked about.
- Reads never run hooks, and a folder shaped like a git directory inside another repository isn't opened as one. Commits and other changes you start do run hooks, as in git, and `core.hooksPath` is listed in the question for information. A scheduled fetch or maintenance runs none.
- `gitem --mcp` can't ask you anything, so a repository's programs stay off there unless you've already said yes in gitem. Its tools can't write a file: anything an agent passes as a revision that would be read as an option is refused. What they can't stop is git's own stat refresh of `.git/index`, as with any `git diff`.
- A `gitem://` link is anyone's to write. `commit` must be a hash and `file` a path inside the repository, or the link is refused. A folder you haven't opened before asks first, showing the full path, and a network path is refused outright. A link whose repository doesn't open does nothing further. Links never pass through a shell, and repo files are revealed rather than launched unless you've configured an editor.
- A ref whose name starts with a dash, which a server can deliver, is shown and used by its full name, like `refs/tags/--hard`, and can still be deleted.
- `.aiignore` and its like now follow gitignore's rules, including `**`, `[a-z]` and `!`, ignoring letter case. Matched files stay out of AI requests under any name, as renames or in conflicts, and `git_blame` over MCP refuses them. Auto-rebase stops once the model has resolved a commit's conflicts, so you can read the result before anything is committed from it. A tick box in its form turns that off. Prompts tell the model that repository text is data, not instructions, which helps but isn't a guarantee.
- Secrets go to the OS vault on Windows. On other platforms they're kept in a private file (mode 0600), and the app says so.
- `GITEM_CONFIG_DIR` moves everything gitem keeps into that folder: settings, window state, and secrets. On Windows that means a plain file instead of the OS vault, so use it for testing, not for real tokens.
- `--shot` leaves your profile alone. It doesn't save settings or the session, keeps window state in a temp folder unless `GITEM_CONFIG_DIR` is set, and uses no stored secrets.
- AI features send the relevant diff to your chosen provider, and only when you ask. Oversized diffs are refused rather than silently cut.
- A pull git refuses because both sides have new commits and nothing says how to join them offers Merge or Rebase, and can remember the choice for the repository as `pull.rebase`.
- A staged rename stays whole: unstaging it, or committing it in the Composer, takes both names, and clicking it shows the rename with its real edits. Unstaging a hunk of one keeps the rename. Staging or taking back part of a new or deleted file works.
- Lists too long for a command line go to git over stdin, except that `git stash` passes names on to another git, so stashing a very long list by name is refused before anything changes.
- In a partial clone, line counts in the graph, Visual History and Contributors no longer download files. Commits whose files aren't here show no counts. Opening a commit's diff still fetches what it needs.
- A squash merge fills in its prepared message and says it's waiting for its commit. A prepared message keeps lines that start with `#`, and drops only git's own list of conflicts.
- In the graph, No merges and First parent draw real connections rather than lanes that never end, and the arrow keys only scroll when the row is off screen. Hiding the current branch now hides it. A search git rejects says why.
- The file view shows PNG and JPEG pictures, reads UTF-16 by its byte order mark, and shows other files that aren't UTF-8 as Latin-1, a guess that it labels as one. The diff view compares only PNG and JPEG.

## Project layout

```
crates/gitem-core   Everything except UI: git runner, parsers, ops, diff and
                    merge engines, graph layout, forges, credentials, AI
crates/gitem        The egui app, its views, and git helper modes
```

Write operations are data (`ops.rs`), so every mutation can be logged, confirmed, and queued in one place.

## Tests

`cargo test --workspace` runs:

- **Unit tests** for parsers, the merge engine, graph layout, patches, URL building, and API response parsing.
- **Integration tests against real git**, in temporary repositories.
- **Remote tests** that run real clone, fetch, pull, and push against a bare repository in a temp folder, covering rejected and forced pushes, conflicts, hooks, pruning, tags, and cancelling.
- **HTTP authentication tests** that run real git against `git http-backend` behind a Basic-auth bridge on localhost, through gitem's credential helper.
- **HTTP client tests:** the hosting, issue-tracker, and AI clients make real requests to a mock server on localhost.
- **Headless UI tests** that click real widgets, right-click for context menus, drag refs, and drive the helper windows, then verify the result with git or on disk.
- **Helper tests** that have real git invoke the built binary.
- **Property tests** (`crates/gitem-core/tests/properties.rs`) of partial patches through real `git apply`, the merge engine, and the parsers. Case counts are small by default: set `PROPTEST_CASES` for a longer hunt.
- **Performance tests** that fail if parsing stops scaling linearly (`crates/gitem-core/tests/perf.rs`), if a frame gets slow, or if an idle window starts running git, with very large histories, diffs, files, ref lists and statuses. The harness doesn't paint, so these don't measure drawing.
- **Lifecycle tests** (`crates/gitem/tests/lifecycle.rs`) of real startup, exit, the saved session, and old, new and broken settings files.

Tests ignored with a bug's ID, such as `#[ignore = "PF-5, Phase 5"]`, show a known bug and fail until it's fixed.

Opt-in tests have real git open the real windows, including the password prompt, which answer by themselves through `GITEM_TEST_ACCEPT`: `cargo test -p gitem --test real_git_windows --test http_auth --test helper_modes -- --ignored --test-threads=1`. Debug builds read that variable and `GITEM_SHOT`; release builds ignore them.

Tests create temporary repositories. Every test binary starts with its own scratch git config, settings, secrets, and home folder (`crates/gitem-testenv`), so results don't depend on the machine and nothing reaches your real profile. That includes the OS vault: the one test of the real Windows vault is opt-in, with `cargo test -p gitem-core --lib windows_vault_round_trip -- --ignored`.

**What the tests don't cover:**

- Remote operations never run against a hosting service or over HTTPS, and SSH remotes and passphrase prompts aren't exercised.
- Hosting, issue-tracker, and AI requests go to the local mock, so they prove gitem's side of each exchange against sample responses, not that a real service accepts them. Try these with your own accounts before relying on them.
- Moving and resizing the custom window were checked by hand-style mouse input on Windows only, not in the test suite. On other platforms window corners stay square, and cancelling kills only git itself, not what it started.
- Native file-picker dialogs aren't automated. Every picker also has a text field.
- Copilot CLI and OpenCode reading is tested against sample data. No real Copilot session was available, and the real OpenCode database had no tool calls in it.
