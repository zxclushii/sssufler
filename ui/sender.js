(function (root) {
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

  function pasteData(message, bracketed) {
    // Treat the transcript and template as one line of terminal input.
    const line = message.replace(/[\u0000-\u001f\u007f-\u009f\u2028\u2029]+/gu, ' ');
    return bracketed ? '\x1b[200~' + line + '\x1b[201~' : line;
  }

  function createSender({ session, write, bracketed, sent, restore, error, wait = sleep, now = Date.now }) {
    const queue = [];
    let running = false;
    let lastKey = -Infinity;
    let betweenPasteAndEnter = false;
    let deferredKeys = [];

    async function key(data) {
      lastKey = now();
      const id = session();
      if (id == null) return;
      if (betweenPasteAndEnter) {
        deferredKeys.push({ id, data });
      } else {
        await write(id, data);
      }
    }

    async function releaseKeys() {
      // A keystroke arriving during an automatic paste belongs after that message's Enter.
      while (deferredKeys.length) {
        const keys = deferredKeys;
        deferredKeys = [];
        const id = session();
        const text = keys.filter((k) => k.id === id).map((k) => k.data).join('');
        if (text && id != null) await write(id, text);
      }
    }

    async function drain() {
      if (running) return;
      running = true;
      try {
        while (queue.length) {
          const item = queue.shift();
          const checkSession = () => {
            if (item.id == null || item.id !== session()) throw new Error('claude не запущен, нажми Enter в терминале');
          };
          try {
            checkSession();
            while (now() - lastKey < 2000) {
              await wait(2000 - (now() - lastKey));
              checkSession();
            }
            betweenPasteAndEnter = true;
            await write(item.id, pasteData(item.message, bracketed()));
            await wait(160);
            checkSession();
            await write(item.id, '\r');
            sent(item.lines);
          } catch (err) {
            restore([item, ...queue.splice(0)].flatMap((message) => message.lines));
            error(err);
          } finally {
            try { await releaseKeys(); } catch (err) { error(err); }
            betweenPasteAndEnter = false;
          }
          await wait(300);
        }
      } finally {
        running = false;
      }
    }

    return {
      key,
      enqueue(message, lines) {
        queue.push({ id: session(), message, lines });
        return drain();
      }
    };
  }
  if (typeof module !== 'undefined' && module.exports) module.exports = { createSender, pasteData };
  else root.createSender = createSender;
})(globalThis);
