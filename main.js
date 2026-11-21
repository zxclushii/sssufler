const { app, BrowserWindow, ipcMain, session, desktopCapturer, globalShortcut, screen, shell, net } = require('electron');
const path = require('path');
const fs = require('fs');
const os = require('os');
const netSock = require('net');
const { spawn } = require('child_process');
const { Readable } = require('stream');
const pty = require('@lydell/node-pty');

const ROOT = __dirname;
const CONFIG_PATH = path.join(ROOT, 'config.json');
const WORKSPACE = path.join(ROOT, 'workspace');
const WHISPER_DIR = path.join(ROOT, 'whisper');
const BIN_DIR = path.join(WHISPER_DIR, 'bin');
const MODEL_DIR = path.join(WHISPER_DIR, 'models');
const RELEASES = 'https://github.com/ggml-org/whisper.cpp/releases';
const RELEASES_API = 'https://api.github.com/repos/ggml-org/whisper.cpp/releases?per_page=15';
const MODEL_URL = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-';
const ICON = path.join(ROOT, 'assets', process.platform === 'win32' ? 'icon.ico' : 'icon.png');

if (process.platform === 'win32') app.setAppUserModelId('suflyor.overlay');

const DEFAULT_PROMPT = [
  'Ты суфлёр в живом разговоре. Тебе присылают расшифровку речи собеседника, в ней бывают ошибки распознавания, додумывай по смыслу.',
  'Если там вопрос или просьба, сразу дай готовый ответ, который можно произнести вслух: 1-3 коротких предложения, без вступлений и без пересказа вопроса.',
  'Если просят код или решить задачу, дай короткое рабочее решение и одну строку пояснения.',
  'Если вопроса нет, ответь одной точкой.',
  'Отвечай на языке собеседника. Не используй инструменты, не читай и не меняй файлы.'
].join(' ');

const DEFAULTS = {
  whisperModel: 'small-q5_1',
  whisperThreads: 0,
  whisperUrl: '',
  language: 'ru',
  source: 'system',
  autoSend: true,
  onlyQuestions: true,
  silenceMs: 700,
  sensitivity: 1,
  opacity: 0.97,
  fontSize: 14,
  claudeCommand: 'claude',
  claudeArgs: '--model haiku --disallowedTools Bash Edit Write NotebookEdit',
  systemPrompt: DEFAULT_PROMPT,
  messageTemplate: 'Собеседник: «{text}»',
  bounds: null
};

let cfg = loadConfig();
let win = null;
let term = null;
let pinned = true;
const engine = { state: 'idle', text: '', progress: null, proc: null, promise: null, failedAt: 0, error: null };

function loadConfig() {
  let data = {};
  try {
    data = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf8'));
  } catch (e) {
    data = {};
  }
  const merged = Object.assign({}, DEFAULTS);
  for (const k of Object.keys(DEFAULTS)) {
    if (k in data) merged[k] = data[k];
  }
  try {
    fs.writeFileSync(CONFIG_PATH, JSON.stringify(merged, null, 2), 'utf8');
  } catch (e) {}
  return merged;
}

function saveConfig() {
  try {
    fs.writeFileSync(CONFIG_PATH, JSON.stringify(cfg, null, 2), 'utf8');
  } catch (e) {}
}

function send(channel, payload) {
  if (win && !win.isDestroyed()) win.webContents.send(channel, payload);
}

function winBuild() {
  if (process.platform !== 'win32') return 0;
  const parts = os.release().split('.');
  return parseInt(parts[2], 10) || 0;
}

function boundsVisible(b) {
  if (!b || typeof b.x !== 'number') return false;
  return screen.getAllDisplays().some((d) => {
    const a = d.workArea;
    return b.x + 80 > a.x && b.y + 40 > a.y && b.x < a.x + a.width - 80 && b.y < a.y + a.height - 40;
  });
}

function keepOnTop() {
  if (!win || win.isDestroyed()) return;
  win.setAlwaysOnTop(pinned, 'screen-saver');
}

