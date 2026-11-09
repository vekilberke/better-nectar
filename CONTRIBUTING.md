# Contributing to Better Nectar

Thanks for wanting to help. Bug reports, ideas, translations and pull requests are all welcome.

## Before you start

- **Found a bug?** Open an [issue](https://github.com/vekilberke/better-nectar/issues/new/choose) with the bug form. The version, your Windows build, and **Copy Diagnostics** from Settings > About make a big difference.
- **Want to send a pull request for a new feature?** Please open an issue first so we can agree on it before you spend time on it. Bug fixes, translations and documentation fixes can go straight to a pull request.
- **Security problem?** Do not open an issue; see [SECURITY.md](SECURITY.md).
- **A bug that is also in upstream Nectar?** Report it here; it may be fixed in [kua8/Nectar](https://github.com/kua8/Nectar) too, but Better Nectar is a separate project.

## Setting up

See [BUILDING.md](BUILDING.md) for the toolchain. In short:

```bash
git clone https://github.com/vekilberke/better-nectar.git
cd better-nectar
bun install --frozen-lockfile
bun run tauri dev
```

Checks before you push:

```bash
bun x tsc --noEmit
bun test
cd src-tauri && cargo test --bin nectar
```

To try a release build, run `bun run tauri build --no-bundle`. The exe is `src-tauri/target/release/better-nectar.exe` and needs `WebView2Loader.dll` next to it (see BUILDING.md).

## Pull requests

- Keep one change per pull request. A small, focused change is reviewed much faster.
- Say what changed and why, and which Windows version and monitor setup you tested on.
- Test on a real build. Many things, like the dock, the taskbar and the tray flyout, behave differently in a dev server than in the real app.
- If you add or change a `nectar-*` setting, document it in [SETTINGS.md](SETTINGS.md) in the same pull request.
- User-facing text goes in `src/locales/en.json`. Add the same keys to the other languages if you can; missing keys fall back to English.
- Keep code comments to a minimum. Match the style of the code around your change.
- Do not commit generated files, build output or your own settings.

## Translations

Translations live in `src/locales/<code>.json`. To add a language, add the file, list it in `src/i18n.ts`, and add it to `LOCALES` in `src-tauri/src/i18n.rs` so the tray menu is translated too. A partial translation is fine.

## License

Better Nectar is licensed under the [GPLv3](LICENSE). By sending a pull request you agree that your contribution is released under the same license. It is a modified version of [Nectar](https://github.com/kua8/Nectar) by kua8, which began as a fork of [Bloom](https://github.com/SehajveerSingh2005/bloom) by Sehajveer Singh; see [NOTICE](NOTICE).
