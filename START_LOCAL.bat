@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"
if not exist "bin\host-agent.exe" (
  echo [ERROR] bin\host-agent.exe not found.
  pause
  exit /b 1
)
"bin\host-agent.exe" --local
