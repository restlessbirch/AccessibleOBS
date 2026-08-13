@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"
set "RSC_URL=http://127.0.0.1:8787/"
set "RSC_PING=http://127.0.0.1:8787/api/public/ping"

if not exist "bin\host-agent.exe" (
  echo [ERROR] bin\host-agent.exe not found.
  pause
  exit /b 1
)

powershell -NoProfile -ExecutionPolicy Bypass -Command "try { Invoke-WebRequest -UseBasicParsing -Uri '%RSC_PING%' -TimeoutSec 1 | Out-Null; exit 0 } catch { exit 1 }" >nul 2>nul
if errorlevel 1 (
  echo Starting Remote Stream Control Local Mode...
  start "Remote Stream Control Local" /min "%~dp0bin\host-agent.exe" --local --no-open
  powershell -NoProfile -ExecutionPolicy Bypass -Command "$ok=$false; for ($i=0; $i -lt 40; $i++) { try { Invoke-WebRequest -UseBasicParsing -Uri '%RSC_PING%' -TimeoutSec 1 | Out-Null; $ok=$true; break } catch { Start-Sleep -Milliseconds 250 } }; if ($ok) { exit 0 } else { exit 1 }" >nul 2>nul
  if errorlevel 1 (
    echo [WARN] Local Mode started, but web panel did not answer yet.
  )
)

start "" "%RSC_URL%"
