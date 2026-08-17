'use strict';

// ------------------------------------------------- отправка ошибок в лог
//
// Ошибка в панели видна только в консоли браузера: незрячий оператор туда не
// заглянет, зрячий не догадается. Отправляем их агенту, чтобы всё лежало в
// одном файле рядом с его собственными ошибками.

let reportedErrors = 0;
/// Предел на сеанс: зациклившаяся ошибка иначе зальёт лог и вытеснит из него
/// всё полезное.
const MAX_REPORTS = 20;

function reportError(where, message, stack) {
  if (reportedErrors >= MAX_REPORTS) return;
  reportedErrors += 1;
  // Намеренно без await и без обработки отказа: если агент недоступен,
  // сообщать об этом некому и незачем.
  fetch('/api/client-error', {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ where, message: String(message || ''), stack: String(stack || '') }),
  }).catch(() => {});
}

window.addEventListener('error', (e) => {
  reportError(`${e.filename || 'страница'}:${e.lineno || 0}`, e.message, e.error?.stack);
});
window.addEventListener('unhandledrejection', (e) => {
  reportError('промис', e.reason?.message || e.reason, e.reason?.stack);
});

/*
 * Панель Accessible OBS.
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

function stateActionButton(active, activeText, inactiveText, onClick, variant) {
  const node = button(active ? activeText : inactiveText, onClick, variant);
  node.dataset.toggleState = active ? 'active' : 'inactive';
  node.setAttribute('aria-pressed', String(active));
  return node;
}

function setStateAction(node, active, activeText, inactiveText, variant) {
  node.textContent = active ? activeText : inactiveText;
  node.dataset.toggleState = active ? 'active' : 'inactive';
  node.setAttribute('aria-pressed', String(active));
  if (variant) node.dataset.variant = variant;
  else delete node.dataset.variant;
}

function replace(container, nodes) {
  container.replaceChildren(...[].concat(nodes).filter(Boolean));
}

function setupCollapsibleSections() {
  document.querySelectorAll('#panel > section.card').forEach((section, index) => {
    const heading = section.querySelector(':scope > h2');
    if (!heading) return;
    const body = el('div', {
      class: 'section-body',
      id: `section-body-${index}`,
    });
    while (heading.nextSibling) body.append(heading.nextSibling);
    section.append(body);

    const key = 'rsc_section_open_' + heading.textContent.trim();
    const buttonNode = button(heading.textContent, () => {
      const open = buttonNode.getAttribute('aria-expanded') !== 'true';
      buttonNode.setAttribute('aria-expanded', String(open));
      body.hidden = !open;
      localStorage.setItem(key, open ? '1' : '0');
    }, 'quiet');
    buttonNode.className = 'section-toggle';
    buttonNode.setAttribute('aria-expanded', localStorage.getItem(key) === '0' ? 'false' : 'true');
    buttonNode.setAttribute('aria-controls', body.id);
    body.hidden = buttonNode.getAttribute('aria-expanded') !== 'true';
    heading.replaceChildren(buttonNode);
  });
}

/// Кладёт текст в живую область так, чтобы диктор его точно произнёс.
///
/// Простая запись в textContent не годится: если та же фраза уже там,DOM не
/// меняется, события об изменении нет, и диктор молчит. А повторы здесь —
/// обычное дело: «Сцена: Игра» дважды подряд, вторая неудачная попытка с той
/// же ошибкой. Для незрячего оператора это выглядит как проглоченное действие.
///
/// Поэтому сначала очищаем область, а текст кладём следующим кадром — так
/// изменений всегда два, и второе диктор объявляет.
function liveSpeak(node, text) {
  node.textContent = '';
  requestAnimationFrame(() => {
    node.textContent = text;
  });
}

/** Вежливое сообщение: NVDA прочитает, не прерывая текущую фразу. */
function say(text) {
  liveSpeak($('live'), text);
}
/** Настойчивое сообщение — для ошибок и донатов. */
function announce(text) {
  liveSpeak($('alerts'), text);
}

function fail(error) {
  const message = error?.message || String(error);
  announce('Ошибка: ' + message);
}

function openAuthTab() {
  const tab = window.open('about:blank', '_blank');
  if (tab) {
    tab.opener = null;
    tab.document.title = 'Открываю авторизацию…';
    tab.document.body.textContent = 'Открываю авторизацию…';
  }
  return tab;
}

function navigateAuthTab(tab, url) {
  if (tab) {
    tab.location.href = url;
    return true;
  }
  return false;
}

