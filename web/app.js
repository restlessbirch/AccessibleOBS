'use strict';

/*
 * Панель Remote Stream Control.
 *
 * Два принципа, от которых лучше не отступать при правках:
 *
 * 1. Никакого innerHTML с данными. Имена сцен и источников задаёт актёр
 *    в OBS, а список донатов приходит из интернета. Строка вида
 *    <img src=x onerror=…> в имени источника выполнила бы скрипт прямо
 *    в панели владельца, у которой есть сессионная кука. Поэтому весь DOM
 *    собирается через el() и textContent.
 *
 * 2. Состояние приходит событиями, а не опросом. EventSource слушает
 *    /api/events, куда агент шлёт события OBS и донаты. Опрос остаётся
 *    только как ручная кнопка «Обновить».
 */

const $ = (id) => document.getElementById(id);

/** Безопасное создание узла: текст всегда попадает через textContent. */
function el(tag, props = {}, children = []) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === null || value === undefined) continue;
    if (key === 'text') node.textContent = String(value);
    else if (key === 'onClick') node.addEventListener('click', value);
    else if (key in node) node[key] = value;
    else node.setAttribute(key, String(value));
  }
  for (const child of [].concat(children)) {
    if (child) node.append(child);
  }
  return node;
}

function button(text, onClick, variant) {
  return el('button', { type: 'button', text, onClick, 'data-variant': variant });
}

function replace(container, nodes) {
  container.replaceChildren(...[].concat(nodes).filter(Boolean));
}

/** Вежливое сообщение: NVDA прочитает, не прерывая текущую фразу. */
function say(text) {
  $('live').textContent = text;
}
/** Настойчивое сообщение — для ошибок и донатов. */
function announce(text) {
  $('alerts').textContent = text;
}

function fail(error) {
  const message = error?.message || String(error);
  announce('Ошибка: ' + message);
}

// ---------------------------------------------------------------- API

async function api(path, options = {}) {
  const res = await fetch(path, {
    credentials: 'same-origin',
    headers: { 'content-type': 'application/json', ...(options.headers || {}) },
    ...options,
  });
  const text = await res.text();
  let data = {};
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = { raw: text };
    }
  }
  if (res.status === 401) {
    showLogin(true);
    throw new Error(data?.error?.message || 'Требуется pairing-код');
  }
  if (!res.ok) {
    throw new Error(data?.error?.message || data?.message || `HTTP ${res.status}`);
  }
  return data;
}

const post = (path, body) =>
  api(path, { method: 'POST', body: JSON.stringify(body ?? {}) });

/** Запрос к OBS, для которого нет отдельной ручки в API агента. */
const obsRequest = (requestType, requestData) =>
  post('/api/obs/request', { requestType, requestData: requestData ?? {} });

// ---------------------------------------------------------------- вход

function showLogin(show) {
  $('login').hidden = !show;
  $('panel').hidden = show;
  if (show) {
    closeEvents();
    // Иначе автообновление кадра продолжало бы дёргать API каждые две
    // секунды после выхода, получая 401 в бесконечном цикле.
    stopAutoPreview();
    $('pairingSecret').focus();
  }
}

$('loginForm').addEventListener('submit', async (e) => {
  e.preventDefault();
  const error = $('loginError');
  error.hidden = true;
  try {
    await post('/api/auth/login', { secret: $('pairingSecret').value });
    $('pairingSecret').value = '';
    showLogin(false);
    say('Панель подключена');
    openEvents();
    await refreshAll();
  } catch (err) {
    error.textContent = err.message;
    error.hidden = false;
    announce(err.message);
  }
});

$('logout').onclick = async () => {
  try {
    await post('/api/auth/logout');
  } catch { /* всё равно возвращаемся к экрану входа */ }
  showLogin(true);
  say('Сеанс завершён');
};

// ------------------------------------------------------- поток событий

let source = null;
let linkLost = false;
/** Обновления после событий склеиваем, чтобы шквал не дёргал API. */
const pending = new Map();

function schedule(key, fn, delay = 250) {
  clearTimeout(pending.get(key));
  pending.set(key, setTimeout(() => {
    pending.delete(key);
    fn().catch(() => {});
  }, delay));
}

function openEvents() {
  if (source) return;
  source = new EventSource('/api/events');
  source.onmessage = (e) => {
    let msg;
    try {
      msg = JSON.parse(e.data);
    } catch {
      return;
    }
    handleEvent(msg);
  };
  source.onopen = () => {
    setState($('linkState'), 'ok', 'Связь с агентом: есть');
    if (linkLost) {
      // За время обрыва у актёра могло измениться что угодно, а событий об
      // этом мы не получили. Перечитываем всё, иначе панель показывала бы
      // устаревшую картину, выглядя при этом исправной.
      linkLost = false;
      say('Связь с агентом восстановлена');
      refreshAll();
    }
  };
  source.onerror = () => {
    // EventSource переподключается сам; наше дело — не врать о состоянии.
    // Индикатор OBS трогать нельзя: OBS у актёра может прекрасно работать,
    // это у нас пропал канал событий.
    linkLost = true;
    setState($('linkState'), 'bad', 'Связь с агентом: восстанавливаю…');
  };
}

function closeEvents() {
  source?.close();
  source = null;
}

