@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"

rem ASCII only, deliberately. cmd.exe tracks its position in this file by byte
rem offset, and "chcp 65001" changes how the remaining bytes are decoded, so
rem multi-byte lines fall apart and comment fragments get executed as commands.
rem All human-readable output lives in scripts\build.ps1 instead.

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build.ps1" %*
set "CODE=%errorlevel%"
pause
exit /b %CODE%
