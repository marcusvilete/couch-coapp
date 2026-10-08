# Drivers the installer offers (M10a)

The installer bundles two drivers' official installers and offers each one, optionally, after Couch Co-App installs. Settings → Drivers can install them later.

| Driver | What it's for | Version | Licence | Source |
|---|---|---|---|---|
| ViGEmBus | Virtual Xbox 360 controllers, so guests' controllers reach games the host streams | 1.22.0 | BSD-3-Clause (`ViGEmBus-LICENSE.txt`, from the `v1.22.0` tag) | https://github.com/nefarius/ViGEmBus/releases/tag/v1.22.0 |
| HidHide | Hides the host's own controller from games while streaming, so it can play through a seat | 1.5.230 | MIT (`HidHide-LICENSE.txt`, from the `v1.5.230.0` tag) | https://github.com/nefarius/HidHide/releases/tag/v1.5.230.0 |

- **The installers aren't in the repository.** `cargo xtask fetch-deps` downloads them, pinned by SHA-256 in `xtask/src/deps.rs` (`DRIVERS`). `cargo xtask stage-dlls` copies them, under fixed names (`vigembus-setup.exe`, `hidhide-setup.exe`) and with these licences, to `target/<profile>/drivers/`.
- **The bundle takes them from there** (`tauri.conf.json`, `bundle.resources`). They are installed next to the app, in `drivers\`.
- **To bump one:** change its URL and hash together, and refresh its licence text from the new tag.