function handleEvent(msg) {
  switch (msg.type) {
    case 'obs':
      handleObsEvent(msg.event?.eventType, msg.event?.eventData || {});
      break;
    case 'obs_status':
      renderObsState(msg.status);
      if (msg.status?.connected) schedule('all', refreshAll, 500);
      break;
    case 'levels':
      handleLevels(msg.levels);
      break;
    case 'alert':
      handleAlert(msg);
      break;
    case 'resync_required':
      // Агент сообщил, что события потерялись. Всё, что панель показывает
      // сейчас, могло устареть, поэтому перечитываем состояние целиком.
      journal(`Потеряно событий: ${msg.lost}. Обновляю состояние.`, 'bad');
      schedule('all', refreshAll, 100);
      break;
    case 'donation':
      addDonation(msg.donation, true);
      break;
    case 'donationalerts_status':
      schedule('da', refreshDa);
      break;
  }
}

function handleObsEvent(type, data) {
  switch (type) {
    case 'CurrentProgramSceneChanged':
      currentScene = data.sceneName || currentScene;
      say('Текущая сцена: ' + currentScene);
      journal('Сцена: ' + currentScene);
      schedule('scenes', refreshScenes);
      break;
    case 'SceneListChanged':
    case 'SceneCreated':
    case 'SceneRemoved':
    case 'SceneNameChanged':
      schedule('scenes', refreshScenes);
      break;
    case 'SceneItemEnableStateChanged':
    case 'SceneItemCreated':
    case 'SceneItemRemoved':
      schedule('sources', refreshSources);
      break;
    case 'InputMuteStateChanged':
      journal(`${data.inputName}: звук ${data.inputMuted ? 'выключен' : 'включён'}`);
      schedule('audio', refreshAudio);
      break;
    case 'InputVolumeChanged':
    case 'InputCreated':
    case 'InputRemoved':
    case 'InputNameChanged':
      schedule('audio', refreshAudio);
      break;
    case 'StreamStateChanged':
      renderOutputState($('streamState'), 'Эфир', data);
      schedule('outputs', refreshOutputs, 1000);
      break;
    case 'RecordStateChanged':
      renderOutputState($('recordState'), 'Запись', data);
      break;
    case 'StudioModeStateChanged':
    case 'CurrentPreviewSceneChanged':
      schedule('studio', refreshStudio);
      break;
    case 'VirtualcamStateChanged':
      schedule('vcam', refreshVcam);
      break;
    case 'ReplayBufferStateChanged':
      schedule('replay', refreshReplay);
      break;
    case 'ReplayBufferSaved':
      say('Повтор сохранён: ' + (data.savedReplayPath || 'файл записан'));
      break;
    case 'CurrentSceneTransitionChanged':
    case 'ProfileListChanged':
    case 'CurrentProfileChanged':
    case 'SceneCollectionListChanged':
    case 'CurrentSceneCollectionChanged':
      schedule('setups', refreshSetups);
      break;
    case 'ExitStarted':
      // OBS предупреждает о закрытии до того, как оборвётся сокет. Без этого
      // владелец увидел бы просто «нет связи» и не понял, что произошло.
      announce('Актёр закрывает OBS. Управление пропадёт.');
      journal('Актёр закрывает OBS', 'bad');
      break;
  }
}

// -------------------------------------------------------- индикаторы

function setState(node, state, text) {
  node.dataset.state = state;
  node.textContent = text;
}

function renderObsState(status) {
  if (status?.connected) {
    const version = status.obs_version ? ` ${status.obs_version}` : '';
    setState($('obsState'), 'ok', `OBS${version}: подключён`);
  } else {
    setState($('obsState'), 'bad', 'OBS: нет связи');
  }
}

/** OBS шлёт промежуточные состояния STARTING/STOPPING — показываем и их. */
function renderOutputState(node, label, data) {
  const active = data.outputActive;
  if (label === 'Эфир') streaming = Boolean(active);
  const raw = data.outputState || '';
  if (raw === 'RECONNECTING') return setState(node, 'bad', `${label}: переподключается`);
  if (raw.endsWith('STARTING')) return setState(node, 'warn', `${label}: запускается`);
  if (raw.endsWith('STOPPING')) return setState(node, 'warn', `${label}: останавливается`);
  if (raw.endsWith('PAUSED')) return setState(node, 'warn', `${label}: на паузе`);
  if (active) {
    setState(node, 'ok', `${label}: идёт`);
    say(`${label} идёт`);
  } else {
    setState(node, 'bad', `${label}: остановлен${label === 'Запись' ? 'а' : ''}`);
  }
}

// ---------------------------------------------------------- обновление

let currentScene = '';
let sceneNames = [];
/// Когда данные обновлялись в последний раз и что не ответило.
let lastRefresh = null;
let streaming = false;
let streamTrouble = false;

