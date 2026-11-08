# gitem

A desktop Git client built with Rust and [egui](https://github.com/emilk/egui). It covers what the GitLens extension does for VS Code, as a standalone app, with its own visual diff and merge tools. All repository access goes through the `git` executable.

## Build and run

```
cargo build --release
target/release/gitem [path-to-repository ...]
```

`cargo build --profile dist` makes a smaller binary, in `target/dist`, and takes minutes.

`packaging/windows/build.ps1` builds that and a per-user Windows installer, with Inno Setup. See `packaging/README.md`: neither has been run, and nothing is signed, so SmartScreen warns about the result. About has Check for updates, and Settings > Workflow can ask once a day. Either only tells you and links to the release's page: nothing is downloaded or run. It's off, and has nowhere to ask, until you give it the address of a releases/latest API.

With no path, gitem restores the repositories from your last session, with each Search or Compare tab's query filled in and waiting for you to run it. With a path, it opens that repository and leaves the saved session in place for next time.

### Which git is used

gitem needs **git 2.38 or newer**, and warns at startup if it finds an older one. Most things still work below that, but merge conflict checks (`merge-tree --write-tree`) and updating dependent branches in a rebase (`rebase --update-refs`) need 2.38. The rebase editor's Check for conflicts needs 2.40, and says so below that.

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
| Graph | Lanes with folding, ref badges, search with highlights and match options, Current and Smart presets, favorites, focus and hide refs, worktree pills, minimap by commits or lines changed, Changes column, columns you can resize, reorder, and hide by dragging or right-clicking headers, remembered between sessions, dim merges, jump to HEAD or upstream, `/` ref finder, multi-select, drag a ref onto a commit to merge, rebase, or compare, work-in-progress row, stashes |
| Commit actions | Reword, squash, drop, revert a selection, Undo Commit, Push to Commit, fixup and Squash Fixups, recompose, copy as patch, compare with common base, conflict prediction |
| Inspect / Commit Details | Metadata, signatures, autolinks, notes, containing refs, files with stats |
| Commits, Branches, Remotes, Tags, Stashes, Contributors views | Sidebar trees, plus full views where useful. Local and remote branches open to their latest commits, and long lists have Load more |
| Repositories, Workspaces | Tabs for several open repositories; a line in the status bar saying which need you, from what each last read; named local workspaces with a status overview that follows what they do, and fetch all, pull all (fast-forward only) and open all, reporting failures together |
| Worktrees | Add, open, move, lock, remove, prune, repair; per-worktree change counts; copy changes or apply a stash into another worktree; open a terminal, run a task, or start an agent CLI there |
| File History, Line History | Commit lists for a file or a line range |
| Visual History | Time and author bubble chart with churn bars, zoom, and pan, for a file, a folder, a branch, or the whole repository |
| Treemaps, Repository Health | Files by size or recent commits; a verdict plus optimizations you can turn on and undo; maintenance on demand or at open |
| Search contents | `git grep` across tracked files, here or at any revision, as plain text or a regular expression. A click opens the file at that line |
| Search & Compare | The same search syntax (`message:`, `-message:`, `re:`, `author:`, `committer:`, `@me`, `commit:`, `file:`, `change:`, `after:`, `before:`, `is:merge`, `type:merge`), and compare of any two refs. Words are plain text and all must match, as with `fix(auth):` or `C++`. `re:` takes a regular expression, such as `re:fix\|feat` |
| Rebase | Drag to reorder; pick, reword, edit, squash, fixup, drop, exec, break. Check for conflicts tries the plan out without touching anything, and names the commit it would stop at. Needs git 2.40. Also works as git's own `sequence.editor` |
| Git Command Palette | `Ctrl+P`: every branch and file, and the commands from gitem's menus that apply to the repository, with guided forms. Commands that need a row to act on, such as those for a graph selection, stay on that row's menu |
| Launchpad | Pull requests grouped by what they need from you, with checks, review, and conflict state; pin, snooze for an hour, a day or a week, stacked-PR labels, a rebase that moves the stack's branches and a push of them all together; the current branch's PR in the status bar, from Launchpad or, if turned on in Settings, without it. Everything you're involved in is one click away, up to 200 per query, from this repository's host and every host your recent repositories use that has a saved token, with each host's failure beside what the others found. It asks only when you click, one or more requests per host. It needs a repository open, though not one on a host |
| Pull requests | A tab for each one: description, changed files with their diffs where the host gives them, each check with a link to its log, the conversation with inline comments in their threads, a reply within a thread (not on Gitea, which says so), Approve, Request changes and Comment, a comment on a line, and Merge with a choice of merge, squash or rebase where the host takes one. Details shows which pull request a commit came in by, when asked |
| Start Work, Start Review | Branch or worktree from an issue; check out a PR, or open it in a worktree; start your agent command there, in a new terminal |
| Remote provider integrations | GitHub, GitLab, Bitbucket, Bitbucket Server/Data Center, Azure DevOps (and Server), and Gitea/Forgejo, including self-hosted: links, PRs and their details, issues, create, merge, approve, request changes, comment. GitLab has no request for changes, Bitbucket Server merges by its own setting, and Azure's file lists come with no diff. Gerrit and Google Source: links only |
| Issue tracker integrations | Jira Cloud, Jira Data Center/Server, Linear, and Trello |
| Remote commands | Open or copy links to commits, files and lines, branches, tags, comparisons, and the repository; open a file from a pasted link |
| Autolinks | Configurable prefixes, plus `#123` for the repository's host |
| AI features | Commit messages; explanations of commits, branches, stashes, and working changes; review; conflict resolution; natural-language search; PR descriptions; stash messages; changelogs; commit composition and recompose. Through Anthropic, OpenAI, Gemini, Mistral, OpenRouter, or any OpenAI-compatible server such as Ollama. `.aiignore`, `.cursorignore`, and `.aiexclude` are honored |
| Patches | A local patch library, shareable as a secret GitHub gist. No account required |
| Hovers, avatars | Commit cards everywhere; initials by default, Gravatar opt-in |
| Commit signatures | Verification status per commit, signing setup, SSH allowed signers editor |
| Agents | A board of Claude Code, Codex, Copilot CLI and OpenCode sessions in this repository's worktrees, with resume, archive, and a file activity treemap; start a session in any worktree |
| Review | Sessions with a file checklist, comment threads, AI findings, a reading-order suggestion, and handing open notes to an agent |
| Auto-rebase | Rebase with AI conflict resolution that pauses when the model is unsure, with an undo that keeps uncommitted changes, and stops without changing anything if they're in a file it has to change |
| Files view | A sidebar view of the working tree: folders first, change marks on files and the folders holding them, a filter, and a changed-only mode. In IDE mode a click opens the file for editing, as a preview the next click replaces, and a double click keeps it |
| Editor | Files of the working tree open for editing (IDE mode, or Edit in any file's menu): syntax colours, several carets, find and replace, bracket pairs, auto-indent, comment toggling, moving and copying lines, and go to line. Line endings, a byte order mark and UTF-16 are kept as they were. Beside each line, what changed against the index and against HEAD, with Stage and Revert for each change, and the line's blame through unsaved edits. A file changed on disk reloads by itself, or asks first if you've edited it. Saving writes a new copy and swaps it in, and refuses if the file changed under you |
| Changes | Files, diff and commit message on one screen. A click shows a file's diff beside the list, a double click or `Space` stages it, and `↑ ↓` walk the files. Staged, unstaged and conflicted files as a flat list or a tree, with folders you can close, and stage or unstage as one. In a pane too narrow for both, a click opens the diff in a tab, and any row's menu can too. During a rebase or similar, Continue and Abort take the commit button's place |
| Themes | Seven dark and five light themes, with nineteen named accent colors or any custom color; named accents are tuned for dark and light, and tests check their contrast in every theme |
| Command palette | `Ctrl+P`: commands, branches, and files in one search, with recent commands first. Start with `>` for commands, `@` for branches, `/` for files, or `:` for commits. With no repository open it offers what works there: open, clone, scan, recent repositories, settings and credentials |
| Window | Every window, helpers included, has the app's own titlebar, and the main one has an icon rail of views with tooltips, so it looks the same on every platform; an About box shows the commit a build came from (also `gitem --version`) |
| Modes | Zen mode (`Ctrl+Shift+Z`) hides the side panels; dockable tabs replace detachable views |
| Terminal links | Not applicable. A Git console shows every command gitem runs and lets you run your own |
| Navigation | The rail has Home, Graph, Changes, Search and Launchpad, with everything else in its More list, whose last view gives way to the next so tabs don't pile up. Any view in More can be pinned to the rail, and a right-click on a rail icon takes it off again; Settings can reset the rail. Settings can put every view on the rail, and put each view's name beside its icon. `Ctrl+P` and the title bar's search field reach everything |
| Small windows | Details gives way first, then the sidebar, and their toolbar buttons show them over the view. With many repositories open, tabs shrink, and those that don't fit go in a list. The Details panel only shows beside views that have commits |
| Dialogs | They dim what's behind them and take its clicks, leaving the window's own controls and title bar working. Enter submits one from a one-line field. Red is kept for what can lose work, and only those won't answer to Enter |
| Accessibility | Whatever has the keyboard gets its own outline. Settings has colour-blind safe diffs (blue and orange) and reduced motion. Zoom keys stay in step with the zoom setting |

### Beyond GitLens

- **Visual diff tool:** side-by-side and unified, syntax highlighting, word-level changes, stage or discard by hunk or line, whitespace and algorithm options, image comparison. Works as git's `difftool`.
- **Visual merge tool:** Ours, Result, Theirs (optionally Base) with synchronised scrolling, per-conflict resolution, inline editing, and auto-resolved regions shown in context. It starts from the file on disk, so hand edits and earlier partial saves are kept, and it asks before saving over a file changed since it was read. Works as git's `mergetool`.
- **Credential manager:** API tokens and git HTTPS credentials in Windows Credential Manager, a git credential helper, removal of stale Git Credential Manager entries, SSH key generation and agent management, connection tests, commit-signing setup, and dialogs for git's password and passphrase prompts.
- **Repository tools:** reflog recovery, guided bisect (including automated runs), submodules, hooks, `.gitignore` editing with path testing, clean with preview, Git LFS, sparse checkout, maintenance, and a config editor.
- **Docking:** drag tabs to split or rearrange the workspace, or right-click a tab to split it right or down, keep a preview open, close the others or those to its right, or have its pane fill the window (`Ctrl+Shift+M`) while the rest of the layout waits. Tabs show their kind's icon and a dot for unsaved edits. The palette has two layouts, **Layout: Git** (overview, graph and changes) and **Layout: IDE** (the editor with Changes beside it, the Console below and the Files sidebar), and saves your own under a name for any repository. Choosing a layout keeps every open tab. `F6` and `Shift+F6`, or `Ctrl+K` then an arrow, move between panes.
- **Your own tools:** a file's menu opens it in your diff tool, by the name git knows or git's own setting, and Settings > Agents and integrations chooses the terminal: cmd, PowerShell, Windows Terminal, Git Bash, or a program of your own.
- **Cleaning up:** Clean up branches lists those merged or gone from the remote, and a branch you aren't on can be fast-forwarded from its menu.
- **Undo:** after a merge, rebase, pull, reset, cherry-pick, revert, branch delete, stash drop, discard, or delete of untracked files, the toolbar's Undo says what it will take back, and Redo follows one that moved HEAD. Undoing a rebase also puts back other branches it moved with `--update-refs`, except one that has moved since or is now checked out in another worktree, which stays and is named. Only the last thing done is on offer, and it says so if things have moved since. `Ctrl+Alt+Z` does the same. Clean, and deleting a branch on a remote, can't be undone.
- **One window:** with Use one window on, opening a folder or a link from a terminal goes to the window already open. Settings can also add Open in gitem to Explorer, and take it away.
- **Keys:** Settings > Workflow changes the keys for the main commands and for lists and views, such as `J` and `K` in the graph, `Space` in Changes and the rebase letters. A key already doing something in the same place is refused, and the setting says so. F1, Esc, Enter, Tab, the arrows, Ctrl+Enter, Ctrl+Tab, Ctrl+1 to Ctrl+9 and Ctrl+Shift+Z stay as they are, since they're how you get around at all.
- **Stash Hunks:** from the palette or a changed file's menu, tick some of the unstaged hunks to stash. Popping brings them back with the rest of the file still changed, which `git stash --patch` refuses. A file with staged changes too, a new one, a binary one and one that isn't UTF-8 say why they can't. If taking the hunks out stops partway, the files go back to what they were, and nothing's stashed.
- **Branch model:** off by default. Settings > Workflow turns on starting and finishing feature, release and hotfix branches, in the spirit of git flow, with names and prefixes of your own, from the palette, Home and a branch's menu. Finishing refuses with changes in the tree, asks first, and has no Undo. A conflict stops it there, and finishing again once it's resolved carries on.
- **Several branches:** `Ctrl+click` picks branches in the sidebar, to delete together with the question and the Undo that Clean up branches has.
- **SVG:** drawn in the file view and in a diff's Pictures, off the UI thread. A repository's SVG is treated as hostile: size, depth, repetition, entities and filters are limited before it's drawn, nothing the file names is read, and scripts don't run. Text is drawn from this computer's fonts and gitem's own, and a font or character that isn't there is said. Pictures embedded in one aren't drawn. Each is drawn in a process of its own, at most two at once, which is killed after 10 seconds or when the view closes or moves to another file. One that's too slow or crashes the renderer says so, with Source still there. That process doesn't outlive gitem, however gitem ends. It isn't a sandbox: it has your rights, and what keeps it from your files is that nothing in it opens what the file names.
- **No repository open:** the welcome page reaches Pull requests I'm in and Credentials. Nothing is asked of a host until you click, and each pull request offers to open its repository, or clone it if there's no copy.
- **Notifications:** Settings > Workflow can add a system notification to the taskbar's flash when something long finishes or anything fails with the window behind others. It's off by default, and has none of git's output. Nothing is watched or polled for it. On Linux it needs `notify-send` and on macOS it uses `osascript`, which may ask to be allowed. Without them nothing shows, and nothing else changes.
- **Staging from the graph:** a setting makes the Work in progress row show the list and the commit box in Details, the same message as in Changes.

### Differences from GitLens

This was compared against the GitLens README, Help Center, and changelog through 19.5.0 (November 2026).

**Not included, because they're GitKraken's own services:** GitKraken sign-in and AI credits, Cloud Patches links and hosted patch storage (the patch library and secret gists cover sharing), Cloud Workspaces sync (workspaces are local), and Kepler. gitem has its own equivalents for Code Suggest (GitHub review suggestions), deep links (`gitem://`), and the MCP server (`gitem --mcp`, read-only).

**Partial:**

- **Agent sessions** are read from the files Claude Code, Codex, Copilot CLI, and OpenCode write, so status is an estimate. You answer permission prompts in the agent, not in gitem. Copilot's format is undocumented and OpenCode's has changed between releases, so both may need updating. OpenCode sessions show no branch, because it doesn't record one.
- **Localization** is experimental, as in GitLens: Spanish, Turkish, and Simplified Chinese cover navigation, toolbars, and context menus. The rest is English.
- **Suggested changes** only work on GitHub.

**Not included, though not GitKraken's:** PR and issue titles on autolink hovers, a conflict panel in the rebase editor (it names the first commit that would conflict, in a line above the plan), Launchpad with no repository open beyond Pull requests I'm in, inline check logs, and pull requests or issues over MCP, which would give an agent your token's reach.

**Not applicable:** follow-the-terminal and terminal links, since gitem has no integrated terminal.

gitem isn't an editor plugin, so the following differ by design.

- Full blame and annotations appear in gitem's read-only file view. The editor shows the current line's blame and change marks; it has no language server, so no completion or go-to-definition. Use **Settings > External editor** to jump to your own editor for those.
- Patches and workspaces are stored locally; there's no GitKraken account or cloud sync.
- Code lens finds symbols with a regex rather than a language server, so it can miss some declarations.
- Bare repositories can't be opened, because gitem works from a working tree. It says so, and names any linked worktrees to open instead. A linked worktree of a bare repository opens as usual.
- The in-app rebase editor works on linear history: merge commits in the range aren't kept, and the view says so. It refuses to start if HEAD has moved since the plan was made. For `--rebase-merges`, use **Rebase onto…**, which has that option, or run the rebase from the command line with gitem as `sequence.editor`.
- Hunk and line staging need UTF-8 text. For other encodings, stage the whole file. The merge tool offers a choice of versions for such files rather than rewriting them. A valid file that really contains U+FFFD is treated the same way.

Scan for repositories, on the welcome screen and the + menu, finds them in a folder and opens the ones you tick. Dropping a folder or a file on the window opens its repository. Branches with `/` in their names fold into a tree in the sidebar, where Up and Down move through the rows and Right and Left open and close them.

## Using gitem from the command line

Register these from **Credentials > Git integration**, or set them yourself. The MCP server's line is in Settings, and links have their own button there:

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
| `F1` | List these shortcuts, with a search box |
| `Ctrl+P` | Command palette |
| `Ctrl+Shift+G` | Changes |
| `Ctrl+Shift+H` | Graph |
| `Ctrl+B` | Toggle sidebar |
| `F5` | Refresh |
| `Ctrl+O` | Open a folder |
| `Ctrl+,` | Settings |
| `Ctrl+W` | Close the tab, asking first if that loses something |
| `Ctrl+Tab` `Ctrl+Shift+Tab` | Next and previous tab in the pane |
| `Ctrl+1` to `Ctrl+9` | Go to that repository |
| `Ctrl+Shift+F` `Ctrl+Shift+L` `Ctrl+Shift+K` | Fetch, pull, push |
| `Enter` | Submit a dialog from a one-line field; answer a question that loses nothing |
| `Ctrl+Enter` | Submit the open form. With none open, or from the commit box, commit |
| `Esc` | Close a dialog or menu |
| `Tab` | Move between controls. What has the keyboard gets an outline |
| `Shift+F10` | Open the menu of the current row, in Changes and the graph |
| `↑ ↓` | Move through the files in Changes, or the commits in the graph |
| `Ctrl+click` | Pick several branches in the sidebar, to delete together |
| `J` `K` | Next and previous commit in the graph |
| `Space` | Stage or unstage the current file in Changes |
| `Alt+↑, Alt+↓` | Previous and next change in a diff |
| `Alt+Up, Alt+Down` | Move a commit in the rebase editor |
| `P` `R` `E` `S` `F` `D` | Set the rebase action |
| `Ctrl+F` | Find in a file or a diff; search in the graph |
| `/` | Find a ref in the graph |
| `F3` `Shift+F3` | Next and previous search match |
| `Alt+B` | Toggle blame in a file |
| `Ctrl+Shift+E` | IDE mode on or off: a click in the Files tree opens the file for editing |
| `Ctrl+S` | Editor: save |
| `Ctrl+Shift+M` | Fill the window with this pane, or bring the layout back |
| `F6`, `Shift+F6` | Next and previous pane |
| `Ctrl+K`, then an arrow | Go to the pane that way |
| `Ctrl+H`, `Ctrl+G` | Editor: replace, go to line |
| `Ctrl+D`, `Ctrl+F2` | Editor: select the next match, or all of them, for typing in several places |
| `Ctrl+Shift+Up`, `Ctrl+Shift+Down` | Editor: add a caret above or below |
| `Alt+Up`, `Alt+Down` | Editor: move lines, or with Shift copy them |
| `Ctrl+/`, `Ctrl+L`, `Ctrl+J` | Editor: toggle comments, select the line, join lines |
| `Ctrl+Shift+Z` | Zen mode |
| `Ctrl+Alt+Z` | Undo the last action |

## Words

The same words are used throughout, and F1 lists them too.

| Word | What it means |
|---|---|
| Check out | Make a branch or commit the one you're on. Git's `switch` |
| Changes | What you've edited and not committed. Unstaged until you stage it |
| Stage | Add to what the next commit holds |
| Graph | The history, with its branches drawn as lanes |
| Details | The panel about the selected commit |
| Launchpad | Pull requests and issues on the repository's host |
| Review | Your own notes on a diff. Review with AI adds to them |
| Compose | Split uncommitted changes into several commits, in the Composer |
| Recompose | The same for commits you've already made |
| Rebase | Replay commits on another base, or edit them. Git's `rebase -i` |
| Upstream | The remote branch a branch follows, for its counts, Pull and Push |
| Patches | A local library of saved changes |

## Safety

- Destructive commands ask first and show the exact git command, including a plain force push from the push form, and any force push of the default branch, lease or no lease. You can turn this off in Settings. Amending a commit that's already on its upstream, or planning a rebase over such commits, says what that will mean. Deleting a stored secret asks too, and a revealed secret hides itself after 30 seconds or when you leave its page.
- Unstaging during a merge, cherry-pick or rebase keeps the operation and its conflicts. The Composer waits until the operation is finished, except while a rebase is stopped at an edit step, where splitting a commit is the point.
- The Composer keeps its plan until each commit has landed, leaves other staged files staged, and asks before committing a partly staged file whole.
- Closing a tab, a repository or the window asks first if that would lose unsaved merge resolutions, a written commit message, a Composer plan, typing in an open dialog, or a running or queued git command. A fetch doesn't count. So do a rebase plan you've changed, a pull request or review you're writing, and review notes being typed.
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
- Checking out a branch with uncommitted changes in the way doesn't just fail: it offers to stash them first, or bring them along with `git switch --merge`, which can leave conflicts.
- A pull request's Check out replaces what `pr/N` held, so it still works after its author force-pushes, but asks first if commits of yours are there, offering to keep them on a new branch. It follows the pull request's branch so Pull works. On that branch, Update brings in new commits and Reset to its tip handles a rewrite, asking first. A fork's branch is told from one of the same name here.
- Push from `pr/N` goes back to the pull request's own branch. A pull request from a fork has Push to the fork, which adds a remote for it. The host decides whether you may.
- Settings > Hosting can name a PEM file of certificate authorities for hosting and AI requests. It replaces the system's store for them, as git's `http.sslCAInfo` does, and git's own connections go by git's settings.
- Launchpad reads up to 200 pull requests or issues per source, a page at a time, and says when it stopped there. It asks the host again for F5, a saved token, a merge, a comment or a changed remote, and not when an unrelated Text tab finishes.
- A remote's port and plain http are kept for API calls and links. A token saved for a host isn't sent to another port or over plain http: those count as a different host, and need a token of their own. Public hosts such as github.com always go to their real API.
- A host name containing `bitbucket` other than `bitbucket.org` counts as Bitbucket Server, and Azure DevOps's older address forms work. Bitbucket Connect links to API tokens and asks for the Atlassian email, as app passwords have stopped working.
- A write to the host that gets redirected fails and says so, where it used to be turned into a read that looked like success. A reply that's cut off, too large, or not a list is an error, not an empty list. A sign-in page or a rate limit is explained, with how long to wait if the host says.
- Certificates for hosting and AI requests are checked against the system's trust store, so a company's own authority works if Windows trusts it.
- AI replies cut off at the length limit are errors, not half an answer, and a busy provider is tried twice more, waiting as long as it asks for up to 30 seconds.
- "Add git credential" saves for the scheme and host, which is what git asks about, unless told to keep the path for `credential.useHttpPath`.
- The file credential store used off Windows refuses to overwrite a file it can't read, and helpers running at once no longer lose each other's writes.
- A tab asked for again gets what was asked for: a second search, compare, patch, pull request form, AI answer or line in a file takes over the open tab. An answer that can still be undone keeps its tab and the new one opens beside it.
- Undo commit asks first. Once the commit is on the remote it says so, and on a first commit it says there's nothing to go back to.
- Start work makes a branch with no upstream, whatever it starts from, so the toolbar offers Publish.
- Scheduled maintenance in Repository Health runs `git maintenance start`, which adds a task to your system's scheduler. Undo takes this repository off git's list and leaves the task, as other repositories may use it. An option set in your global config says so, and Undo stays off: it couldn't change it.
- Contributors, the graph's Changes column and Visual History follow `.mailmap`. A person's heatmap and Search their commits find exactly that address, in any case.
- Clean names any repository inside this one that `git clean` leaves alone. Removing the worktree a tab has open isn't offered, as git would empty it and then fail. A locked worktree is removed with its Force remove.
- A reviewed mark goes away when the file changes after it was marked.
- Secondary text and status colours now reach WCAG AA on panels and cards in every theme, and button text picks black or white by fill. Side-by-side diffs show + and - signs for readers who can't tell red from green.
- Settings can put every preference back to how gitem came, keeping open and recent repositories, workspaces, favorites and secrets.
- The file view shows PNG and JPEG pictures, reads UTF-16 by its byte order mark, and shows other files that aren't UTF-8 as Latin-1, a guess that it labels as one. The diff view compares only PNG and JPEG.

## Project layout

```
crates/gitem-core   Everything except UI: git runner, parsers, ops, diff and
                    merge engines, graph layout, forges, credentials, AI
crates/gitem        The egui app, its views, and git helper modes
```

Write operations are data (`ops.rs`), so git commands can be logged, confirmed, and queued in one place. What isn't a git command doesn't go through it: renaming or deleting a hook, adding to an ignore file, settings, and the registry entries for links and Explorer. Those are direct file or `reg` writes, each from its own button, and don't appear in the Console.

## Tests

`cargo test --workspace` runs:

- **Unit tests** for parsers, the merge engine, graph layout, patches, URL building, and API response parsing.
- **Integration tests against real git**, in temporary repositories.
- **Remote tests** that run real clone, fetch, pull, and push against a bare repository in a temp folder, covering rejected and forced pushes, conflicts, hooks, pruning, tags, and cancelling.
- **HTTP authentication tests** that run real git against `git http-backend` behind a Basic-auth bridge on localhost, through gitem's credential helper.
- **HTTP client tests:** the hosting, issue-tracker, and AI clients make real requests to a mock server on localhost, including paging, redirected writes, cut-off replies, and a certificate nobody vouches for.
- **Integration UI tests** (`crates/gitem/tests/integrations.rs`) that click through Launchpad's actions against a mock host, which also serves a real bare repository over HTTP, so Check out runs real git. The Agents view reads a scratch folder, and `ssh-keygen` writes to one.
- **Headless UI tests** that click real widgets, right-click for context menus, drag refs, and drive the helper windows, then verify the result with git or on disk.
- **Helper tests** that have real git invoke the built binary.
- **Property tests** (`crates/gitem-core/tests/properties.rs`) of partial patches through real `git apply`, the merge engine, and the parsers. Case counts are small by default: set `PROPTEST_CASES` for a longer hunt.
- **Performance tests** that fail if parsing stops scaling linearly (`crates/gitem-core/tests/perf.rs`), if a frame gets slow, or if an idle window starts running git, with very large histories, diffs, files, ref lists and statuses. The harness doesn't paint, so these don't measure drawing.
- **Daily flows** (`crates/gitem/tests/daily_flows.rs`, `crates/gitem-core/tests/undo.rs`, `crates/gitem-core/tests/daily_ops.rs`): undo, branch cleanup and the smaller options, against real git, with the update check against a mock on localhost.
- **Lifecycle tests** (`crates/gitem/tests/lifecycle.rs`) of real startup, exit, the saved session, and old, new and broken settings files.

A contract test against a real Gitea, which needs a container, is opt-in. How to run it is at the top of `crates/gitem-core/tests/gitea_contract.rs`.

Tests ignored with a bug's ID, such as `#[ignore = "PF-5, Phase 5"]`, show a known bug and fail until it's fixed.

Opt-in tests have real git open the real windows, including the password prompt, which answer by themselves through `GITEM_TEST_ACCEPT`: `cargo test -p gitem --test real_git_windows --test http_auth --test helper_modes -- --ignored --test-threads=1`. Debug builds read that variable, `GITEM_SHOT` and `GITEM_SVG_TEST`; release builds ignore them.

Tests create temporary repositories. Every test binary starts with its own scratch git config, settings, secrets, and home folder (`crates/gitem-testenv`), so results don't depend on the machine and nothing reaches your real profile. That includes the OS vault: the one test of the real Windows vault is opt-in, with `cargo test -p gitem-core --lib windows_vault_round_trip -- --ignored`.

**What the tests don't cover:**

- Remote operations never run against a hosting service or over HTTPS, and SSH remotes and passphrase prompts aren't exercised.
- Hosting, issue-tracker, and AI requests go to the local mock, so they prove gitem's side of each exchange against sample responses, not that a real service accepts them. Try these with your own accounts before relying on them.
- Moving and resizing the custom window were checked by hand-style mouse input on Windows only, not in the test suite. On other platforms window corners stay square, and cancelling kills only git itself, not what it started.
- Native file-picker dialogs aren't automated. Every picker also has a text field.
- The installer has never been built or run: its script is only read by a test. The AppImage has never been built either: its script ran only up to a stand-in for appimagetool, on Windows. The release workflow has never run. A system notification has never been seen: the decision and the call are tested with a stand-in. On Linux and macOS it has only been compiled, with the arguments it would pass tested on Windows, and the SVG process's handling on those two has never run. Registry entries for Explorer are checked as the commands they'd run, and never written. A flashing taskbar wasn't seen. Handing over is tested through its socket and the window's code in one process, and a real second launch was run by hand once: it ended in about a quarter of a second and the first window took the repository, on Windows.
- Copilot CLI and OpenCode reading is tested against sample data. No real Copilot session was available, and the real OpenCode database had no tool calls in it.
