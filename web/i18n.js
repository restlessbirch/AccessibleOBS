'use strict';

/*
 * Переключение языка панели.
 *
 * Ключом служит сама русская строка, а не выдуманный идентификатор вроде
 * `panel.audio.title`. Причина простая: строк около пятисот, и придумывать под
 * каждую имя — верный способ развести имена и тексты, а потом получить в
 * интерфейсе голый ключ вместо слова. Здесь непереведённая строка остаётся
 * по-русски: это плохо, но это понятно, а `panel.audio.title` на экране —
 * поломка, которую незрячий человек даже не опознает.
 *
 * Переводится текст в трёх местах: в готовой разметке, в узлах, которые панель
 * создаёт сама, и в сообщениях для экранного диктора. Все три проходят через
 * `t()`, поэтому вызовы по коду расставлять не пришлось.
 */

const I18N = {
  en: {
    // --- шапка и вход ---
    'Remote Stream Control': 'Accessible OBS',
    'Accessible OBS': 'Accessible OBS',
    'Подключение': 'Connection',
    'Pairing-код': 'Pairing code',
    'Войти': 'Sign in',
    'Выйти': 'Sign out',
    'Локальная панель управления OBS, эфиром, записью, Twitch и DonationAlerts на этом компьютере.':
      'Local control panel for OBS, streaming, recording, Twitch and DonationAlerts on this computer.',
    'Локальный режим уже авторизован на этом компьютере. Pairing-код не нужен.':
      'Local mode is already authorised on this computer. No pairing code needed.',
    'Панель подключена': 'Panel connected',
    'Сеанс завершён': 'Session ended',
    'Требуется pairing-код': 'Pairing code required',

    // --- состояние ---
    'Состояние': 'Status',
    'Связь с агентом: есть': 'Agent link: connected',
    'Связь с агентом: восстанавливаю…': 'Agent link: reconnecting…',
    'Связь с агентом восстановлена': 'Agent link restored',
    'OBS: нет связи': 'OBS: no connection',
    'Данные ещё не загружены': 'No data loaded yet',
    'Обновить всё': 'Refresh everything',
    'Состояние обновлено полностью': 'State fully refreshed',
    'Tailscale': 'Tailscale',
    'OBS процесс': 'OBS process',
    'OBS WebSocket': 'OBS WebSocket',
    'Автозапуск у актёра': 'Autostart on the actor machine',
    'OBS готов к управлению': 'OBS ready for control',
    'запущен': 'running',
    'не запущен': 'not running',
    'подключён': 'connected',
    'нет связи': 'no connection',
    'настроен': 'configured',
    'не настроен': 'not configured',
    'да': 'yes',
    'нет': 'no',
    'неизвестно': 'unknown',
    'Ошибка': 'Error',
    'Внимание': 'Warning',
    'нет данных': 'no data',

    // --- сцены и источники ---
    'Сцены': 'Scenes',
    'Обновить сцены': 'Refresh scenes',
    'Добавить сцену': 'Add scene',
    'Название': 'Name',
    'Добавить': 'Add',
    'Сцена': 'Scene',
    'Переключить': 'Switch',
    'Источники текущей сцены': 'Sources in the current scene',
    'Обновить источники': 'Refresh sources',
    'Добавить источник': 'Add source',
    'Тип': 'Kind',
    'Название источника обязательно': 'Source name is required',
    'в этой сцене нет источников': 'no sources in this scene',
    'Только что добавлен в OBS.': 'Just added to OBS.',
    'Настройки источника в панели': 'Source settings in the panel',
    'Скрыть настройки в панели': 'Hide settings in the panel',
    'Открыть окно свойств в OBS у актёра': 'Open the OBS properties window on the actor machine',
    'Открыто родное окно свойств OBS: ': 'Opened the native OBS properties window: ',
    'Загружаю реальные настройки OBS…': 'Loading actual OBS settings…',
    'Загружаю списки выбора из OBS…': 'Loading choices from OBS…',
    'Сохранить настройки в OBS': 'Save settings to OBS',
    'Монитор': 'Monitor',
    'Монитор (номер)': 'Monitor (index)',
    'Окно': 'Window',
    'Режим захвата': 'Capture mode',
    'Камера': 'Camera',
    'Микрофон': 'Microphone',
    'Звуковое устройство': 'Audio device',
    'Разрешение': 'Resolution',
    '— не выбрано —': '— not selected —',
    'Удалить источник': 'Remove source',
    'Удалить сцену': 'Remove scene',

    // --- аудио ---
    'Аудио': 'Audio',
    'Обновить аудио': 'Refresh audio',
    'Громкость': 'Volume',
    'Уровень': 'Level',
    'тишина': 'silence',
    'В этой сцене': 'In this scene',
    'включён': 'on',
    'выключен': 'off',

    // --- эфир и запись ---
    'Эфир': 'Stream',
    'Запись': 'Recording',
    'Эфир идёт — остановить эфир': 'Streaming — stop the stream',
    'Эфир остановлен — начать эфир': 'Stream stopped — start streaming',
    'Запись идёт — остановить запись': 'Recording — stop recording',
    'Запись остановлена — начать запись': 'Recording stopped — start recording',
    'Запись идёт — поставить на паузу': 'Recording — pause',
    'Запись на паузе — продолжить запись': 'Recording paused — resume',
    'Studio Mode': 'Studio Mode',
    'Studio Mode включён — выключить': 'Studio Mode on — turn off',
    'Studio Mode выключен — включить': 'Studio Mode off — turn on',
    'Виртуальная камера': 'Virtual camera',
    'Виртуальная камера включена — выключить': 'Virtual camera on — turn off',
    'Виртуальная камера выключена — включить': 'Virtual camera off — turn on',
    'Буфер повтора': 'Replay buffer',
    'Буфер повтора включён — выключить': 'Replay buffer on — turn off',
    'Буфер повтора выключен — включить': 'Replay buffer off — turn on',
    'Сохранить повтор': 'Save replay',
    'Кадр эфира': 'Stream frame',
    'Обновить кадр': 'Refresh frame',
    'Проверяю…': 'Checking…',

    // --- готовность ---
    'Готовность к эфиру': 'Stream readiness',
    'Проверить готовность': 'Check readiness',
    'Проверка не запускалась': 'Check has not run',
    'Картинка в эфире': 'Picture on air',
    'Кадр не пустой': 'Frame is not empty',
    'Сигнал микрофона': 'Microphone signal',
    'Звук рабочего стола': 'Desktop audio',
    'Содержимое сцены': 'Scene contents',
    'Текущая сцена': 'Current scene',
    'Сервис вещания': 'Streaming service',
    'Свободно на диске': 'Free disk space',
    'Можно начинать': 'Ready to start',
    'Начинать нельзя': 'Not ready',

    // --- журнал, клавиши, диагностика ---
    'Журнал эфира': 'Session log',
    'Горячие клавиши': 'Keyboard shortcuts',
    'Работают, когда курсор не в поле ввода.': 'Active when the cursor is not in a text field.',
    'Статистика OBS': 'OBS statistics',
    'Диагностика': 'Diagnostics',
    'Собрать сводку': 'Collect summary',
    'Во время эфира не переключайте: OBS перезагрузит настройки.':
      'Do not switch while live: OBS reloads its settings.',
    'Профиль, коллекция сцен и переход': 'Profile, scene collection and transition',

    // --- экран актёра ---
    'Экран актёра': 'Actor display',
    'Открыть экран актёра': 'Open actor display',
    'Открыть только чат': 'Open chat only',
    'Открыть только донаты': 'Open donations only',
    'Проектор закрывается только у актёра, клавишей Escape.':
      'The projector can only be closed on the actor machine, with Escape.',
    'Вывести на монитор': 'Send to monitor',
    'Обновить список': 'Refresh list',
    'Чат': 'Chat',
    'Донаты': 'Donations',
    'Донатов пока нет': 'No donations yet',

    // --- роли ---
    'Роли источников': 'Source roles',
    'Сохранить роли': 'Save roles',
    'Откуда взят': 'Source of the value',
    'взят из настроек OBS': 'taken from OBS settings',
    'не назначена': 'not assigned',
    'не назначено': 'not assigned',

    // --- DonationAlerts ---
    'DonationAlerts': 'DonationAlerts',
    'Регистрация DonationAlerts': 'DonationAlerts registration',
    'Сохранить регистрацию DonationAlerts': 'Save DonationAlerts registration',
    'Подключить DonationAlerts': 'Connect DonationAlerts',
    'Client ID': 'Client ID',
    'Client secret': 'Client secret',
    'Redirect URI': 'Redirect URI',
    'Scopes': 'Scopes',
    'Виджет настроен': 'Widget configured',
    'Звук ведётся в OBS': 'Audio routed into OBS',
    'Звук виджета': 'Widget audio',
    'Громкость виджета': 'Widget volume',
    'Сцена оверлея': 'Overlay scene',
    'Источник': 'Source',
    'Лента донатов': 'Donation feed',
    'подключена': 'connected',
    'не подключена': 'not connected',
    'OAuth не пройден': 'OAuth not completed',
    'Слышите ли донаты вы': 'Do you hear donations',
    'нет, только зрители': 'no, viewers only',
    'Последняя попытка подключения': 'Last connection attempt',
    'секрет сохранён; пусто = не менять': 'secret saved; leave empty to keep',
    'обязателен для OAuth': 'required for OAuth',
    'оставьте пустым, чтобы не менять': 'leave empty to keep the current value',

    // --- Twitch ---
    'Twitch': 'Twitch',
    'Регистрация Twitch': 'Twitch registration',
    'Подключить Twitch': 'Connect Twitch',
    'Проверить авторизацию': 'Check authorisation',
    'Настроен client_id': 'client_id configured',
    'Подключён': 'Connected',
    'Название трансляции': 'Stream title',
    'Категория': 'Category',
    'Язык': 'Language',
    'Сохранить параметры канала': 'Save channel settings',
    'Поставить метку': 'Create marker',

    // --- язык ---
    'Язык панели': 'Panel language',
    'Русский': 'Russian',
    'Английский': 'English',

    // --- навигация и вход ---
    'Перейти к управлению': 'Skip to controls',
    'Подключиться': 'Connect',
    'Запустить OBS': 'Start OBS',

    // --- состояния эфира и записи ---
    'Эфир: остановлен': 'Stream: stopped',
    'Эфир: идёт': 'Stream: live',
    'Эфир: запускается': 'Stream: starting',
    'Эфир: останавливается': 'Stream: stopping',
    'Эфир: переподключается': 'Stream: reconnecting',
    'Запись: остановлена': 'Recording: stopped',
    'Запись: идёт': 'Recording: running',
    'Запись: на паузе': 'Recording: paused',
    'Запись: запускается': 'Recording: starting',
    'Запись: останавливается': 'Recording: stopping',
    'Остановить эфир': 'Stop the stream',
    'Остановить запись': 'Stop recording',
    'Продолжить запись': 'Resume recording',
    'Длительность': 'Duration',
    'Потеряно кадров': 'Dropped frames',
    'Отправлено': 'Sent',
    'Studio Mode выключен': 'Studio Mode off',
    'Studio Mode включён': 'Studio Mode on',
    'Выключить': 'Turn off',
    'Сцена на предпросмотре': 'Preview scene',
    'Выбрать': 'Select',
    'Вывести предпросмотр в эфир': 'Send preview to air',
    'Обновлять автоматически': 'Refresh automatically',
    'Виртуальная камера выключена': 'Virtual camera off',
    'Виртуальная камера включена': 'Virtual camera on',
    'Буфер повтора выключен': 'Replay buffer off',
    'Буфер повтора включён': 'Replay buffer on',
    'Буфер повтора выключен в настройках OBS у актёра (Настройки → Вывод → Буфер повтора).':
      'The replay buffer is disabled in OBS on the actor machine (Settings → Output → Replay Buffer).',

    // --- сцены, источники, аудио ---
    'Переключить текущую сцену': 'Switch the current scene',
    'Все сцены': 'All scenes',
    'Применить': 'Apply',
    'Проверить микрофон': 'Test the microphone',
    'Проверить звук': 'Test the audio',
    'Микрофон включён — выключить': 'Microphone on — mute',
    'Микрофон выключен — включить': 'Microphone muted — unmute',
    'Основной микрофон': 'Main microphone',
    'Сейчас виден — скрыть источник': 'Visible — hide the source',
    'Сейчас скрыт — показать источник': 'Hidden — show the source',
    'В этой сцене включён — выключить здесь': 'On in this scene — turn off here',
    'В этой сцене выключен — включить здесь': 'Off in this scene — turn on here',
    'Профиль': 'Profile',
    'Коллекция сцен': 'Scene collection',
    'Переход': 'Transition',

    // --- журнал и клавиши ---
    'Очистить журнал': 'Clear the log',
    'Горячие клавиши включены': 'Keyboard shortcuts enabled',
    'переключить сцену с этим номером': 'switch to the scene with this number',
    'выключить или включить микрофон': 'mute or unmute the microphone',
    'обновить кадр эфира': 'refresh the stream frame',
    'обновить всё': 'refresh everything',
    'вывести предпросмотр в эфир': 'send the preview to air',
    'Получить статистику': 'Get statistics',

    // --- экран актёра и диагностика ---
    'Открыть чат и донаты у актёра': 'Open chat and donations on the actor machine',
    'Вывести на второй монитор актёра': 'Send to the actor second monitor',
    'Вывести на этот монитор': 'Send to this monitor',
    'Открыть этот экран в текущем браузере': 'Open this display in the current browser',
    'Собрать диагностику': 'Collect diagnostics',
    'Скопировать': 'Copy',
    'Вывод на второй монитор скрыт: в доступном режиме чат и донаты зачитываются вслух, а окно проектора OBS экранный диктор прочитать не может. Переключить режим можно на начальной странице.':
      'Second-monitor output is hidden: in accessible mode chat and donations are announced aloud, and an OBS projector window cannot be read by a screen reader. The mode can be changed on the launcher page.',

    // --- DonationAlerts ---
    'Ссылка на Alerts Widget': 'Alerts Widget link',
    'Сохранить и настроить OBS': 'Save and configure OBS',
    'Проверить и восстановить в OBS': 'Verify and repair in OBS',
    'Перезагрузить виджет': 'Reload the widget',
    'Громкость оповещений': 'Alert volume',
    'Громкость, dB': 'Volume, dB',
    'Звук донатов включён — выключить': 'Donation audio on — turn off',
    'Звук донатов выключен — включить': 'Donation audio off — turn on',
    'Включить звук': 'Turn audio on',
    'Лента донатов в панели': 'Donation feed in the panel',
    'Нужно только для показа списка донатов здесь. На озвучку в эфире не влияет.':
      'Only needed to show the donation list here. Does not affect audio on the stream.',
    'Объявлять новые донаты вслух': 'Announce new donations aloud',
    'Последние донаты': 'Recent donations',
    'донатов пока нет': 'no donations yet',
    'Примечание': 'Note',
    'Озвучка донатов включается на стороне DonationAlerts:':
      'Donation speech is enabled on the DonationAlerts side:',
    'Панель управления → Оповещения': 'Dashboard → Alerts',

    // --- Twitch ---
    'Сохранить регистрацию Twitch': 'Save Twitch registration',
    'Сохраните Twitch client_id в веб-панели': 'Save the Twitch client_id in the panel',
    'Параметры канала': 'Channel settings',
    'ID категории': 'Category ID',
    'Сохранить параметры': 'Save settings',
    'Маркер трансляции': 'Stream marker',
    'Описание': 'Description',
    'Создать маркер': 'Create marker',
    'Подключить Twitch (сохранит Client ID)': 'Connect Twitch (saves the Client ID)',

    // --- экран актёра ---
    'Загружаю чат и донаты…': 'Loading chat and donations…',
    'Экран актёра активен.': 'Actor display is active.',
    'Экран актёра обновлён': 'Actor display refreshed',
    'На весь экран': 'Full screen',
    'Обновить': 'Refresh',
    'Проверяю DonationAlerts…': 'Checking DonationAlerts…',
    'Twitch-чат пока недоступен: подключите Twitch в панели оператора.':
      'Twitch chat is not available yet: connect Twitch in the operator panel.',
    'Связь с агентом потеряна, восстанавливаю': 'Agent link lost, reconnecting',
    'Связь с агентом восстанавливается…': 'Reconnecting to the agent…',
    'виджет настроен': 'widget configured',
    'виджет не настроен': 'widget not configured',
    'лента подключена': 'feed connected',
    'лента не подключена': 'feed not connected',
    'Аноним': 'Anonymous',
    'Гость': 'Guest',
    'Перейти к экрану актёра': 'Skip to the actor display',
    'Сцена в эфире неизвестна': 'Scene on air is unknown',
    'Лента донатов в панели': 'Donation feed in the panel',
    'Параметры канала': 'Channel settings',
    'Просмотр сцены': 'Scene preview',
    'Показать источники и звук сцены': 'Show sources and audio of a scene',
    'Вывести эту сцену в эфир': 'Put this scene on air',
    'Экран актёра — Accessible OBS': 'Actor display — Accessible OBS',
    'Код устарел. Нажмите «Подключить Twitch» заново.':
      'The code has expired. Press "Connect Twitch" again.',
    'Код ещё не подтверждён на сайте Twitch.': 'The code has not been confirmed on Twitch yet.',

    // --- виды источников OBS ---
    //
    // Скобки с системным именем оставлены: по нему человек найдёт тот же
    // пункт в самом OBS, каким бы языком тот ни говорил.
    'Изображение (image_source)': 'Image (image_source)',
    'Цвет (color_source_v3)': 'Colour (color_source_v3)',
    'Слайд-шоу (slideshow_v2)': 'Slideshow (slideshow_v2)',
    'Браузер (browser_source)': 'Browser (browser_source)',
    'Медиафайл (ffmpeg_source)': 'Media file (ffmpeg_source)',
    'Текст (text_gdiplus_v3)': 'Text (text_gdiplus_v3)',
    'Текст FreeType (text_ft2_source_v2)': 'FreeType text (text_ft2_source_v2)',
    'Захват экрана (monitor_capture)': 'Display capture (monitor_capture)',
    'Захват окна (window_capture)': 'Window capture (window_capture)',
    'Захват игры (game_capture)': 'Game capture (game_capture)',
    'Устройство видео (dshow_input)': 'Video device (dshow_input)',
    'Захват входного аудио (wasapi_input_capture)': 'Audio input capture (wasapi_input_capture)',
    'Захват выходного аудио (wasapi_output_capture)': 'Audio output capture (wasapi_output_capture)',
    'Захват звука приложения (wasapi_process_output_capture)':
      'Application audio capture (wasapi_process_output_capture)',
    'неизвестный тип': 'unknown kind',
    '— не назначено —': '— not assigned —',
    'Уровень:': 'Level:',
    ', раздел «Озвучка сообщений». Панель лишь добавляет виджет в OBS и ведёт звук в эфир.':
      ', the "Message speech" section. The panel only adds the widget to OBS and routes its audio to the stream.',
  },
};

