@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"
rem ASCII only: see the note in BUILD.bat about chcp and multi-byte lines.
if not exist "bin\host-agent.exe" (
  echo [ERROR] bin\host-agent.exe not found.
  pause
  exit /b 1
)
"bin\bootstrap.exe" --local
