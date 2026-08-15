#requires -Version 5.1
<#
Обёртка сборки: проверяет окружение, зовёт package_release.ps1 и объясняет
результат по-русски.

Почему это отдельный файл, а не текст внутри BUILD.bat. Русские сообщения
в .bat не работают: cmd.exe читает файл по смещению в байтах, а `chcp 65001`
меняет разбор на середине, из-за чего многобайтные строки распадаются и куски
комментариев уходят на исполнение как команды. Проверено — вылезает
«"онтрольные" is not recognized as an internal or external command».
Поэтому BUILD.bat остаётся на чистом ASCII, а всё, что читает человек
(и экранный диктор), печатает PowerShell.
#>
# Параметры перечислены явно, а не собраны в «всё остальное».
# Пересылка массивом здесь не работает: элементы уходят позиционно, и
# package_release.ps1 получает строку "-Version" как номер версии, после чего
# спокойно собирает архив с именем AccessibleOBS_ready_-Version.zip.
# Значение по умолчанию намеренно не дублируем — оно одно, в package_release.ps1.
param(
  [string]$Version,
  [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

Write-Host "Accessible OBS — сборка релиза"
Write-Host ""

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  Write-Host "[ОШИБКА] Не найден cargo. Установите Rust: https://rustup.rs"
  exit 1
}

# Установщики Tailscale и OBS весят около 200 МБ. Если они уже скачаны,
# скрипт проверит контрольные суммы и качать не будет.
Write-Host "Собираю бинарники и архив."
Write-Host "Первый раз это долго: качаются установщики Tailscale и OBS."
Write-Host ""

$forward = @{}
if ($Version) { $forward.Version = $Version }
if ($SkipBuild) { $forward.SkipBuild = $true }

try {
  & (Join-Path $PSScriptRoot "package_release.ps1") @forward
  $code = if ($null -eq $LASTEXITCODE) { 0 } else { $LASTEXITCODE }
} catch {
  Write-Host ""
  Write-Host "[ОШИБКА] $($_.Exception.Message)"
  exit 1
}

if ($code -ne 0) {
  Write-Host ""
  Write-Host "[ОШИБКА] Сборка не удалась, код $code. Подробности выше."
  exit $code
}

Write-Host ""
Write-Host "Готово. Архив лежит в папке dist."
Write-Host "Его целиком отправляют актёру: внутри AccessibleOBS.exe,"
Write-Host "панель и официальные установщики Tailscale и OBS."
exit 0