/// Строки, собранные из шаблона и данных.
///
/// Их нельзя перечислить в словаре: внутри имя сцены, версия OBS или время.
/// Переводим обрамление, а подставленное оставляем как есть — имена сцен и
/// источников придумал актёр, и переводить их значило бы показывать человеку
/// то, чего он не найдёт у себя в OBS.
const I18N_PATTERNS = {
  en: [
    [/^Данные актуальны на (.+)$/, 'Data current as of $1'],
    [/^Обновлено частично на (.+)\. Не отвечают: (.+)$/, 'Partially refreshed at $1. Not responding: $2'],
    [/^OBS (.+): подключён$/, 'OBS $1: connected'],
    [/^(.+) — текущая$/, '$1 — current'],
    [/^Тип OBS: (.+)\. Состояние: виден\.$/, 'OBS kind: $1. State: visible.'],
    [/^Тип OBS: (.+)\. Состояние: скрыт\.$/, 'OBS kind: $1. State: hidden.'],
    [/^Звук: включён\. Громкость: (.+)\.$/, 'Audio: on. Volume: $1.'],
    [/^Звук: выключен\. Громкость: (.+)\.$/, 'Audio: muted. Volume: $1.'],
    [/^В этой сцене: включён\. Громкость: (.+)\.$/, 'In this scene: on. Volume: $1.'],
    [/^В этой сцене: выключен\. Громкость: (.+)\.$/, 'In this scene: muted. Volume: $1.'],
    [/^Уровень: (.+)$/, 'Level: $1'],
    [/^Основной микрофон: (.+)$/, 'Main microphone: $1'],
    [/^Сцена: (.+)$/, 'Scene: $1'],
    [/^Текущая сцена: (.+)$/, 'Current scene: $1'],
    [/^Можно начинать, но есть предупреждений: (\d+)$/, 'Ready to start, warnings: $1'],
    [/^Начинать нельзя, критических проблем: (\d+), предупреждений: (\d+)$/,
      'Not ready: $1 critical, $2 warnings'],
    [/^Активных источников с картинкой: (\d+) из (\d+)$/, 'Active visual sources: $1 of $2'],
    [/^Свободно (.+)$/, '$1 free'],
    [/^Подключён, версия (.+)$/, 'Connected, version $1'],
    // Единицы измерения. Числа не трогаем, меняем только подпись.
    [/^([\d.,]+) МБ$/, '$1 MB'],
    [/^([\d.,]+) ГБ$/, '$1 GB'],
    [/^([\d.,]+) КБ$/, '$1 KB'],
  ],
};

