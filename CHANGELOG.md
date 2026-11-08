# Changelog

## Unreleased

Added:

- IDE mode (`Ctrl+Shift+E`): a click in the Files tree opens the file for editing, as a preview the next click replaces, and a double click keeps it. Edit is in every working-tree file's menu either way. The editor has syntax colours, several carets, find and replace, bracket pairs, auto-indent, comment toggling, moving and copying lines and go to line, and keeps line endings, a byte order mark and UTF-16 as they were. It marks what changed against the index and HEAD beside each line, stages or reverts one change at a time, shows the line's blame through unsaved edits, reloads a file changed on disk or asks first if it's been edited, and asks before a checkout, pull or the like rewrites a file with unsaved edits.
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
