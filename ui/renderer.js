const $ = (s) => document.querySelector(s);
let cfg = {};
let listening = false;
let starting = false;
let speaking = false;
let pending = [];
let sessionId = null;
let opening = false;
let generation = 0;
const earlyExits = new Set();
let outputDrained = false;
let exitNotice = false;
let errorTimer;
let errorText = '';
let engineInfo = { state: 'idle', text: '', progress: null };

const term = new Terminal({
  fontFamily: "'Cascadia Mono', Consolas, 'Courier New', monospace",
  fontSize: 14,
  lineHeight: 1.1,
  cursorBlink: true,
  cursorStyle: 'block',
  scrollback: 5000,
  allowProposedApi: true,
  macOptionIsMeta: true,
  theme: {
    background: '#181818',
    foreground: '#cccccc',
    cursor: '#aeafad',
    cursorAccent: '#181818',
    selectionBackground: '#264f78',
    black: '#000000',
    red: '#cd3131',
    green: '#0dbc79',
    yellow: '#e5e510',
    blue: '#2472c8',
    magenta: '#bc3fbc',
    cyan: '#11a8cd',
    white: '#e5e5e5',
    brightBlack: '#666666',
    brightRed: '#f14c4c',
    brightGreen: '#23d18b',
    brightYellow: '#f5f543',
    brightBlue: '#3b8eea',
    brightMagenta: '#d670d6',
    brightCyan: '#29b8db',
    brightWhite: '#e5e5e5'
  }
});
const fitAddon = new FitAddon.FitAddon();
term.loadAddon(fitAddon);
term.open($('#term'));


const sender = createSender({
  session: () => sessionId,
  write: api.ptyInput,
  bracketed: () => term.modes.bracketedPasteMode,
  sent(lines) {
    for (const { line } of lines) {
      line.classList.remove('pending');
      line.classList.add('sent');
    }
  },
  restore(lines) { pending = [...lines, ...pending]; trimPending(); },
  error: showError
});

function guard(fn) {
  return (...args) => {
    try { Promise.resolve(fn(...args)).catch(showError); } catch (err) { showError(err); }
  };
}

function showError(err) {
  errorText = String(err?.message || err || 'ошибка');
  clearTimeout(errorTimer);
  errorTimer = setTimeout(() => { errorText = ''; renderStatus(); }, 7500);
  renderStatus();
}

function fitTerm() {
  if (!$('#term').clientWidth || !$('#term').clientHeight) return;
  fitAddon.fit();
  if (sessionId != null) return api.ptyResize(sessionId, term.cols, term.rows);
}
new ResizeObserver(guard(fitTerm)).observe($('#term'));

term.onData(guard((data) => {
  if (sessionId != null) return sender.key(data);
  if (data === '\r' && !opening) return startPty(true);
}));
term.attachCustomKeyEventHandler((e) => {
  if (e.type !== 'keydown' || !e.ctrlKey || e.altKey) return true;
  if (e.code === 'KeyC' && term.hasSelection()) {
    navigator.clipboard.writeText(term.getSelection()).then(() => term.clearSelection()).catch(showError);
    return false;
  }
  return e.code !== 'KeyV';
});

function exited(id) {
  if (id !== sessionId) { if (opening) earlyExits.add(id); return; }
  sessionId = null;
  exitNotice = true;
  showExitAfterOutput();
}

function showExitAfterOutput() {
  if (!outputDrained || !exitNotice) return;
  exitNotice = false;
  term.write('\r\n\x1b[90m[процесс завершён, нажми Enter чтобы перезапустить]\x1b[0m\r\n');
}

async function startPty(restart) {
  if (opening) return;
  const current = ++generation;
  opening = true;
  sessionId = null;
  earlyExits.clear();
  outputDrained = false;
  exitNotice = false;
  if (restart) term.reset();
  try {
    const id = await api.ptyStart(restart, term.cols, term.rows, guard((buf) => {
      if (generation !== current) return;
      const bytes = new Uint8Array(buf);
      if (bytes.length) term.write(bytes);
      else { outputDrained = true; showExitAfterOutput(); }
    }));
    sessionId = id;
    if (earlyExits.has(id)) exited(id);
    else await fitTerm();
  } catch (err) {
    const text = err?.message || String(err);
    term.write('\r\n\x1b[31m' + text + '\x1b[0m\r\n');
    showError(err);
  } finally { opening = false; }
}

