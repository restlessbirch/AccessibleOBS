Official third-party installers are staged here only for release packaging.

The source repository tracks `installers.json`, not installer binaries.
`scripts\package_release.ps1` downloads missing pinned installers, verifies
their SHA256 values, and places the verified files into the release zip.

Runtime users normally start `AccessibleOBS.exe`; the launcher installs
or reuses OBS Studio and Tailscale from the packaged installers when available,
or opens the official download pages when an installer is missing.