let currentLang = 'ru';

/// Ключи со схлопнутыми пробелами.
///
/// В разметке строка может быть разбита переносом с отступом, а в коде —
/// склеена из кусков с пробелом на конце. Для человека это одна и та же фраза,
/// для точного сравнения — три разные, и словарь молча промахивался.
const I18N_LOOSE = {};
for (const [lang, table] of Object.entries(I18N)) {
  I18N_LOOSE[lang] = {};
  for (const [key, value] of Object.entries(table)) {
    I18N_LOOSE[lang][key.replace(/\s+/g, ' ').trim()] = value;
  }
}

/** Переводит строку. Незнакомая остаётся как есть — по-русски. */
function t(text) {
  if (currentLang === 'ru' || text === null || text === undefined) return text;
  const table = I18N[currentLang];
  if (!table) return text;
  const raw = String(text);
  if (table[raw] !== undefined) return table[raw];

  // Тот же ключ, но без разницы в пробелах. Ведущие и хвостовые пробелы
  // возвращаем на место: на них держится вёрстка вроде «Уровень: тишина».
  const loose = raw.replace(/\s+/g, ' ').trim();
  const found = I18N_LOOSE[currentLang][loose];
  if (found !== undefined) {
    const lead = raw.match(/^\s*/)[0];
    const tail = raw.match(/\s*$/)[0];
    return lead + found + tail;
  }

  for (const [pattern, replacement] of I18N_PATTERNS[currentLang] || []) {
    const match = raw.match(pattern);
    if (!match) continue;
    // Подставленные куски прогоняем через словарь тоже: внутри может оказаться
    // не имя сцены, а наш же термин — например вид источника OBS. Незнакомое
    // останется как есть, и это правильно: имена придумал актёр.
    return replacement.replace(/\$(\d)/g, (_, n) => t(match[Number(n)] ?? ''));
  }

  // Частый случай: строка собрана как «Метка: значение». Переводим метку,
  // значение оставляем — это имя сцены или устройства, его переводить нельзя.
  const at = raw.indexOf(': ');
  if (at > 0) {
    const head = raw.slice(0, at);
    if (table[head] !== undefined) return table[head] + raw.slice(at);
  }
  return raw;
}

