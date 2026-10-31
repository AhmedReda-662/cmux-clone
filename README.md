<div align="center">

# cmux — Windows Port

**A Ghostty-based Windows terminal with vertical tabs, AI agent notifications, and browser automation**

[![Windows](https://img.shields.io/badge/Windows-10%2F11-blue?logo=windows)](https://microsoft.com/windows)
[![Rust](https://img.shields.io/badge/Rust-1.95%2B-orange?logo=rust)](https://rust-lang.org)
[![License](https://img.shields.io/badge/License-GPL--3.0-or-later-green)](LICENSE)
[![Build](https://img.shields.io/badge/Build-passing-brightgreen)](https://github.com/AhmedReda-662/cmux-clone/actions)

</div>

---

## Overview

**cmux** is a native Windows terminal application built on **Ghostty's VT engine** and **egui/eframe**. It brings the power of a modern terminal multiplexer to Windows with first-class support for AI coding agents, vertical tabs, split panes, embedded browser, and a scriptable CLI.

This is a **clean-room Windows port** of the macOS cmux application (originally by Manaflow AI), rebuilt from the ground up using Windows-native primitives: ConPTY, Job Objects, Named Pipes, WebView2, and Windows Toast notifications.

---

## Features

### 🖥️ Terminal Engine
- **Ghostty VT emulation** — Same rendering engine as Ghostty terminal, compiled from source via Zig
- **ConPTY backend** — Native Windows pseudo-terminal with bundled ConPTY runtime (newer than in-box)
- **True color, ligatures, mouse reporting** — Full modern terminal capabilities
- **Windows Job Objects** — Kernel-enforced process tree containment (no orphaned children)

### 📑 Vertical Tabs & Split Panes
- **Vertical sidebar** — Workspace tabs on the left with drag-reorder, inline rename
- **Split panes** — Horizontal/vertical splits with keyboard resize (Ctrl+Shift+R/D)
- **Equalize splits** — One keystroke rebalances all panes
- **Tab-per-workspace** — Each tab is a full workspace with its own pane tree

### 🔔 AI Agent Notifications
- **OSC 9/777 parsing** — Detects agent attention requests from terminal output
- **Notification rings** — Visual indicator on pane + sidebar badge
- **Windows Toast** — Native desktop notifications via `cmux-toast`
- **Agent hooks** — Claude Code, Codex, and custom agents via `cmux hooks`
- **Arrival policy** — Smart routing: focused pane = inline, background = toast + ring

### 🌐 Embedded Browser (WebView2)
- **Split browser pane** — `cmux browser open-split https://example.com`
- **CDP automation** — Full Chrome DevTools Protocol via `cmux-browser` CLI
- **Element interaction** — Click, type, fill, hover, select, scroll, screenshot
- **Ref system** — Ephemeral `@eN` references from `browser.snapshot`
- **Wait predicates** — `browser.wait` with selector/text/URL/load-state conditions

### ⌨️ Scriptable CLI & IPC
- **Named Pipe IPC** — Windows Named Pipes with SDDL security
- **JSON Lines protocol** — `{"id": "...", "method": "workspace.list", "params": {}}`
- **Commands**: `workspace`, `surface`, `send`, `read-screen`, `browser`, `agent`, `command`, `ssh`
- **Custom commands** — `cmux.json` definitions with `cmux command run <name>`

### 🔐 SSH Integration
- **Native OpenSSH** — `cmux ssh user@host` spawns SSH pane
- **Full flag support** — `-p`, `-i`, `-o`, `--command`, `--no-focus`
- **Agent forwarding** — `-A` / `-a` passed through to `ssh.exe`

### ⚙️ Configuration
- **JSONC config** — `%USERPROFILE%\.config\cmux\cmux.json` (comments + trailing commas)
- **Themes** — Dark/light, custom background, cursor color
- **Keybindings** — Fully customizable via `shortcuts.bindings`
- **Font scaling** — 50–300% magnification

---

## Installation

### Via MSI Installer (Recommended)
```powershell
# Download latest MSI from Releases
msiexec /i cmux-installer.msi
```

### From Source
```powershell
# Prerequisites
winget install Rustlang.Rustup WiXToolset.WiX
winget install zig.zig  # for Ghostty VT engine

# Build
cd port
cargo build --release

# Build MSI (requires WiX Toolset)
.\installer\build-installer.ps1

# Run directly
.\target\release\cmux-shell.exe
```

### Configuration Location
```
Config:     %USERPROFILE%\.config\cmux\cmux.json
Sessions:   %APPDATA%\cmux\session.json
Logs:       %APPDATA%\cmux\logs\
```

---

## Quick Start

### Launch Terminal
```powershell
cmux-shell.exe
# Or from Start Menu: "cmux"
```

### Split Panes
| Shortcut | Action |
|----------|--------|
| `Ctrl+Shift+R` | Split right |
| `Ctrl+Shift+D` | Split down |
| `Ctrl+Tab` | Next pane |
| `Ctrl+Shift+Tab` | Previous pane |
| `Ctrl+Shift+W` | Close pane |
| `Ctrl+Shift+E` | Equalize splits |

### Workspace (Tabs)
| Shortcut | Action |
|----------|--------|
| `Ctrl+Shift+N` | New workspace |
| `Ctrl+Shift+]` | Next workspace |
| `Ctrl+Shift+[` | Previous workspace |
| `Ctrl+1..9` | Jump to workspace 1–9 |

### Browser Automation
```bash
# Open browser split
cmux browser open-split https://github.com

# Navigate & interact
cmux browser navigate https://api.example.com
cmux browser snapshot          # Get page text + element refs
cmux browser click @e1         # Click element by ref
cmux browser type @e2 "text"   # Type into element
cmux browser eval "document.title"  # Run JavaScript
```

### Agent Hooks (Claude Code / Codex)
```bash
# Install hooks
cmux hooks install
cmux hooks install codex

# In your agent config, hooks will auto-detect cmux and send notifications
```

---

## Architecture

```
cmux-clone/
├── cmux-main/          # Upstream macOS reference (read-only)
├── port/               # Windows port (this project)
│   ├── crates/
│   │   ├── cmux-shell/       # Main application (eframe + egui)
│   │   ├── cmux-pty-port/    # ConPTY + Job Objects
│   │   ├── cmux-layout/      # Pane tree geometry
│   │   ├── cmux-ipc/         # Named Pipe IPC
│   │   ├── cmux-webview2/    # WebView2 hosting
│   │   ├── cmux-config/      # JSONC config parser
│   │   ├── cmux-workspaces/  # Session persistence
│   │   ├── cmux-hooks/       # Agent hook system
│   │   ├── cmux-toast/       # Windows Toast delivery
│   │   └── cmux-testsupport/ # Test utilities
│   ├── installer/            # WiX MSI installer
│   └── docs/                 # Documentation
└── README.md
```

### Key Technical Decisions
| Area | Choice | Rationale |
|------|--------|-----------|
| Terminal Engine | `ghostty-vt` (Rust) | Same VT parser as Ghostty, no C++ deps |
| PTY | ConPTY + `portable-pty` | Native Windows PTY, no Unix emulation |
| Process Tree | Job Objects (`KILL_ON_JOB_CLOSE`) | Kernel-enforced containment |
| IPC | Named Pipes + SDDL | Windows-native, secure by default |
| GUI | egui/eframe | Immediate-mode, no heavy runtime |
| Browser | WebView2 (Evergreen) | System WebView2, no bundled Chromium |
| Config | JSONC (serde_json + custom parser) | Comments + trailing commas, upstream compatible |

---

## CLI Reference

### Core Commands
```bash
cmux ping                           # Test IPC connection
cmux identify                       # Show focused workspace/pane
cmux workspace list                 # List workspaces
cmux workspace select <id>          # Switch workspace
cmux workspace create               # New workspace
cmux surface list                   # List terminal surfaces
cmux surface send-text "text"       # Type into surface
cmux read-screen                    # Read visible screen text
```

### Browser Commands
```bash
cmux browser open-split [url]       # Open browser pane
cmux browser navigate <url>         # Navigate
cmux browser back|forward|reload    # History
cmux browser snapshot               # Text + refs
cmux browser eval <js>              # JavaScript
cmux browser click|dblclick <ref|selector>
cmux browser type|fill <ref|selector> <text>
cmux browser hover|focus <ref|selector>
cmux browser wait --selector ".btn" --timeout-ms 5000
cmux browser press <key>            # e.g. "Enter", "Control+c"
cmux browser screenshot --out file.png
```

### Agent & Custom Commands
```bash
cmux hooks enqueue <agent> <sub>    # Hook notification (stdin: JSON)
cmux command list                   # List custom commands
cmux command run <name> [--yes]     # Execute command
```

### SSH
```bash
cmux ssh user@host                  # New SSH workspace
cmux ssh -p 2222 -i ~/.ssh/key user@host --command "tmux attach"
```

---

## Building from Source

### Prerequisites
| Tool | Version | Notes |
|------|---------|-------|
| Rust | 1.95+ (stable-msvc) | `rustup-init.exe -y --default-toolchain stable-msvc --profile minimal` |
| Zig | 0.16.0 | Required for `ghostty-vt` engine |
| WiX Toolset | 3.14+ | For MSI installer |
| MSVC Build Tools | VS 2022 | `x86_64-pc-windows-msvc` target |

### Build Commands
```powershell
# Setup environment
. .\port\scripts\env.ps1

# Build application
cd port
cargo build --release

# Run directly
.\target\release\cmux-shell.exe

# Build MSI installer (requires WiX Toolset)
.\installer\build-installer.ps1
# Output: target/installer/cmux-installer.msi

# Run tests
cargo test --workspace
```

### Environment Variables
| Variable | Purpose |
|----------|---------|
| `CMUX_CONFIG_FILE` | Override config path |
| `CMUX_SOCKET_PATH` | Override IPC pipe path |
| `CMUX_IPC_DISABLED=1` | Disable IPC server |
| `CMUX_SOCKET_MODE` | `cmuxOnly` \| `automation` \| `password` |
| `CMUX_SHELL_AUTO_EXIT_MS` | Auto-close for smoke tests |
| `CMUX_SHELL_AUTO_SPLIT_MS` | Auto-split for smoke tests |

---

## Project Structure

```
cmux-clone/
├── AGENTS.md                    # Agent instructions
├── LICENSE                      # GPL-3.0-or-later
├── README.md                    # This file
├── cmux-main/                   # Upstream macOS reference (read-only)
│   ├── Sources/                 # Swift AppKit app
│   ├── Packages/                # Swift packages
│   ├── CLI/                     # Swift CLI
│   └── cmux-tui/                # Rust TUI core (upstream)
├── port/                        # Windows port (production code)
│   ├── Cargo.toml               # Workspace manifest
│   ├── crates/                  # Rust crates (see Architecture)
│   ├── installer/               # WiX MSI installer
│   ├── scripts/env.ps1          # Build environment setup
│   └── docs/                    # Development docs
└── docs/
    ├── architecture/            # ADRs, decisions
    ├── development/             # Building, testing
    └── research/                # REA findings
```

---

## License

This project is licensed under **GPL-3.0-or-later** — see [LICENSE](LICENSE) for details.

The upstream macOS cmux is also GPL-3.0-or-later for the application code. Server components (web, workers, relays) use BUSL-1.1.

---

## Acknowledgments

- **Ghostty** — Terminal engine by Mitchell Hashimoto & contributors
- **egui/eframe** — Immediate-mode GUI by Emil Ernerfeldt
- **portable-pty** — Cross-platform PTY by Wez Furlong
- **WebView2** — Microsoft Evergreen WebView2
- **WiX Toolset** — Windows Installer XML
- **Manaflow AI** — Original cmux macOS application

---

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make changes with tests
4. Run `cargo test --workspace` and `cargo fmt --check`
5. Submit a Pull Request

See [CONTRIBUTING.md](CONTRIBUTING.md) for detailed guidelines.

---

## Support

- **Issues**: [GitHub Issues](https://github.com/AhmedReda-662/cmux-clone/issues)
- **Discussions**: [GitHub Discussions](https://github.com/AhmedReda-662/cmux-clone/discussions)
- **Documentation**: [docs/](docs/)

---

<div align="center">

**Built with ❤️ for Windows developers who love their terminal**

</div>