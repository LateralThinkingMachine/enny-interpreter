// Standalone AudioWorklet module. Also importable in Node for deterministic DSP tests.
// No imports: Vite emits this exact module as an independently addressable asset.
export class PCM16Resampler {
  constructor(inputRate, onFrame) {
    if (!Number.isInteger(inputRate) || inputRate < 16000 || inputRate > 96000) throw new Error('unsupported_sample_rate');
    this.inputRate = inputRate;
    this.onFrame = onFrame;
    this.phase = 0;
    this.position = 0;
    this.frame = new Int16Array(320);
    this.filled = 0;
    // A fixed duration gives the same speech passband and anti-alias rejection
    // at every supported device rate. At 48/96kHz the group delay is 1.333ms.
    this.half = Math.ceil(inputRate * 64 / 48000);
    this.taps = this.half * 2 + 2;
    this.ring = new Float64Array(this.taps);
    const gcd = (a, b) => b === 0 ? a : gcd(b, a % b);
    // Common rates have exact phase tables. Other integer rates interpolate
    // adjacent FIR coefficients, never linearly interpolate audio samples.
    // <=513 banks *258 taps *8 bytes =1,058,832 bytes per owned resampler.
    this.phases = Math.min(512, 16000 / gcd(inputRate, 16000));
    this.filter = new Float64Array((this.phases + 1) * this.taps);
    const cutoff = 7000 / inputRate;
    if (inputRate !== 16000) for (let bank = 0; bank <= this.phases; bank++) {
      const offset = bank * this.taps, delay = bank / this.phases;
      let sum = 0;
      for (let tap = 0; tap < this.taps; tap++) {
        const x = tap - this.half - delay;
        const sinc = x === 0 ? 2 * cutoff : Math.sin(2 * Math.PI * cutoff * x) / (Math.PI * x);
        const window = Math.abs(x) >= this.half ? 0 : 0.42 + 0.5 * Math.cos(Math.PI * x / this.half) + 0.08 * Math.cos(2 * Math.PI * x / this.half);
        this.filter[offset + tap] = sinc * window;
        sum += this.filter[offset + tap];
      }
      for (let tap = 0; tap < this.taps; tap++) this.filter[offset + tap] /= sum;
    }
  }

  push(input) {
    for (const sample of input) {
      if (!Number.isFinite(sample)) throw new Error('invalid_audio_sample');
      this.ring[this.position] = Math.max(-1, Math.min(1, sample));
      this.phase += 16000;
      if (this.phase >= this.inputRate) {
        this.phase -= this.inputRate;
        let value = this.ring[this.position];
        if (this.inputRate !== 16000) {
          const phase = this.phase * this.phases / 16000;
          const bank = Math.floor(phase), blend = phase - bank, offset = bank * this.taps;
          let position = this.position;
          value = 0;
          // Convolution only runs when an output sample is due, not at the
          // input device rate. AudioWorklet input chunk sizes cannot alter phase.
          for (let tap = 0; tap < this.taps; tap++) {
            const weight = this.filter[offset + tap];
            const coefficient = blend === 0 ? weight : weight + blend * (this.filter[offset + this.taps + tap] - weight);
            value += coefficient * this.ring[position];
            if (--position < 0) position = this.taps - 1;
          }
        }
        value = Math.max(-1, Math.min(1, value));
        this.frame[this.filled++] = Math.round(value < 0 ? value * 32768 : value * 32767);
        if (this.filled === 320) {
          const frame = this.frame;
          this.frame = new Int16Array(320);
          this.filled = 0;
          this.onFrame(frame);
        }
      }
      this.position = (this.position + 1) % this.taps;
    }
  }
}

// Bounded credits stop worklet->main-thread buffering during suspension or overload.
if (typeof globalThis.AudioWorkletProcessor === 'function') {
  class PilotCaptureProcessor extends globalThis.AudioWorkletProcessor {
    constructor() {
      super();
      this.active = true;
      this.pending = 0;
      this.port.onmessage = ({data}) => {
        if (data === 'stop') this.active = false;
        else if (data === 'ack' && this.pending > 0) this.pending--;
      };
      this.resampler = new PCM16Resampler(globalThis.sampleRate, frame => {
        if (!this.active) return;
        if (this.pending >= 8) { this.fail('capture_backpressure'); return; }
        this.pending++;
        this.port.postMessage({type: 'pcm', at: globalThis.currentTime, pcm: frame.buffer}, [frame.buffer]);
      });
    }

    fail(code) {
      if (this.active) this.port.postMessage({type: 'gap', code});
      this.active = false;
    }

    process(inputs, outputs) {
      for (const output of outputs) for (const channel of output) channel.fill(0);
      if (!this.active) return false;
      const channels = inputs[0];
      if (channels?.length) {
        // Web Audio's explicit mono channel configuration performs the downmix.
        try { this.resampler.push(channels[0]); } catch { this.fail('invalid_audio_sample'); }
      }
      return this.active;
    }
  }
  globalThis.registerProcessor('pilot-pcm-capture', PilotCaptureProcessor);
}
