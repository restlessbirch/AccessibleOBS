param(
  [string]$Version = "v1",
  [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Dist = Join-Path $Root "dist"
$Bin = Join-Path $Root "bin"
$Installers = Join-Path $Root "third_party\installers"
$StageRoot = Join-Path $Dist "stage"
$Stage = Join-Path $StageRoot "RemoteStreamControl"

New-Item -ItemType Directory -Force -Path $Dist, $Bin, $Installers | Out-Null

if (-not $SkipBuild) {
  Push-Location $Root
  cargo build --release
  Pop-Location
}

Copy-Item (Join-Path $Root "target\release\bootstrap.exe") (Join-Path $Bin "bootstrap.exe") -Force
Copy-Item (Join-Path $Root "target\release\host-agent.exe") (Join-Path $Bin "host-agent.exe") -Force

$tailscale = Join-Path $Installers "tailscale-setup-latest-amd64.msi"
if (-not (Test-Path -LiteralPath $tailscale)) {
  Invoke-WebRequest "https://pkgs.tailscale.com/stable/tailscale-setup-latest-amd64.msi" -OutFile $tailscale
}

$obsInstaller = Get-ChildItem -LiteralPath $Installers -Filter "OBS-Studio-*-Windows-Installer.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $obsInstaller) {
  $release = Invoke-RestMethod -Headers @{ "User-Agent" = "RemoteStreamControl/1.0" } "https://api.github.com/repos/obsproject/obs-studio/releases/latest"
  $asset = $release.assets | Where-Object { $_.name -like "*Windows*Installer.exe" } | Select-Object -First 1
  if (-not $asset) { throw "OBS Windows installer asset was not found in latest release." }
  $obsPath = Join-Path $Installers $asset.name
  Invoke-WebRequest $asset.browser_download_url -OutFile $obsPath
} else {
  $obsPath = $obsInstaller.FullName
}

if (Test-Path -LiteralPath $StageRoot) { Remove-Item -LiteralPath $StageRoot -Recurse -Force }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null

$items = @(
  "bin",
  "config",
  "web",
  "third_party",
  "START_FRIEND.bat",
  "START_ME.bat",
  "README_FIRST.txt",
  "CHECKLIST_FOR_ACTOR.txt",
  "CHECKLIST_FOR_OWNER.txt",
  "TECHNICAL_NOTES.txt",
  "LICENSE",
  "THIRD_PARTY_NOTICES.txt",
  "PACKAGING_MICROSOFT.md"
)

foreach ($item in $items) {
  Copy-Item -LiteralPath (Join-Path $Root $item) -Destination $Stage -Recurse -Force
}

New-Item -ItemType Directory -Force -Path (Join-Path $Stage "third_party\installers") | Out-Null
Copy-Item -LiteralPath $tailscale -Destination (Join-Path $Stage "third_party\installers") -Force
Copy-Item -LiteralPath $obsPath -Destination (Join-Path $Stage "third_party\installers") -Force

$zip = Join-Path $Dist "RemoteStreamControl_ready_$Version.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path (Join-Path $StageRoot "RemoteStreamControl") -DestinationPath $zip -Force

Get-FileHash -Algorithm SHA256 -LiteralPath $zip | Format-List
Write-Host "Created $zip"