function closeAuthTab(tab) {
  try {
    tab?.close();
  } catch { /* браузер мог не дать доступ к вкладке */ }
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

let runtime = {
  mode: 'remote',
  loopback_only: false,
  tailscale_required: true,
};

const isLocalRuntime = () => runtime.mode === 'local';
/// Режим выбирается на начальной странице и решает, что показывать и что
/// произносить. По умолчанию доступный: лишняя настройка зрячему не мешает,
/// а бесполезная кнопка незрячему мешает.
const isAccessibleMode = () => runtime.accessible !== false;

async function loadRuntime() {
  try {
    runtime = { ...runtime, ...(await api('/api/runtime')) };
  } catch {
    runtime = { mode: 'remote', loopback_only: false, tailscale_required: true };
  }
  applyRuntimeUi();
}

function applyRuntimeUi() {
  document.body.dataset.runtimeMode = runtime.mode;
  document.body.dataset.interfaceMode = isAccessibleMode() ? 'accessible' : 'standard';

  // Проектор OBS — окно отрисовки видео, у него нет дерева доступности, и
  // экранный диктор не прочитает оттуда ничего. Незрячему эта кнопка не
  // просто бесполезна: нажав её, он получит окно, которое не сможет ни
  // прочитать, ни найти, чтобы закрыть.
  const projector = $('projectorBlock');
  if (projector) projector.hidden = isAccessibleMode();
  if (isAccessibleMode()) {
    const note = $('projectorNote');
    if (note) {
      note.textContent = 'Вывод на второй монитор скрыт: в доступном режиме '
        + 'чат и донаты зачитываются вслух, а окно проектора OBS экранный '
        + 'диктор прочитать не может. Переключить режим можно на начальной странице.';
      note.hidden = false;
    }
  }
  if (!isLocalRuntime()) return;

  $('appSubtitle').textContent = 'Локальная панель управления OBS, эфиром, записью, Twitch и DonationAlerts на этом компьютере.';
  $('loginIntro').textContent = 'Локальный режим уже авторизован на этом компьютере. Pairing-код не нужен.';
  $('pairingSecret').required = false;
  $('logout').hidden = true;
}

// ---------------------------------------------------------------- вход

function showLogin(show) {
  if (show && isLocalRuntime()) show = false;
  $('login').hidden = !show;
  $('panel').hidden = show;
  if (!show) {
    // Фокус остаётся в форме входа, которую мы только что спрятали. Для
    // зрячего это незаметно, а незрячий оказывается неизвестно где: диктор
    // читает пустоту, и до панели надо добираться самому.
    const start = $('panelStart');
    if (start) start.focus();
  }
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
      // Исход подключения приходит сюда с самой страницы обратного вызова.
      // Объявляем настойчиво: незрячий владелец до этого узнавал об отказе
      // только по тому, что ничего не произошло.
      if (msg.message) {
        announce(msg.message);
        journal('DonationAlerts: ' + msg.message, msg.oauth_ok ? 'ok' : 'bad');
      }
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
      schedule('audio', refreshAudio);
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
  const raw = data.outputState || '';
  if (label === 'Эфир') streaming = Boolean(active);
  if (label === 'Запись') {
    recording = Boolean(active);
    recordPaused = Boolean(data.outputPaused) || raw.endsWith('PAUSED');
  }
  if (raw === 'RECONNECTING') {
    setState(node, 'bad', `${label}: переподключается`);
    return updateActionButtons();
  }
  if (raw.endsWith('STARTING')) {
    setState(node, 'warn', `${label}: запускается`);
    return updateActionButtons();
  }
  if (raw.endsWith('STOPPING')) {
    setState(node, 'warn', `${label}: останавливается`);
    return updateActionButtons();
  }
  if (raw.endsWith('PAUSED')) {
    setState(node, 'warn', `${label}: на паузе`);
    return updateActionButtons();
  }
  if (active) {
    setState(node, 'ok', `${label}: идёт`);
    say(`${label} идёт`);
  } else {
    setState(node, 'bad', `${label}: остановлен${label === 'Запись' ? 'а' : ''}`);
  }
  updateActionButtons();
}

// ---------------------------------------------------------- обновление

let currentScene = '';
let sceneNames = [];
let inputKinds = [];
let lastCreatedSceneName = '';
let lastCreatedSourceName = '';
let expandedSourceSettings = '';
const sourceSettingsCache = new Map();
/// Когда данные обновлялись в последний раз и что не ответило.
let lastRefresh = null;
let streaming = false;
let recording = false;
let recordPaused = false;
let streamTrouble = false;
let studioEnabled = false;
let vcamActive = false;
let replayAvailable = true;
let replayActive = false;
let daWidgetMuted = null;

function setupStatefulActions() {
  for (const id of ['stopStream', 'stopRecord', 'resumeRecord', 'studioOff', 'vcamStop', 'replayStop', 'daUnmute']) {
    $(id).hidden = true;
  }
  updateActionButtons();
}

function updateActionButtons() {
  setStateAction(
    $('startStream'),
    streaming,
    'Эфир идёт — остановить эфир',
    'Эфир остановлен — начать эфир',
    streaming ? 'danger' : null,
  );
  setStateAction(
    $('startRecord'),
    recording,
    'Запись идёт — остановить запись',
    'Запись остановлена — начать запись',
    recording ? 'danger' : null,
  );
  $('pauseRecord').hidden = !recording;
  if (recording) {
    setStateAction(
      $('pauseRecord'),
      !recordPaused,
      'Запись идёт — поставить на паузу',
      'Запись на паузе — продолжить запись',
      'quiet',
    );
  }
  setStateAction(
    $('studioOn'),
    studioEnabled,
    'Studio Mode включён — выключить',
    'Studio Mode выключен — включить',
    'quiet',
  );
  setStateAction(
    $('vcamStart'),
    vcamActive,
    'Виртуальная камера включена — выключить',
    'Виртуальная камера выключена — включить',
    'quiet',
  );
  setStateAction(
    $('replayStart'),
    replayActive,
    'Буфер повтора включён — выключить',
    'Буфер повтора выключен — включить',
    'quiet',
  );
  $('replayStart').disabled = !replayAvailable;
  $('replaySave').disabled = !replayActive;
  if (daWidgetMuted === null) {
    $('daMute').textContent = 'Звук донатов: состояние неизвестно — выключить';
    $('daMute').dataset.toggleState = 'unknown';
    $('daMute').setAttribute('aria-pressed', 'false');
  } else {
    setStateAction(
      $('daMute'),
      !daWidgetMuted,
      'Звук донатов включён — выключить',
      'Звук донатов выключен — включить',
      'quiet',
    );
  }
}

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
  // Каждая функция обновления возвращает признак успеха и сама рисует свою
  // ошибку. Полагаться на исключения тут нельзя: раньше они ловились внутри и
  // наружу не выходили, поэтому Promise.allSettled считал такие вызовы
  // успешными — раздел показывал ошибку, а шапка бодро сообщала «данные
  // актуальны». Для оператора, который не видит экран, это ровно та ложь,
  // ради устранения которой всё и затевалось.
  const failed = [];
  const note = (name, ok) => {
    if (ok === false) failed.push(name);
  };

  try {
    note('Сцены', await refreshScenes());
  } catch {
    failed.push('Сцены');
  }

  const rest = [
    ['Состояние', refreshHealth],
    ['Аудио', refreshAudio],
    ['Типы источников', refreshInputKinds],
    ['Эфир и запись', refreshOutputs],
    ['Studio Mode', refreshStudio],
    ['Виртуальная камера', refreshVcam],
    ['Буфер повтора', refreshReplay],
    ['Профили и переходы', refreshSetups],
    ['Регистрация DonationAlerts', refreshDaConfig],
    ['DonationAlerts', refreshDa],
    ['Регистрация Twitch', refreshTwitchConfig],
    ['Twitch', refreshTwitch],
    ['Донаты', refreshDonations],
    // Роли зависят от списков аудио и источников, поэтому идут последними:
    // к этому моменту оба уже заполнены.
    ['Роли источников', refreshRoles],
    ['Мониторы', refreshMonitors],
  ];
  const results = await Promise.allSettled(rest.map(([, fn]) => fn()));
  results.forEach((r, i) => {
    if (r.status === 'rejected') failed.push(rest[i][0]);
    else note(rest[i][0], r.value);
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
      'OBS готов к управлению': health.obs_controllable ? 'да' : 'нет',
      ...(health.obs_crashed_last_run
        ? { 'Внимание': 'прошлый сеанс OBS завершился аварийно' }
        : {}),
    });
  } catch (e) {
    renderDl($('health'), { 'Ошибка': e.message });
    return false;
  }
  return true;
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
  let ok = true;
  try {
    const stream = await obsRequest('GetStreamStatus');
    renderOutputState($('streamState'), 'Эфир', {
      outputActive: stream.outputActive,
      outputState: stream.outputReconnecting ? 'RECONNECTING' : '',
    });
    renderStreamHealth(stream);
  } catch {
    // Индикатор останется в состоянии «неизвестно», но молчать об этом
    // нельзя: сверху не должно появиться «данные актуальны».
    ok = false;
  }
  try {
    const record = await obsRequest('GetRecordStatus');
    renderOutputState($('recordState'), 'Запись', {
      outputActive: record.outputActive,
      outputState: record.outputPaused ? 'PAUSED' : '',
      outputDuration: record.outputDuration,
    });
  } catch {
    ok = false;
  }
  return ok;
}

