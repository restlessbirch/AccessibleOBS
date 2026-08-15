@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"

rem Один вход для сборки: помнить длинную команду PowerShell не нужно.
rem Всё остальное делает scripts\package_release.ps1 — здесь только вызов
rem и понятные сообщения о том, что пошло не так.

echo Remote Stream Control — сборка релиза
echo.

where cargo >nul 2>nul
if errorlevel 1 (
  echo [ОШИБКА] Не найден cargo. Установите Rust: https://rustup.rs
  pause
  exit /b 1
)

rem Установщики Tailscale и OBS весят около 200 МБ. Если они уже скачаны,
rem скрипт просто проверит их контрольные суммы и качать не будет.
echo Собираю бинарники и архив. Первый раз это долго: качаются установщики.
echo.

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\package_release.ps1" %*
if errorlevel 1 (
  echo.
  echo [ОШИБКА] Сборка не удалась. Смотрите сообщения выше.
  pause
  exit /b 1
)

echo.
echo Готово. Архив лежит в папке dist.
echo Его целиком отправляют актёру: внутри RemoteStreamControl.exe,
echo панель и официальные установщики Tailscale и OBS.
pause
