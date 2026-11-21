const { test } = require('node:test');
const assert = require('node:assert/strict');
const { createSender, pasteData } = require('./sender.js');

function base(overrides) {
  return {session: () => 1, write: async () => {}, bracketed: () => false, sent() {}, restore() {}, error(e) { throw e; }, wait: async () => {}, ...overrides};
}
test('two questions produce two submissions in order', async () => {
  const out = [];
  const sender = createSender(base({ write: async (_, s) => out.push(s), wait: async (ms) => {out.push(ms);} }));
  const first = sender.enqueue('Один?', [1]); sender.enqueue('Два?', [2]); await first;
  assert.deepEqual(out, ['Один?',160,'\r',300,'Два?',160,'\r',300]);
});
test('typing gets two seconds and keys during a paste arrive after Enter', async () => {
  let t = 0;
  const out = [];
  const sender = createSender(base({now: () => t, write: async (_, s) => out.push(s), wait: async (ms) => {
    t += ms; out.push(ms); if (ms === 160) await sender.key('z');
  }}));
  await sender.key('x'); await sender.enqueue('auto', [1]);
  assert.deepEqual(out, ['x',2000,'auto',160,'\r','z',300]);
});
test('exit between paste and Enter restores both queued questions', async () => {
  let session = 1;
  const writes = [], recovered = [];
  const sender = createSender(base({session: () => session, write: async (_, s) => writes.push(s), wait: async (ms) => {if (ms === 160) session = 2;},
    restore: (lines) => recovered.push(...lines), error() {}}));
  const first = sender.enqueue('A',[1]); sender.enqueue('B',[2]); await first;
  assert.deepEqual(writes,['A']);assert.deepEqual(recovered,[1,2]);
});
test('control codes in speech never terminate the paste', () => {
  assert.equal(pasteData('a\r\nb\x1b[201~',true),'\x1b[200~a b [201~\x1b[201~');
});