function createWindow() {
  const area = screen.getPrimaryDisplay().workArea;
  const width = 640;
  const height = 660;
  const b = boundsVisible(cfg.bounds) ? cfg.bounds : {
    width,
    height,
    x: area.x + area.width - width - 24,
    y: area.y + area.height - height - 24
  };

  win = new BrowserWindow({
    x: b.x,
    y: b.y,
    width: b.width,
    height: b.height,
    minWidth: 380,
    minHeight: 260,
    frame: false,
    alwaysOnTop: true,
    backgroundColor: '#181818',
    title: 'Суфлёр',
    icon: ICON,
    show: false,
    webPreferences: {
      preload: path.join(ROOT, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      autoplayPolicy: 'no-user-gesture-required',
      backgroundThrottling: false
    }
  });

  keepOnTop();
  if (process.platform !== 'win32') win.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true });
  win.setOpacity(clamp(cfg.opacity, 0.3, 1));
  win.on('blur', keepOnTop);
  win.once('ready-to-show', () => win.show());
  win.webContents.setWindowOpenHandler(({ url }) => {
    shell.openExternal(url);
    return { action: 'deny' };
  });
  win.on('close', () => {
    cfg.bounds = win.getBounds();
    saveConfig();
  });
  win.on('closed', () => {
    win = null;
  });
  win.loadFile(path.join(ROOT, 'src', 'index.html'));
}

function clamp(v, a, b) {
  const n = Number(v);
  if (!Number.isFinite(n)) return b;
  return Math.min(b, Math.max(a, n));
}

