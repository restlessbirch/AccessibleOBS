param(
  [string]$Version = "v1.1",
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

# Версии стороннего ПО берём из манифеста, а не из "latest".
#
# Прежде скрипт качал последнюю версию на момент сборки, поэтому один и тот же
# релиз, собранный в разные дни, зависел от разных бинарников. Это не только
# невоспроизводимость: подменённый установщик уехал бы к актёру внутри нашего
# архива и с нашей репутацией.
$manifestPath = Join-Path $Root "third_party\installers.json"
if (-not (Test-Path -LiteralPath $manifestPath)) {
  throw "Не найден манифест $manifestPath"
}
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json

function Get-PinnedInstaller {
  param($Entry, [string]$Name)

  $path = Join-Path $Installers $Entry.file
  if (-not (Test-Path -LiteralPath $path)) {
    Write-Host "Скачиваю $Name $($Entry.version)..."
    Invoke-WebRequest $Entry.url -OutFile $path
  }

  $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
  $expected = "$($Entry.sha256)".Trim().ToLowerInvariant()

  if ([string]::IsNullOrEmpty($expected)) {
    # Останавливаемся намеренно: подставить посчитанную сумму автоматически
    # значило бы «доверяем чему угодно, что скачалось» — ровно то, от чего
    # контрольная сумма и защищает.
    throw @"
Для $Name не зафиксирована контрольная сумма.
Файл: $path
Посчитано: $actual
Проверьте происхождение файла и впишите это значение в поле sha256
для «$Name» в third_party\installers.json.
"@
  }
  if ($actual -ne $expected) {
    throw @"
Контрольная сумма $Name не совпала.
Файл:     $path
Ожидали:  $expected
Получили: $actual
Файл повреждён или подменён. В архив он не попадёт.
"@
  }
  Write-Host "$Name $($Entry.version): контрольная сумма совпала"
  return $path
}

$tailscale = Get-PinnedInstaller -Entry $manifest.tailscale -Name "Tailscale"
$obsPath = Get-PinnedInstaller -Entry $manifest.obs -Name "OBS Studio"

if (Test-Path -LiteralPath $StageRoot) { Remove-Item -LiteralPath $StageRoot -Recurse -Force }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null

$items = @(
  "bin",
  "web",
  "third_party",
  "START_FRIEND.bat",
  "START_ME.bat",
  "START_LOCAL.bat",
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

$StageConfig = Join-Path $Stage "config"
New-Item -ItemType Directory -Force -Path $StageConfig | Out-Null
Copy-Item -LiteralPath (Join-Path $Root "config\host.json") -Destination $StageConfig -Force
Copy-Item -LiteralPath (Join-Path $Root "config\controller.json") -Destination $StageConfig -Force

New-Item -ItemType Directory -Force -Path (Join-Path $Stage "third_party\installers") | Out-Null
Copy-Item -LiteralPath $tailscale -Destination (Join-Path $Stage "third_party\installers") -Force
Copy-Item -LiteralPath $obsPath -Destination (Join-Path $Stage "third_party\installers") -Force

$forbiddenPackageFiles = Get-ChildItem -LiteralPath $Stage -Recurse -Force | Where-Object {
  $_.FullName -match '\\config\\secrets\\' -or
  $_.Name -like "*.dpapi" -or
  $_.Name -like ".env" -or
  $_.Name -like ".env.*" -or
  $_.Name -like "*.key" -or
  $_.Name -like "*.pem" -or
  $_.Name -like "*.pfx" -or
  $_.Name -like "*.p12"
}
if ($forbiddenPackageFiles) {
  throw "Refusing to package secrets or private key material."
}

$zip = Join-Path $Dist "RemoteStreamControl_ready_$Version.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path (Join-Path $StageRoot "RemoteStreamControl") -DestinationPath $zip -Force

Get-FileHash -Algorithm SHA256 -LiteralPath $zip | Format-List
Write-Host "Created $zip"
