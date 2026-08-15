# Remote Stream Control

Remote Stream Control is an accessible Windows control panel for OBS Studio.
It is built for two real workflows:

- a remote operator controls OBS on an actor's computer through Tailscale;
- a blind streamer controls OBS locally with a keyboard and screen reader.

The release package has one obvious entry point: `RemoteStreamControl.exe`.
The source tree stays normal Rust/HTML/PowerShell code, not a checked-in
single binary.

## What It Does

- Installs or downloads OBS Studio and Tailscale during first setup.
- Enables obs-websocket locally and keeps the OBS port closed to the network.
- Runs a background host agent on the actor computer.
- Opens a browser control panel for the operator.
- Starts and stops stream, recording, replay buffer, virtual camera and Studio Mode.
- Switches scenes, edits sources, toggles visibility and changes OBS input settings.
- Shows a preview frame, OBS stats, stream health and a readiness check.
- Configures Twitch and DonationAlerts from the web panel.
- Adds DonationAlerts overlay/audio to OBS and shows recent donations in the panel.
- Opens a dedicated actor display for a second monitor with chat and donations.
- Provides a local accessible mode without Tailscale or pairing code.

## Quick Start

Download or build the release zip, extract it to a permanent folder, then run:

```text
RemoteStreamControl.exe
```

The launcher opens an accessible mini menu in the browser and creates a desktop
shortcut. The menu has three primary actions:

- `Actor`: prepare this computer for streaming, install dependencies, start the
  host agent and show the pairing code.
- `Operator`: open the remote control panel for the actor computer.
- `Local accessible mode`: open the panel on this computer only, bound to
  `127.0.0.1`, without Tailscale and without a pairing code.

Legacy batch wrappers are still present for advanced/manual use:

```text
START_FRIEND.bat   actor setup
START_ME.bat       operator panel
START_LOCAL.bat    local accessible mode
```

## Remote Mode

Remote mode is for two computers.

1. Put the actor computer and operator computer in the same Tailscale tailnet.
2. On the actor computer, run `RemoteStreamControl.exe` and choose actor setup.
3. Give the pairing code shown on the actor computer to the operator.
4. On the operator computer, run `RemoteStreamControl.exe` and choose operator.
5. Enter the pairing code in the web panel.

The actor does not need to keep using OBS directly after setup. Tailscale, OBS
and the host agent are started automatically on login.

## Local Accessible Mode

Local mode is for one computer. It is intended for a blind streamer who wants a
screen-reader-friendly OBS control surface.

Run `RemoteStreamControl.exe`, choose local accessible mode, and use the browser
panel that opens at:

```text
http://127.0.0.1:8787/
```

In local mode the agent listens only on loopback and bypasses pairing. Remote
mode and local mode cannot both own the same port at the same time.

## Actor Second Screen

The operator panel has an `Actor display` section. It can open, on the actor
computer, a view-only page for a second monitor:

```text
http://127.0.0.1:8787/display.html?panels=both
```

Available variants are:

- chat and donations;
- chat only;
- donations only.

The display page uses the existing DonationAlerts realtime feed and embeds
Twitch chat when Twitch is connected. It does not expose the DonationAlerts
widget URL. Local display APIs are loopback-only; non-loopback access still
requires the normal authenticated panel session.

Browser placement on a particular physical monitor is still controlled by
Windows and the browser. The page includes a fullscreen button for the actor.

## Readiness Check

`Preflight` checks whether the stream is actually ready instead of only checking
that OBS answers:

- OBS connection and stream key;
- current program scene content;
- nested scenes and groups, with bounded recursive expansion;
- whether visual sources are active/showing via `GetSourceActive`;
- hidden or inactive sources that would produce a black screen;
- microphone assignment, mute state and live level;
- risky system audio capture that could broadcast a screen reader;
- disk space while recording;
- Twitch and DonationAlerts overlay state.

Unknown state is reported as unknown or warning, not as a false green.

## Twitch

Twitch is configured from the web panel. Save a Twitch Client ID, connect with
Device Code OAuth, then the panel can manage title, category, language and
stream markers.

Required scope:

```text
channel:manage:broadcast
```

## DonationAlerts

There are two separate DonationAlerts features.

For alerts in the stream, paste the official Alerts Widget URL into the panel.
Remote Stream Control creates or repairs:

- `RSC_OVERLAYS` scene;
- `RSC_DonationAlerts` browser source;
- overlay placement in user scenes;
- browser-source audio routing into OBS.

For the donation list in the panel and actor display, configure DonationAlerts
OAuth in the web panel and connect the account. The widget URL is treated as a
secret and is stored through Windows DPAPI.

## Security Model

- obs-websocket stays on `127.0.0.1`; the network talks only to the host agent.
- Remote access uses Tailscale and a pairing-code session.
- Local mode is loopback-only.
- Secrets are stored under `config\secrets\*.dpapi` using Windows DPAPI.
- Raw OBS RPC is allowlisted.
- Origin/Host checks protect local mode from browser drive-by requests.

Read [SECURITY.md](SECURITY.md) before using this for a real stream.

## Release Package Contents

The release zip contains the runtime files needed by a Windows user:

```text
RemoteStreamControl.exe
START_*.bat
bin\
web\
config\
third_party\
README.md
LICENSE
SECURITY.md
THIRD_PARTY_NOTICES.txt
```

If pinned installers are available, the package also includes official OBS and
Tailscale installers under `third_party\installers\`. Their SHA256 values are
verified from `third_party\installers.json` before packaging.

Build the package with:

```powershell
scripts\package_release.ps1
```

## Development

Requirements:

- Windows 10 or 11;
- Rust toolchain compatible with `rust-version` in `Cargo.toml`;
- PowerShell;
- Node.js for JavaScript syntax checks;
- OBS Studio for live local validation.

Useful checks:

```powershell
cargo fmt --all -- --check
node --check web\app.js
node --check web\display.js
cargo check
cargo test --all
cargo clippy --all-targets -- -D warnings
scripts\scan_secrets.ps1
scripts\smoke_test.ps1 -Port 18787
scripts\smoke_test.ps1 -Port 18788 -Local
scripts\package_release.ps1
```

## Known Limits

- A true single-file self-extracting setup executable is not implemented yet.
  The current release is a zip with one obvious launcher executable inside.
- Live Twitch and DonationAlerts validation requires real accounts.
- Choosing the exact physical monitor for the actor display is still left to
  Windows/browser behavior.
- There is no per-operator permission model; an authenticated operator controls
  the full OBS panel.

## License

MIT. Copyright (c) 2026 restlessbirch.