function sendMode() {
  if (!cfg.autoSend) return 'manual';
  return cfg.onlyQuestions ? 'questions' : 'all';
}

const MODE_LABEL = { questions: 'авто: вопросы', all: 'авто: всё', manual: 'вручную' };

function modePatch(mode) {
  if (mode === 'manual') return { autoSend: false };
  return { autoSend: true, onlyQuestions: mode === 'questions' };
}

function renderMode() {
  $('#mode').textContent = MODE_LABEL[sendMode()];
}

function renderStatus() {
  const dot = $('#dot');
  const txt = $('#status-text');
  dot.className = '';
  txt.className = '';
  if (errorText) {
    dot.classList.add('err');
    txt.classList.add('err');
    txt.textContent = errorText;
  } else if (engineInfo.state === 'download' || engineInfo.state === 'loading') {
    dot.classList.add('busy');
    txt.textContent = engineInfo.text;
  } else if (engineInfo.state === 'error') {
    dot.classList.add('err');
    txt.classList.add('err');
    txt.textContent = 'Whisper: ' + engineInfo.text;
  } else if (starting) {
    dot.classList.add('busy');
    txt.textContent = 'подключаю звук…';
  } else if (listening && speaking) {
    dot.classList.add('speech');
    txt.textContent = 'говорят…';
  } else if (listening) {
    dot.classList.add('on');
    txt.textContent = cfg.source === 'system' ? 'слушаю наушники' : 'слушаю микрофон';
  } else {
    dot.classList.add('off');
    txt.textContent = 'выключено';
  }
  $('#btn-listen').classList.toggle('on', listening);
}


function setLevel(level, active) {
  $('#meter i').style.width = Math.round(level * 100) + '%';
  if (speaking !== active) { speaking = active; renderStatus(); }
}

function trimPending() {
  while (pending.length > 8) pending.shift().line.classList.remove('pending');
}

function handleText({ text: raw, stale, endedAt }) {
  const text = cleanText(raw);
  if (!text || isJunk(text)) return;
  pending.push({ text, line: addLine(text, endedAt) });
  trimPending();
  if (!stale && cfg.autoSend && (!cfg.onlyQuestions || isQuestion(text))) return flush();
}

function addLine(text, endedAt) {
  const box = $('#lines');
  const el = document.createElement('div');
  el.className = 'line pending';
  const time = document.createElement('time');
  time.textContent = new Date(endedAt || Date.now()).toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  const span = document.createElement('span');
  span.textContent = text;
  el.append(time, span);
  box.append(el);
  while (box.children.length > 40) box.firstChild.remove();
  box.scrollTop = box.scrollHeight;
  return el;
}


function flush() {
  if (!pending.length) return;
  if (sessionId == null) { showError('claude не запущен, нажми Enter в терминале'); return; }
  const lines = pending;
  pending = [];
  const text = lines.map((p) => p.text).join(' ');
  const tpl = cfg.messageTemplate || '{text}';
  return sender.enqueue(tpl.includes('{text}') ? tpl.replace('{text}', () => text) : tpl + ' ' + text, lines);
}
function clearPending() {
  pending.forEach((p) => p.line.classList.remove('pending'));
  pending = [];
}