/// Переводит уже готовую разметку: тексты, подсказки полей и метки для диктора.
///
/// Обход дерева вместо data-атрибутов в каждом теге: разметка писалась
/// по-русски и остаётся источником правды, а расставлять полторы сотни
/// атрибутов вручную — работа, при которой обязательно что-нибудь пропустишь.
function translateDom(root) {
  if (currentLang === 'ru') return;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const texts = [];
  while (walker.nextNode()) texts.push(walker.currentNode);
  for (const node of texts) {
    const value = node.nodeValue.trim();
    if (!value) continue;
    const translated = t(value);
    if (translated !== value) node.nodeValue = node.nodeValue.replace(value, translated);
  }
  for (const el of root.querySelectorAll('[placeholder]')) {
    el.placeholder = t(el.placeholder);
  }
  for (const el of root.querySelectorAll('[aria-label]')) {
    el.setAttribute('aria-label', t(el.getAttribute('aria-label')));
  }
  for (const el of root.querySelectorAll('[title]')) {
    el.title = t(el.title);
  }
  // Заголовок вкладки: он вне body, обходом дерева его не достать, а диктор
  // читает его при переключении окон.
  if (root === document.body) {
    document.title = t(document.title);
  }
}

function setLanguage(lang) {
  currentLang = lang === 'en' ? 'en' : 'ru';
  document.documentElement.lang = currentLang;
}

function currentLanguage() {
  return currentLang;
}
