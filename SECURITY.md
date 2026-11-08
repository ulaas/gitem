# Security

## Reporting a vulnerability

Please don't open a public issue. Use GitHub's private reporting instead: the repository's **Security** tab, then **Report a vulnerability**. Include what you did, what happened, and the gitem version (`gitem --version`) and operating system.

You'll get an answer within a week. Fixes go into the next release, and the advisory is published once one is out.

## What's in scope

gitem opens repositories you didn't write, so these matter most:

- A repository's contents or configuration making gitem run a program without your say-so (hooks, filters, `core.fsmonitor`, external diff or merge drivers, and the like). gitem asks before trusting a repository's config that runs programs; a way around that is a vulnerability.
- Tokens, passwords or keys leaving the system credential store, showing up in logs, the Console view, crash reports or AI requests.
- Files outside the repository being read or written through a crafted path, link or patch.
- The `gitem://` deep links or the read-only MCP server (`gitem --mcp`) doing more than they say.

Bugs that need someone to change their own settings to something unsafe first, or that only crash the app, are welcome as ordinary issues.

## Supported versions

Only the latest release gets fixes.
