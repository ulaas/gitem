# Changelog

## Unreleased

Changed:

- Branches wear the colour of their lane in the graph: their badges there and in Details, and their names and dots in the sidebar's branch list, so you can tell at a glance which line is which branch. Tags and stashes keep their own colours, and a branch that isn't in the graph keeps the plain look.

## 0.7.1 (2026-11-10)

Changed:

- Every one-line text box has a × at its end while there's text in it, which empties it and leaves the keyboard there: find in files, filters, branch names, settings and the rest.

## 0.7.0 (2026-11-10)

Added:

- Find in files, with Ctrl+Shift+F: the text of every file, searched as you type, in the sidebar's new Search view or in a tab of its own. Results come in as they're found, grouped by file with the matches marked and git's mark beside each file, and a click opens the file at the match, selected. Match case, whole words and regular expressions are switches beside the box (Alt+C, Alt+W, Alt+R); files to include and exclude take globs such as `*.rs` or `src/**`; what `.gitignore` leaves out is skipped unless you ask. Look in the whole working tree, only the changed files, only the files open in editors (unsaved edits included), or the files at any commit, branch or tag. F4 and Shift+F4 go through the results, Up and Down in the box bring back earlier searches, and Find in folder… in the Files tree looks in one folder. The results keep up as files change. The search runs inside gitem on every core and finds the first result in a few milliseconds even in a large tree. At a revision, git hands over the files and the same search reads them, so regular expressions, searches across lines, the globs and the size limit work there as they do on disk. Settings, Editing has its own group: search as you type or on Enter, globs always left out, the largest file searched, and how many files are read at once. It replaces Search contents.

Changed:

- Fetch's key is now Ctrl+Shift+J: Ctrl+Shift+F is Find in files, as in most editors. Keys you've set yourself in Settings stay as they are.