async function refreshScenes() {
  try {
    const data = await api('/api/obs/scenes');
    const scenes = data.scenes || [];
    sceneNames = scenes.map((s) => s.sceneName);
    if (!currentScene || !sceneNames.includes(currentScene)) {
      currentScene = data.currentProgramSceneName || currentScene;
    }

    replace($('sceneSelect'), scenes.map((s) =>
      el('option', {
        value: s.sceneName,
        text: s.sceneName,
        selected: s.sceneName === currentScene,
      })));

    replace($('sceneList'), scenes.length
      ? scenes.map((s) => el('li', {
          class: s.sceneName === lastCreatedSceneName ? 'just-added' : '',
          text: s.sceneName + (s.sceneName === currentScene ? ' — текущая' : ''),
        }))
      : el('li', { class: 'empty', text: 'сцен нет' }));

    await refreshSources();
    await refreshAudio();
  } catch (e) {
    replace($('sceneList'), el('li', { text: e.message }));
    return false;
  }
  return true;
}

$('sceneSelect').onchange = async (event) => {
  currentScene = event.target.value;
  await refreshSources();
  await refreshAudio();
};

$('setScene').onclick = async () => {
  const scene = $('sceneSelect').value;
  if (!scene) return;
  try {
    await post('/api/obs/scenes/current', { sceneName: scene });
    currentScene = scene;
    say('Сцена переключена: ' + scene);
    await refreshScenes();
  } catch (e) {
    fail(e);
  }
};

$('createSceneForm').onsubmit = async (event) => {
  event.preventDefault();
  const sceneName = $('newSceneName').value.trim();
  if (!sceneName) return fail(new Error('Введите название сцены'));
  try {
    await post('/api/obs/scenes', { sceneName });
    await post('/api/obs/scenes/current', { sceneName });
    $('newSceneName').value = '';
    currentScene = sceneName;
    lastCreatedSceneName = sceneName;
    say('Сцена добавлена в OBS и выбрана: ' + sceneName);
    await refreshScenes();
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
    studioEnabled = Boolean(data.enabled);
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
    updateActionButtons();
  } catch (e) {
    setState($('studioState'), 'warn', e.message);
    return false;
  }
  return true;
}

async function refreshVcam() {
  try {
    const data = await api('/api/obs/virtualcam');
    vcamActive = Boolean(data.outputActive);
    setState(
      $('vcamState'),
      data.outputActive ? 'ok' : 'off',
      data.outputActive ? 'Виртуальная камера работает' : 'Виртуальная камера выключена',
    );
    updateActionButtons();
  } catch (e) {
    setState($('vcamState'), 'warn', e.message);
    return false;
  }
  return true;
}

async function refreshReplay() {
  try {
    const data = await api('/api/obs/replay');
    replayAvailable = Boolean(data.available);
    replayActive = Boolean(data.outputActive);
    if (!data.available) {
      setState($('replayState'), 'off', data.message || 'Буфер повтора недоступен');
    } else {
      setState(
        $('replayState'),
        data.outputActive ? 'ok' : 'off',
        data.outputActive ? 'Буфер повтора работает' : 'Буфер повтора выключен',
      );
    }
    updateActionButtons();
  } catch (e) {
    setState($('replayState'), 'warn', e.message);
    return false;
  }
  return true;
}

/// Профили, коллекции сцен и переходы — три одинаковых по форме списка.
async function refreshSetups() {
  const lists = [
    ['/api/obs/profiles', 'profileSelect', 'profiles', 'currentProfileName'],
    ['/api/obs/collections', 'collectionSelect', 'sceneCollections', 'currentSceneCollectionName'],
    ['/api/obs/transitions', 'transitionSelect', 'transitions', 'currentSceneTransitionName'],
  ];
  let ok = true;
  for (const [path, nodeId, listKey, currentKey] of lists) {
    try {
      const data = await api(path);
      const raw = data[listKey] || [];
      // Профили и коллекции приходят строками, переходы — объектами.
      const names = raw.map((i) => (typeof i === 'string' ? i : i.transitionName));
      fillSelect($(nodeId), names, data[currentKey]);
    } catch {
      replace($(nodeId), el('option', { text: 'недоступно' }));
      ok = false;
    }
  }
  return ok;
}

// ------------------------------------------------------------ кадр эфира

let previewTimer = null;
let previewInFlight = false;

async function grabPreview() {
  if (previewInFlight) return;
  previewInFlight = true;
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
  } finally {
    previewInFlight = false;
  }
}

function stopAutoPreview() {
  clearTimeout(previewTimer);
  previewTimer = null;
  $('autoPreview').checked = false;
}

$('grabPreview').onclick = grabPreview;

$('autoPreview').onchange = (event) => {
  clearTimeout(previewTimer);
  previewTimer = null;
  if (event.target.checked) {
    const tick = async () => {
      if (!previewTimer) return;
      await grabPreview();
      if (previewTimer) previewTimer = setTimeout(tick, 2000);
    };
    previewTimer = setTimeout(tick, 1);
    say('Автообновление кадра включено');
  } else {
    say('Автообновление кадра выключено');
  }
};

function inputKindLabel(kind) {
  const labels = {
    image_source: 'Изображение',
    color_source_v3: 'Цвет',
    slideshow_v2: 'Слайд-шоу',
    browser_source: 'Браузер',
    ffmpeg_source: 'Медиафайл',
    text_gdiplus_v3: 'Текст',
    text_ft2_source_v2: 'Текст FreeType',
    monitor_capture: 'Захват экрана',
    window_capture: 'Захват окна',
    game_capture: 'Захват игры',
    dshow_input: 'Устройство видео',
    wasapi_input_capture: 'Захват входного аудио',
    wasapi_output_capture: 'Захват выходного аудио',
    wasapi_process_output_capture: 'Захват звука приложения',
  };
  return labels[kind] ? `${labels[kind]} (${kind})` : kind;
}

async function refreshInputKinds() {
  try {
    const data = await api('/api/obs/input-kinds');
    inputKinds = data.inputKinds || [];
    const selected = $('newSourceKind').value;
    replace($('newSourceKind'), inputKinds.length
      ? inputKinds.map((kind) => el('option', {
          value: kind,
          text: inputKindLabel(kind),
          selected: kind === selected,
        }))
      : el('option', { value: '', text: 'OBS не вернул типы источников' }));
  } catch (e) {
    replace($('newSourceKind'), el('option', { value: '', text: e.message }));
    return false;
  }
  return true;
}

function settingsCacheKey(sourceName) {
  return 'source:' + sourceName;
}

function settingsTextareaId(sourceName) {
  return 'settings_' + btoa(unescape(encodeURIComponent(sourceName))).replace(/=+$/g, '');
}