/// Обновляет всё и честно сообщает, что именно обновить не удалось.
///
/// Прежде здесь был Promise.allSettled без разбора результатов, и панель
/// говорила «Состояние обновлено», даже когда половина разделов не ответила.
/// Для оператора, который не видит экран, это худший вид неправды: он слышит
/// подтверждение и считает, что данные перед ним свежие.
///
/// Возвращает список имён разделов, которые обновить не вышло.
async function refreshAll() {
  // Сцены обновляем первыми и дожидаемся: от их списка зависит выбор сцены
  // предпросмотра в Studio Mode. Запуск всего скопом оставлял бы этот
  // список пустым при первой загрузке — кто успел, тот и прав.
  const failed = [];
  try {
    await refreshScenes();
  } catch {
    failed.push('Сцены');
  }

  const rest = [
    ['Состояние', refreshHealth],
    ['Аудио', refreshAudio],
    ['Эфир и запись', refreshOutputs],
    ['Studio Mode', refreshStudio],
    ['Виртуальная камера', refreshVcam],
    ['Буфер повтора', refreshReplay],
    ['Профили и переходы', refreshSetups],
    ['DonationAlerts', refreshDa],
    ['Twitch', refreshTwitch],
    ['Донаты', refreshDonations],
  ];
  const results = await Promise.allSettled(rest.map(([, fn]) => fn()));
  results.forEach((r, i) => {
    if (r.status === 'rejected') failed.push(rest[i][0]);
  });

  lastRefresh = { at: new Date(), failed };
  renderFreshness();
  return failed;
}

/// Показывает, когда данные обновлялись и что именно устарело.
function renderFreshness() {
  const node = $('freshness');
  if (!lastRefresh) return setState(node, 'warn', 'Данные ещё не загружены');
  const { at, failed } = lastRefresh;
  if (failed.length === 0) {
    setState(node, 'ok', `Данные актуальны на ${timeLabel(at)}`);
  } else {
    setState(
      node,
      'bad',
      `Обновлено частично на ${timeLabel(at)}. Не отвечают: ${failed.join(', ')}`,
    );
  }
}

function renderDl(node, obj) {
  const rows = [];
  for (const [key, value] of Object.entries(obj || {})) {
    rows.push(el('dt', { text: key }));
    rows.push(
      el('dd', {
        text: value !== null && typeof value === 'object'
          ? JSON.stringify(value)
          : String(value),
      }),
    );
  }
  replace(node, rows.length ? rows : [el('dd', { class: 'empty', text: 'нет данных' })]);
}

async function refreshHealth() {
  try {
    const health = await api('/api/health');
    renderObsState(health.obs);
    renderDl($('health'), {
      'Tailscale': health.tailscale,
      'OBS процесс': health.obs_process_running ? 'запущен' : 'не запущен',
      'OBS WebSocket': health.obs?.connected ? 'подключён' : (health.obs?.error || 'нет связи'),
      'Автозапуск у актёра': health.autostart ? 'настроен' : 'не настроен',
      'Готов к эфиру': health.ready_to_stream ? 'да' : 'нет',
      ...(health.obs_crashed_last_run
        ? { 'Внимание': 'прошлый сеанс OBS завершился аварийно' }
        : {}),
    });
  } catch (e) {
    renderDl($('health'), { 'Ошибка': e.message });
  }
}

function formatDuration(ms) {
  const total = Math.floor(Number(ms || 0) / 1000);
  const pad = (n) => String(n).padStart(2, '0');
  return `${pad(Math.floor(total / 3600))}:${pad(Math.floor(total / 60) % 60)}:${pad(total % 60)}`;
}

/// Показывает здоровье эфира, а не только «идёт / не идёт».
///
/// Потеря кадров и переподключение — аварии, которые владелец иначе не
/// заметит: у актёра всё выглядит работающим, а зрители видят заикания или
/// чёрный экран.
function renderStreamHealth(stream) {
  const total = Number(stream.outputTotalFrames || 0);
  const skipped = Number(stream.outputSkippedFrames || 0);
  const percent = total > 0 ? (skipped / total) * 100 : 0;
  const rows = {
    'Длительность': formatDuration(stream.outputDuration),
    'Потеряно кадров': total > 0
      ? `${skipped} из ${total} (${percent.toFixed(2)} %)`
      : 'нет данных',
    'Отправлено': (Number(stream.outputBytes || 0) / 1048576).toFixed(0) + ' МБ',
  };
  if (stream.outputReconnecting) rows['Внимание'] = 'эфир переподключается';
  renderDl($('streamHealth'), rows);

  // Переподключение и заметная потеря кадров — то, ради чего владелец здесь.
  if (stream.outputReconnecting) {
    if (!streamTrouble) {
      streamTrouble = true;
      announce('Внимание: эфир переподключается');
    }
  } else if (streamTrouble) {
    streamTrouble = false;
    say('Эфир восстановлен');
  }
}

async function refreshOutputs() {
  try {
    const stream = await obsRequest('GetStreamStatus');
    renderOutputState($('streamState'), 'Эфир', {
      outputActive: stream.outputActive,
      outputState: stream.outputReconnecting ? 'RECONNECTING' : '',
    });
    renderStreamHealth(stream);
  } catch { /* индикатор останется в состоянии «неизвестно» */ }
  try {
    const record = await obsRequest('GetRecordStatus');
    renderOutputState($('recordState'), 'Запись', {
      outputActive: record.outputActive,
      outputState: record.outputPaused ? 'PAUSED' : '',
      outputDuration: record.outputDuration,
    });
  } catch { /* см. выше */ }
}

