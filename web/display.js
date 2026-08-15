'use strict';

const $ = (id) => document.getElementById(id);

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

function replace(container, nodes) {
  container.replaceChildren(...[].concat(nodes).filter(Boolean));
}

function say(text) {
  $('live').textContent = text;
}

function announce(text) {
  $('alerts').textContent = text;
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

function renderChat(twitch) {
  const login = safeTwitchLogin(twitch?.channel_login);
  if (!login) {
    replace($('chatBox'), el('p', {
      class: 'empty',
      text: 'Twitch-чат пока недоступен: подключите Twitch в панели оператора.',
    }));
    return;
  }
  // Twitch принимает в parent имя узла, а не IP-адрес. Страницу открывают по
  // localhost, но если кто-то зашёл по 127.0.0.1 напрямую, подставляем имя —
  // иначе виджет откажется встраиваться и вместо чата будет ошибка.
  const host = window.location.hostname;
  const parent = !host || /^[\d.]+$/.test(host) || host.includes(':') ? 'localhost' : host;
  const iframe = el('iframe', {
    title: `Twitch chat ${login}`,
    class: 'chat-frame',
    src: `https://www.twitch.tv/embed/${encodeURIComponent(login)}/chat?parent=${encodeURIComponent(parent)}&darkpopout=`,
  });
  replace($('chatBox'), iframe);
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
  const realtime = da?.realtime?.connected ? 'лента подключена' : 'лента не подключена';
  const widget = da?.widget_url_configured ? 'виджет настроен' : 'виджет не настроен';
  $('donationState').textContent = `${widget}; ${realtime}`;
}

async function refreshDisplay() {
  try {
    const state = await api('/api/actor-display/state');
    renderChat(state.twitch || {});
    renderDonationState(state.donationalerts || {});
    renderDonations(state.donations || []);
    $('displayStatus').textContent = 'Экран актёра активен.';
    say('Экран актёра обновлён');
  } catch (e) {
    $('displayStatus').textContent = e.message;
    announce('Ошибка экрана актёра: ' + e.message);
  }
}

let linkLost = false;

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
    } else if (msg.type === 'donationalerts_status' || msg.type === 'resync_required') {
      refreshDisplay();
    } else if (msg.type === 'alert' && msg.urgent) {
      announce(msg.message || 'Срочное предупреждение');
    }
  };
  events.onopen = () => {
    $('displayStatus').textContent = 'Экран актёра активен.';
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
