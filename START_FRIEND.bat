@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"
if not exist "bin\bootstrap.exe" (
  echo [ERROR] bin\bootstrap.exe not found.
  pause
  exit /b 1
)
"bin\bootstrap.exe" --host