function psQuote(s) {
  return "'" + String(s).replace(/'/g, "''") + "'";
}

function shQuote(s) {
  return "'" + String(s).replace(/'/g, "'\\''") + "'";
}

function claudeLine(quote) {
  const parts = [cfg.claudeCommand.trim() || 'claude'];
  const extra = String(cfg.claudeArgs || '').trim();
  if (extra) parts.push(extra);
  const prompt = String(cfg.systemPrompt || '').replace(/"/g, "'").replace(/\s+/g, ' ').trim();
  if (prompt) parts.push('--append-system-prompt', quote(prompt));
  return parts.join(' ');
}

function startPty(cols, rows) {
  if (term) return;
  fs.mkdirSync(WORKSPACE, { recursive: true });
  let file;
  let args;
  if (process.platform === 'win32') {
    const script = path.join(ROOT, '.launch.ps1');
    const lines = [
      'try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}',
      '$OutputEncoding = [System.Text.Encoding]::UTF8',
      'Clear-Host',
      '& ' + claudeLine(psQuote)
    ];
    fs.writeFileSync(script, '﻿' + lines.join('\r\n') + '\r\n', 'utf8');
    file = 'powershell.exe';
    args = ['-NoLogo', '-NoExit', '-ExecutionPolicy', 'Bypass', '-File', script];
  } else {
    file = process.env.SHELL || '/bin/bash';
    args = ['-c', claudeLine(shQuote) + '; exec ' + file];
  }

  const env = Object.assign({}, process.env, { COLORTERM: 'truecolor', FORCE_COLOR: '1' });
  delete env.TERM_PROGRAM;
  delete env.TERM_PROGRAM_VERSION;
  delete env.ELECTRON_RUN_AS_NODE;

  let p;
  try {
    p = pty.spawn(file, args, {
      name: 'xterm-256color',
      cols: Math.max(20, cols || 100),
      rows: Math.max(5, rows || 30),
      cwd: WORKSPACE,
      env
    });
  } catch (e) {
    send('pty:data', '\r\n\x1b[31mНе удалось запустить терминал: ' + e.message + '\x1b[0m\r\n');
    send('pty:exit', -1);
    return;
  }
  term = p;
  p.onData((d) => send('pty:data', d));
  p.onExit(({ exitCode }) => {
    if (term === p) term = null;
    if (!p.quiet) send('pty:exit', exitCode);
  });
}

function killPty() {
  const p = term;
  term = null;
  if (!p) return;
  p.quiet = true;
  try {
    p.kill();
  } catch (e) {}
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function setEngine(state, text, progress) {
  engine.state = state;
  engine.text = text || '';
  engine.progress = progress == null ? null : progress;
  send('engine', { state: engine.state, text: engine.text, progress: engine.progress });
}

function findFile(dir, name, depth) {
  if (!fs.existsSync(dir)) return null;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isFile() && e.name.toLowerCase() === name) return p;
  }
  if (depth <= 0) return null;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (!e.isDirectory()) continue;
    const found = findFile(path.join(dir, e.name), name, depth - 1);
    if (found) return found;
  }
  return null;
}

function mb(n) {
  return (n / 1048576).toFixed(0);
}

async function download(url, dest, label) {
  const part = dest + '.part';
  fs.rmSync(part, { force: true });
  const res = await net.fetch(url, { redirect: 'follow' });
  if (!res.ok || !res.body) throw new Error(label + ': HTTP ' + res.status);
  const total = Number(res.headers.get('content-length')) || 0;
  let got = 0;
  let last = 0;
  const out = fs.createWriteStream(part);
  try {
    for await (const chunk of Readable.fromWeb(res.body)) {
      got += chunk.length;
      if (!out.write(chunk)) await new Promise((r) => out.once('drain', r));
      const now = Date.now();
      if (now - last > 250) {
        last = now;
        const pct = total ? Math.floor((got / total) * 100) : null;
        setEngine('download', label + (pct == null ? ' ' + mb(got) + ' МБ' : ' ' + pct + '% из ' + mb(total) + ' МБ'), pct);
      }
    }
    await new Promise((resolve, reject) => out.end((err) => (err ? reject(err) : resolve())));
  } catch (e) {
    out.destroy();
    fs.rmSync(part, { force: true });
    throw new Error(label + ': обрыв загрузки (' + e.message + ')');
  }
  if (total && got < total) {
    fs.rmSync(part, { force: true });
    throw new Error(label + ': файл скачался не полностью');
  }
  fs.renameSync(part, dest);
}

function run(file, args) {
  return new Promise((resolve) => {
    const p = spawn(file, args, { windowsHide: true, stdio: 'ignore' });
    p.on('error', () => resolve(false));
    p.on('exit', (code) => resolve(code === 0));
  });
}

async function extract(zip, dest) {
  const sysTar = path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'tar.exe');
  if (fs.existsSync(sysTar) && (await run(sysTar, ['-xf', zip, '-C', dest]))) return;
  const ps = "Expand-Archive -LiteralPath '" + zip.replace(/'/g, "''") + "' -DestinationPath '" + dest.replace(/'/g, "''") + "' -Force";
  if (await run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', ps])) return;
  throw new Error('не получилось распаковать ' + path.basename(zip));
}

async function releaseAssetUrl(asset) {
  const direct = RELEASES + '/latest/download/' + asset;
  try {
    const head = await net.fetch(direct, { method: 'HEAD', redirect: 'follow' });
    if (head.ok) return direct;
  } catch (e) {}
  const res = await net.fetch(RELEASES_API, { headers: { Accept: 'application/vnd.github+json' } });
  if (!res.ok) throw new Error('GitHub недоступен: HTTP ' + res.status);
  const list = await res.json();
  for (const r of list) {
    const a = (r.assets || []).find((x) => x.name === asset);
    if (a) return a.browser_download_url;
  }
  throw new Error('не нашёл ' + asset + ' в релизах whisper.cpp');
}

async function ensureBinary() {
  let exe = findFile(BIN_DIR, 'whisper-server.exe', 3);
  if (exe) return exe;
  fs.mkdirSync(BIN_DIR, { recursive: true });
  const asset = process.arch === 'arm64' ? 'whisper-bin-win-cpu-arm64.zip' : 'whisper-bin-x64.zip';
  setEngine('download', 'ищу Whisper на GitHub…');
  const url = await releaseAssetUrl(asset);
  const zip = path.join(WHISPER_DIR, asset);
  await download(url, zip, 'качаю Whisper');
  setEngine('download', 'распаковываю Whisper…');
  await extract(zip, BIN_DIR);
  fs.rmSync(zip, { force: true });
  exe = findFile(BIN_DIR, 'whisper-server.exe', 3);
  if (!exe) throw new Error('в архиве нет whisper-server.exe');
  return exe;
}

async function ensureModel(name) {
  const clean = String(name || DEFAULTS.whisperModel).replace(/[^a-z0-9._-]/gi, '');
  fs.mkdirSync(MODEL_DIR, { recursive: true });
  const file = path.join(MODEL_DIR, 'ggml-' + clean + '.bin');
  if (fs.existsSync(file) && fs.statSync(file).size > 10 * 1048576) return file;
  await download(MODEL_URL + clean + '.bin', file, 'качаю модель ' + clean);
  if (fs.statSync(file).size < 10 * 1048576) {
    fs.rmSync(file, { force: true });
    throw new Error('модель ' + clean + ' скачалась битой');
  }
  return file;
}

function freePort() {
  return new Promise((resolve, reject) => {
    const srv = netSock.createServer();
    srv.once('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const port = srv.address().port;
      srv.close(() => resolve(port));
    });
  });
}

