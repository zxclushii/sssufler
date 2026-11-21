const api = (() => {
  const { invoke, Channel } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  return {
    getConfig: () => invoke('config_get'),
    setConfig: (patch) => invoke('config_set', { patch }),
    ptyStart: (restart, cols, rows, receive) =>
      invoke(restart ? 'pty_restart' : 'pty_start', { cols, rows, onData: new Channel(receive) }),
    ptyInput: (sessionId, data) => invoke('pty_write', { sessionId, data }),
    ptyResize: (sessionId, cols, rows) => invoke('pty_resize', { sessionId, cols, rows }),
    listenStart: () => invoke('listen_start'),
    listenStop: () => invoke('listen_stop'),
    devices: () => invoke('audio_devices'),
    minimize: () => invoke('win_minimize'),
    close: () => invoke('win_close'),
    setPin: (value) => invoke('win_set_pin', { value }),
    ready: () => invoke('ui_ready'),
    on: (name, fn) => listen(name, ({ payload }) => fn(payload))
  };
})();
