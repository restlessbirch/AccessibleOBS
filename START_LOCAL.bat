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

rem Порт 8787 может быть уже занят УДАЛЁННЫМ агентом: START_FRIEND.bat
rem прописывает его в автозагрузку, и он слушает тот же 127.0.0.1.
rem Раньше проверялся только факт отклика, поэтому в самом обычном случае
rem скрипт решал, что локальный режим уже поднят, и открывал браузер —
rem человек получал удалённый режим с требованием pairing-кода.
rem Поэтому смотрим не «отвечает ли», а «кто отвечает».
set "RSC_MODE="
for /f "usebackq delims=" %%m in (`powershell -NoProfile -ExecutionPolicy Bypass -Command "try { (Invoke-RestMethod -Uri '%RSC_PING%' -TimeoutSec 2).mode } catch { '' }"`) do set "RSC_MODE=%%m"

if /i "%RSC_MODE%"=="local" goto :open

if /i "%RSC_MODE%"=="remote" (
  echo.
  echo На порту 8787 уже работает Remote Stream Control в УДАЛЁННОМ режиме.
  echo Скорее всего он запустился сам после настройки START_FRIEND.bat.
  echo.
  echo Локальный режим на этом порту одновременно работать не может.
  echo Что делать:
  echo   1. Пользоваться удалённым режимом: откройте %RSC_URL% и введите pairing-код.
  echo   2. Либо отключить автозапуск удалённого агента и запустить локальный:
  echo        bin\bootstrap.exe --remove-autostart
  echo        затем завершите host-agent.exe в диспетчере задач и запустите этот файл снова.
  echo.
  pause
  exit /b 1
)

echo Запускаю Remote Stream Control в локальном режиме...
start "Remote Stream Control Local" /min "%~dp0bin\host-agent.exe" --local --no-open
powershell -NoProfile -ExecutionPolicy Bypass -Command "$ok=$false; for ($i=0; $i -lt 40; $i++) { try { if ((Invoke-RestMethod -Uri '%RSC_PING%' -TimeoutSec 1).mode -eq 'local') { $ok=$true; break } } catch { } Start-Sleep -Milliseconds 250 }; if ($ok) { exit 0 } else { exit 1 }" >nul 2>nul
if errorlevel 1 (
  echo [ВНИМАНИЕ] Локальный режим запущен, но панель пока не отвечает.
  echo Проверьте logs\host.log
)

:open
start "" "%RSC_URL%"