async function refreshScenes() {
  try {
    const data = await api('/api/obs/scenes');
    const scenes = data.scenes || [];
    currentScene = data.currentProgramSceneName || currentScene;
    sceneNames = scenes.map((s) => s.sceneName);

    replace($('sceneSelect'), scenes.map((s) =>
      el('option', {
        value: s.sceneName,
        text: s.sceneName,
        selected: s.sceneName === currentScene,
      })));

    replace($('sceneList'), scenes.length
      ? scenes.map((s) => el('li', {
          text: s.sceneName + (s.sceneName === currentScene ? ' — текущая' : ''),
        }))
      : el('li', { class: 'empty', text: 'сцен нет' }));

    await refreshSources();
  } catch (e) {
    replace($('sceneList'), el('li', { text: e.message }));
  }
}

$('setScene').onclick = async () => {
  const scene = $('sceneSelect').value;
  if (!scene) return;
  try {
    await post('/api/obs/scenes/current', { sceneName: scene });
    say('Сцена переключена: ' + scene);
  } catch (e) {
    fail(e);
  }
};

// -------------------------------------------------- Studio Mode и прочее

/// Заполняет выпадающий список, сохраняя выбранное значение.
function fillSelect(node, items, current) {
  replace(node, items.map((name) =>
    el('option', { value: name, text: name, selected: name === current })));
}

async function refreshStudio() {
  try {
    const data = await api('/api/obs/studio');
    setState(
      $('studioState'),
      data.enabled ? 'ok' : 'off',
      data.enabled
        ? 'Studio Mode включён. Предпросмотр: ' + (data.previewScene || 'не выбран')
        : 'Studio Mode выключен',
    );
    fillSelect($('previewScene'), sceneNames, data.previewScene);
    for (const id of ['previewScene', 'setPreviewScene', 'studioTransition']) {
      $(id).disabled = !data.enabled;
    }
  } catch (e) {
    setState($('studioState'), 'warn', e.message);
  }
}

async function refreshVcam() {
  try {
    const data = await api('/api/obs/virtualcam');
    setState(
      $('vcamState'),
      data.outputActive ? 'ok' : 'off',
      data.outputActive ? 'Виртуальная камера работает' : 'Виртуальная камера выключена',
    );
  } catch (e) {
    setState($('vcamState'), 'warn', e.message);
  }
}

async function refreshReplay() {
  try {
    const data = await api('/api/obs/replay');
    if (!data.available) {
      setState($('replayState'), 'off', data.message || 'Буфер повтора недоступен');
    } else {
      setState(
        $('replayState'),
        data.outputActive ? 'ok' : 'off',
        data.outputActive ? 'Буфер повтора работает' : 'Буфер повтора выключен',
      );
    }
    for (const id of ['replayStart', 'replayStop']) $(id).disabled = !data.available;
    $('replaySave').disabled = !data.outputActive;
  } catch (e) {
    setState($('replayState'), 'warn', e.message);
  }
}

/// Профили, коллекции сцен и переходы — три одинаковых по форме списка.
async function refreshSetups() {
  const lists = [
    ['/api/obs/profiles', 'profileSelect', 'profiles', 'currentProfileName'],
    ['/api/obs/collections', 'collectionSelect', 'sceneCollections', 'currentSceneCollectionName'],
    ['/api/obs/transitions', 'transitionSelect', 'transitions', 'currentSceneTransitionName'],
  ];
  for (const [path, nodeId, listKey, currentKey] of lists) {
    try {
      const data = await api(path);
      const raw = data[listKey] || [];
      // Профили и коллекции приходят строками, переходы — объектами.
      const names = raw.map((i) => (typeof i === 'string' ? i : i.transitionName));
      fillSelect($(nodeId), names, data[currentKey]);
    } catch {
      replace($(nodeId), el('option', { text: 'недоступно' }));
    }
  }
}

// ------------------------------------------------------------ кадр эфира

let previewTimer = null;

async function grabPreview() {
  try {
    const data = await api('/api/obs/preview?width=640');
    const img = el('img', { alt: 'Кадр сцены ' + (data.sceneName || '') });
    img.src = data.imageData;
    replace($('previewBox'), [
      img,
      el('p', { class: 'hint', text: 'Сцена: ' + (data.sceneName || 'неизвестна') }),
    ]);
  } catch (e) {
    replace($('previewBox'), el('p', { text: e.message }));
  }
}

function stopAutoPreview() {
  clearInterval(previewTimer);
  previewTimer = null;
  $('autoPreview').checked = false;
}

$('grabPreview').onclick = grabPreview;

$('autoPreview').onchange = (event) => {
  clearInterval(previewTimer);
  previewTimer = null;
  if (event.target.checked) {
    grabPreview();
    previewTimer = setInterval(grabPreview, 2000);
    say('Автообновление кадра включено');
  } else {
    say('Автообновление кадра выключено');
  }
};

async function refreshSources() {
  if (!currentScene) return;
  try {
    const data = await api('/api/obs/sources?scene=' + encodeURIComponent(currentScene));
    const items = data.sceneItems || [];
    replace($('sources'), items.length
      ? items.map((item) => {
          const name = item.sourceName;
          const visible = !!item.sceneItemEnabled;
          return el('div', { class: 'source' }, [
            el('h3', { text: name }),
            el('p', { text: 'Состояние: ' + (visible ? 'виден' : 'скрыт') }),
            button('Показать', () => setSource(name, true)),
            button('Скрыть', () => setSource(name, false), 'quiet'),
          ]);
        })
      : el('p', { class: 'empty', text: 'в этой сцене нет источников' }));
  } catch (e) {
    replace($('sources'), el('p', { text: e.message }));
  }
}