function renderSourceSettings(sourceName, inputKind) {
  const key = settingsCacheKey(sourceName);
  const cached = sourceSettingsCache.get(key);
  if (!cached) {
    return el('div', { class: 'settings-panel' }, [
      el('p', { class: 'hint', text: 'Загружаю реальные настройки OBS…' }),
    ]);
  }

  const textareaId = settingsTextareaId(sourceName);
  const textarea = el('textarea', {
    id: textareaId,
    rows: 10,
    value: JSON.stringify(cached.inputSettings || {}, null, 2),
  });
  textarea.setAttribute('aria-label', `JSON настройки OBS для ${sourceName}`);

  return el('form', {
    class: 'settings-panel',
    onSubmit: (event) => saveSourceSettings(event, sourceName),
  }, [
    el('p', {
      class: 'hint',
      text: `Реальные настройки OBS для ${inputKindLabel(cached.inputKind || inputKind)}. Поля совпадают с тем, что OBS отдаёт через WebSocket.`,
    }),
    ...renderPropertyChoosers(sourceName, cached),
    el('label', { htmlFor: textareaId, text: 'inputSettings JSON' }),
    textarea,
    el('button', { type: 'submit', text: 'Сохранить настройки в OBS' }),
  ]);
}

/// Человеческие названия для свойств, которые OBS отдаёт готовым списком.
const PROPERTY_LABELS = {
  monitor_id: 'Монитор',
  monitor: 'Монитор (номер)',
  window: 'Окно',
  capture_mode: 'Режим захвата',
  video_device_id: 'Камера',
  audio_device_id: 'Микрофон',
  device_id: 'Звуковое устройство',
  res_type: 'Разрешение',
};

/// Списки выбора для тех свойств, где значение нельзя придумать.
///
/// Идентификатор монитора выглядит как `\\.\DISPLAY1`, устройства — как строка
/// из фигурных скобок. Вписать такое в JSON вслепую невозможно, а без этого
/// источник молча отдаёт пустоту: захват монитора, которому не выбрали монитор,
/// показывает чёрный экран и никак об этом не сообщает.
function renderPropertyChoosers(sourceName, cached) {
  const options = cached.propertyItems;
  if (!options) {
    return [el('p', { class: 'hint', text: 'Загружаю списки выбора из OBS…' })];
  }
  const names = Object.keys(options);
  if (names.length === 0) return [];

  return names.map((property) => {
    const current = cached.inputSettings?.[property];
    const id = settingsTextareaId(sourceName + ':' + property);
    const select = el('select', { id });
    // Если OBS ещё не знает выбранного значения, показываем это явно, а не
    // подсовываем молча первый пункт списка.
    const known = options[property].some((o) => String(o.value) === String(current));
    if (!known) {
      select.append(el('option', {
        value: '',
        text: current === undefined ? '— не выбрано —' : `— сейчас: ${current} —`,
        selected: true,
      }));
    }
    for (const option of options[property]) {
      select.append(el('option', {
        value: String(option.value),
        text: option.name,
        selected: String(option.value) === String(current),
      }));
    }
    select.addEventListener('change', () => {
      if (select.value === '') return;
      const chosen = options[property].find((o) => String(o.value) === select.value);
      applySourceProperty(sourceName, property, chosen ? chosen.value : select.value);
    });
    return el('div', { class: 'field' }, [
      el('label', { htmlFor: id, text: PROPERTY_LABELS[property] || property }),
      select,
    ]);
  });
}

/// Применяет выбранное значение сразу, не дожидаясь правки JSON.
async function applySourceProperty(sourceName, property, value) {
  try {
    await post('/api/obs/source/settings', {
      sourceName,
      inputSettings: { [property]: value },
    });
    say(`${PROPERTY_LABELS[property] || property}: выбрано`);
    await loadSourceSettings(sourceName);
  } catch (e) {
    fail(e);
  }
}

async function refreshSources() {
  if (!currentScene) return;
  try {
    const data = await api('/api/obs/sources?scene=' + encodeURIComponent(currentScene));
    const items = data.sceneItems || [];
    sceneSourceNames = items.map((i) => i.sourceName).filter(Boolean);
    replace($('sources'), items.length
      ? items.map((item) => {
          const name = item.sourceName;
          const sceneItemId = item.sceneItemId;
          const kind = item.inputKind || 'неизвестный тип';
          const visible = !!item.sceneItemEnabled;
          return el('div', {
            class: 'source',
            'data-highlight': name === lastCreatedSourceName ? 'true' : null,
          }, [
            el('h3', { text: name }),
            el('p', { text: `Тип OBS: ${inputKindLabel(kind)}. Состояние: ${visible ? 'виден' : 'скрыт'}.` }),
            stateActionButton(
              visible,
              'Сейчас виден — скрыть источник',
              'Сейчас скрыт — показать источник',
              () => setSource(sceneItemId, name, !visible),
              'quiet',
            ),
            button(
              expandedSourceSettings === name
                ? 'Скрыть настройки в панели'
                : 'Настройки источника в панели',
              () => toggleSourceSettings(name),
              'quiet',
            ),
            // Родное окно свойств OBS. Функция для него была написана, но
            // кнопки к ней не существовало, и возможность просто не
            // существовала для пользователя.
            //
            // Окно открывается на компьютере актёра, а не у оператора: это
            // окно самого OBS, и по сети его не покажешь. Незрячему оператору
            // оно бесполезно — у окна OBS нет дерева доступности, — зато
            // выручает, когда рядом есть зрячий и надо выбрать из списка то,
            // чего в панели нет: монитор, устройство, окно для захвата.
            button(
              'Открыть окно свойств в OBS у актёра',
              () => openSourceProperties(name),
              'quiet',
            ),
            name === lastCreatedSourceName
              ? el('p', { class: 'notice', text: 'Только что добавлен в OBS.' })
              : null,
            expandedSourceSettings === name ? renderSourceSettings(name, kind) : null,
          ]);
        })
      : el('p', { class: 'empty', text: 'в этой сцене нет источников' }));
  } catch (e) {
    replace($('sources'), el('p', { text: e.message }));
  }
}

async function setSource(sceneItemId, sourceName, enabled) {
  try {
    await post('/api/obs/source/visibility', {
      sceneName: currentScene,
      sceneItemId,
      enabled,
    });
    say(`${sourceName}: ${enabled ? 'показан' : 'скрыт'}`);
    await refreshSources();
    await refreshAudio();
  } catch (e) {
    fail(e);
  }
}

function sourceCreateBody() {
  const inputKind = $('newSourceKind').value;
  const body = {
    sceneName: currentScene,
    sourceName: $('newSourceName').value.trim(),
    inputKind,
  };
  if (!body.sourceName) throw new Error('Введите название источника');
  if (!body.inputKind) throw new Error('Выберите тип источника OBS');
  if (!currentScene) throw new Error('Сначала выберите или создайте сцену');
  return body;
}

