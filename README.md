# Accessible OBS

An accessible web panel and remote control for OBS Studio on Windows.

> **Interface language.** The panel, the launcher and all messages are currently
> in Russian. The code and documentation are in English. If you need an English
> interface, open an issue — the strings are in one place and translating them
> is straightforward.

## The problem

OBS Studio is not usable with a screen reader. Its window is a rendering
surface: much of what matters on screen has no accessibility tree, so a blind
streamer cannot tell whether the microphone is live, whether the scene is
black, or whether the stream is actually reaching viewers.

The usual answer is "ask a sighted friend to sit next to you". That does not
scale to a live stream, and it does not work at all when the friend is in
another city.

Accessible OBS replaces the OBS window with something a screen reader can
read, and lets it run on a different computer than the person operating it.

## Who it is for

**A blind streamer on their own computer.** Local mode gives a keyboard-driven,
screen-reader-friendly panel for OBS running on the same machine. No network
setup, no pairing code.

**An operator running someone else's stream.** Remote mode puts the panel on
the operator's computer and OBS on the streamer's, connected over Tailscale.
The person in front of the camera does nothing after the one-time setup:
Tailscale, OBS and the agent all start themselves at logon.

## How it fits together

The streamer's computer runs the game, OBS Studio and a small background agent.
The agent talks to OBS locally over obs-websocket and exposes a web panel.

The operator's computer runs only a browser with a screen reader. It reaches
the agent through Tailscale — an encrypted private network between the two
machines, with nothing published to the public internet.

```
        Streamer's computer
   ┌─────────────────────────┐
   │  Game                   │
   │  OBS Studio             │
   │  Accessible OBS agent   │
   └────────────┬────────────┘
                │
                │  Tailscale
                │
   ┌────────────▼────────────┐
   │  Remote operator        │
   │  Browser + screen reader│
   └─────────────────────────┘
```

## Two interface modes

The mode is chosen on the launcher's start page and changes what the panel
shows and what it says out loud.

**Accessible mode**, the default. Chat, donations and stream alerts are
announced through live regions, so a screen reader speaks them as they arrive.
The second-monitor projector is hidden, because an OBS projector window is a
rendering surface with no accessibility tree — it would be a button that opens
something a blind operator can neither read nor find again to close.

**Standard mode**, for a sighted operator. Chat and donations can be projected
onto a second monitor through OBS. Nothing is read aloud.

## Quick start

Download or build the release zip.

Extract it to a permanent folder.

Run `AccessibleOBS.exe`.

The launcher opens a small accessible menu in the browser and creates a desktop
shortcut. Choose one of:

`Actor` — prepare this computer for streaming. Installs OBS and Tailscale if
missing, enables obs-websocket, registers the agent to start at logon and shows
the pairing code.

`Operator` — open the remote control panel and enter the pairing code.

`Local accessible mode` — open the panel for OBS on this same computer, bound
to `127.0.0.1`, with no Tailscale and no pairing code.

## What the panel does

Starts and stops the stream, recording, replay buffer, virtual camera and
Studio Mode.

Switches scenes, adds and removes sources, toggles visibility and edits OBS
input settings.

Shows microphone and audio levels, so the operator can tell a live microphone
from a muted one, and a muted one from an unplugged one.

Reads Twitch chat as an ordinary list of page elements, not an embedded widget,
so a screen reader announces new messages instead of hiding them in a frame.

Shows recent donations, and sets up DonationAlerts inside OBS so alerts are
both visible and audible on the stream.

Manages the Twitch channel title, category, language and stream markers.

Grabs a preview frame on demand, so the operator can check for a black screen
without a permanent video feed eating the streamer's bandwidth.

Runs a readiness check before going live.

Writes browser-side errors into the same log file as the agent's own, and
offers a diagnostics summary for troubleshooting.

## Readiness check

The check answers "can this actually go on air", not "does OBS respond":

OBS connection and configured stream key.

Whether the current program scene has anything in it, following nested scenes
and groups.

Whether visual sources are genuinely active, via `GetSourceActive`.

Microphone assignment, mute state and live audio level.

System-audio capture that would broadcast the operator's own screen reader to
viewers.

Free disk space while recording.

Twitch connection and the state of the donation overlay.

An unknown result is reported as unknown, never as a green light. A check that
could not run is not a check that passed.

## Second screen for the streamer

The panel can open a view-only page on the streamer's computer showing chat and
donations, with variants for chat only or donations only.

In standard mode the panel can place that page on a chosen physical monitor
through an OBS projector, so nobody has to drag a window across screens.

The page never exposes the DonationAlerts widget URL, which is a secret. Its
API is reachable without a panel session only from the local machine, and only
with a trusted request origin.

## Twitch and DonationAlerts

Twitch is configured from the panel: save a Client ID, connect with Device Code
OAuth, and the panel can manage the channel. The required scope is
`channel:manage:broadcast`.

Chat is read anonymously and needs no token at all, so it works before OAuth.

For alerts on the stream, paste the official DonationAlerts Alerts Widget URL
into the panel. Accessible OBS creates and repairs the overlay scene, the
browser source, its placement above other sources in every scene, and the audio
routing that makes alerts audible to viewers. It then re-reads OBS to confirm,
rather than reporting success because the commands were sent.

For the donation list in the panel, connect DonationAlerts OAuth.

## Security

obs-websocket stays bound to `127.0.0.1`; only the agent talks to the network.

Remote access runs over Tailscale with a pairing-code session.

Local mode listens on loopback only.

Secrets are stored with Windows DPAPI under `config\secrets\*.dpapi` and never
appear in API responses or logs.

The raw OBS RPC endpoint is restricted to an allowlist of read-only requests.

Origin and Host are checked so a page on another site cannot drive the panel
through the operator's own browser.

Read [SECURITY.md](SECURITY.md) before using this for a real stream.

## Building

Requirements: Windows 10 or 11, a Rust toolchain matching `rust-version` in
`Cargo.toml`, PowerShell, and OBS Studio for live validation.

Build the release package:

```powershell
BUILD.bat
```

Checks used in CI:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
scripts\scan_secrets.ps1
```

## Known limits

The release is a zip with one obvious launcher inside, not a single
self-extracting installer.

Remote operation between two machines, a clean installation, autostart across a
reboot, and Twitch and DonationAlerts with live accounts are not covered by
automated testing and need real hardware and accounts.

There is no per-operator permission model: an authenticated operator controls
the whole panel.

OBS scenes and sources created by the program carry an `RSC_` prefix from an
earlier name of the project. Renaming them requires a migration step, otherwise
existing setups would be left with orphaned scenes.

## License

GNU General Public License, version 2 or later. Copyright (c) 2026 restlessbirch.

See [LICENSE](LICENSE) for the full text.

The source is offered under "GPLv2 or later", so you may take the source alone
under version 2. **A compiled binary, however, is conveyed under GPLv3**: it is
linked with libraries published under Apache-2.0, which the FSF states is
incompatible with GPLv2 but compatible with GPLv3. The "or later" option is
precisely what makes that combination lawful.

Release archives ship both texts, `LICENSE-GPL-2.0.txt` and
`LICENSE-GPL-3.0.txt`, together with `THIRD_PARTY_NOTICES.txt`, which names the
Apache-2.0 components and the licences of the bundled OBS Studio (GPLv2) and
Tailscale (BSD-3-Clause) installers.
