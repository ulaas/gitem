# gitem

A desktop Git client built with Rust and [egui](https://github.com/emilk/egui). It covers what the GitLens extension does for VS Code, as a standalone app, with its own visual diff and merge tools. All repository access goes through the `git` executable.

## Build and run

```
cargo build --release
target/release/gitem [path-to-repository ...]
```

With no path, gitem restores the repositories from your last session.

### Which git is used

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
| Commits, Branches, Remotes, Tags, Stashes, Contributors views | Sidebar trees, plus full views where useful |
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
| Auto-rebase | Rebase with AI conflict resolution that pauses when the model is unsure, with one-click undo |
| Files view | A sidebar view of the working tree: folders first, change marks on files and the folders holding them, a filter, and a changed-only mode |
| Themes | Seven dark and five light themes, with nineteen named accent colors or any custom color; named accents are tuned for dark and light, and tests check their contrast in every theme |
| Command palette | `Ctrl+P`: commands, branches, and files in one search, with recent commands first. Start with `>` for commands, `@` for branches, `/` for files, or `:` for commits |
| Window | Every window, helpers included, has the app's own titlebar, and the main one has an icon rail of views with tooltips, so it looks the same on every platform; an About box shows the commit a build came from (also `gitem --version`) |
| Modes | Zen mode (`Ctrl+Shift+Z`) hides the side panels; dockable tabs replace detachable views |
| Terminal links | Not applicable. A Git console shows every command gitem runs and lets you run your own |

### Beyond GitLens

- **Visual diff tool:** side-by-side and unified, syntax highlighting, word-level changes, stage or discard by hunk or line, whitespace and algorithm options, image comparison. Works as git's `difftool`.
- **Visual merge tool:** Ours, Result, Theirs (optionally Base) with synchronised scrolling, per-conflict resolution, inline editing, and auto-resolved regions shown in context. Works as git's `mergetool`.
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
- Bare repositories can't be opened, because gitem works from a working tree.
- The in-app rebase editor works on linear history: merge commits in the range aren't kept, and the view says so. It refuses to start if HEAD has moved since the plan was made. For `--rebase-merges`, use **Rebase onto…**, which has that option, or run the rebase from the command line with gitem as `sequence.editor`.
- Hunk and line staging need UTF-8 text. For other encodings, stage the whole file. The merge tool offers a choice of versions for such files rather than rewriting them.

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
| `Ctrl+Enter` | Commit, or submit a form |
| `↑` `↓` or `J` `K` | Move through the graph |
| `Alt+↑` `Alt+↓` | Previous and next change in a diff; move a commit in the rebase editor |
| `P` `R` `E` `S` `F` `D` | Set the rebase action |
| `Ctrl+F` | Find in file; search in the graph |
| `/` | Find a ref in the graph |
| `F3` `Shift+F3` | Next and previous search match |
| `Alt+B` | Toggle blame in a file |
| `Ctrl+Shift+Z` | Zen mode |

## Safety

- Destructive commands ask first and show the exact git command. You can turn this off in Settings.
- Repository content is treated as untrusted. Background reads disable `core.fsmonitor`, so opening a repository can't run a program it configures. Links never pass through a shell, and repo files are revealed rather than launched unless you've configured an editor.
- Secrets go to the OS vault on Windows. On other platforms they're kept in a private file (mode 0600), and the app says so.
- AI features send the relevant diff to your chosen provider, and only when you ask. Oversized diffs are refused rather than silently cut.

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

Opt-in tests have real git open the real windows, including the password prompt, which answer by themselves through `GITEM_TEST_ACCEPT`: `cargo test -p gitem --test real_git_windows --test http_auth -- --ignored --test-threads=1`.

Tests create temporary repositories and briefly store a uniquely named test credential in the OS vault, which is always removed.

**What the tests don't cover:**

- Remote operations never run against a hosting service or over HTTPS, and SSH remotes and passphrase prompts aren't exercised.
- Hosting, issue-tracker, and AI requests go to the local mock, so they prove gitem's side of each exchange against sample responses, not that a real service accepts them. Try these with your own accounts before relying on them.
- Moving and resizing the custom window were checked by hand-style mouse input on Windows only, not in the test suite. On other platforms window corners stay square, and cancelling kills only git itself, not what it started.
- Native file-picker dialogs aren't automated. Every picker also has a text field.
- Copilot CLI and OpenCode reading is tested against sample data. No real Copilot session was available, and the real OpenCode database had no tool calls in it.