$('createSourceForm').onsubmit = async (event) => {
  event.preventDefault();
  try {
    const body = sourceCreateBody();
    const result = await post('/api/obs/sources', body);
    $('newSourceName').value = '';
    lastCreatedSourceName = body.sourceName;
    if (result.createdInput) {
      say(`Источник создан и добавлен: ${body.sourceName}`);
    } else if (result.createdSceneItem) {
      say(`Существующий источник добавлен в эту сцену: ${body.sourceName}`);
    } else {
      say(`Источник уже есть в этой сцене: ${body.sourceName}`);
    }
    await refreshSources();
  } catch (e) {
    fail(e);
  }
};

async function toggleSourceSettings(sourceName) {
  if (expandedSourceSettings === sourceName) {
    expandedSourceSettings = '';
    await refreshSources();
    return;
  }
  expandedSourceSettings = sourceName;
  await refreshSources();
  await loadSourceSettings(sourceName);
}

async function loadSourceSettings(sourceName) {
  try {
    const data = await api('/api/obs/source/settings?source=' + encodeURIComponent(sourceName));
    // Списки выбора запрашиваем отдельно и молча переживаем их отсутствие:
    // они полезны, но без них настройки всё равно можно править как JSON.
    let propertyItems = {};
    try {
      const items = await api('/api/obs/source/property-items?source=' + encodeURIComponent(sourceName));
      propertyItems = items.properties || {};
    } catch { /* оставляем пустым */ }
    sourceSettingsCache.set(settingsCacheKey(sourceName), {
      inputKind: data.inputKind,
      inputSettings: data.inputSettings || {},
      propertyItems,
    });
    await refreshSources();
  } catch (e) {
    fail(e);
  }
}

async function saveSourceSettings(event, sourceName) {
  event.preventDefault();
  const textarea = $(settingsTextareaId(sourceName));
  try {
    const inputSettings = JSON.parse(textarea.value || '{}');
    if (!inputSettings || Array.isArray(inputSettings) || typeof inputSettings !== 'object') {
      throw new Error('inputSettings должен быть JSON-объектом');
    }
    await post('/api/obs/source/settings', { sourceName, inputSettings });
    sourceSettingsCache.set(settingsCacheKey(sourceName), {
      ...(sourceSettingsCache.get(settingsCacheKey(sourceName)) || {}),
      inputSettings,
    });
    say('Настройки источника сохранены в OBS: ' + sourceName);
    await refreshSources();
  } catch (e) {
    fail(e);
  }
}

