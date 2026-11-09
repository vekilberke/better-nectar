# Building Better Nectar

Windows 10/11 x64 only. The release build uses the LLVM MinGW toolchain, so neither Visual Studio nor administrator rights are needed.

## Toolchain

| Tool | Notes |
|---|---|
| [Rust](https://rustup.rs/) | Default toolchain `stable-x86_64-pc-windows-gnullvm` (`rustup default stable-x86_64-pc-windows-gnullvm`). Not `-gnu` (msvcrt): it does not link with the UCRT llvm-mingw below. |
| [llvm-mingw](https://github.com/mstorsjo/llvm-mingw/releases) | The **UCRT** x86_64 build, unpacked anywhere, with its `bin` folder on `PATH` (C compiler, linker, `windres`, `llvm-objdump`). |
| [Bun](https://bun.sh/) | Package manager and script runner (`npm i -g bun` works too). |
| Node.js 20+ | For the helper scripts in `scripts/`. |
| WebView2 Runtime | Built into Windows 11; the installer bootstraps it on older systems. |

## Commands

```bash
bun install --frozen-lockfile        # dependencies, exactly as in bun.lock
bun run tauri dev                    # development (Vite on :1420 + Rust)
bun x tsc --noEmit                   # type check
bun test                             # frontend and script tests
cd src-tauri && cargo test --bin nectar --locked
bun run tauri build --no-bundle -- --locked   # release exe: <target>/release/better-nectar.exe
```

The dock, the taskbar and the tray flyout behave differently under `tauri dev`; test them with a release build. The development and release builds share the settings folder `%APPDATA%\io.github.vekilberke.betternectar`.

## What the release needs next to the exe

- **libunwind:** `src-tauri/.cargo/config.toml` sets `+crt-static` for `x86_64-pc-windows-gnullvm`, so it is linked into the exe. A `RUSTFLAGS` environment variable overrides that file; leave it unset. Check with `llvm-objdump -p better-nectar.exe | findstr "DLL Name"`: no `libunwind.dll`.
- **WebView2Loader.dll:** still loaded at run time. The installer ships it from `src-tauri/runtime/WebView2Loader.dll` (`src-tauri/tauri.release.conf.json`, `bundle.resources`); `scripts/release.ps1` copies it there from the `webview2-com-sys` crate (`<cargo registry>/webview2-com-sys-*/x64/`). A bare exe without this DLL next to it hangs at start.

## Installer

```powershell
bun x tauri signer generate -w <path>\updater.key   # once; keep the key out of the repo
$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content <path>\updater.key -Raw
bun run tauri bundle --bundles nsis --config src-tauri/tauri.release.conf.json
```

This gives a per-user NSIS setup (`bundle/nsis/Better Nectar_<version>_x64-setup.exe`) and its updater signature. The bundler warns when the key does not match `plugins.updater.pubkey` in `tauri.conf.json`; a build signed with another key can not update installed copies. `scripts/release.ps1 -Version X.Y.Z` does all of the above, checks the result (version, clean tree, no libunwind import, DLL in the installer, no phone link material) and writes the release assets with space-free names, `latest.json` and `SHA256SUMS.txt` to `release/`.

## Flavours

The default build is the shared one. `--features phone-link` (with `VITE_PHONE_LINK=1` and `--config src-tauri/tauri.own.conf.json`) is the maintainer's private build with the phone link; it needs files that are not in this repository and is not needed to build or run Better Nectar. `node scripts/check-shared-build.mjs` fails if a build contains the phone link relay or its page.

## Licences

`node scripts/third-party-licenses.mjs` regenerates `THIRD_PARTY_LICENSES.md` from `cargo metadata` and `node_modules`.
