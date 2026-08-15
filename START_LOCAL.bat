@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"
set "RSC_URL=http://127.0.0.1:8787/"
set "RSC_PING=http://127.0.0.1:8787/api/public/ping"

if not exist "bin\host-agent.exe" (
  echo [ОШИБКА] Не найден bin\host-agent.exe
  pause
  exit /b 1
)
"bin\bootstrap.exe" --local