async function openSourceProperties(sourceName) {
  try {
    await post('/api/obs/source/properties', { sourceName });
    say('Открыто родное окно свойств OBS: ' + sourceName);
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
/// Имена для выпадающих списков ролей. Заполняются при обновлении аудио и
/// источников: там эти данные уже есть, а второй запрос был бы лишним.
let audioInputNames = [];
let sceneSourceNames = [];
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

function renderMicSummary(rows) {
  const mic = rows.find((a) => a.role === 'mic');
  if (!mic) {
    replace($('micSummary'), el('p', {
      class: 'notice',
      text: 'Основной микрофон не определён. Клавиша M и тревога тишины ничего не изменят.',
    }));
    return;
  }
  const db = Number(mic.volumeDb || 0);
  replace($('micSummary'), el('div', { class: 'audio-row' }, [
    el('h3', { text: 'Основной микрофон: ' + mic.inputName }),
    el('p', {
      text: `Звук: ${mic.muted ? 'выключен' : 'включён'}. Громкость: ${db.toFixed(1)} dB.`,
    }),
    button('Проверить микрофон', () => announceLevel(mic.inputName), 'quiet'),
    stateActionButton(
      !mic.muted,
      'Микрофон включён — выключить',
      'Микрофон выключен — включить',
      () => audioMute(mic.inputName, !mic.muted),
      'quiet',
    ),
  ]));
}

async function refreshAudio() {
  try {
    const globalData = await api('/api/obs/audio');
    const scenePath = currentScene
      ? '/api/obs/audio?scene=' + encodeURIComponent(currentScene)
      : '/api/obs/audio';
    const data = currentScene ? await api(scenePath) : globalData;
    const rows = data.audio || [];
    levelNodes.clear();
    micName = ((globalData.audio || []).find((a) => a.role === 'mic') || {}).inputName || null;
    audioInputNames = (globalData.audio || []).map((a) => a.inputName).filter(Boolean);
    renderMicSummary(globalData.audio || []);
    replace($('audio'), rows.length
      ? rows.map((a) => {
          const name = a.inputName;
          const db = Number(a.volumeDb || 0);
          const sceneItemId = a.sceneItemId;
          const inScene = sceneItemId !== null && sceneItemId !== undefined;
          const sceneEnabled = a.sceneItemEnabled !== false;
          const sharedAcrossScenes = inScene && Number(a.sceneUseCount || 0) > 1;
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
          const volumeControls = sharedAcrossScenes
            ? [el('p', {
                class: 'hint',
                text: 'Громкость общая: этот источник есть в нескольких сценах.',
              })]
            : [
                button('−1 dB', () => audioVolume(name, db - 1), 'quiet'),
                button('+1 dB', () => audioVolume(name, db + 1), 'quiet'),
                field,
                button('Применить', () => audioVolume(name, Number(field.value))),
              ];

          return el('div', { class: 'audio-row' }, [
            el('h3', { text: name + role }),
            el('p', {
              text: inScene
                ? `В этой сцене: ${sceneEnabled ? 'включён' : 'выключен'}. Громкость: ${db.toFixed(1)} dB.`
                : `Звук: ${a.muted ? 'выключен' : 'включён'}. Громкость: ${db.toFixed(1)} dB.`,
            }),
            el('p', { class: 'hint' }, [document.createTextNode('Уровень: '), level]),
            button('Проверить звук', () => announceLevel(name), 'quiet'),
            inScene
              ? stateActionButton(
                  sceneEnabled,
                  'В этой сцене включён — выключить здесь',
                  'В этой сцене выключен — включить здесь',
                  () => setSource(sceneItemId, name, !sceneEnabled),
                  'quiet',
                )
              : stateActionButton(
                  !a.muted,
                  'Звук включён — выключить',
                  'Звук выключен — включить',
                  () => audioMute(name, !a.muted),
                  'quiet',
                ),
            ...volumeControls,
          ]);
        })
      : el('p', {
          class: 'empty',
          text: currentScene
            ? 'в выбранной сцене нет аудиоисточников'
            : 'аудиоисточников нет',
        }));
  } catch (e) {
    replace($('micSummary'), el('p', { text: e.message }));
    replace($('audio'), el('p', { text: e.message }));
    return false;
  }
  return true;
}

async function audioMute(inputName, muted) {
  try {
    await post('/api/obs/audio/mute', { inputName, muted });
    say(`${inputName}: звук ${muted ? 'выключен' : 'включён'}`);
    await refreshAudio();
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
    schedule('all', refreshAll, 500);
  } catch (e) {
    fail(e);
  }
}

$('startStream').onclick = () => streaming
  ? command('/api/obs/stream/stop', 'Эфир останавливается', 'Остановить активный эфир?')
  : command('/api/obs/stream/start', 'Эфир запускается');
$('stopStream').onclick = () =>
  command('/api/obs/stream/stop', 'Эфир останавливается', 'Остановить активный эфир?');
$('startRecord').onclick = () => recording
  ? command('/api/obs/record/stop', 'Запись останавливается', 'Остановить запись?')
  : command('/api/obs/record/start', 'Запись запускается');
$('stopRecord').onclick = () =>
  command('/api/obs/record/stop', 'Запись останавливается', 'Остановить запись?');
$('pauseRecord').onclick = () => recordPaused
  ? command('/api/obs/record/resume', 'Запись продолжена')
  : command('/api/obs/record/pause', 'Запись на паузе');
$('resumeRecord').onclick = () => command('/api/obs/record/resume', 'Запись продолжена');

$('vcamStart').onclick = () => vcamActive
  ? command('/api/obs/virtualcam/stop', 'Виртуальная камера выключается')
  : command('/api/obs/virtualcam/start', 'Виртуальная камера включается');
$('vcamStop').onclick = () => command('/api/obs/virtualcam/stop', 'Виртуальная камера выключается');
$('replayStart').onclick = () => replayActive
  ? command('/api/obs/replay/stop', 'Буфер повтора выключается')
  : command('/api/obs/replay/start', 'Буфер повтора включается');
$('replayStop').onclick = () => command('/api/obs/replay/stop', 'Буфер повтора выключается');
$('replaySave').onclick = () => command('/api/obs/replay/save', 'Повтор сохранён');
$('studioTransition').onclick = () =>
  command('/api/obs/studio/transition', 'Предпросмотр выведен в эфир');

$('studioOn').onclick = async () => {
  try {
    const enabled = !studioEnabled;
    await post('/api/obs/studio', { enabled });
    say(`Studio Mode ${enabled ? 'включён' : 'выключен'}`);
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

// --------------------------------------------------------- экран актёра

async function openActorDisplay(panels) {
  const status = $('actorDisplayStatus');
  status.hidden = true;
  try {
    const r = await post('/api/actor-display/open', { panels });
    status.textContent = r.message + '.';
    status.hidden = false;
    say(r.message);
  } catch (e) {
    status.textContent = e.message;
    status.hidden = false;
    fail(e);
  }
}

$('openActorDisplayBoth').onclick = () => openActorDisplay('both');
$('openActorDisplayChat').onclick = () => openActorDisplay('chat');
$('openActorDisplayDonations').onclick = () => openActorDisplay('donations');

// -------------------------------------------- вывод на второй монитор

/// Список мониторов берём у OBS: он единственный, кто их видит со стороны
/// актёра. Владелец выбирает номер, не видя самих экранов, поэтому в подписи
/// нужны разрешение и координаты — по ним монитор и опознают.
async function refreshMonitors() {
  try {
    const data = await api('/api/actor-display/monitors');
    const list = data.monitors || [];
    replace($('monitorSelect'), list.length
      ? list.map((m, i) => {
          const index = m.monitorIndex ?? i;
          const size = m.monitorWidth && m.monitorHeight
            ? ` ${m.monitorWidth}×${m.monitorHeight}`
            : '';
          const name = m.monitorName || `Монитор ${index}`;
          return el('option', { value: String(index), text: `${index}: ${name}${size}` });
        })
      : el('option', { value: '', text: 'мониторы не найдены' }));
    return true;
  } catch (e) {
    replace($('monitorSelect'), el('option', { value: '', text: 'недоступно' }));
    return false;
  }
}

$('refreshMonitors').onclick = () =>
  refreshMonitors().then((ok) => say(ok ? 'Список мониторов обновлён' : 'Список мониторов недоступен'));

$('projectActorDisplay').onclick = async () => {
  const value = $('monitorSelect').value;
  if (value === '') return say('Сначала выберите монитор');
  const status = $('actorDisplayStatus');
  try {
    const r = await post('/api/actor-display/project', {
      monitorIndex: Number(value),
      panels: 'both',
    });
    status.textContent = r.message;
    status.hidden = false;
    say(r.message);
  } catch (e) {
    status.textContent = e.message;
    status.hidden = false;
    fail(e);
  }
};

// ------------------------------------------------------ роли источников

/// Заполняет выбор роли: пустой пункт означает «не назначено».
function fillRoleSelect(node, names, current) {
  const options = [el('option', { value: '', text: '— не назначено —' })];
  for (const name of names) {
    options.push(el('option', { value: name, text: name, selected: name === current }));
  }
  replace(node, options);
  node.value = current || '';
}

async function refreshRoles() {
  try {
    const data = await api('/api/roles');
    // Камерой может быть любой источник сцены, не только аудиовход.
    const sourceNames = [...new Set([...audioInputNames, ...sceneSourceNames])];

    fillRoleSelect($('roleMicrophone'), audioInputNames, data.microphone);
    fillRoleSelect($('roleCamera'), sourceNames, data.camera);

    const origin = {
      assigned: 'назначен вручную',
      obs_special: 'взят из настроек OBS',
      missing: 'не назначен нигде',
    }[data.microphone_origin] || 'неизвестно';
    renderDl($('rolesStatus'), {
      'Микрофон': data.microphone || data.obs_microphone || 'не назначен',
      'Откуда взят': origin,
      'Камера': data.camera || 'не назначена',
    });
    return true;
  } catch (e) {
    renderDl($('rolesStatus'), { 'Ошибка': e.message });
    return false;
  }
}

$('saveRoles').onclick = async () => {
  try {
    await post('/api/roles', {
      microphone: $('roleMicrophone').value,
      camera: $('roleCamera').value,
    });
    say('Роли сохранены');
    // Роли меняют то, что считается микрофоном, поэтому перечитываем всё:
    // от них зависят и аудио, и проверка готовности.
    await refreshAll();
  } catch (e) {
    fail(e);
  }
};

// ------------------------------------------------------ DonationAlerts

let daSecretConfigured = false;
let daOauthPoll = null;

function defaultDaRedirectUri() {
  return window.location.origin + '/api/donationalerts/oauth/callback';
}

function donationAlertsRegistrationBody() {
  return {
    clientId: $('daClientId').value.trim(),
    clientSecret: $('daClientSecret').value.trim(),
    redirectUri: $('daRedirectUri').value.trim() || defaultDaRedirectUri(),
    scopes: $('daScopes').value.trim(),
  };
}

function requireDaRegistrationForConnect() {
  const body = donationAlertsRegistrationBody();
  if (!body.clientId) {
    $('daClientId').focus();
    replace($('daOauthStatus'), el('p', { text: 'Введите DonationAlerts Client ID выше и нажмите «Подключить DonationAlerts» ещё раз.' }));
    return false;
  }
  if (!body.clientSecret && !daSecretConfigured) {
    $('daClientSecret').focus();
    replace($('daOauthStatus'), el('p', { text: 'Введите DonationAlerts Client secret выше и нажмите «Подключить DonationAlerts» ещё раз.' }));
    return false;
  }
  return true;
}

async function saveDaConfigFromForm() {
  const body = donationAlertsRegistrationBody();
  if (!body.clientId) throw new Error('Заполните DonationAlerts Client ID');
  if (!body.clientSecret && !daSecretConfigured) {
    throw new Error('Заполните DonationAlerts client secret');
  }
  const saved = await post('/api/donationalerts/config', body);
  daSecretConfigured = Boolean(saved.client_secret_configured);
  $('daClientSecret').value = '';
  return saved;
}

async function refreshDaConfig() {
  try {
    const c = await api('/api/donationalerts/config');
    daSecretConfigured = Boolean(c.client_secret_configured);
    $('daClientId').value = c.client_id || '';
    $('daClientSecret').placeholder = c.client_secret_configured
      ? 'секрет сохранён; пусто = не менять'
      : 'обязателен для OAuth';
    $('daRedirectUri').value = c.redirect_uri || defaultDaRedirectUri();
    $('daScopes').value = (c.oauth_scopes || []).join(' ');
  } catch (e) {
    fail(e);
    return false;
  }
  return true;
}

$('daConfigForm').onsubmit = async (e) => {
  e.preventDefault();
  try {
    await saveDaConfigFromForm();
    say('Регистрация DonationAlerts сохранена');
    await refreshDaConfig();
    await refreshDa();
  } catch (err) {
    fail(err);
  }
};

async function refreshDa() {
  try {
    const da = await api('/api/donationalerts/status');
    daWidgetMuted = typeof da.widget_muted === 'boolean' ? da.widget_muted : null;
    renderDl($('daStatus'), {
      'Виджет настроен': da.widget_url_configured ? 'да' : 'нет',
      'Звук ведётся в OBS': da.widget_url_configured ? 'да' : 'нет',
      'Слышите ли донаты вы': da.heard_by_owner === null || da.heard_by_owner === undefined
        ? 'неизвестно'
        : (da.heard_by_owner ? 'да' : 'нет, только зрители'),
      'Звук виджета': daWidgetMuted === null
        ? 'неизвестно'
        : (daWidgetMuted ? 'выключен' : 'включён'),
      'Громкость виджета': Number.isFinite(Number(da.widget_volume_db))
        ? Number(da.widget_volume_db).toFixed(1) + ' dB'
        : 'неизвестно',
      'Сцена оверлея': da.overlay_scene,
      'Источник': da.input_name,
      'Лента донатов': da.realtime?.connected
        ? 'подключена'
        : (da.tokens_stored ? (da.realtime?.error || 'подключаюсь…') : 'OAuth не пройден'),
      // Причина отказа приходит на отдельную страницу обратного вызова, которую
      // владелец может не увидеть вовсе. Без неё панель говорила «OAuth не
      // пройден» и умолкала, не подсказав, что именно исправлять.
      ...(da.oauth_last_result ? { 'Последняя попытка подключения': da.oauth_last_result } : {}),
    });
    updateActionButtons();
    return da;
  } catch (e) {
    renderDl($('daStatus'), { 'Ошибка': e.message });
    throw e;
    return false;
  }
  return true;
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
    const muted = daWidgetMuted !== true;
    await post('/api/donationalerts/widget/mute', { muted });
    say(`Звук оповещений ${muted ? 'выключен' : 'включён'}`);
    daWidgetMuted = muted;
    updateActionButtons();
    await refreshDa();
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
  if (!requireDaRegistrationForConnect()) return;
  const tab = openAuthTab();
  try {
    await saveDaConfigFromForm();
    const r = await post('/api/donationalerts/oauth/start');
    if (!navigateAuthTab(tab, r.authorize_url)) {
      replace($('daOauthStatus'), el('p', {}, [
        el('span', { text: 'Откройте авторизацию DonationAlerts: ' }),
        el('a', { href: r.authorize_url, target: '_blank', rel: 'noopener', text: 'перейти' }),
      ]));
    } else {
      replace($('daOauthStatus'), el('p', { text: 'Открыта страница авторизации DonationAlerts.' }));
    }
    say('Авторизация DonationAlerts открыта. После входа панель сама обновит статус.');
    startDaOauthPolling();
  } catch (e) {
    closeAuthTab(tab);
    fail(e);
  }
};

function startDaOauthPolling() {
  clearInterval(daOauthPoll);
  let tries = 0;
  daOauthPoll = setInterval(async () => {
    tries += 1;
    try {
      const da = await refreshDa();
      if (da.tokens_stored) {
        clearInterval(daOauthPoll);
        daOauthPoll = null;
        say('DonationAlerts подключён');
        await refreshDonations();
      } else if (tries >= 120) {
        clearInterval(daOauthPoll);
        daOauthPoll = null;
        say('DonationAlerts ещё не подключён. Завершите вход на открытой странице.');
      }
    } catch {
      if (tries >= 120) {
        clearInterval(daOauthPoll);
        daOauthPoll = null;
      }
    }
  }, 3000);
}

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
  } catch {
    return false;
  }
  return true;
}

// -------------------------------------------------------------- Twitch

let savedTwitchClientId = '';
let twitchDevicePoll = null;

function twitchRegistrationBody() {
  return {
    clientId: $('twitchClientId').value.trim(),
    scopes: $('twitchScopes').value.trim(),
  };
}

function updateTwitchConnectButton() {
  const fieldClientId = $('twitchClientId').value.trim();
  const hasClientId = Boolean(fieldClientId || savedTwitchClientId);
  $('twitchStart').textContent = fieldClientId && fieldClientId !== savedTwitchClientId
    ? 'Подключить Twitch (сохранит Client ID)'
    : 'Подключить Twitch';
  $('twitchStart').dataset.toggleState = hasClientId ? 'active' : 'unknown';
}

async function refreshTwitchConfig() {
  try {
    const c = await api('/api/twitch/config');
    savedTwitchClientId = c.client_id || '';
    $('twitchClientId').value = c.client_id || '';
    $('twitchScopes').value = (c.scopes || []).join(' ');
    updateTwitchConnectButton();
  } catch (e) {
    fail(e);
    return false;
  }
  return true;
}

async function saveTwitchConfigFromForm() {
  const body = twitchRegistrationBody();
  if (!body.clientId) throw new Error('Заполните Twitch Client ID');
  const saved = await post('/api/twitch/config', body);
  savedTwitchClientId = saved.client_id || body.clientId;
  updateTwitchConnectButton();
  return saved;
}

$('twitchConfigForm').onsubmit = async (e) => {
  e.preventDefault();
  try {
    await saveTwitchConfigFromForm();
    replace($('twitchDevice'), el('p', { text: 'Регистрация сохранена. Теперь можно подключать Twitch.' }));
    say('Регистрация Twitch сохранена');
    await refreshTwitchConfig();
    await refreshTwitch();
  } catch (err) {
    fail(err);
  }
};

$('twitchClientId').addEventListener('input', updateTwitchConnectButton);
$('twitchScopes').addEventListener('input', updateTwitchConnectButton);

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
    return false;
  }
  return true;
}

