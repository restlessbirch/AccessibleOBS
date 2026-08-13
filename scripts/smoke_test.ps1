param(
  [int]$Port = 18787,
  [switch]$Local
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$TempRoot = Join-Path $env:TEMP ("rsc-smoke-" + [guid]::NewGuid().ToString("N"))
$App = Join-Path $TempRoot "RemoteStreamControl"

New-Item -ItemType Directory -Force -Path $App | Out-Null
Copy-Item -LiteralPath (Join-Path $Root "bin") -Destination $App -Recurse -Force
Copy-Item -LiteralPath (Join-Path $Root "web") -Destination $App -Recurse -Force
New-Item -ItemType Directory -Force -Path (Join-Path $App "config"), (Join-Path $App "logs") | Out-Null

$hostConfig = @{
  runtime_mode = "remote"
  obs_path = ""
  obs_websocket_host = "127.0.0.1"
  obs_websocket_port = 4455
  listen_mode = "localhost"
  web_port = $Port
  auto_start_obs = $false
  enable_tailscale_unattended_after_login = $false
  twitch = @{ enabled = $true; client_id = ""; scopes = @("channel:manage:broadcast") }
  donationalerts = @{
    enabled = $true
    client_id = ""
    client_secret = ""
    redirect_uri = ""
    overlay_scene_name = "RSC_OVERLAYS"
    input_name = "RSC_DonationAlerts"
    enforce_overlays = $true
    browser_width = "obs_canvas"
    browser_height = "obs_canvas"
    reroute_audio = $true
    initial_volume_db = -8.0
    monitoring = "off"
    oauth_enabled = $true
    oauth_scopes = @("oauth-user-show", "oauth-donation-subscribe", "oauth-donation-index")
    announce_new_donations_to_nvda = $false
  }
}
$hostConfig | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 -LiteralPath (Join-Path $App "config\host.json")

$proc = $null
try {
  $args = @()
  if ($Local) { $args = @("--local", "--no-open") }
  $start = @{
    FilePath = (Join-Path $App "bin\host-agent.exe")
    WorkingDirectory = $App
    WindowStyle = "Hidden"
    PassThru = $true
  }
  if ($args.Count -gt 0) {
    $start.ArgumentList = $args
  }
  $proc = Start-Process @start
  $deadline = (Get-Date).AddSeconds(20)
  do {
    try {
      $ping = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/public/ping" -TimeoutSec 1
      if ($ping.ok -eq $true -and $ping.app -eq "Remote Stream Control") {
        if ($Local) {
          $runtime = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/runtime" -TimeoutSec 1
          if ($runtime.mode -ne "local" -or $runtime.loopback_only -ne $true -or $runtime.tailscale_required -ne $false) {
            throw "unexpected local runtime response"
          }
          $listeners = Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue
          $badListener = $listeners | Where-Object { $_.LocalAddress -notin @("127.0.0.1", "::1") } | Select-Object -First 1
          if ($badListener) {
            throw "local mode is listening on non-loopback address $($badListener.LocalAddress):$Port"
          }
          $auth = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/auth/status" -TimeoutSec 1
          if ($auth.authenticated -ne $true -or $auth.paired -ne $true) {
            throw "local auth status is not bypassed"
          }
          Write-Host "SMOKE_OK local host-agent ping runtime auth"
          exit 0
        }
        Write-Host "SMOKE_OK host-agent ping"
        exit 0
      }
    } catch {
      Start-Sleep -Milliseconds 500
    }
  } while ((Get-Date) -lt $deadline)
  throw "host-agent did not answer /api/public/ping"
}
finally {
  if ($proc -and -not $proc.HasExited) {
    Stop-Process -Id $proc.Id -Force
  }
  if (Test-Path -LiteralPath $TempRoot) {
    Remove-Item -LiteralPath $TempRoot -Recurse -Force
  }
}