async function setSource(sourceName, enabled) {
  try {
    await post('/api/obs/source/visibility', {
      sceneName: currentScene,
      sourceName,
      enabled,
    });
    say(`${sourceName}: ${enabled ? 'показан' : 'скрыт'}`);
  } catch (e) {
    fail(e);
  }
}

// -------------------------------------------------------- тревоги эфира
//
// Агент сам следит за потерей кадров и местом на диске, потому что панель
// может быть закрыта. Здесь мы только показываем и озвучиваем.

const MAX_JOURNAL = 100;

function timeLabel(date = new Date()) {
  return date.toLocaleTimeString('ru-RU', { hour12: false });
}

/// Журнал ведём и для обычных событий тоже: разбирать стрим постфактум
/// иначе не по чему, а логи агента для этого слишком подробны.
function journal(text, kind = 'info') {
  const list = $('journal');
  const row = el('li', { class: 'journal-row', 'data-kind': kind }, [
    el('span', { class: 'journal-time', text: timeLabel() }),
    el('span', { text }),
  ]);
  list.prepend(row);
  while (list.children.length > MAX_JOURNAL) list.lastChild.remove();
}

function handleAlert(msg) {
  const text = msg.message || 'Событие эфира';
  journal(text, msg.urgent ? 'bad' : 'ok');
  // Срочное перебивает речь скринридера, отбой — нет: иначе сообщение
  // «всё восстановилось» прерывало бы то, что владелец читает сейчас.
  if (msg.urgent) announce(text);
  else say(text);
}

// ------------------------------------------------------- уровни звука
//
// Владелец не слышит, что происходит у актёра. Пропавший микрофон — самая
// частая авария на стриме, и заметить её иначе нечем.

/// Ниже этого порога считаем, что звука нет.
const SILENCE_DB = -50;
/// Столько замеров тишины подряд (по 250 мс) до предупреждения.
const SILENCE_SAMPLES = 40;

const levelNodes = new Map();
const lastLevels = new Map();
/// Имя микрофона по версии самого OBS. Используется горячей клавишей M.
let micName = null;
const silenceCount = new Map();
const silenceWarned = new Set();

function levelText(db) {
  if (db <= -100) return 'тишина';
  return db.toFixed(0) + ' dB';
}

function announceLevel(name) {
  const db = lastLevels.get(name);
  if (db === undefined) {
    say(`${name}: уровень пока не известен`);
  } else if (db <= SILENCE_DB) {
    say(`${name}: тишина`);
  } else {
    say(`${name}: ${db.toFixed(0)} децибел, звук идёт`);
  }
}

function handleLevels(levels) {
  for (const [name, db] of Object.entries(levels || {})) {
    lastLevels.set(name, db);
    const node = levelNodes.get(name);
    if (node) node.textContent = levelText(db);
  }

  // Тишину сторожим только у микрофона.
  //
  // Донаты, медиа-вставки и вспомогательные входы молчат почти всегда — это
  // их нормальное состояние. Тревожась о каждом, панель выдавала бы поток
  // ложных предупреждений, и настоящее, про пропавший микрофон, потерялось
  // бы среди них. Оператор перестал бы их слушать, а это хуже, чем не иметь
  // тревог вовсе.
  if (!micName) return;
  const db = levels?.[micName];
  if (db === undefined) return;

  if (db > SILENCE_DB) {
    silenceCount.set(micName, 0);
    if (silenceWarned.delete(micName)) {
      say('Микрофон снова звучит');
      journal('Микрофон снова звучит', 'ok');
    }
    return;
  }
  // Предупреждаем один раз за период тишины и только во время эфира:
  // вне эфира молчащий микрофон — нормальное положение дел.
  const count = (silenceCount.get(micName) || 0) + 1;
  silenceCount.set(micName, count);
  if (streaming && count >= SILENCE_SAMPLES && !silenceWarned.has(micName)) {
    silenceWarned.add(micName);
    const text = `Внимание: микрофон ${micName} молчит десять секунд, а эфир идёт`;
    announce(text);
    journal(text, 'bad');
  }
}

async function refreshAudio() {
  try {
    const data = await api('/api/obs/audio');
    const rows = data.audio || [];
    levelNodes.clear();
    micName = (rows.find((a) => a.role === 'mic') || {}).inputName || null;
    replace($('audio'), rows.length
      ? rows.map((a) => {
          const name = a.inputName;
          const db = Number(a.volumeDb || 0);
          const field = el('input', {
            type: 'number',
            step: '0.5',
            value: db.toFixed(1),
          });
          field.setAttribute('aria-label', `Громкость источника ${name}, dB`);

          // Уровень обновляется четыре раза в секунду. Озвучивать это
          // непрерывно нельзя — скринридер превратится в пытку, поэтому
          // читаем его только по кнопке «Проверить звук».
          const level = el('span', { class: 'level', text: '—' });
          level.setAttribute('aria-hidden', 'true');
          levelNodes.set(name, level);

          const role = a.role === 'mic' ? ' — микрофон'
            : a.role === 'desktop' ? ' — звук системы' : '';

          return el('div', { class: 'audio-row' }, [
            el('h3', { text: name + role }),
            el('p', {
              text: `Звук: ${a.muted ? 'выключен' : 'включён'}. Громкость: ${db.toFixed(1)} dB.`,
            }),
            el('p', { class: 'hint' }, [document.createTextNode('Уровень: '), level]),
            button('Проверить звук', () => announceLevel(name), 'quiet'),
            button('Выключить звук', () => audioMute(name, true), 'quiet'),
            button('Включить звук', () => audioMute(name, false), 'quiet'),
            button('−1 dB', () => audioVolume(name, db - 1), 'quiet'),
            button('+1 dB', () => audioVolume(name, db + 1), 'quiet'),
            field,
            button('Применить', () => audioVolume(name, Number(field.value))),
          ]);
        })
      : el('p', { class: 'empty', text: 'аудиоисточников нет' }));
  } catch (e) {
    replace($('audio'), el('p', { text: e.message }));
  }
}

