# cmux Windows Installer (M6 Packaging)

This directory contains the WiX-based MSI installer for the cmux Windows port.

## Prerequisites

1. **WiX Toolset v3.14+** — Install from [GitHub Releases](https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314.exe)
   - Required tools: `candle.exe`, `light.exe` in PATH
   
2. **Built cmux binaries** — Run `cargo build --release` in the `port/` directory
   - Required artifacts in `target/release/`:
     - `cmux-shell.exe` — Main application
     - `conpty.dll` — Bundled ConPTY runtime
     - `OpenConsole.exe` — ConPTY console helper
     - `cmux-cli.exe` — CLI control tool

## Building the MSI

### Using PowerShell (Recommended)

```powershell
cd port
.\installer\build-installer.ps1
# Or with signing:
.\installer\build-installer.ps1 -Sign
```

### Using Bash (WSL/Git Bash)

```bash
cd port
./installer/build-installer.sh
# Or with signing:
./installer/build-installer.sh --sign
```

### Manual Build

```bash
# 1. Stage files
cp target/release/cmux-shell.exe installer/
cp target/release/conpty.dll installer/
cp target/release/OpenConsole.exe installer/
cp target/release/cmux-cli.exe installer/

# 2. Compile WiX source
cd installer
candle.exe -dSourceDir="<absolute-path-to-installer>" \
    -dBuildDir="<absolute-path-to-target/release>" \
    -dProductVersion="0.1.0" \
    cmux-installer.wxs -out cmux-installer.wixobj

# 3. Link MSI
light.exe cmux-installer.wixobj -out ../target/installer/cmux-installer.msi -ext WixUIExtension -cultures:en-us
```

## Output

The MSI is created at `port/target/installer/cmux-installer.msi`

### Install
```powershell
msiexec /i port\target\installer\cmux-installer.msi
```

### Uninstall
```powershell
msiexec /x {ProductCode}
# Or via Add/Remove Programs
```

## Code Signing

The build scripts support optional Authenticode signing when credentials are available:

### Environment Variables

| Variable | Description |
|----------|-------------|
| `SIGNTOOL_PATH` | Path to `signtool.exe` (default: `signtool.exe` in PATH) |
| `CERT_THUMBPRINT` | SHA1 thumbprint of certificate in Windows cert store |
| `PFX_PATH` | Path to `.pfx` certificate file |
| `CERT_PASSWORD` | Password for PFX file |

### Signing Methods

**Using PFX file:**
```powershell
$env:PFX_PATH = "C:\path\to\cert.pfx"
$env:CERT_PASSWORD = "password"
.\installer\build-installer.ps1 -Sign
```

**Using Certificate Store:**
```powershell
$env:CERT_THUMBPRINT = "A1B2C3D4E5F6..."
.\installer\build-installer.ps1 -Sign
```

**Without Credentials:**
The MSI builds and functions perfectly without signing. Windows will show "Unknown publisher" but the installer is fully functional.

## Installer Contents

The MSI installs to `C:\Program Files\cmux\`:

| File | Purpose |
|------|---------|
| `cmux-shell.exe` | Main terminal application |
| `conpty.dll` | Bundled ConPTY runtime (newer than in-box) |
| `OpenConsole.exe` | ConPTY console helper |
| `cmux-cli.exe` | CLI control tool |
| `cmux.json.template` | Default config template |
| `LICENSE` | GPL-3.0 license |

### Shortcuts Created
- Start Menu: `cmux` → `cmux-shell.exe`
- Desktop: `cmux` (optional, can be disabled)

### Registry
- `HKCU\Software\Manaflow\cmux` — Install tracking
- `CMUX_CONFIG_FILE` environment variable (system-wide)

## Configuration

On first run, cmux creates config at `%USERPROFILE%\.config\cmux\cmux.json` from the embedded template. The template is installed alongside the executable.

## Upgrade Behavior

- **Major Upgrade**: New version uninstalls previous version automatically
- **Downgrade Blocked**: Prevents accidental downgrades
- **User Data Preserved**: Config at `%USERPROFILE%\.config\cmux\` and session data at `%APPDATA%\cmux\` are preserved

## Verification

### Check MSI Contents
```powershell
msiexec /a "cmux-installer.msi" /qb TARGETDIR="C:\temp\cmux-extract"
```

### Silent Install
```powershell
msiexec /i cmux-installer.msi /qn /norestart
```

### Log Install
```powershell
msiexec /i cmux-installer.msi /l*v install.log
```

## Troubleshooting

| Issue | Resolution |
|-------|------------|
| `candle.exe` not found | Install WiX Toolset v3.14+ |
| `light.exe` error - missing WixUIExtension | Ensure `-ext WixUIExtension` is passed |
| Build artifacts missing | Run `cargo build --release` in `port/` |
| Signing fails | Verify certificate, timestamp server reachable |
| Install fails with "Another version installed" | Uninstall previous version first |

## CI/CD Integration

Example GitHub Actions workflow:

```yaml
- name: Build cmux
  run: cargo build --release --manifest-path port/Cargo.toml

- name: Install WiX
  uses: actions/setup-msbuild@v2
  with:
    wix-version: '3.14'

- name: Build MSI
  run: ./port/installer/build-installer.sh

- name: Sign MSI
  if: env.SIGNTOOL_PATH != ''
  env:
    PFX_PATH: ${{ secrets.PFX_PATH }}
    CERT_PASSWORD: ${{ secrets.CERT_PASSWORD }}
  run: ./port/installer/build-installer.sh --sign

- name: Upload MSI
  uses: actions/upload-artifact@v4
  with:
    name: cmux-installer
    path: port/target/installer/cmux-installer.msi
```

## License

The installer is part of the cmux Windows port and follows the same license (GPL-3.0-or-later for the application code).