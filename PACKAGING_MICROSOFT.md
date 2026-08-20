# Windows publisher notes

Project publisher/author: restlessbirch

The source code is licensed under GPL-2.0-or-later. That is separate from Windows publisher
identity.

To make Windows, SmartScreen, MSIX, or Microsoft Store show a trusted publisher,
the executable or installer must be signed with a code-signing certificate, or
published through a Microsoft Partner Center account whose publisher display
name is restlessbirch.

Recommended release path:

1. Keep source license as GPL-2.0-or-later with copyright `restlessbirch`.
2. Build release binaries from a clean git commit.
3. Create an installer or MSIX package.
4. Sign the package with a code-signing certificate for restlessbirch.
5. Publish via Microsoft Partner Center if Store distribution is needed.

Without code signing, Windows may show "Unknown publisher" even though the code
has a valid open-source license.
