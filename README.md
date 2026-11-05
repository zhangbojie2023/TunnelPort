# TunnelPort

A lightweight macOS menu bar app that opens an SSH dynamic-forward tunnel and routes your system traffic through it — one click, no terminal.

TunnelPort spins up a local SOCKS5 proxy via `ssh -D`, bridges it to an HTTP proxy, verifies the route end to end, and points macOS system proxy settings at it. Disconnect and everything is torn down cleanly.

---

## Why

Reaching internal services through a bastion host usually means remembering the right `ssh -D` invocation every session, then manually switching system proxy settings and remembering to switch them back.

TunnelPort is that workflow as a button: it manages the SSH process, keeps the proxy bridge alive, runs three connectivity probes before declaring success, and restores your proxy settings on exit.

---

## How it works

```
Browser / any app
      │
      ▼
  127.0.0.1:1081          HTTP proxy (Rust bridge)
      │
      ▼
  127.0.0.1:1080          SOCKS5 (ssh -D)
      │
      ▼
  remote host  ────────►  target sites
```

Connecting runs through these stages, each cancellable:

1. **SSH tunnel** — spawns `ssh -D <socks> -N` and waits for the SOCKS port to accept connections (30 s timeout). Password auth uses `sshpass`; key auth uses `-i`.
2. **Proxy bridge** — a Rust HTTP→SOCKS5 bridge binds `<http>`. Falls back to `gost` if unavailable.
3. **Self-test** — SOCKS5 greeting and `CONNECT`, an HTTP `CONNECT`, and a TCP check to the target.
4. **System proxy** — applied via `networksetup` when *Auto proxy* is enabled.

A health check runs every 10 s while connected and reports port state, latency, and transferred bytes to the UI.

---

## Features

- **Multiple profiles** — keep several hosts side by side and switch from the title bar
- **Cancel mid-connect** — every stage of the connect sequence responds to Cancel; a cancelled attempt never leaks a bound port or a stray `ssh` process
- **Live throughput** — up/down totals and instantaneous speed, part of a compact instrument-style readout
- **Built-in log viewer** — backend logs forwarded to the UI in real time, also written to a daily rotating file
- **Menu bar control** — connect, disconnect, show panel, or quit without opening the window
- **Start at login** — registers a `LaunchAgent` so the app is ready in the menu bar
- **Offline by design** — system fonts, inline SVG icons, no network requests at runtime

---

## Requirements

- macOS 10.15 or later
- `ssh` (ships with macOS)
- [`sshpass`](https://formulae.brew.sh/formula/sshpass) — only if you authenticate with a password instead of a key

Optional: [`gost`](https://github.com/ginuerzh/gost) as a fallback proxy bridge.

```bash
brew install hudochenkov/sshpass/sshpass   # password auth only
```

---

## Install

Download `TunnelPort_<version>_aarch64.dmg` from [Releases](../../releases), open it, and drag **TunnelPort** into *Applications*.

The build is unsigned, so the first launch needs **Right-click → Open** (or *System Settings → Privacy & Security → Open Anyway*).

> Plugins are not installed by the app. If you use password authentication, `sshpass` must be on your `PATH`.

---

## Build from source

**Prerequisites:** Node.js 18+, Rust (stable), and the Xcode Command Line Tools.

```bash
git clone git@github.com:zhangbojie2023/TunnelPort.git
cd TunnelPort
npm install
npm run tauri build
```

Outputs:

| Artifact | Path |
|---|---|
| App bundle | `src-tauri/target/release/bundle/macos/TunnelPort.app` |
| Installer | `src-tauri/target/release/bundle/dmg/TunnelPort_<version>_aarch64.dmg` |

For live development with hot reload:

```bash
npm run tauri -- dev
```

---

## Configuration

All settings live in a profile, editable from the app (**sliders icon → Server / Proxy / Options**).

### Server

| Field | Description |
|---|---|
| Profile name | Label shown in the title bar |
| Target host | SSH host to tunnel through |
| Username | SSH user |
| Port | SSH port (default `22`) |
| Password | Optional — leave blank to use your SSH key |

### Proxy

| Field | Description |
|---|---|
| SOCKS port | Local SOCKS5 port (default `1080`) |
| HTTP port | Local HTTP proxy port (default `1081`) |
| Auto proxy | Point macOS system proxy at the HTTP port on connect |

### Options

| Field | Description |
|---|---|
| Auto reconnect | Re-establish the tunnel when it drops |
| Test URL | Destination used by the self-test (default `https://www.google.com`) |
| Start at login | Launch TunnelPort at login via `LaunchAgent` |

---

## Where data lives

| What | Location |
|---|---|
| Profiles and settings | `~/Library/Application Support/com.zhangbojie.tunnelport/` |
| Logs | `~/Library/Application Support/tunnelport/logs/tunnelport.log.<YYYY-MM-DD>` |
| Autostart entry | `~/Library/LaunchAgents/com.zhangbojie.tunnelport.plist` |

> Profiles are stored as plain JSON in the Tauri store. Saved passwords are **not** encrypted — prefer key-based authentication when you can.

---

## Project layout

```
.
├── index.html              # Markup for all three pages (Home / Settings / Logs)
├── src/
│   ├── main.js             # UI state machine, rendering, Tauri command calls
│   └── styles.css          # Design tokens and component styles
├── src-tauri/
│   ├── tauri.conf.json     # Window, bundle, and security config
│   └── src/
│       ├── lib.rs          # App setup, tray, health-check loop
│       ├── commands.rs     # All #[tauri::command] handlers
│       ├── config.rs       # TunnelProfile / AppConfig
│       ├── state.rs        # Shared state and cancellation
│       ├── tunnel/         # SSH process lifecycle
│       ├── proxy/          # SOCKS5→HTTP bridge
│       ├── network/        # Port and reachability probes
│       ├── platform/       # System proxy, autostart (per OS)
│       ├── stats.rs        # Traffic counters
│       └── log_layer.rs    # tracing → frontend log forwarding
└── package.json
```

---

## Platform support

macOS is the primary and only tested target. A Windows module exists (`src-tauri/src/platform/windows.rs`) but its system-proxy and autostart functions are still stubs, so the app does not yet apply proxy settings on Windows.

---

## License

No license has been declared yet. Until one is added, all rights are reserved.
