# cmux Windows Installer Changelog

## 0.1.0 (2026-10-31) — M6 Packaging Milestone

### Added
- Initial WiX-based MSI installer for cmux Windows port
- Installs to `C:\Program Files\cmux\` with all runtime dependencies
- Includes bundled ConPTY runtime (`conpty.dll`, `OpenConsole.exe`)
- Installs `cmux-shell.exe`, `cmux-cli.exe`, config template, and license
- Creates Start Menu and optional Desktop shortcuts
- Registers `CMUX_CONFIG_FILE` environment variable (system-wide)
- Major upgrade support with automatic previous version removal
- Downgrade prevention
- User data preservation (config at `%USERPROFILE%\.config\cmux\`, sessions at `%APPDATA%\cmux\`)

### Installer Contents
- `cmux-shell.exe` — Main terminal application (eframe/egui + ghostty-vt)
- `conpty.dll` — Bundled ConPTY runtime (Microsoft.Windows.Console.ConPTY)
- `OpenConsole.exe` — ConPTY console helper
- `cmux-cli.exe` — CLI control tool for automation
- `cmux.json.template` — Default configuration template
- `LICENSE` — GPL-3.0-or-later license

### Signing Support
- Optional Authenticode signing via `signtool.exe`
- Supports PFX file or certificate store (thumbprint)
- Timestamp server: `http://timestamp.digicert.com`
- SHA256 digest algorithm
- Unsigned MSI is fully functional (shows "Unknown publisher")

### Build System
- PowerShell script: `build-installer.ps1` (Windows native)
- Bash script: `build-installer.sh` (WSL/Git Bash)
- WiX Toolset v3.14+ required (`candle.exe`, `light.exe`)
- Outputs to `port/target/installer/cmux-installer.msi`

### Known Limitations
- WiX Toolset v3.14+ must be installed separately
- Code signing requires valid Authenticode certificate
- Network required for timestamp server during signing
- Tested on Windows 10/11 x64 only

### Files Changed
- `installer/cmux-installer.wxs` — Main WiX source
- `installer/cmux.json.template` — Default config template
- `installer/build-installer.ps1` — PowerShell build script
- `installer/build-installer.sh` — Bash build script
- `installer/README.md` — Documentation