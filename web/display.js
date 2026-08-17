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

const $ = (id) => document.getElementById(id);

function el(tag, props = {}, children = []) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === null || value === undefined) continue;
    if (key === 'text') node.textContent = t(String(value));
    else if (key === 'onClick') node.addEventListener('click', value);
    else if (key in node) node[key] = value;
    else node.setAttribute(key, String(value));
  }
  for (const child of [].concat(children)) {
    if (child) node.append(child);
  }
  return node;
}

function replace(container, nodes) {
  container.replaceChildren(...[].concat(nodes).filter(Boolean));
}

function say(text) {
  $('live').textContent = t(text);
}

function announce(text) {
  $('alerts').textContent = t(text);
}

async function api(path) {
  const res = await fetch(path, { credentials: 'same-origin' });
  const text = await res.text();
  let data = {};
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = { raw: text };
    }
  }
  if (!res.ok) {
    throw new Error(data?.error?.message || data?.message || `HTTP ${res.status}`);
  }
  return data;
}

let linkLost = false;
/// В доступном режиме сообщения зачитываются, в обычном — только видны.
/// Значение приходит из состояния: режим задаётся на начальной странице.
let accessible = true;

const mode = new URLSearchParams(window.location.search).get('panels') || 'both';

function applyPanelMode() {
  const chatOnly = mode === 'chat';
  const donationsOnly = mode === 'donations';
  $('chatPanel').hidden = donationsOnly;
  $('donationPanel').hidden = chatOnly;
  document.body.dataset.displayMode = chatOnly ? 'chat' : donationsOnly ? 'donations' : 'both';
}

function safeTwitchLogin(login) {
  const value = String(login || '').trim();
  return /^[A-Za-z0-9_]{3,25}$/.test(value) ? value : '';
}

/// Сколько сообщений держать. Больше незачем: чат читают по мере поступления,
/// а длинный список экранный диктор обходит мучительно долго.
const MAX_CHAT = 60;

/// Показывает сообщения своим списком, а не чужим iframe.
///
/// Виджет Twitch незрячему почти бесполезен: диктор читает чужой фрейм со
/// скрипом, а новые сообщения не объявляются вовсе — приходится самому лазить
/// туда курсором и проверять. Свой список — обычные элементы страницы,
/// которые можно и прочитать, и озвучить сразу.
function appendChatMessage(author, text) {
  const box = $('chatBox');
  const list = box.querySelector('ul') || (() => {
    const fresh = el('ul', { class: 'chat-list' });
    replace(box, fresh);
    return fresh;
  })();
  list.append(el('li', {}, [
    el('span', { class: 'chat-author', text: `${author}: ` }),
    el('span', { text }),
  ]));
  while (list.children.length > MAX_CHAT) list.firstElementChild.remove();
  // Держим последнее сообщение на виду: зрячий читает низ списка.
  list.lastElementChild?.scrollIntoView({ block: 'nearest' });
}

function renderChat(twitch) {
  const login = safeTwitchLogin(twitch?.channel_login);
  if (!login) {
    replace($('chatBox'), el('p', {
      class: 'empty',
      text: 'Twitch-чат пока недоступен: подключите Twitch в панели оператора.',
    }));
    return;
  }
  // Сообщения приходят потоком событий; здесь только готовим место под них,
  // не затирая то, что уже успело прийти.
  if (!$('chatBox').querySelector('ul')) {
    replace($('chatBox'), el('p', {
      class: 'empty',
      text: `Чат канала ${login}: жду сообщений.`,
    }));
  }
}

function donationText(d) {
  const who = d.username || d.name || 'Аноним';
  const amount = d.amount_in_user_currency ?? d.amount ?? '';
  const currency = d.currency || '';
  const message = d.message ? `. ${d.message}` : '';
  return `${who} — ${amount} ${currency}${message}`.replace(/\s+/g, ' ').trim();
}

function renderDonations(donations) {
  replace($('displayDonations'), donations?.length
    ? donations.map((d) => el('li', { text: donationText(d) }))
    : el('li', { class: 'empty', text: 'Донатов пока нет' }));
}

function renderDonationState(da) {
  // Переводим части, а не готовую строку: сочетаний четыре, и держать их все
  // в словаре значило бы дублировать одно и то же четырежды.
  const realtime = t(da?.realtime?.connected ? 'лента подключена' : 'лента не подключена');
  const widget = t(da?.widget_url_configured ? 'виджет настроен' : 'виджет не настроен');
  $('donationState').textContent = `${widget}; ${realtime}`;
}

async function refreshDisplay() {
  try {
    const state = await api('/api/actor-display/state');
    // Язык приходит от агента: у экрана актёра нет своей настройки, и
    // расходиться с панелью он не должен.
    setLanguage(state.runtime?.language || 'ru');
    translateDom(document.body);
    accessible = state.runtime?.accessible !== false;
    renderChat(state.twitch || {});
    renderDonationState(state.donationalerts || {});
    renderDonations(state.donations || []);
    $('displayStatus').textContent = t('Экран актёра активен.');
    say('Экран актёра обновлён');
  } catch (e) {
    $('displayStatus').textContent = e.message;
    announce('Ошибка экрана актёра: ' + e.message);
  }
}

function openEvents() {
  const events = new EventSource('/api/actor-display/events');
  events.onmessage = (event) => {
    let msg;
    try {
      msg = JSON.parse(event.data);
    } catch {
      return;
    }
    if (msg.type === 'donation') {
      const text = donationText(msg.donation || {});
      const list = $('displayDonations');
      if (list.firstElementChild?.classList.contains('empty')) list.replaceChildren();
      list.prepend(el('li', { text }));
      while (list.children.length > 30) list.lastElementChild.remove();
      announce('Новый донат: ' + text);
    } else if (msg.type === 'chat') {
      appendChatMessage(msg.author || 'Гость', msg.text || '');
      // Вслух только в доступном режиме: зрячему непрерывное чтение чата
      // мешало бы, а незрячему без него чат бесполезен. Вежливой областью,
      // а не настойчивой: сообщения идут потоком и не должны перебивать
      // тревоги об эфире.
      if (accessible) say(msg.spoken || `${msg.author}: ${msg.text}`);
    } else if (msg.type === 'donationalerts_status' || msg.type === 'resync_required') {
      refreshDisplay();
    } else if (msg.type === 'alert' && msg.urgent) {
      announce(msg.message || 'Срочное предупреждение');
    }
  };
  events.onopen = () => {
    $('displayStatus').textContent = t('Экран актёра активен.');
    if (linkLost) {
      linkLost = false;
      // За время обрыва могли прийти донаты, о которых мы не узнали.
      say('Связь с агентом восстановлена');
      refreshDisplay();
    }
  };
  events.onerror = () => {
    $('displayStatus').textContent = 'Связь с агентом восстанавливается…';
    // Молча менять надпись нельзя: незрячий не смотрит на экран и будет
    // считать, что донатов просто нет, хотя на деле их некому доставить.
    if (!linkLost) {
      linkLost = true;
      announce('Связь с агентом потеряна, восстанавливаю');
    }
  };
}

$('fullscreen').onclick = async () => {
  try {
    await document.documentElement.requestFullscreen();
  } catch (e) {
    announce('Полноэкранный режим не включился: ' + e.message);
  }
};

$('refreshDisplay').onclick = refreshDisplay;

applyPanelMode();
refreshDisplay();
openEvents();
