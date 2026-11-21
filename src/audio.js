class AudioTap {
  constructor(options, handlers) {
    this.options = options;
    this.handlers = handlers;
    this.stream = null;
    this.ctx = null;
    this.node = null;
    this.seg = null;
    this.stopped = false;
  }

  async start(source) {
    let stream;
    if (source === 'system') {
      stream = await navigator.mediaDevices.getDisplayMedia({ video: true, audio: true });
    } else {
      const audio = { echoCancellation: false, noiseSuppression: true, autoGainControl: true };
      if (source && source !== 'default') audio.deviceId = { exact: source };
      stream = await navigator.mediaDevices.getUserMedia({ audio, video: false });
    }
    this.stream = stream;
    stream.getVideoTracks().forEach((t) => {
      t.enabled = false;
    });
    const tracks = stream.getAudioTracks();
    if (!tracks.length) {
      this.stop();
      throw new Error('в потоке нет звука');
    }
    tracks[0].addEventListener('ended', () => {
      if (!this.stopped && this.handlers.onEnded) this.handlers.onEnded();
    });

    this.ctx = new AudioContext();
    await this.ctx.audioWorklet.addModule('worklet.js');
    const src = this.ctx.createMediaStreamSource(new MediaStream(tracks));
    this.node = new AudioWorkletNode(this.ctx, 'tap', { numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [1] });
    const mute = this.ctx.createGain();
    mute.gain.value = 0;
    src.connect(this.node);
    this.node.connect(mute);
    mute.connect(this.ctx.destination);

    this.seg = new Segmenter({
      sampleRate: this.ctx.sampleRate,
      silenceMs: this.options.silenceMs,
      sensitivity: this.options.sensitivity
    });
    this.node.port.onmessage = (e) => {
      if (this.stopped) return;
      const r = this.seg.push(e.data);
      if (this.handlers.onLevel) this.handlers.onLevel(r.level, r.speaking);
      if (r.segment) this.handlers.onSegment(r.segment, this.ctx.sampleRate);
    };
    if (this.ctx.state === 'suspended') await this.ctx.resume();
  }

  stop() {
    this.stopped = true;
    if (this.node) this.node.port.onmessage = null;
    if (this.seg && this.ctx) {
      const tail = this.seg.flush();
      if (tail) this.handlers.onSegment(tail, this.ctx.sampleRate);
    }
    if (this.stream) this.stream.getTracks().forEach((t) => t.stop());
    if (this.ctx) this.ctx.close().catch(() => {});
    this.stream = null;
    this.ctx = null;
    this.node = null;
  }
}
