param(
  [switch]$Staged
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

Push-Location $Root
try {
  if ($Staged) {
    $files = git diff --cached --name-only --diff-filter=ACMR
  } else {
    $files = git ls-files
  }

  $files = @($files | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) })

  $blockedPathPatterns = @(
    '(^|/)\.env(\.|$)',
    '(^|/)config/secrets(/|$)',
    '\.dpapi$',
    '\.pfx$',
    '\.p12$',
    '\.pem$',
    '\.key$',
    '\.zip$',
    '\.7z$',
    '\.rar$',
    '\.exe$',
    '\.msi$',
    '\.pdb$',
    '(^|/)dist(/|$)',
    '(^|/)third_party/installers(/|$)'
  )

  $secretValuePatterns = @(
    'sk-[A-Za-z0-9_-]{20,}',
    'ghp_[A-Za-z0-9_]{20,}',
    'github_pat_[A-Za-z0-9_]{20,}',
    'xox[baprs]-[A-Za-z0-9-]{20,}',
    'AIza[0-9A-Za-z_-]{20,}',
    'ya29\.[0-9A-Za-z_-]+',
    '(?i)"(access_token|refresh_token|device_code|client_secret|obs_websocket_password|api_key|secret|password|token)"\s*:\s*"[^"]{4,}"'
  )

  $failures = @()

  foreach ($file in $files) {
    $normalized = $file -replace '\\', '/'
    foreach ($pattern in $blockedPathPatterns) {
      if ($normalized -match $pattern) {
        $failures += "blocked path: $file"
        break
      }
    }
  }

  foreach ($file in $files) {
    if ($file -eq "Cargo.lock") { continue }
    $matches = Select-String -LiteralPath $file -Pattern $secretValuePatterns -AllMatches -ErrorAction SilentlyContinue
    foreach ($match in $matches) {
      $failures += "possible secret value: ${file}:$($match.LineNumber)"
    }
  }

  if ($failures.Count -gt 0) {
    Write-Error ("Secret scan failed.`n" + (($failures | Sort-Object -Unique) -join "`n"))
  }

  Write-Host "Secret scan OK"
} finally {
  Pop-Location
}
