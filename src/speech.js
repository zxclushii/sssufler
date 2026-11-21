(function (root) {
  class Segmenter {
    constructor(o) {
      this.rate = o.sampleRate;
      this.silenceMs = o.silenceMs || 700;
      this.sens = o.sensitivity || 1;
      this.minSpeechMs = o.minSpeechMs || 350;
      this.maxMs = o.maxSegmentMs || 15000;
      this.preRollMs = o.preRollMs || 300;
      this.noise = 0.003;
      this.reset(true);
    }

    reset(full) {
      this.active = false;
      this.chunks = [];
      this.len = 0;
      this.voiced = 0;
      this.silence = 0;
      if (full) {
        this.pre = [];
        this.preLen = 0;
      }
    }

    push(frame) {
      const ms = (frame.length / this.rate) * 1000;
      let sum = 0;
      for (let i = 0; i < frame.length; i++) sum += frame[i] * frame[i];
      const rms = Math.sqrt(sum / Math.max(1, frame.length));
      const thr = Math.max(0.006, this.noise * 3) / this.sens;
      const loud = rms > (this.active ? thr * 0.6 : thr);
      let segment = null;

      if (!this.active) {
        if (loud) {
          this.active = true;
          this.chunks = this.pre.slice();
          this.len = this.preLen;
          this.chunks.push(frame);
          this.len += frame.length;
          this.voiced = ms;
          this.silence = 0;
          this.pre = [];
          this.preLen = 0;
        } else {
          this.noise = rms < this.noise ? this.noise * 0.8 + rms * 0.2 : this.noise * 0.98 + rms * 0.02;
          this.pre.push(frame);
          this.preLen += frame.length;
          const maxPre = (this.preRollMs / 1000) * this.rate;
          while (this.pre.length > 1 && this.preLen - this.pre[0].length >= maxPre) {
            this.preLen -= this.pre.shift().length;
          }
        }
      } else {
        this.chunks.push(frame);
        this.len += frame.length;
        if (loud) {
          this.voiced += ms;
          this.silence = 0;
        } else {
          this.silence += ms;
        }
        if (this.silence >= this.silenceMs) {
          segment = this.finish(true);
        } else if ((this.len / this.rate) * 1000 >= this.maxMs) {
          segment = this.finish(false);
        }
      }

      return {
        rms,
        level: Math.min(1, Math.sqrt(rms) * 2.2),
        speaking: this.active && this.silence < 250,
        segment
      };
    }

    finish(ended) {
      const tailMs = ended ? Math.max(0, this.silence - 250) : 0;
      const drop = Math.floor((tailMs / 1000) * this.rate);
      const total = Math.max(0, this.len - drop);
      const out = new Float32Array(total);
      let off = 0;
      for (const c of this.chunks) {
        if (off >= total) break;
        const n = Math.min(c.length, total - off);
        out.set(n === c.length ? c : c.subarray(0, n), off);
        off += n;
      }
      const ok = this.voiced >= this.minSpeechMs;
      if (ended) {
        this.reset(true);
      } else {
        this.chunks = [];
        this.len = 0;
        this.voiced = 0;
        this.silence = 0;
      }
      return ok ? out : null;
    }

    flush() {
      if (!this.active) return null;
      return this.finish(true);
    }
  }

  function resample(input, inRate, outRate) {
    if (inRate === outRate) return input;
    const ratio = inRate / outRate;
    const outLen = Math.floor(input.length / ratio);
    const out = new Float32Array(outLen);
    if (ratio < 1) {
      for (let i = 0; i < outLen; i++) {
        const x = i * ratio;
        const j = Math.floor(x);
        const t = x - j;
        const a = input[j] || 0;
        const b = j + 1 < input.length ? input[j + 1] : a;
        out[i] = a + (b - a) * t;
      }
      return out;
    }
    for (let i = 0; i < outLen; i++) {
      const start = Math.floor(i * ratio);
      const end = Math.min(input.length, Math.floor((i + 1) * ratio));
      let sum = 0;
      for (let j = start; j < end; j++) sum += input[j];
      out[i] = end > start ? sum / (end - start) : 0;
    }
    return out;
  }

  function normalize(samples) {
    let peak = 0;
    for (let i = 0; i < samples.length; i++) {
      const v = Math.abs(samples[i]);
      if (v > peak) peak = v;
    }
    if (peak <= 0 || peak >= 0.5) return samples;
    const gain = Math.min(8, 0.7 / peak);
    const out = new Float32Array(samples.length);
    for (let i = 0; i < samples.length; i++) out[i] = samples[i] * gain;
    return out;
  }

  function encodeWav(samples, rate) {
    const buf = new ArrayBuffer(44 + samples.length * 2);
    const v = new DataView(buf);
    const str = (o, s) => {
      for (let i = 0; i < s.length; i++) v.setUint8(o + i, s.charCodeAt(i));
    };
    str(0, 'RIFF');
    v.setUint32(4, 36 + samples.length * 2, true);
    str(8, 'WAVE');
    str(12, 'fmt ');
    v.setUint32(16, 16, true);
    v.setUint16(20, 1, true);
    v.setUint16(22, 1, true);
    v.setUint32(24, rate, true);
    v.setUint32(28, rate * 2, true);
    v.setUint16(32, 2, true);
    v.setUint16(34, 16, true);
    str(36, 'data');
    v.setUint32(40, samples.length * 2, true);
    let o = 44;
    for (let i = 0; i < samples.length; i++) {
      const s = Math.max(-1, Math.min(1, samples[i]));
      v.setInt16(o, s < 0 ? s * 0x8000 : s * 0x7fff, true);
      o += 2;
    }
    return new Uint8Array(buf);
  }

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

  const ASK = [
    'как', 'каким образом', 'что', 'чем', 'чего', 'почему', 'зачем', 'когда', 'где', 'куда', 'откуда',
    'кто', 'кого', 'кому', 'сколько', 'какой', 'какая', 'какое', 'какие', 'каким', 'какую', 'каков',
    'какова', 'чей', 'чья', 'чьё', 'можешь', 'можете', 'мог бы', 'могли бы', 'расскажи', 'расскажите',
    'объясни', 'объясните', 'назови', 'назовите', 'скажи', 'скажите', 'опиши', 'опишите', 'напиши',
    'напишите', 'приведи', 'приведите', 'реши', 'решите', 'перечисли', 'перечислите', 'сравни',
    'сравните', 'в чём', 'в чем', 'знаешь ли', 'правда ли', 'верно ли',
    'як', 'що', 'чому', 'навіщо', 'коли', 'де', 'хто', 'скільки', 'який', 'яка', 'яке', 'які',
    'розкажи', 'розкажіть', 'поясни', 'поясніть', 'назви', 'назвіть', 'опиши', 'напиши', 'чи',
    'what', 'how', 'why', 'when', 'where', 'who', 'which', 'can you', 'could you', 'tell me',
    'explain', 'describe', 'do you', 'is it', 'are there'
  ];

  const FILLER = /^(а|и|ну|так|вот|слушай|слушайте|окей|ок|ok|okay|хорошо|ладно|так вот|а вот|скажи-ка|well|so|and)[\s,]+/u;

  function isQuestion(t) {
    const text = cleanText(t);
    if (!text) return false;
    if (text.includes('?')) return true;
    const sentences = text.toLowerCase().split(/[.!;:]\s+/u);
    return sentences.some((raw) => {
      let s = raw.trim();
      for (let i = 0; i < 3; i++) s = s.replace(FILLER, '');
      return ASK.some((w) => s.startsWith(w) && !/[\p{L}\p{N}]/u.test(s.charAt(w.length)));
    });
  }

  const api = { Segmenter, resample, normalize, encodeWav, cleanText, isJunk, isQuestion };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else Object.assign(root, api);
})(typeof globalThis !== 'undefined' ? globalThis : this);
