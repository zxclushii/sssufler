const { test } = require('node:test');
const assert = require('node:assert/strict');
const { cleanText, isJunk, isQuestion } = require('./text.js');

const cases = [
  ['Когда закончим, созвонимся.', false],
  ['Что ж, ладно.', false],
  ['Где-то так.', false],
  ['Как я и говорил, всё работает.', false],
  ['Кто рано встаёт, тому бог подаёт.', false],
  ['Что-то я устал.', false],
  ['Ну, как бы да.', false],
  ['Как только смогу, напишу.', false],
  ['Что касается Rust, я его учу.', false],
  ['Как дела', true],
  ['Что такое замыкание', true],
  ['Где ты работал раньше', true],
  ['Расскажи о себе', true],
  ['Почему ты ушёл с прошлой работы', true],
  ['Сколько тебе лет', true],
  ['Можешь объяснить, как это работает', true],
  ['Ну а что ты думаешь про Rust', true],
  ['Как ты думаешь, это сработает', false]
];
for (const [phrase, expected] of cases) {
  test(phrase, () => assert.equal(isQuestion(phrase), expected));
}
test('Українська й English', () => {
  for (const t of ['Як справи','Розкажіть, як це працює','Can you explain, briefly','Fine. Where are we']) assert.ok(isQuestion(t));
  for (const t of ['Де-не-де','Whatever happens','Whenever we can, we will']) assert.ok(!isQuestion(t));
});
test('Whisper noise and subtitles', () => {
  for (const t of ['', '...','[музыка]','(пауза)','♪ музыка ♪','Спасибо за просмотр!','Дякую за перегляд','Thank you for watching']) assert.ok(isJunk(t), t);
  assert.ok(!isJunk('Привет. Ты здесь?'));
  assert.equal(cleanText(' один \n два '), 'один два');
});