const form = $('#form');
const settings = $('#settings');
const field = (name) => form.elements.namedItem(name);
function syncOutputs() {
  for (const o of form.querySelectorAll('output[data-for]')) {
    const k = o.dataset.for, n = Number(field(k).value);
    o.textContent = k === 'silenceMs' ? n + ' мс' : k === 'opacity' ? Math.round(100 * n) + '%' : '×' + n.toFixed(1);
  }
}
function opacity(value) { $('#app').style.opacity = String(value); }
function option(select, value, text) {
  const o = document.createElement('option');
  o.value = value;
  o.textContent = text;
  select.append(o);
}
async function openSettings() {
  const select = field('source');
  select.replaceChildren();
  option(select, 'system', 'Системный звук (наушники, колонки)');
  option(select, 'default', 'Микрофон по умолчанию');
  try {
    for (const dev of await api.devices()) option(select, dev.id, dev.name);
  } catch (err) { showError(err); }
  if (![...select.options].some((o) => o.value === cfg.source)) option(select, cfg.source, 'Сохранённое устройство');
  const models = field('whisperModel');
  if (![...models.options].some((o) => o.value === cfg.whisperModel)) option(models, cfg.whisperModel, cfg.whisperModel);
  for (const el of form.elements) {
    if (!el.name || !(el.name in cfg)) continue;
    if (el.type === 'checkbox') el.checked = cfg[el.name];
    else el.value = cfg[el.name];
  }
  field('mode').value = sendMode();
  syncOutputs();
  settings.hidden = false;
}
function closeSettings() {
  settings.hidden = true;
  opacity(cfg.opacity);
  term.focus();
}
const click = (id, fn) => $(id).addEventListener('click', guard(fn));
click('#btn-listen', () => listening || starting ? api.listenStop() : api.listenStart());
click('#btn-send', flush);
click('#btn-clear', clearPending);
click('#btn-min', api.minimize);
click('#btn-close', api.close);
click('#btn-pin', async () => {
  const pin = $('#btn-pin');
  pin.classList.toggle('on', await api.setPin(!pin.classList.contains('on')));
});
click('#mode', async () => {
  const order = ['questions','all','manual'];
  cfg = await api.setConfig(modePatch(order[(order.indexOf(sendMode()) + 1) % order.length]));
  renderMode();
});
click('#btn-settings', () => settings.hidden ? openSettings() : closeSettings());
click('#btn-cancel', closeSettings);
click('#btn-restart', async () => { closeSettings(); await startPty(true); });
form.addEventListener('input', guard((e) => {
  syncOutputs();
  if (e.target.name === 'opacity') opacity(e.target.value);
}));
form.addEventListener('submit', guard(async (e) => {
  e.preventDefault();
  const before = cfg;
  const patch = modePatch(field('mode').value);
  for (const el of form.elements) {
    if (!el.name || el.name === 'mode') continue;
    if (el.type === 'checkbox') patch[el.name] = el.checked;
    else if (el.type === 'number' || el.type === 'range') patch[el.name] = Number(el.value);
    else patch[el.name] = el.value;
  }
  cfg = await api.setConfig(patch);
  closeSettings();
  renderMode();
  term.options.fontSize = cfg.fontSize;
  await fitTerm();
  if (['claudeCommand','claudeArgs','systemPrompt','webSearch'].some((k) => cfg[k] !== before[k])) await startPty(true);
  if (listening && ['source','silenceMs','sensitivity'].some((k) => cfg[k] !== before[k])) {
    await api.listenStop(); await api.listenStart();
  }
}));
document.addEventListener('keydown', guard((e) => { if (e.key === 'Escape' && !settings.hidden) closeSettings(); }));

async function init() {
  const handlers = {
    engine: (info) => { engineInfo = info; renderStatus(); },
    transcript: handleText,
    transcript_error: showError,
    status_warning: showError,
    pty_exit: ({sessionId: id}) => exited(id),
    hotkey: (key) => key === 'send' ? flush() : clearPending(),
    level: ({level, speaking}) => setLevel(level, speaking),
    listen_state: ({state, message}) => {
      starting = state === 'starting'; listening = state === 'on';
      if (!listening) setLevel(0, false);
      if (message) showError(message);
      renderStatus();
    }
  };
  await Promise.all(Object.entries(handlers).map(([name, fn]) => api.on(name, guard(fn))));
  const info = await api.getConfig();
  cfg = info.config;
  engineInfo = info.engine;
  $('#btn-pin').classList.toggle('on', info.pinned);
  $('#status').title = info.dataDir;
  term.options.fontSize = cfg.fontSize;
  term.options.windowsPty = { backend: 'conpty' };
  opacity(cfg.opacity);
  await document.fonts.ready;
  await fitTerm();
  renderMode();
  renderStatus();
  await Promise.all([startPty(false), api.ready()]);
  if (info.warnings.length) showError(info.warnings.join(' · '));
}
init().catch(showError);