function threads() {
  const n = Number(cfg.whisperThreads);
  if (n > 0) return Math.floor(n);
  const cpus = (os.availableParallelism ? os.availableParallelism() : os.cpus().length) || 4;
  return Math.max(2, Math.min(8, cpus - 2));
}

async function waitHealth(url, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try {
      const r = await fetch(url + '/health', { signal: AbortSignal.timeout(1500) });
      if (r.ok) return;
    } catch (e) {}
    await sleep(400);
  }
  throw new Error('Whisper не запустился за ' + Math.round(ms / 1000) + ' с');
}

async function bootEngine() {
  if (process.platform !== 'win32') throw new Error('локальный Whisper ставится только на Windows, укажи whisperUrl в config.json');
  const exe = await ensureBinary();
  const model = await ensureModel(cfg.whisperModel);
  setEngine('loading', 'запускаю Whisper…');
  const port = await freePort();
  const args = ['-m', model, '-l', cfg.language || 'auto', '-t', String(threads()), '--host', '127.0.0.1', '--port', String(port), '-nt', '-sns'];
  const proc = spawn(exe, args, { cwd: path.dirname(exe), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let log = '';
  const keep = (d) => {
    log = (log + d.toString()).slice(-3000);
  };
  proc.stdout.on('data', keep);
  proc.stderr.on('data', keep);
  engine.proc = proc;
  const died = new Promise((resolve, reject) => {
    proc.once('error', (e) => reject(new Error('не запустился whisper-server: ' + e.message)));
    proc.once('exit', (code) => {
      const tail = log.trim().split(/\r?\n/).slice(-2).join(' ');
      reject(new Error('whisper-server закрылся (код ' + code + ') ' + tail));
    });
  });
  died.catch(() => {});
  proc.once('exit', () => {
    if (engine.proc !== proc) return;
    engine.proc = null;
    engine.promise = null;
    if (engine.state === 'ready') setEngine('error', 'Whisper остановился, перезапущу при следующей фразе');
  });
  const url = 'http://127.0.0.1:' + port;
  await Promise.race([waitHealth(url, 120000), died]);
  setEngine('ready', 'Whisper готов');
  return url;
}

function ensureEngine() {
  const custom = String(cfg.whisperUrl || '').trim().replace(/\/+$/, '');
  if (custom) {
    if (engine.state !== 'ready') setEngine('ready', 'внешний Whisper');
    return Promise.resolve(custom);
  }
  if (engine.promise) return engine.promise;
  if (engine.error && Date.now() - engine.failedAt < 20000) return Promise.reject(engine.error);
  engine.error = null;
  engine.promise = bootEngine().catch((e) => {
    engine.promise = null;
    engine.error = e;
    engine.failedAt = Date.now();
    stopEngine(true);
    setEngine('error', e.message);
    throw e;
  });
  return engine.promise;
}

function stopEngine(keepError) {
  const p = engine.proc;
  engine.proc = null;
  engine.promise = null;
  if (!keepError) {
    engine.error = null;
    engine.failedAt = 0;
  }
  if (p) {
    try {
      p.kill();
    } catch (e) {}
  }
}

async function transcribe(wav) {
  const url = await ensureEngine();
  const form = new FormData();
  form.append('file', new Blob([Buffer.from(wav)], { type: 'audio/wav' }), 'speech.wav');
  form.append('response_format', 'json');
  form.append('temperature', '0');
  form.append('language', cfg.language || 'auto');
  let res;
  try {
    res = await fetch(url + '/inference', { method: 'POST', body: form, signal: AbortSignal.timeout(90000) });
  } catch (e) {
    throw new Error('Whisper не ответил: ' + (e.cause && e.cause.code ? e.cause.code : e.message));
  }
  if (!res.ok) throw new Error('Whisper: HTTP ' + res.status);
  const data = await res.json();
  if (data.error) throw new Error('Whisper: ' + data.error);
  return String(data.text || '').trim();
}

function registerShortcuts() {
  const map = {
    'CommandOrControl+Alt+Space': () => send('hotkey', 'send'),
    'CommandOrControl+Alt+L': () => {
      if (win) win.webContents.executeJavaScript('toggleListening()', true).catch(() => {});
    },
    'CommandOrControl+Alt+X': () => send('hotkey', 'clear'),
    'CommandOrControl+Alt+H': () => {
      if (!win) return;
      if (win.isVisible()) {
        win.hide();
      } else {
        win.showInactive();
        keepOnTop();
      }
    }
  };
  for (const [key, fn] of Object.entries(map)) {
    try {
      globalShortcut.register(key, fn);
    } catch (e) {}
  }
}

ipcMain.handle('config:get', () => ({
  config: cfg,
  platform: process.platform,
  build: winBuild(),
  engine: { state: engine.state, text: engine.text, progress: engine.progress },
  pinned
}));

ipcMain.handle('config:set', (e, patch) => {
  const before = { whisperModel: cfg.whisperModel, whisperThreads: cfg.whisperThreads, whisperUrl: cfg.whisperUrl };
  if (patch && typeof patch === 'object') {
    for (const k of Object.keys(patch)) {
      if (k in DEFAULTS && k !== 'bounds') cfg[k] = patch[k];
    }
  }
  cfg.opacity = clamp(cfg.opacity, 0.3, 1);
  cfg.silenceMs = clamp(cfg.silenceMs, 200, 4000);
  cfg.sensitivity = clamp(cfg.sensitivity, 0.2, 6);
  cfg.fontSize = clamp(cfg.fontSize, 9, 30);
  if (win) win.setOpacity(cfg.opacity);
  saveConfig();
  if (Object.keys(before).some((k) => String(before[k]) !== String(cfg[k]))) {
    stopEngine(false);
    setEngine('idle', '');
    ensureEngine().catch(() => {});
  }
  return cfg;
});

ipcMain.handle('transcribe', (e, { wav }) => transcribe(wav));

ipcMain.on('ready', () => {
  ensureEngine().catch(() => {});
  if (win) win.webContents.executeJavaScript('startListening()', true).catch(() => {});
});

ipcMain.on('pty:start', (e, { cols, rows }) => startPty(cols, rows));
ipcMain.on('pty:input', (e, data) => {
  if (term) term.write(data);
});
ipcMain.on('pty:resize', (e, { cols, rows }) => {
  if (!term || !(cols > 0) || !(rows > 0)) return;
  try {
    term.resize(cols, rows);
  } catch (err) {}
});
ipcMain.on('pty:restart', (e, { cols, rows }) => {
  killPty();
  setTimeout(() => startPty(cols, rows), 150);
});

ipcMain.on('win:minimize', () => win && win.minimize());
ipcMain.on('win:close', () => app.quit());
ipcMain.handle('win:pin', (e, value) => {
  pinned = !!value;
  keepOnTop();
  return pinned;
});

app.whenReady().then(() => {
  session.defaultSession.setDisplayMediaRequestHandler((request, callback) => {
    const deny = () => {
      try {
        callback({});
      } catch (e) {}
    };
    desktopCapturer.getSources({ types: ['screen'] }).then((sources) => {
      if (!sources.length) return deny();
      callback({ video: sources[0], audio: 'loopback' });
    }).catch(deny);
  });
  createWindow();
  registerShortcuts();
});

app.on('will-quit', () => {
  globalShortcut.unregisterAll();
  killPty();
  stopEngine(false);
});

app.on('window-all-closed', () => app.quit());