async function audioMute(inputName, muted) {
  try {
    await post('/api/obs/audio/mute', { inputName, muted });
    say(`${inputName}: звук ${muted ? 'выключен' : 'включён'}`);
  } catch (e) {
    fail(e);
  }
}

async function audioVolume(inputName, volumeDb) {
  if (!Number.isFinite(volumeDb)) return fail(new Error('Введите число'));
  try {
    await post('/api/obs/audio/volume', { inputName, volumeDb });
    say(`${inputName}: громкость ${volumeDb.toFixed(1)} dB`);
  } catch (e) {
    fail(e);
  }
}

async function command(path, message, confirmText) {
  if (confirmText && !confirm(confirmText)) return;
  try {
    await post(path);
    say(message);
  } catch (e) {
    fail(e);
  }
}

$('startStream').onclick = () => command('/api/obs/stream/start', 'Эфир запускается');
$('stopStream').onclick = () =>
  command('/api/obs/stream/stop', 'Эфир останавливается', 'Остановить активный эфир?');
$('startRecord').onclick = () => command('/api/obs/record/start', 'Запись запускается');
$('stopRecord').onclick = () =>
  command('/api/obs/record/stop', 'Запись останавливается', 'Остановить запись?');
$('pauseRecord').onclick = () => command('/api/obs/record/pause', 'Запись на паузе');
$('resumeRecord').onclick = () => command('/api/obs/record/resume', 'Запись продолжена');

$('vcamStart').onclick = () => command('/api/obs/virtualcam/start', 'Виртуальная камера включается');
$('vcamStop').onclick = () => command('/api/obs/virtualcam/stop', 'Виртуальная камера выключается');
$('replayStart').onclick = () => command('/api/obs/replay/start', 'Буфер повтора включается');
$('replayStop').onclick = () => command('/api/obs/replay/stop', 'Буфер повтора выключается');
$('replaySave').onclick = () => command('/api/obs/replay/save', 'Повтор сохранён');
$('studioTransition').onclick = () =>
  command('/api/obs/studio/transition', 'Предпросмотр выведен в эфир');

$('studioOn').onclick = async () => {
  try {
    await post('/api/obs/studio', { enabled: true });
    say('Studio Mode включён');
    await refreshStudio();
  } catch (e) { fail(e); }
};

$('studioOff').onclick = async () => {
  try {
    await post('/api/obs/studio', { enabled: false });
    say('Studio Mode выключен');
    await refreshStudio();
  } catch (e) { fail(e); }
};

$('setPreviewScene').onclick = async () => {
  const sceneName = $('previewScene').value;
  if (!sceneName) return;
  try {
    await post('/api/obs/studio/preview', { sceneName });
    say('Сцена предпросмотра: ' + sceneName);
  } catch (e) { fail(e); }
};

/// Переключатели профиля, коллекции и перехода устроены одинаково.
for (const [buttonId, selectId, path, field, label, warn] of [
  ['setProfile', 'profileSelect', '/api/obs/profiles', 'profileName', 'Профиль',
    'Смена профиля перезагрузит настройки OBS у актёра. Продолжить?'],
  ['setCollection', 'collectionSelect', '/api/obs/collections', 'sceneCollectionName',
    'Коллекция сцен', 'Смена коллекции перезагрузит сцены у актёра. Продолжить?'],
  ['setTransition', 'transitionSelect', '/api/obs/transitions', 'transitionName',
    'Переход', null],
]) {
  $(buttonId).onclick = async () => {
    const value = $(selectId).value;
    if (!value) return;
    if (warn && !confirm(warn)) return;
    try {
      await post(path, { [field]: value });
      say(label + ': ' + value);
      schedule('all', refreshAll, 800);
    } catch (e) { fail(e); }
  };
}

$('refreshStats').onclick = async () => {
  try {
    const s = await api('/api/obs/stats');
    renderDl($('stats'), {
      'Загрузка CPU, %': Number(s.cpuUsage || 0).toFixed(1),
      'Память, МБ': Number(s.memoryUsage || 0).toFixed(0),
      'Свободно на диске, МБ': Number(s.availableDiskSpace || 0).toFixed(0),
      'FPS': Number(s.activeFps || 0).toFixed(1),
      'Пропущено кадров (рендер)': s.renderSkippedFrames ?? 0,
      'Пропущено кадров (вывод)': s.outputSkippedFrames ?? 0,
    });
    say('Статистика обновлена');
  } catch (e) {
    renderDl($('stats'), { 'Ошибка': e.message });
  }
};

