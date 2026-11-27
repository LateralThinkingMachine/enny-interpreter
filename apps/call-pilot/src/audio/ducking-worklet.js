// This render-thread module is also imported by signal-level tests. It never
// changes source buffers, queues PCM, or sends samples to the main thread.
export const DUCKING_PROFILE = Object.freeze({
  version: 'private-ducking.v1', attackMs: 25, releaseMs: 180, holdMs: 180,
  volumeRampMs: 20, thresholdOn: 0.008, thresholdOff: 0.004,
  duckGain: 0.20, headroom: 0.5,
});
export const DEFAULT_DUCKING_SETTINGS = Object.freeze({
  version: DUCKING_PROFILE.version, enabled: true,
  originalVolume: 1, translatedVolume: 1,
  originalMuted: false, translatedMuted: false,
});

export function validateDuckingSettings(settings) {
  if (!settings || typeof settings !== 'object' || Array.isArray(settings)
      || Object.keys(settings).length !== 6
      || settings.version !== DUCKING_PROFILE.version
      || typeof settings.enabled !== 'boolean'
      || typeof settings.originalMuted !== 'boolean'
      || typeof settings.translatedMuted !== 'boolean'
      || !Number.isFinite(settings.originalVolume) || settings.originalVolume < 0 || settings.originalVolume > 1
      || !Number.isFinite(settings.translatedVolume) || settings.translatedVolume < 0 || settings.translatedVolume > 1) {
    throw new Error('invalid_ducking_settings');
  }
  return Object.freeze({...settings});
}

class LinearGain {
  constructor(value, sampleRate) { this.value = value; this.target = value; this.remaining = 0; this.step = 0; this.sampleRate = sampleRate; }
  set(target, milliseconds) {
    if (target === this.target) return;
    this.target = target;
    this.remaining = Math.max(1, Math.ceil(this.sampleRate * milliseconds / 1000));
    this.step = (target - this.value) / this.remaining;
  }
  silence() { this.value = 0; this.target = 0; this.remaining = 0; this.step = 0; }
  next() {
    if (this.remaining > 0) { this.remaining--; this.value = this.remaining ? this.value + this.step : this.target; }
    return this.value;
  }
}

/** Two borrowed signal branches enter one private, unity-safe playback mix. */
export class DuckingMixer {
  constructor(sampleRate, settings = DEFAULT_DUCKING_SETTINGS) {
    if (!Number.isFinite(sampleRate) || sampleRate < 8000 || sampleRate > 192000) throw new Error('invalid_ducking_sample_rate');
    this.sampleRate = sampleRate;
    this.settings = validateDuckingSettings(settings);
    this.original = new LinearGain(0, sampleRate);
    this.translated = new LinearGain(0, sampleRate);
    this.active = false;
    this.ducking = false;
    this.silentFrames = 0;
    this.frames = 0;
    this.controlChanged = true;
    this.translatedSignalSeen = false;
    this.renderedSignalSeen = false;
  }
  updateSettings(settings) { this.settings = validateDuckingSettings(settings); this.controlChanged = true; }
  render(original, translated, output, translationEnabled = true) {
    const settings = this.settings, count = output[0]?.length ?? 0;
    const allowed = translationEnabled && !settings.translatedMuted && settings.translatedVolume > 0;
    let energy = 0;
    if (allowed) {
      for (const channel of translated) {
        let sum = 0;
        for (let index = 0; index < count; index++) {
          const sample = Number.isFinite(channel[index]) ? channel[index] * settings.translatedVolume : 0;
          sum += sample * sample;
        }
        energy = Math.max(energy, count ? Math.sqrt(sum / count) : 0);
      }
    }
    if (!allowed) { this.active = false; this.silentFrames = 0; this.translated.silence(); }
    else if (energy >= (this.active ? DUCKING_PROFILE.thresholdOff : DUCKING_PROFILE.thresholdOn)) {
      this.active = true; this.silentFrames = 0;
    } else if (this.active) {
      this.silentFrames += count;
      if (this.silentFrames >= this.sampleRate * DUCKING_PROFILE.holdMs / 1000) this.active = false;
    }
    if (energy > 0) this.translatedSignalSeen = true;
    const nextDucking = settings.enabled && this.active && !settings.originalMuted && settings.originalVolume > 0;
    const changed = this.ducking !== nextDucking;
    this.ducking = nextDucking;
    const originalLevel = settings.originalVolume * (nextDucking ? DUCKING_PROFILE.duckGain : 1);
    if (settings.originalMuted) this.original.silence();
    else this.original.set(originalLevel, this.controlChanged && !changed ? DUCKING_PROFILE.volumeRampMs
      : originalLevel < this.original.value ? DUCKING_PROFILE.attackMs : DUCKING_PROFILE.releaseMs);
    this.controlChanged = false;
    if (allowed) this.translated.set(settings.translatedVolume, DUCKING_PROFILE.volumeRampMs);
    for (let index = 0; index < count; index++) {
      const originalGain = this.original.next(), translatedGain = this.translated.next();
      for (let channel = 0; channel < output.length; channel++) {
        const source = original[Math.min(channel, original.length - 1)]?.[index] ?? 0;
        const speech = translated[Math.min(channel, translated.length - 1)]?.[index] ?? 0;
        // Valid media is already unit-range. Guard corrupt/nonfinite samples;
        // normal audio is neither compressed nor clipped by this mixer.
        const left = Number.isFinite(source) ? Math.max(-1, Math.min(1, source)) : 0;
        const right = Number.isFinite(speech) ? Math.max(-1, Math.min(1, speech)) : 0;
        if (right * translatedGain !== 0) this.renderedSignalSeen = true;
        output[channel][index] = (left * originalGain + right * translatedGain) * DUCKING_PROFILE.headroom;
      }
    }
    this.frames += count;
    return changed;
  }
}

