# Security policy

## Supported versions

Only the latest release of Better Nectar gets security fixes. It updates itself, so most people are on the latest version already. If you are on an older one, update first and check whether the problem is still there.

## Reporting a vulnerability

Please do not open a public issue for a security problem.

Report it privately through GitHub: go to the [Security tab](https://github.com/vekilberke/better-nectar/security/advisories/new) and choose **Report a vulnerability**. Only the maintainer can see the report.

Please include:

- What the problem is and what an attacker could do with it.
- The steps to reproduce it, or a small proof of concept.
- Your Better Nectar version and Windows version.

This is a one-person project, so replies are best effort. You should hear back within about a week. Once a fix is released, the advisory is published and you are credited, unless you would rather stay anonymous.

## What counts

Better Nectar runs on your machine, hides the Windows taskbar, installs and updates itself, reads calendars and talks to Claude Code over a local port. These are the areas where a bug matters most:

- The installer, uninstaller and the auto-updater, including how updates are signed and checked.
- Anything that lets a web page, calendar entry, file or dropped text run code or read files it should not.
- The local agent API (`127.0.0.1`, token in `agent.json`) and the hooks it writes to `~/.claude/settings.json`.
- Calendar passwords and links, which are stored in Windows Credential Manager, and anything that leaks them into `settings.json`, logs or diagnostics.
- Settings import and export, and the files Better Nectar writes in `%APPDATA%\io.github.vekilberke.betternectar`.

Bugs that need an attacker who already has full control of your account, or issues in Windows itself, are out of scope.

## Dependencies

Better Nectar depends on Rust crates and npm packages. If you find a vulnerable dependency that affects it, report it the same way.
