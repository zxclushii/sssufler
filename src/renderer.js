const $ = (s) => document.querySelector(s);

let cfg = {};
let sys = {};
let tap = null;
let listening = false;
let starting = false;
let ptyAlive = false;
let pending = [];
let chain = Promise.resolve();
let inflight = 0;
let speaking = false;
let errorTimer = null;
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

function fitTerm() {
  try {
    fitAddon.fit();
  } catch (e) {}
  api.ptyResize(term.cols, term.rows);
}

new ResizeObserver(() => fitTerm()).observe($('#term'));

term.onData((data) => {
  if (!ptyAlive) {
    if (data === '\r') restartPty();
    return;
  }
  api.ptyInput(data);
});

term.attachCustomKeyEventHandler((e) => {
  if (e.type !== 'keydown') return true;
  if (e.ctrlKey && !e.altKey && e.code === 'KeyC' && term.hasSelection()) {
    navigator.clipboard.writeText(term.getSelection()).catch(() => {});
    term.clearSelection();
    return false;
  }
  if (e.ctrlKey && !e.altKey && e.code === 'KeyV') return false;
  return true;
});

api.onPtyData((data) => term.write(data));
api.onPtyExit(() => {
  ptyAlive = false;
  term.write('\r\n\x1b[90m[процесс завершён, нажми Enter чтобы перезапустить]\x1b[0m\r\n');
});