if (typeof globalThis.AudioWorkletProcessor === 'function') {
  class PrivateDuckingProcessor extends globalThis.AudioWorkletProcessor {
    static get parameterDescriptors() { return [{name: 'translationEnabled', defaultValue: 1, minValue: 0, maxValue: 1, automationRate: 'k-rate'}]; }
    constructor(options) {
      super();
      this.mixer = new DuckingMixer(globalThis.sampleRate, options.processorOptions.settings);
      this.stopped = false;
      this.signalGeneration = 0;
      this.reportedTranslated = false;
      this.reportedRendered = false;
      this.port.onmessage = ({data}) => {
        if (data?.type === 'stop') { this.stopped = true; this.port.onmessage = null; }
        else if (!this.stopped && data?.type === 'reset_signals' && Number.isSafeInteger(data.generation) && data.generation >= 0) {
          this.signalGeneration = data.generation;
          this.mixer.translatedSignalSeen = this.mixer.renderedSignalSeen = false;
          this.reportedTranslated = this.reportedRendered = false;
        }
        else if (!this.stopped && data?.type === 'settings') {
          try { this.mixer.updateSettings(data.settings); }
          catch { this.stopped = true; this.port.postMessage({type: 'invalid_settings'}); }
        }
      };
    }
    process(inputs, outputs, parameters) {
      if (this.stopped) { for (const channel of outputs[0] ?? []) channel.fill(0); return false; }
      if (this.mixer.render(inputs[0] ?? [], inputs[1] ?? [], outputs[0] ?? [], parameters.translationEnabled[0] === 1)) {
        this.port.postMessage({type: 'ducking', active: this.mixer.ducking, frame: this.mixer.frames});
      }
      if (this.reportedTranslated !== this.mixer.translatedSignalSeen || this.reportedRendered !== this.mixer.renderedSignalSeen) {
        this.reportedTranslated = this.mixer.translatedSignalSeen; this.reportedRendered = this.mixer.renderedSignalSeen;
        this.port.postMessage({type: 'signal', generation: this.signalGeneration, translated: this.reportedTranslated, rendered: this.reportedRendered});
      }
      return true;
    }
  }
  globalThis.registerProcessor('private-audio-ducking', PrivateDuckingProcessor);
}
