# Better Nectar

**A notch and a dock for Windows.** Better Nectar is an unofficial, modified version of [Nectar](https://github.com/kua8/Nectar) by kua8. It is not affiliated with or endorsed by kua8. Bugs in Better Nectar belong here, not in the Nectar repository.

## What is different from Nectar

- **Claude Code in the notch:** every Claude Code session with its state, permission requests you can answer from the notch, a usage meter, and a pixel cat that reacts to the agents.
- **Clipboard history** (text and images) and a redesigned shelf and clipboard panel.
- **Dock system area:** tray, network, volume, clock and date, notifications and a mini player, with Nectar menus instead of Windows Quick Settings, and a hybrid dock mode.
- **Notch notifications, downloads in the notch, a microphone and camera in-use dot, and a meeting-mode hotkey** that mutes every microphone.
- **A first-start screen:** nothing touches your taskbar, logon tasks or location until you choose.
- **Its own name, settings folder and updates**, so it never collides with an installed Nectar. Quit Nectar before you start Better Nectar; the two do not run side by side.

Not in this version: Nectar's phone link (phone ↔ PC transfer). It is single-owner today and may come back later.

## Install

1. Download `better-nectar_<version>_x64-setup.exe` from the [latest release](https://github.com/vekilberke/better-nectar/releases/latest).
2. Run it. It installs for your user only and needs no administrator rights.
3. Windows SmartScreen warns you, because the installer is not code-signed: choose **More info › Run anyway**.
4. On first start, pick what you want (dock, start with Windows, weather location, clock, Claude Code). Everything can be changed later in Settings.

**Smart App Control:** on Windows 11 with Smart App Control on (including its evaluation mode on new installs), unsigned apps are blocked outright and there is no "Run anyway". Better Nectar cannot be installed on such a PC until it is code-signed. Do not turn Smart App Control off for it: Windows only lets you turn it back on by reinstalling Windows.

**Windows versions:** Windows 11 is supported. Windows 10 22H2 is best effort.

Better Nectar updates itself from this repository's releases: a dot on the notch means an update is ready, and **Settings › About** installs it. Turn on **Get pre-releases** there to receive test builds first.

<details>
<summary>Verify your download</summary>

<br />

Only download Better Nectar from the [releases page](https://github.com/vekilberke/better-nectar/releases). Each release has a `SHA256SUMS.txt`; compare it with:

```powershell
Get-FileHash .\better-nectar_<version>_x64-setup.exe -Algorithm SHA256
```

</details>

## Uninstall

Windows **Settings › Apps › Installed apps › Better Nectar › Uninstall**, or **Settings › About › Uninstall** in the app. Uninstalling puts the taskbar back, removes the logon task, and removes Better Nectar's Claude Code hooks and status line (a status line you had before comes back). Your settings stay in `%APPDATA%\io.github.vekilberke.betternectar` in case you reinstall; delete that folder to remove them too.

## Getting the taskbar back

Better Nectar hides the Windows taskbar while the dock is on. If the app stops without putting it back:

1. Start menu › **Restore Windows taskbar (Better Nectar)**, or start Better Nectar again (it restores the taskbar on start).
2. If that does not help: Task Manager › **Windows Explorer** › Restart, then Settings › Personalization › Taskbar › Taskbar behaviors › turn **Automatically hide the taskbar** off.

## Usage

**Notch panels.** Scroll or swipe on the notch, or use the tab strip, to change panel: Control Center, music, status, calendar, shelf, clipboard and AI agents.

**Visibility modes.** The notch and the dock each have **Fixed** (always visible), **Smart** (hides when a window overlaps it, reveals on hover) and **Peek** (hidden until the cursor reaches the edge). In Smart and Peek both stay hidden over fullscreen apps.

**Claude Code.** Settings › Notch › AI agents. With Claude Code installed, Better Nectar adds its hooks to `~/.claude/settings.json` (the original file is kept as `settings.json.nectar-backup`) and removes them when you turn it off. See [docs/agents.md](docs/agents.md).

All settings are documented in [SETTINGS.md](SETTINGS.md).

## Reporting a bug

Open an [issue](https://github.com/vekilberke/better-nectar/issues/new/choose). **Settings › About › Copy Diagnostics** helps a lot: it has your versions, monitors and recent log, with your user name, folders and location removed. Read it before you post it, or send it privately if you prefer.

## Build from source

See [BUILDING.md](BUILDING.md).

## License and credits

Better Nectar is free software under the [GNU General Public License v3.0](LICENSE), with no warranty. See [NOTICE](NOTICE) for what was changed and when, and [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) for the libraries it ships.

- Based on [Nectar](https://github.com/kua8/Nectar) by kua8 (v1.9.3).
- Nectar began as a fork of [Bloom](https://github.com/SehajveerSingh2005/bloom) by Sehajveer Singh.
- The pixel cat is based on SondeR cat by Verisonder (Apache-2.0, see `third_party/sonder-cat/`). The cat is not the product's logo.
