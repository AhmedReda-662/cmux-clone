# cmux Windows Installer Changelog

## 0.0.1 (2026-11-01) — M6 packaging: first machine-built MSI

### Built and verified
- WiX 3.14.1.8722 (portable, per `docs/development/building-windows.md`);
  `candle -arch x64` + `light -ext WixUIExtension` — zero compile/ICE errors
- Machine MSI: `port/target/installer/cmux-installer.msi` (5.43 MB)
- `validate-installer.ps1` administrative extraction: all six payload
  files present and intact

### Package contents (matches cmux-installer.wxs)
- `cmux-shell.exe` — main terminal application (eframe/egui + ghostty-vt)
- `cmux.exe` — CLI control tool (crate-declared bin name; the hooks
  ownership rules match `\cmux.exe`, so this is load-bearing)
- `conpty.dll` + `OpenConsole.exe` — bundled ConPTY redistributable pair
  (NuGet `Microsoft.Windows.Console.ConPTY` 1.24.260710001, MIT;
  provenance: `port/third_party/conpty/ORIGIN.md`, decision: ADR-0017)
- `cmux.json.template` — default config template (`Vital=no`)
- `NOTICES.txt` — third-party notices for every shipped binary

### Install layout
- Per-machine: `C:\Program Files\cmux\` (x64)
- Start Menu + desktop shortcuts for `cmux-shell.exe`; uninstall shortcut
- Registry keys under `HKCU\Software\cmux-clone\cmux` (per-component
  keypaths); user data untouched (config at
  `%USERPROFILE%\.config\cmux\`, sessions at `%APPDATA%\cmux\`)
- Major upgrade + downgrade prevention (UpgradeCode
  `{5A3C8F2E-7B4D-4A1F-9E6C-2D8F1A7B3C9D}`)

### Deliberately absent
- No system-wide environment variables (an earlier draft registered a
  machine-wide `CMUX_CONFIG_FILE`; removed — it broke per-user config
  discovery)
- No sign-at-install action (Authenticode signing is a build-time step;
  unsigned build remains fully functional)
- No EULA/license agreement — `NOTICES.txt` covers third-party notices;
  the port's own license is an open owner decision

### Known limitations
- Live silent install/uninstall smoke test still owner-run
- Code signing requires an Authenticode certificate (owner-provided)
- Portable WiX must be present locally or fetched in CI

### Historical: 0.1.0 (2026-10-31) draft
The 2026-10-31 draft entry described `cmux-cli.exe`, a GPL-3.0 LICENSE
payload and the system-wide env var — none of which shipped; superseded
by this entry (the WiX source of that draft did not compile).