- Home is laid out again as cards: the repository's name and host at the top, four tiles for the branch, changes, stashes and worktrees, quick-start tiles, recent commits with their authors, recent branches and workspaces. It stays readable on a wide window and stacks into one column on a narrow one.
- Home has Git on this computer, for the global config every repository starts with: your name and email (with a warning when they aren't set), the default branch name, how pull joins branches (merge, rebase or fast-forward only), line endings, publishing new branches on first push, pruning on fetch, remembering conflict resolutions and signing commits. Where this repository sets its own value, it says so.

## 0.6.1 (2026-11-08)

Changed:

- Check for updates (off unless you turn it on in Settings) asks GitHub for the latest release, the first one with downloads.
- The rail's Editor icon turns IDE mode on and off, and is lit while it's on, rather than opening an empty editor tab.
- The file view has Edit, opening the file in the editor at the line you were reading, and the editor has Blame, leading back to it.
- Fetch, Pull and Push in the toolbar show their action under way, with git's percentage and a bar along the button, and can't be pressed again until it's done, nor can their keys. Waiting behind something else, they say queued. The same goes for the buttons in Tools, Repository Health, Clean up branches and a pull request's Update, and the commit button says Committing… while it does.
- A file opens in the editor from history as well: the file view at an older revision has Edit current, a blame line's menu has Edit at this line, a diff's toolbar has Edit, at the first change, and every file menu on a revision has Edit the current version. Each opens the file as it is now.
- A double click on a preview's tab, a diff or a file opened from the Files tree, keeps it open, as Keep open in its menu does.
- A changed file in Changes shows small buttons under the pointer: open it in the editor, discard its changes or delete it if it's new (asking first), and stage or unstage it. A folder in the tree has the same for everything in it, discarding only its tracked files. The right-click menu is unchanged.

Fixed:

- Reveal in file manager opens the folder in the file manager Windows has registered for folders, such as Directory Opus, Files or XYplorer, rather than always in Explorer. With Explorer it still selects the file.
- On Windows, switching keyboard layout while gitem ran (Win+Space, Alt+Shift) froze it, and no key worked after. The renderer's OpenGL backend, which gitem doesn't draw with, kept a hidden window that never answered the switch; it's no longer started. `WGPU_BACKEND` can still choose it.

## 0.6.0 (2026-11-08)

Added:

- IDE mode (`Ctrl+Shift+E`): a click in the Files tree opens the file for editing, as a preview the next click replaces, and a double click keeps it. Edit is in every working-tree file's menu either way. The editor has syntax colours, several carets, find and replace, bracket pairs, auto-indent, comment toggling, moving and copying lines and go to line, and keeps line endings, a byte order mark and UTF-16 as they were. It marks what changed against the index and HEAD beside each line, stages or reverts one change at a time, shows the line's blame through unsaved edits, reloads a file changed on disk or asks first if it's been edited, and asks before a checkout, pull or the like rewrites a file with unsaved edits. Word wrap (`Alt+Z`), folding by indent, a minimap, Windows-1252 files saved in their own encoding, palette commands for the editor, and search hits opening in it. Turning IDE mode on offers the IDE layout. An outline of the file's definitions (`Ctrl+Shift+O`), read from the syntax with the optional `tree-sitter` build, which folds by syntax too. Saving when the window loses focus, as a setting. A place for a language server to plug in, with its problems underlined.
- Docking: a tab's menu splits it right or down, keeps a preview, closes the others or those to its right, and has its pane fill the window (`Ctrl+Shift+M`). Tab icons and an unsaved dot, a themed drop overlay, two built-in layouts and saved ones that keep every open tab, and `F6` / `Ctrl+K` then an arrow between panes.
- Any view in the More list can be pinned to the rail, and any rail icon but Settings taken off it.
- A Pull Request tab with files, diffs, checks, conversation, a verdict, comments on a line, and a choice of merge method. Everything you're involved in across a host, in Launchpad. A commit's pull request, in Details.
- Undo in the toolbar for merge, rebase, pull, reset, cherry-pick, revert, branch delete, stash drop, discard and deleting untracked files, with Redo after one that moved HEAD.
- Clean up branches, and fast-forwarding a branch you aren't on.
- Find in a diff, search of file contents, a conflict check in the rebase editor, and a push of a whole stack.
- The commit template and recent messages in the commit box, resetting author or date on amend.
- Choices of remote, replacing and all-at-once for tags. Merge favouring a side, with your own message. Cherry-pick of a merge commit or a range. Revert of a graph selection. Partial clones, and unshallow.
- Replies within a review thread on GitHub, GitLab, Bitbucket, Bitbucket Server and Azure DevOps, with the conversation shown in threads. Gitea says it can't.
- A line in the status bar across open repositories, with a row for each that goes to it.
- With no repository open: the palette, Pull requests I'm in, Credentials.
- Stash Hunks, whose stash pops back with the rest of the file still changed.
- An opt-in branch model: start and finish feature, release and hotfix branches.
- SVG drawn in the file view and diffs, in a process of its own, with hostile files refused or kept to size, and the process killed after 10 seconds or when the view moves on.
- Several branches picked in the sidebar with `Ctrl+click`, deleted together.
- Keys for lists and views can be changed, as well as the main commands.
- Optional: a system notification on Windows, Linux (`notify-send`) and macOS (`osascript`), the last two never yet run; names on the rail, staging and committing in Details.
- An AppImage script and a release job for it, neither of them run yet.
- Your own diff and merge tools and terminal. Pull all and open all in a workspace. Changeable keys. One window. Open in gitem in Explorer. Settings export and import. An installer script and an opt-in update check.

Changed:

- Buttons, small buttons, menus, text boxes and icon buttons in a row all stand 24 px tall.
- The Git page's editor setting is now called Editor program, beside gitem's own editor.
- Launchpad doesn't offer Approve on your own pull request, or Merge on a draft or one with conflicts.
- A force push of the default branch asks first, even with a lease.
- Everything I'm in asks every host your recent repositories use that has a saved token, not just this repository's, and only when you click. Each host's failure shows beside what the others found.
- A restored Search or Compare tab keeps its query, and waits for you to run it.
- Undoing a rebase puts back the other branches it moved with `--update-refs`, unless they've moved since.
- Check for conflicts says plainly that it needs git 2.40, on an older one.
- The graph holds much less memory on a history with many lanes, and draws only the lanes its column has room for.

Fixed:

- A remote branch whose name is taken goes to the local branch rather than failing.
- A pull request with no date from its host no longer says `56 years ago`.
- Rebase stack goes onto what the bottom of the stack sits on. It used to go onto the branch just below, which moved nothing else.
- Replies on Bitbucket Server, which the conversation left out, are shown.
- Tag, stash, worktree and submodule rows in the sidebar have names for a screen reader.

Earlier work isn't listed here. `git log` has it.