// ------------------------------------------------------ DonationAlerts

async function refreshDa() {
  try {
    const da = await api('/api/donationalerts/status');
    renderDl($('daStatus'), {
      'Виджет настроен': da.widget_url_configured ? 'да' : 'нет',
      'Звук ведётся в OBS': da.widget_url_configured ? 'да' : 'нет',
      'Сцена оверлея': da.overlay_scene,
      'Источник': da.input_name,
      'Лента донатов': da.realtime?.connected
        ? 'подключена'
        : (da.tokens_stored ? (da.realtime?.error || 'подключаюсь…') : 'OAuth не пройден'),
    });
  } catch (e) {
    renderDl($('daStatus'), { 'Ошибка': e.message });
  }
}

$('daUrlForm').onsubmit = async (e) => {
  e.preventDefault();
  try {
    const r = await post('/api/donationalerts/widget-url', { url: $('daUrl').value });
    $('daUrl').value = '';
    say(`DonationAlerts настроен. Оверлей добавлен в ${r.scenes_added_overlay} сцен, уже был в ${r.scenes_already_had_overlay}.`);
    await refreshDa();
  } catch (err) {
    fail(err);
  }
};

$('daReconcile').onclick = () =>
  command('/api/donationalerts/reconcile', 'Конфигурация DonationAlerts проверена');
$('daRefreshWidget').onclick = () =>
  command('/api/donationalerts/widget/refresh', 'Виджет перезагружен');
$('daMute').onclick = async () => {
  try {
    await post('/api/donationalerts/widget/mute', { muted: true });
    say('Звук оповещений выключен');
  } catch (e) { fail(e); }
};
$('daUnmute').onclick = async () => {
  try {
    await post('/api/donationalerts/widget/mute', { muted: false });
    say('Звук оповещений включён');
  } catch (e) { fail(e); }
};
$('daSetVolume').onclick = async () => {
  try {
    await post('/api/donationalerts/widget/volume', { volumeDb: Number($('daVolume').value) });
    say('Громкость оповещений изменена');
  } catch (e) { fail(e); }
};
$('daOauth').onclick = async () => {
  try {
    const r = await post('/api/donationalerts/oauth/start');
    window.open(r.authorize_url, '_blank', 'noopener');
    say('Откройте вкладку авторизации DonationAlerts');
  } catch (e) {
    fail(e);
  }
};

function donationText(d) {
  const who = d.username || d.name || 'Аноним';
  const amount = d.amount_in_user_currency ?? d.amount ?? '';
  const currency = d.currency || '';
  const message = d.message ? `. ${d.message}` : '';
  return `${who} — ${amount} ${currency}${message}`.replace(/\s+/g, ' ').trim();
}

function addDonation(donation, isNew) {
  const list = $('donations');
  if (list.firstElementChild?.classList.contains('empty')) list.replaceChildren();
  const text = donationText(donation);
  list.prepend(el('li', { text }));
  while (list.children.length > 50) list.lastElementChild.remove();
  if (isNew && $('announceDonations').checked) {
    announce('Новый донат: ' + text);
  }
}

async function refreshDonations() {
  try {
    const r = await api('/api/donationalerts/recent');
    const donations = r.donations || [];
    replace($('donations'), donations.length
      ? donations.map((d) => el('li', { text: donationText(d) }))
      : el('li', { class: 'empty', text: 'донатов пока нет' }));
  } catch { /* лента необязательна, молчим */ }
}

// -------------------------------------------------------------- Twitch

async function refreshTwitch() {
  try {
    const t = await api('/api/twitch/status');
    renderDl($('twitchStatus'), {
      'Настроен client_id': t.configured ? 'да' : 'нет',
      'Подключён': t.connected ? 'да' : 'нет',
      ...(t.message ? { 'Примечание': t.message } : {}),
    });
  } catch (e) {
    renderDl($('twitchStatus'), { 'Ошибка': e.message });
  }
}

$('twitchStart').onclick = async () => {
  try {
    const r = await post('/api/twitch/device/start');
    const link = el('a', {
      href: r.verification_uri_complete || r.verification_uri,
      target: '_blank',
      rel: 'noopener',
      text: 'страницу активации Twitch',
    });
    replace($('twitchDevice'), el('p', {}, [
      el('span', { text: 'Откройте ' }),
      link,
      el('span', { text: ' и введите код: ' }),
      el('strong', { text: r.user_code || '' }),
    ]));
    say('Код Twitch получен: ' + (r.user_code || ''));
  } catch (e) {
    fail(e);
  }
};

$('twitchCheck').onclick = async () => {
  try {
    // Backend различает три исхода: подключено, ждём подтверждения, код
    // устарел. Раньше панель смотрела на наличие access_token и любую неудачу
    // показывала одинаково — владелец не понимал, ждать ему или начинать заново.
    const r = await post('/api/twitch/device/check');
    const text = {
      connected: 'Twitch подключён.',
      pending: 'Пока не подтверждено. Завершите вход на странице Twitch и нажмите проверку ещё раз.',
      expired: 'Код устарел. Нажмите «Подключить Twitch» заново.',
    }[r.status] || r.message || 'Непонятный ответ Twitch.';
    replace($('twitchDevice'), el('p', { text }));
    say(text);
    await refreshTwitch();
  } catch (e) {
    fail(e);
  }
};

