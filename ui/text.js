(function (root) {
  const JUNK = [
    'продолжение следует',
    'субтитр',
    'dimatorzok',
    'спасибо за просмотр',
    'подписывайтесь',
    'подпишись на канал',
    'ставьте лайк',
    'thanks for watching',
    'thank you for watching',
    'amara.org',
    'дякую за перегляд',
    'підписуйтесь'
  ];

  function cleanText(t) {
    return String(t || '').replace(/\s+/g, ' ').trim();
  }

  function isJunk(t) {
    const s = cleanText(t).toLowerCase();
    if (!s) return true;
    if (!/[\p{L}\p{N}]/u.test(s)) return true;
    if (/^[[(*♪].*[\])*♪]$/u.test(s)) return true;
    return s.length < 140 && JUNK.some((j) => s.includes(j));
  }

  const REQUEST = [
    'можешь', 'можете', 'мог бы', 'могли бы', 'расскажи', 'расскажите', 'объясни',
    'объясните', 'назови', 'назовите', 'скажи', 'скажите', 'опиши', 'опишите',
    'напиши', 'напишите', 'приведи', 'приведите', 'реши', 'решите', 'перечисли',
    'перечислите', 'сравни', 'сравните', 'знаешь ли', 'правда ли', 'верно ли',
    'розкажи', 'розкажіть', 'поясни', 'поясніть', 'назви', 'назвіть', 'можеш',
    'можете пояснити', 'can you', 'could you', 'tell me', 'explain', 'describe',
    'do you', 'is it', 'are there'
  ];
  const ASK = [
    'как', 'что', 'чем', 'чего', 'почему', 'зачем', 'когда', 'где', 'куда',
    'откуда', 'кто', 'кого', 'кому', 'сколько', 'какой', 'какая', 'какое',
    'какие', 'каким', 'какую', 'каков', 'какова', 'чей', 'чья', 'чьё', 'в чём', 'в чем',
    'як', 'що', 'чому', 'навіщо', 'коли', 'де', 'хто', 'скільки', 'який',
    'яка', 'яке', 'які', 'чи', 'what', 'how', 'why', 'when', 'where', 'who', 'which'
  ];
  const FILLER = /^(?:как бы|что ж|так вот|а вот|скажи-ка|ну|а|и|так|вот|слушай|слушайте|окей|ок|ok|okay|хорошо|ладно|well|so|and)(?:[\s,]+|$)/u;

  function startsWithWord(text, word) {
    return text.startsWith(word) && !/[\p{L}\p{N}\p{M}_\p{Pd}'’]/u.test(text.charAt(word.length));
  }

  function isQuestion(t) {
    const text = cleanText(t);
    if (text.includes('?')) return true;
    return text.toLowerCase().split(/[.!;:]+/u).some((raw) => {
      let s = raw.trim();
      while (FILLER.test(s)) s = s.replace(FILLER, '');
      return REQUEST.some((w) => startsWithWord(s, w)) ||
        (!s.includes(',') && ASK.some((w) => startsWithWord(s, w)));
    });
  }

  const api = { cleanText, isJunk, isQuestion };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else Object.assign(root, api);
})(globalThis);
