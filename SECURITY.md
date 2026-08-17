# Security

What Accessible OBS protects against, what it does not, and what is required
from you for that protection to hold.

## What is at stake

The panel gives another person full control over OBS on your computer: scene
switching, audio, going live, recording, editing sources. This is remote
control of your broadcast, not a convenience remote. Whoever reaches the panel
has your stream.

## Threat model

### Protected against

**The public internet.** No forwarded ports and no Tailscale auth keys in
files. The agent listens only on its tailnet address and `127.0.0.1`, and there
is no route to it from outside the tailnet.

**Your local network.** The obs-websocket port (4455) is never exposed. Only
the agent talks to it, over loopback, which does not pass through the firewall.
If Windows asks whether to allow OBS through the firewall, choose **Cancel** —
the permission is not needed.

**A random member of your tailnet.** They can reach the panel but cannot sign
in without the pairing code. Failed attempts are rate-limited per peer address.

**A stolen session cookie.** Tokens expire after two hours idle and twelve
hours absolute. Each sign-in issues a new token, and event streams opened with
an earlier one close themselves.

**A page trying to drive the panel from another site.** Requests are checked
against their origin, and the panel refuses to be embedded in a third-party
page: `frame-ancestors 'none'` and `X-Frame-Options: DENY`. Origin checks alone
would not be enough, because a script inside an iframe runs as the panel's own
origin.

**A tampered installer.** OBS Studio and Tailscale are downloaded only from
URLs pinned in the installer manifest and verified against their SHA-256 hashes
before execution, including files shipped inside the release archive.

**Reading secrets from disk.** Passwords and tokens live in
`config\secrets\*.dpapi`, encrypted with Windows DPAPI scoped to the current
user. Another user of the same computer cannot decrypt them.

### Not protected against

**Anyone already signed in to Windows as you.** DPAPI is bound to the user
account: whoever holds the Windows session holds the secrets. This is a
boundary, not an oversight.

**People you gave the pairing code to.** There are no permission levels inside
the panel — whoever signs in can do everything.

**An administrator of the computer.** They can read process memory and files.

**A leaked Alerts Widget URL.** That link is a secret in itself: anyone holding
it can display an arbitrary alert on your stream. The panel stops showing it
once saved, but if it has already leaked, reissue it in your DonationAlerts
account.

## What is required from you

**Keep your tailnet closed.** Invite only people you would trust with your
stream. Review the device list with `tailscale status` or in the Tailscale
admin console.

**Do not post the pairing code in public chats.** It is single-use in intent
but not in mechanics: it stays valid while the agent runs. If it leaks, run
`AccessibleOBS.exe`, choose actor setup and generate a new one.

**Do not put secrets into BAT or JSON files.** The one exception is
`donationalerts.client_secret`, which has to be placed in `config\host.json`
once because it is needed before the first authorization. The agent moves it
into DPAPI on the next start and clears the field; check that it is empty
afterwards.

**Do not publish the contents of `config\secrets\`.** The folder is excluded by
`.gitignore`, and both a pre-commit hook and `scripts\scan_secrets.ps1` check
commits and release archives.

**Logs are safe to share.** `logs\host.log` and `logs\bootstrap.log` contain no
secrets: values pass through redaction, so only their length and first few
characters are recorded.

## Deliberate trade-offs

**The session cookie has no `Secure` flag.** Traffic inside the tailnet is http
and encrypted by WireGuard at the network layer. With `Secure` set, the browser
would discard the cookie and sign-in would stop working. `HttpOnly` and
`SameSite=Lax` are both set.

**One obs-websocket password for everyone.** OBS has no separate accounts.
Setup adopts an existing password instead of generating a new one, otherwise
Stream Deck, Touch Portal and other controllers configured against it would
break.

**The generic OBS endpoint is restricted to an allowlist.**
`/api/obs/request` accepts only `GetStreamStatus` and `GetRecordStatus`.
Without that limit, a compromised session would grant access to the whole
obs-websocket protocol, including output settings.

**The OBS crash sentinel is cleared automatically.** Otherwise a single crash
would leave a modal dialog with nobody present to dismiss it, and remote
control would be lost until someone walked over. The crash itself is still
reported in the panel.

## Reporting a vulnerability

Open an issue in the repository. If you believe the finding is dangerous to
existing installations, contact the author directly and do not publish details
before a fix is available.

Useful to include: the Accessible OBS version, the OBS version and steps to
reproduce. Attaching logs is safe.

## Known gaps

- No permission levels between operators: whoever signs in can do everything.
- No per-session audit trail. If several people use the panel, the logs will
  not tell you who switched the scene.
- A true single-file installer does not exist yet; the release is a zip with
  one obvious `AccessibleOBS.exe` inside.
- Live testing of Twitch and DonationAlerts requires real accounts.
- Integration tests cover the OBS protocol and smoke scenarios, but external
  services are exercised without their production APIs.