function twitchDeviceText(r) {
  return {
    connected: 'Twitch подключён.',
    pending: 'Жду подтверждение на странице Twitch…',
    expired: 'Код устарел. Нажмите «Подключить Twitch» заново.',
  }[r.status] || r.message || 'Непонятный ответ Twitch.';
}

async function checkTwitchAuthorization(announcePending = true) {
  const r = await post('/api/twitch/device/check');
  const text = twitchDeviceText(r);
  replace($('twitchDevice'), el('p', { text }));
  if (announcePending || r.status !== 'pending') say(text);
  await refreshTwitch();
  return r;
}

function startTwitchDevicePolling(intervalSeconds = 5) {
  clearInterval(twitchDevicePoll);
  let tries = 0;
  const delay = Math.max(3000, Number(intervalSeconds || 5) * 1000);
  twitchDevicePoll = setInterval(async () => {
    tries += 1;
    try {
      const r = await checkTwitchAuthorization(false);
      if (r.status === 'connected' || r.status === 'expired') {
        clearInterval(twitchDevicePoll);
        twitchDevicePoll = null;
      } else if (tries >= 120) {
        clearInterval(twitchDevicePoll);
        twitchDevicePoll = null;
        say('Twitch ещё не подключён. Завершите вход на открытой странице.');
      }
    } catch {
      if (tries >= 120) {
        clearInterval(twitchDevicePoll);
        twitchDevicePoll = null;
      }
    }
  }, delay);
}