function restartPty() {
  term.reset();
  ptyAlive = true;
  api.ptyRestart(term.cols, term.rows);
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
  } else if (inflight > 0) {
    dot.classList.add('busy');
    txt.textContent = 'распознаю…';
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

function showError(err) {
  const msg = String((err && err.message) || err || 'ошибка')
    .replace(/^Error invoking remote method '[^']+': /, '')
    .replace(/^Error: /, '');
  errorText = msg;
  clearTimeout(errorTimer);
  errorTimer = setTimeout(() => {
    errorText = '';
    renderStatus();
  }, 6000);
  renderStatus();
}

function setLevel(level, isSpeaking) {
  $('#meter i').style.width = Math.round(level * 100) + '%';
  if (isSpeaking !== speaking) {
    speaking = isSpeaking;
    renderStatus();
  }
}

async function startListening() {
  if (listening || starting) return;
  starting = true;
  renderStatus();
  const t = new AudioTap(
    { silenceMs: cfg.silenceMs, sensitivity: cfg.sensitivity },
    {
      onSegment,
      onLevel: setLevel,
      onEnded: () => {
        stopListening();
        showError('поток звука оборвался, включи заново');
      }
    }
  );
  try {
    await t.start(cfg.source);
    tap = t;
    listening = true;
  } catch (e) {
    t.stop();
    showError('не удалось захватить звук: ' + (e.message || e.name));
  }
  starting = false;
  renderStatus();
}

function stopListening() {
  if (tap) tap.stop();
  tap = null;
  listening = false;
  speaking = false;
  setLevel(0, false);
  renderStatus();
}

function toggleListening() {
  if (listening) stopListening();
  else startListening();
}

async function restartListening() {
  if (!listening) return;
  stopListening();
  await startListening();
}

function onSegment(samples, rate) {
  const pcm = normalize(resample(samples, rate, 16000));
  if (pcm.length < 16000 * 0.3) return;
  const wav = encodeWav(pcm, 16000);
  inflight++;
  renderStatus();
  const req = api.transcribe(wav).then(
    (text) => ({ text }),
    (error) => ({ error })
  );
  chain = chain.then(async () => {
    const r = await req;
    inflight--;
    if (r.error) showError(r.error);
    else handleText(r.text);
    renderStatus();
  });
}

function handleText(raw) {
  const text = cleanText(raw);
  if (!text || isJunk(text)) return;
  const line = addLine(text);
  pending.push({ text, line });
  while (pending.length > 8) {
    const old = pending.shift();
    old.line.classList.remove('pending');
  }
  if (cfg.autoSend && (!cfg.onlyQuestions || isQuestion(text))) flush();
}

function addLine(text) {
  const box = $('#lines');
  const el = document.createElement('div');
  el.className = 'line pending';
  const time = document.createElement('time');
  time.textContent = new Date().toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
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
  if (!ptyAlive) {
    showError('claude не запущен, нажми Enter в терминале');
    return;
  }
  const text = pending.map((p) => p.text).join(' ');
  pending.forEach((p) => {
    p.line.classList.remove('pending');
    p.line.classList.add('sent');
  });
  pending = [];
  const tpl = cfg.messageTemplate || '{text}';
  const message = (tpl.includes('{text}') ? tpl.replace('{text}', text) : tpl + ' ' + text).replace(/[\r\n]+/g, ' ');
  term.paste(message);
  setTimeout(() => api.ptyInput('\r'), 160);
}

function clearPending() {
  pending.forEach((p) => p.line.classList.remove('pending'));
  pending = [];
}

api.onEngine((info) => {
  engineInfo = info;
  renderStatus();
});

api.onHotkey((key) => {
  if (key === 'send') flush();
  if (key === 'clear') clearPending();
});

$('#btn-listen').addEventListener('click', toggleListening);
$('#btn-send').addEventListener('click', flush);
$('#btn-clear').addEventListener('click', clearPending);
$('#btn-min').addEventListener('click', () => api.minimize());
$('#btn-close').addEventListener('click', () => api.close());
$('#btn-pin').addEventListener('click', async () => {
  const on = await api.setPin(!$('#btn-pin').classList.contains('on'));
  $('#btn-pin').classList.toggle('on', on);
});
$('#mode').addEventListener('click', async () => {
  const order = ['questions', 'all', 'manual'];
  const next = order[(order.indexOf(sendMode()) + 1) % order.length];
  cfg = await api.setConfig(modePatch(next));
  renderMode();
});

const form = $('#form');
const settings = $('#settings');

function field(name) {
  return form.elements.namedItem(name);
}

function syncOutputs() {
  form.querySelectorAll('output[data-for]').forEach((o) => {
    const v = Number(field(o.dataset.for).value);
    const name = o.dataset.for;
    if (name === 'silenceMs') o.textContent = v + ' мс';
    else if (name === 'opacity') o.textContent = Math.round(v * 100) + '%';
    else o.textContent = '×' + v.toFixed(1);
  });
}

async function fillSources(selected) {
  const sel = field('source');
  sel.innerHTML = '';
  const add = (value, label) => {
    const o = document.createElement('option');
    o.value = value;
    o.textContent = label;
    sel.append(o);
  };
  add('system', 'Системный звук (наушники, колонки)');
  add('default', 'Микрофон по умолчанию');
  try {
    const devices = await navigator.mediaDevices.enumerateDevices();
    let n = 1;
    devices
      .filter((d) => d.kind === 'audioinput' && d.deviceId && d.deviceId !== 'default' && d.deviceId !== 'communications')
      .forEach((d) => add(d.deviceId, d.label || 'Микрофон ' + n++));
  } catch (e) {}
  if (![...sel.options].some((o) => o.value === selected)) add(selected, 'Сохранённое устройство');
  sel.value = selected;
}

async function openSettings() {
  for (const el of form.elements) {
    if (!el.name || !(el.name in cfg)) continue;
    el.value = cfg[el.name] == null ? '' : cfg[el.name];
  }
  field('mode').value = sendMode();
  if (![...field('whisperModel').options].some((o) => o.value === cfg.whisperModel)) {
    const o = document.createElement('option');
    o.value = cfg.whisperModel;
    o.textContent = cfg.whisperModel;
    field('whisperModel').append(o);
    field('whisperModel').value = cfg.whisperModel;
  }
  syncOutputs();
  await fillSources(cfg.source || 'system');
  settings.hidden = false;
}

function closeSettings() {
  settings.hidden = true;
  api.setConfig({ opacity: cfg.opacity });
  term.focus();
}

$('#btn-settings').addEventListener('click', () => (settings.hidden ? openSettings() : closeSettings()));
$('#btn-cancel').addEventListener('click', closeSettings);
$('#btn-restart').addEventListener('click', () => {
  settings.hidden = true;
  restartPty();
  term.focus();
});
form.addEventListener('input', (e) => {
  syncOutputs();
  if (e.target.name === 'opacity') api.setConfig({ opacity: Number(e.target.value) });
});

form.addEventListener('submit', async (e) => {
  e.preventDefault();
  const before = Object.assign({}, cfg);
  const patch = {
    whisperModel: field('whisperModel').value,
    whisperThreads: Math.max(0, Math.floor(Number(field('whisperThreads').value) || 0)),
    language: field('language').value,
    source: field('source').value,
    silenceMs: Number(field('silenceMs').value),
    sensitivity: Number(field('sensitivity').value),
    opacity: Number(field('opacity').value),
    fontSize: Number(field('fontSize').value) || 14,
    claudeCommand: field('claudeCommand').value.trim() || 'claude',
    claudeArgs: field('claudeArgs').value.trim(),
    systemPrompt: field('systemPrompt').value.trim(),
    messageTemplate: field('messageTemplate').value.trim() || '{text}'
  };
  Object.assign(patch, modePatch(field('mode').value));
  cfg = await api.setConfig(patch);
  settings.hidden = true;
  renderMode();
  if (cfg.fontSize !== term.options.fontSize) {
    term.options.fontSize = cfg.fontSize;
    fitTerm();
  }
  const claudeChanged = ['claudeCommand', 'claudeArgs', 'systemPrompt'].some((k) => before[k] !== cfg[k]);
  const audioChanged = ['source', 'silenceMs', 'sensitivity'].some((k) => before[k] !== cfg[k]);
  if (claudeChanged) restartPty();
  if (audioChanged) await restartListening();
  renderStatus();
  term.focus();
});

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !settings.hidden) closeSettings();
});

(async () => {
  const info = await api.getConfig();
  sys = info;
  cfg = info.config;
  engineInfo = info.engine || engineInfo;
  $('#btn-pin').classList.toggle('on', info.pinned);
  if (info.platform === 'win32' && info.build) term.options.windowsPty = { backend: 'conpty', buildNumber: info.build };
  term.options.fontSize = cfg.fontSize || 14;
  try {
    await document.fonts.ready;
  } catch (e) {}
  fitTerm();
  ptyAlive = true;
  api.ptyStart(term.cols, term.rows);
  renderMode();
  renderStatus();
  term.focus();
  api.ready();
})();
