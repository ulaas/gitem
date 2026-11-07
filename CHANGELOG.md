# Changelog

## Unreleased

Added:

- A Pull Request tab with files, diffs, checks, conversation, a verdict, comments on a line, and a choice of merge method. Everything you're involved in across a host, in Launchpad. A commit's pull request, in Details.
- Undo in the toolbar for merge, rebase, pull, reset, cherry-pick, revert, branch delete, stash drop, discard and deleting untracked files, with Redo after one that moved HEAD.
- Clean up branches, and fast-forwarding a branch you aren't on.
- Find in a diff, search of file contents, a conflict check in the rebase editor, and a push of a whole stack.
- The commit template and recent messages in the commit box, resetting author or date on amend.
- Choices of remote, replacing and all-at-once for tags. Merge favouring a side, with your own message. Cherry-pick of a merge commit or a range. Revert of a graph selection. Partial clones, and unshallow.
- Your own diff and merge tools and terminal. Pull all and open all in a workspace. Changeable keys. One window. Open in gitem in Explorer. Settings export and import. An installer script and an opt-in update check.

Changed:

- Launchpad doesn't offer Approve on your own pull request, or Merge on a draft or one with conflicts.
- A force push of the default branch asks first, even with a lease.

Fixed:

- A remote branch whose name is taken goes to the local branch rather than failing.
- A pull request with no date from its host no longer says `56 years ago`.

Earlier work isn't listed here. `git log` has it.
