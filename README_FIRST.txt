REMOTE STREAM CONTROL — README FIRST
====================================

Что это
-------
Папка для удалённого управления OBS на компьютере актёра через Tailscale.
Пользовательский интерфейс владельца — обычная web-панель в браузере, доступная с NVDA.
Код проекта лицензирован MIT, издатель/автор проекта: restlessbirch.

Быстрый запуск
--------------

На компьютере актёра:
1. Распаковать папку RemoteStreamControl.
2. Запустить START_FRIEND.bat.
3. Если Tailscale просит логин — пройти стандартную авторизацию Tailscale.
4. Если OBS ещё не установлен — установщик OBS откроется/запустится автоматически; после установки снова запустить START_FRIEND.bat.
5. В конце появится pairing-код. Его надо один раз сообщить владельцу.

Если архив собран release-скриптом, внутри уже есть официальные установщики:
third_party\installers\tailscale-setup-latest-amd64.msi
third_party\installers\OBS-Studio-*-Windows-Installer.exe

На компьютере владельца:
1. Распаковать эту же папку или минимум START_ME.bat/bin/config.
2. Открыть config\controller.json и указать friend_machine_name — MagicDNS имя компьютера актёра в Tailscale. Можно указать friend_tailscale_ip_fallback.
3. Запустить START_ME.bat.
4. Если Tailscale просит логин — пройти стандартную авторизацию.
5. В браузере откроется Remote Stream Control.
6. Ввести pairing-код с компьютера актёра.

Обычный сценарий после первичной настройки
------------------------------------------
Актёр: START_FRIEND.bat
Владелец: START_ME.bat

Что умеет панель
----------------
- состояние связи/Tailscale/OBS;
- сцены OBS: список и переключение;
- источники текущей сцены: показать/скрыть;
- аудиоисточники: mute/unmute, +/-1 dB, точное dB;
- Start/Stop Stream;
- Start/Stop/Pause/Resume Recording;
- OBS statistics;
- DonationAlerts: сохранить Alerts Widget URL, автоматически создать RSC_OVERLAYS и RSC_DonationAlerts, включить reroute_audio=true, добавить overlay во все сцены;
- DonationAlerts audio: mute/unmute/volume;
- Twitch Device Code OAuth, изменение channel title/category/language, stream marker.

Секреты
-------
Не храните секреты в BAT/JSON.
Приложение хранит эти данные через Windows DPAPI в config\secrets\*.dpapi:
- OBS WebSocket password;
- pairing secret/session;
- DonationAlerts widget URL;
- DonationAlerts OAuth tokens;
- Twitch OAuth tokens.

Настройка Twitch
----------------
Для Twitch Device Code Flow нужен client_id приложения Twitch.
Укажите его в config\host.json:
  twitch.client_id
Scope: channel:manage:broadcast.

Настройка DonationAlerts OAuth
------------------------------
Официальный Alerts Widget работает после вставки URL в панели.
OAuth DonationAlerts нужен отдельно для истории/realtime статуса донатов.
Укажите в config\host.json:
  donationalerts.client_id
  donationalerts.client_secret
  donationalerts.redirect_uri

Важно
-----
DonationAlerts Alerts Widget URL является секретом. После сохранения панель больше не показывает полный URL.

Логи
----
logs\bootstrap.log
logs\host.log
Секреты в лог не пишутся.

Если не открывается панель
--------------------------
1. Проверьте, что оба компьютера в одном Tailscale tailnet.
2. На компьютере владельца проверьте config\controller.json.
3. На компьютере актёра проверьте, что START_FRIEND.bat запущен.
4. Откройте logs\host.log и logs\bootstrap.log.