$('twitchStart').onclick = async () => {
  if (!$('twitchClientId').value.trim() && !savedTwitchClientId) {
    $('twitchClientId').focus();
    replace($('twitchDevice'), el('p', { text: 'Введите Twitch Client ID выше и нажмите «Подключить Twitch» ещё раз.' }));
    updateTwitchConnectButton();
    return;
  }
  const tab = openAuthTab();
  try {
    if ($('twitchClientId').value.trim() !== savedTwitchClientId) {
      await saveTwitchConfigFromForm();
    }
    const r = await post('/api/twitch/device/start');
    const url = r.verification_uri_complete || r.verification_uri;
    const opened = navigateAuthTab(tab, url);
    const link = el('a', {
      href: url,
      target: '_blank',
      rel: 'noopener',
      text: 'открыть Twitch',
    });
    replace($('twitchDevice'), el('p', {}, opened ? [
      el('span', { text: 'Открыта страница Twitch. Если браузер всё равно попросит код: ' }),
      el('strong', { text: r.user_code || '' }),
      el('span', { text: '. Панель сама проверит подключение.' }),
    ] : [
      el('span', { text: 'Браузер заблокировал вкладку: ' }),
      link,
      el('span', { text: '. Код: ' }),
      el('strong', { text: r.user_code || '' }),
    ]));
    say('Авторизация Twitch открыта. После подтверждения панель сама обновит статус.');
    startTwitchDevicePolling(r.interval);
  } catch (e) {
    closeAuthTab(tab);
    fail(e);
  }
};

$('twitchCheck').onclick = async () => {
  try {
    await checkTwitchAuthorization(true);
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

// ------------------------------------------------- готовность к эфиру

const SEVERITY_WORD = {
  ok: 'в порядке',
  warning: 'предупреждение',
  critical: 'критично',
};

/// Показывает результат проверки готовности.
///
/// Каждая строка начинается со слова, а не только с цвета: экранный диктор
/// цвет не читает, а именно им пользуется владелец панели.
async function runPreflight() {
  setState($('preflightSummary'), 'warn', 'Проверяю…');
  replace($('preflightChecks'), []);
  try {
    const r = await api('/api/preflight');
    const checks = r.checks || [];
    const worst = checks.some((c) => c.severity === 'critical') ? 'bad'
      : checks.some((c) => c.severity === 'warning') ? 'warn' : 'ok';
    setState($('preflightSummary'), worst, r.summary);

    replace($('preflightChecks'), checks.map((c) => el('li', {
      class: 'check-row',
      'data-severity': c.severity,
    }, [
      el('strong', { text: `${c.title}: ${SEVERITY_WORD[c.severity] || c.severity}. ` }),
      document.createTextNode(c.detail + '.'),
      ...(c.fix ? [el('p', { class: 'hint', text: c.fix })] : []),
    ])));

    // Критичное перебивает речь: если начинать нельзя, узнать об этом надо
    // сразу, а не дочитав список до конца.
    if (worst === 'bad') announce(r.summary);
    else say(r.summary);
    journal(r.summary, worst === 'bad' ? 'bad' : 'ok');
  } catch (e) {
    setState($('preflightSummary'), 'bad', e.message);
    fail(e);
  }
}

$('runPreflight').onclick = runPreflight;

$('collectDiagnostics').onclick = async () => {
  const status = $('diagnosticsStatus');
  try {
    const data = await api('/api/diagnostics');
    const text = JSON.stringify(data, null, 2);
    const box = $('diagnosticsText');
    box.value = text;
    box.hidden = false;
    $('copyDiagnostics').hidden = false;
    status.textContent = `Собрано, ${text.length} символов. Скопируйте и пришлите этот текст.`;
    status.hidden = false;
    say('Диагностика собрана');
  } catch (e) {
    status.textContent = e.message;
    status.hidden = false;
    fail(e);
  }
};

$('copyDiagnostics').onclick = async () => {
  try {
    await navigator.clipboard.writeText($('diagnosticsText').value);
    say('Диагностика скопирована в буфер обмена');
  } catch {
    // Буфер может быть недоступен без явного разрешения — тогда текст
    // выделяем, и его можно скопировать клавишами.
    $('diagnosticsText').select();
    say('Текст выделен, скопируйте клавишами Ctrl плюс C');
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
    const setupPlace = isLocalRuntime() ? 'В OBS: ' : 'У актёра в OBS: ';
    return announce(
      'Микрофон не назначен, ничего не изменено. ' + setupPlace
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
  setupCollapsibleSections();
  setupStatefulActions();
  await loadRuntime();
  const status = await api('/api/auth/status').catch(() => ({ authenticated: false }));
  if (!status.authenticated) {
    showLogin(true);
    return;
  }
  showLogin(false);
  openEvents();
  await refreshAll();
})();