$('twitchChannelForm').onsubmit = async (e) => {
  e.preventDefault();
  const body = {};
  for (const id of ['streamTitle', 'streamGame', 'streamLang']) {
    const field = $(id);
    const value = field.value.trim();
    if (value) body[field.name] = value;
  }
  if (!Object.keys(body).length) return fail(new Error('Заполните хотя бы одно поле'));
  try {
    const r = await post('/api/twitch/channel', body);
    if (!r.ok) throw new Error(`Twitch отклонил изменение (код ${r.status})`);
    say('Параметры канала обновлены');
  } catch (err) {
    fail(err);
  }
};

$('markerForm').onsubmit = async (e) => {
  e.preventDefault();
  try {
    await post('/api/twitch/marker', { description: $('markerDescription').value });
    $('markerDescription').value = '';
    say('Маркер создан');
  } catch (err) {
    fail(err);
  }
};

// --------------------------------------------------------------- старт

$('launchObs').onclick = async () => {
  try {
    const r = await post('/api/obs/launch');
    say(r.message);
  } catch (e) {
    fail(e);
  }
};

$('clearJournal').onclick = () => {
  replace($('journal'), []);
  say('Журнал очищен');
};

$('refreshAll').onclick = async () => {
  const failed = await refreshAll();
  say(failed.length === 0
    ? 'Состояние обновлено полностью'
    : `Обновлено частично. Не отвечают: ${failed.join(', ')}`);
};
$('refreshScenes').onclick = refreshScenes;
$('refreshSources').onclick = refreshSources;
$('refreshAudio').onclick = refreshAudio;

// ---------------------------------------------------- горячие клавиши
//
// Панель длинная, а часть действий срочные: заглушить микрофон или
// переключить сцену иногда надо за секунду. Искать кнопку табуляцией
// в такой момент — непозволительная роскошь.

/// Глушит только тот вход, который сам OBS назначил микрофоном.
///
/// Запасного варианта нет намеренно. Раньше при неопознанном микрофоне бралась
/// первая попавшаяся дорожка — и клавиша, которую владелец жмёт как аварийное
/// «заглушить себя», могла выключить звук игры или донатов. Незрячий оператор
/// подмены не заметит: на экране подтверждения нет, а последствия услышат
/// только зрители. Честно отказаться лучше, чем молча промахнуться.
async function toggleMic() {
  if (!micName) {
    return announce(
      'Микрофон не назначен, ничего не изменено. У актёра в OBS: '
      + 'Настройки, раздел Аудио, поле «Микрофон/дополнительное аудио».',
    );
  }
  try {
    // Состояние читаем из OBS, а не из разметки: она могла устареть.
    const data = await api('/api/obs/audio');
    const row = (data.audio || []).find((a) => a.inputName === micName);
    if (!row) return announce(`Микрофон ${micName} исчез из OBS. Ничего не изменено.`);
    await post('/api/obs/audio/mute', { inputName: micName, muted: !row.muted });
    say(`${micName}: звук ${row.muted ? 'включён' : 'выключен'}`);
  } catch (e) {
    fail(e);
  }
}

async function switchSceneByIndex(index) {
  const sceneName = sceneNames[index];
  if (!sceneName) return say(`Сцены номер ${index + 1} нет`);
  try {
    await post('/api/obs/scenes/current', { sceneName });
    say('Сцена: ' + sceneName);
  } catch (e) {
    fail(e);
  }
}

const SHORTCUTS = [
  ['1 … 9', 'переключить сцену с этим номером', (key) => switchSceneByIndex(Number(key) - 1)],
  ['M', 'выключить или включить микрофон', toggleMic],
  ['P', 'обновить кадр эфира', grabPreview],
  ['R', 'обновить всё', () => $('refreshAll').click()],
  ['T', 'вывести предпросмотр в эфир', () =>
    command('/api/obs/studio/transition', 'Предпросмотр выведен в эфир')],
];

renderDl($('shortcuts'), Object.fromEntries(SHORTCUTS.map(([k, d]) => [k, d])));

document.addEventListener('keydown', (event) => {
  if (!$('shortcutsOn').checked) return;
  // Не перехватываем ввод текста и служебные сочетания браузера.
  const tag = event.target.tagName;
  if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') return;
  if (event.ctrlKey || event.altKey || event.metaKey) return;
  if ($('panel').hidden) return;

  const key = event.key.toUpperCase();
  if (key >= '1' && key <= '9') {
    event.preventDefault();
    switchSceneByIndex(Number(key) - 1);
    return;
  }
  const found = SHORTCUTS.find(([label]) => label === key);
  if (found) {
    event.preventDefault();
    found[2](key);
  }
});

// Выбор владельца переживает перезагрузку страницы.
$('shortcutsOn').checked = localStorage.getItem('rsc_shortcuts') !== 'off';
$('shortcutsOn').onchange = (event) => {
  localStorage.setItem('rsc_shortcuts', event.target.checked ? 'on' : 'off');
  say(event.target.checked ? 'Горячие клавиши включены' : 'Горячие клавиши выключены');
};

(async function start() {
  const status = await api('/api/auth/status').catch(() => ({ authenticated: false }));
  if (!status.authenticated) {
    showLogin(true);
    return;
  }
  showLogin(false);
  openEvents();
  await refreshAll();
})();
