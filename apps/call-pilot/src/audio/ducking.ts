import {DEFAULT_DUCKING_SETTINGS, DUCKING_PROFILE, validateDuckingSettings} from './ducking-worklet.js';
import type {DuckingSettings} from './ducking-worklet.js';

export {DEFAULT_DUCKING_SETTINGS, DUCKING_PROFILE, validateDuckingSettings};
export type {DuckingSettings};
export type DuckingFault = 'playback_processor_failed' | 'translated_stream_ended' | 'original_stream_ended' | 'playback_context_unavailable' | 'translated_renderer_failed';
export interface DuckingPlaybackState {
  settings: Readonly<DuckingSettings>;
  ducking: boolean;
  selectiveDuckingAvailable: boolean;
  translationEnabled: boolean;
  rendererReady: boolean;
  translatedSignalSeen: boolean;
  renderedSignalSeen: boolean;
  status: 'playing' | 'translation_suspended' | 'failed' | 'disposed';
}
export interface DuckingPlaybackOptions {
  /** Borrowed, already authorized playback streams. Never captured here. */
  original: MediaStream | null;
  translated: MediaStream | null;
  /** Optional context armed under Start's user gesture. Ownership transfers
   * after stream/settings validation; this mixer closes it on disposal. */
  context?: AudioContext;
  settings?: DuckingSettings;
  signal?: AbortSignal;
  onState?(state: DuckingPlaybackState): void;
  onFault?(fault: DuckingFault): void;
}

async function deadline<T>(promise: Promise<T>, milliseconds: number, code: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([promise, new Promise<never>((_, reject) => {timer = setTimeout(() => reject(new Error(code)), milliseconds);})]);
  } finally { clearTimeout(timer); }
}

/** Browser playout only. Rust continues to own the paid session and policy.
 * The caller owns/stops capture and WebRTC tracks and must not also attach
 * these streams to audible HTMLMediaElements or another destination. */
export class BrowserDuckingPlayback {
  private context: AudioContext;
  private mixer?: AudioWorkletNode;
  private original?: MediaStreamAudioSourceNode;
  private translated?: MediaStreamAudioSourceNode;
  private translatedGate?: GainNode;
  private removeTranslatedListener?: () => void;
  private renderer?: HTMLAudioElement;
  private rejectRenderer?: () => void;
  private signalGeneration = 0;
  private fallback?: GainNode;
  private removers: Array<() => void> = [];
  private state: DuckingPlaybackState;
  private disposed = false;
  private disposal?: Promise<void>;
  private initialized = false;

  private constructor(private readonly options: DuckingPlaybackOptions) {
    this.state = {
      settings: validateDuckingSettings(options.settings ?? DEFAULT_DUCKING_SETTINGS),
      selectiveDuckingAvailable: options.original !== null, ducking: false,
      translationEnabled: false, rendererReady: false, translatedSignalSeen: false, renderedSignalSeen: false, status: 'translation_suspended',
    };
    this.context = options.context ?? new AudioContext({latencyHint: 'interactive'});
  }

  static async create(options: DuckingPlaybackOptions): Promise<BrowserDuckingPlayback> {
    if (options.signal?.aborted) throw new Error('private_audio_cancelled');
    const translated = options.translated?.getAudioTracks() ?? [], original = options.original?.getAudioTracks() ?? [];
    if ((options.translated !== null && (translated.length !== 1 || translated[0].readyState !== 'live'))
        || (options.translated === null && options.original === null)
        || (options.original !== null && (original.length !== 1 || original[0].readyState !== 'live'))
        || original.some(track => translated.some(other => track === other || track.id === other.id))) {
      throw new Error('separate_audio_tracks_required');
    }
    const playback = new BrowserDuckingPlayback(options);
    let rejectAbort: (error: Error) => void = () => {};
    const cancelled = new Promise<never>((_, reject) => {rejectAbort = reject;});
    const abort = () => {
      void playback.dispose().catch(() => { /* create/owner observes cleanup independently */ });
      rejectAbort(new Error('private_audio_cancelled'));
    };
    options.signal?.addEventListener('abort', abort, {once: true});
    playback.removers.push(() => options.signal?.removeEventListener('abort', abort));
    try {
      const resumed = playback.context.resume();
      await deadline(Promise.race([cancelled, Promise.all([
        resumed, playback.context.audioWorklet.addModule(new URL('./ducking-worklet.js', import.meta.url).href),
      ])]), 5_000, 'private_audio_setup_timeout');
      if (playback.disposed || options.signal?.aborted) throw new Error('private_audio_cancelled');
      if (translated.some(track => track.readyState !== 'live') || original.some(track => track.readyState !== 'live')) throw new Error('private_audio_stream_ended');
      if (playback.context.state !== 'running') throw new Error('private_audio_not_running');
      const mixer = new AudioWorkletNode(playback.context, 'private-audio-ducking', {
        numberOfInputs: 2, numberOfOutputs: 1, outputChannelCount: [2],
        channelCount: 2, channelCountMode: 'explicit', channelInterpretation: 'speakers',
        processorOptions: {settings: playback.state.settings},
      });
      playback.mixer = mixer;
      if (options.original) {
        playback.original = playback.context.createMediaStreamSource(options.original);
        playback.original.connect(mixer, 0, 0);
      }
      playback.translatedGate = playback.context.createGain();
      playback.translatedGate.gain.value = 0;
      playback.translatedGate.connect(mixer, 0, 1);
      mixer.connect(playback.context.destination);
      mixer.port.onmessage = ({data}: MessageEvent<unknown>) => {
        if (playback.disposed || playback.state.status === 'failed') return;
        if (typeof data !== 'object' || data === null) return;
        const message = data as Record<string, unknown>;
        if (message.type === 'invalid_settings') {playback.fail('playback_processor_failed'); return;}
        if (message.type === 'signal' && message.generation === playback.signalGeneration
            && typeof message.translated === 'boolean' && typeof message.rendered === 'boolean') {
          playback.state = {...playback.state, translatedSignalSeen: playback.state.translatedSignalSeen || message.translated,
            renderedSignalSeen: playback.state.renderedSignalSeen || message.rendered};
          playback.publish(); return;
        }
        if (message.type !== 'ducking' || typeof message.active !== 'boolean') return;
        playback.state = {...playback.state, ducking: message.active && playback.state.translationEnabled
          && playback.state.settings.enabled && !playback.state.settings.translatedMuted && !playback.state.settings.originalMuted
          && playback.state.settings.translatedVolume > 0 && playback.state.settings.originalVolume > 0 && playback.state.selectiveDuckingAvailable};
        playback.publish();
      };
      mixer.onprocessorerror = () => playback.fail('playback_processor_failed');
      if (original[0]) {
        const sourceEnded = () => playback.fail('original_stream_ended');
        original[0].addEventListener('ended', sourceEnded, {once: true});
        playback.removers.push(() => original[0].removeEventListener('ended', sourceEnded));
      }
      playback.context.onstatechange = () => {
        if (playback.initialized && !playback.disposed && playback.context.state !== 'running') playback.fail('playback_context_unavailable');
      };
      playback.initialized = true;
      if (options.translated) await playback.replaceTranslation(options.translated);
      else playback.suspendTranslation();
      playback.publish();
      return playback;
    } catch (error) {
      try {await playback.dispose();} catch {throw new Error('private_audio_cleanup_incomplete');}
      throw error;
    }
  }

  getState(): DuckingPlaybackState { return {...this.state}; }

  /** Replace only the returned speech branch. Original monitoring and source
   * capture continue uninterrupted through provider renewal. Remains gated
   * until the session owner explicitly acknowledges readiness and resumes. */
  async replaceTranslation(stream: MediaStream): Promise<void> {
    if (this.disposed || this.state.status === 'failed') throw new Error('private_audio_retired');
    const tracks = stream.getAudioTracks(), original = this.options.original?.getAudioTracks() ?? [];
    if (tracks.length !== 1 || tracks[0].readyState !== 'live'
        || original.some(track => track === tracks[0] || track.id === tracks[0].id)) throw new Error('separate_audio_tracks_required');
    this.detachTranslation();
    this.translated = this.context.createMediaStreamSource(stream);
    this.translated.connect(this.translatedGate!);
    const ended = () => this.fail('translated_stream_ended');
    tracks[0].addEventListener('ended', ended, {once: true});
    this.removeTranslatedListener = () => tracks[0].removeEventListener('ended', ended);
    // Chromium may receive RTC packets yet deliver zeros to WebAudio until a
    // media renderer plays that remote track. This element is always silent;
    // the worklet remains the sole audible route. It never receives capture.
    const generation = this.signalGeneration;
    const renderer = document.createElement('audio');
    renderer.muted = true; renderer.defaultMuted = true; renderer.volume = 0; renderer.controls = false; renderer.hidden = true;
    renderer.disableRemotePlayback = true; renderer.setAttribute('aria-hidden', 'true');
    this.renderer = renderer;
    renderer.srcObject = stream;
    document.body.append(renderer);
    const cancelled = new Promise<never>((_, reject) => {this.rejectRenderer = () => reject(new Error('private_audio_renderer_retired'));});
    void cancelled.catch(() => {}); // a synchronous native play() throw still leaves cancellation observed
    renderer.onerror = () => {
      if (this.renderer === renderer && generation === this.signalGeneration && !this.disposed) {
        this.rejectRenderer?.(); this.fail('translated_renderer_failed');
      }
    };
    try {
      await deadline(Promise.race([renderer.play(), cancelled]), 5_000, 'private_audio_renderer_timeout');
      if (this.disposed || this.renderer !== renderer || generation !== this.signalGeneration) throw new Error('private_audio_renderer_retired');
      if (this.context.state !== 'running' || tracks[0].readyState !== 'live') throw new Error('private_audio_not_running');
      this.rejectRenderer = undefined;
      this.state = {...this.state, rendererReady: true}; this.publish();
    } catch (error) {
      if (this.renderer === renderer) this.detachTranslation();
      throw error;
    }
  }

  /** Detach before closing the predecessor peer. Borrowed track lifetime stays
   * with the WebRTC owner; no ended callback from it can poison its successor. */
  detachTranslation(): void {
    if (this.disposed) return;
    let failed = false;
    const attempt = (work: () => void) => {try {work();} catch {failed = true;}};
    attempt(() => this.suspendTranslation());
    attempt(() => this.rejectRenderer?.()); this.rejectRenderer = undefined;
    this.signalGeneration++;
    this.state = {...this.state, rendererReady: false, translatedSignalSeen: false, renderedSignalSeen: false};
    attempt(() => this.mixer?.port.postMessage({type: 'reset_signals', generation: this.signalGeneration}));
    const renderer = this.renderer; this.renderer = undefined;
    if (renderer) {
      attempt(() => {renderer.onerror = null;});
      attempt(() => renderer.pause());
      attempt(() => {renderer.srcObject = null;});
      attempt(() => renderer.remove());
    }
    attempt(() => this.removeTranslatedListener?.()); this.removeTranslatedListener = undefined;
    const previous = this.translated; this.translated = undefined;
    attempt(() => previous?.disconnect());
    this.publish();
    if (failed) throw new Error('private_audio_renderer_cleanup_incomplete');
  }

  /** Caller applies the current, acknowledged Rust policy. No server roundtrip
   * occurs on the audio rendering thread. Invalid settings change nothing. */
  updateSettings(patch: Partial<DuckingSettings>): void {
    if (this.disposed) return;
    const settings = validateDuckingSettings({...this.state.settings, ...patch});
    this.state = {...this.state, settings, ducking: this.state.ducking && settings.enabled
      && !settings.originalMuted && !settings.translatedMuted && settings.originalVolume > 0 && settings.translatedVolume > 0};
    this.translatedGate?.gain.setValueAtTime(this.state.translationEnabled && !settings.translatedMuted ? 1 : 0, this.context.currentTime);
    this.mixer?.port.postMessage({type: 'settings', settings});
    this.restoreFallback();
    this.publish();
  }

  /** Immediate generated-speech fence; it neither mutes nor alters capture. */
  suspendTranslation(): void {
    if (this.disposed) return;
    this.state = {...this.state, translationEnabled: false, ducking: false,
      status: this.state.status === 'failed' ? 'failed' : 'translation_suspended'};
    this.translatedGate?.gain.setValueAtTime(0, this.context.currentTime);
    this.mixer?.parameters.get('translationEnabled')?.setValueAtTime(0, this.context.currentTime);
    this.publish();
  }

  /** Explicit owner action only; faults cannot automatically resume speech. */
  resumeTranslation(): void {
    if (this.disposed || this.state.status === 'failed' || !this.translated || !this.state.rendererReady) return;
    this.state = {...this.state, translationEnabled: true, status: 'playing'};
    this.mixer?.parameters.get('translationEnabled')?.setValueAtTime(1, this.context.currentTime);
    this.translatedGate?.gain.setValueAtTime(this.state.settings.translatedMuted ? 0 : 1, this.context.currentTime);
    this.publish();
  }

  private fail(fault: DuckingFault): void {
    if (this.disposed || this.state.status === 'failed') return;
    this.suspendTranslation();
    this.state = {...this.state, status: 'failed'};
    if (fault === 'playback_processor_failed') {
      // A failed worklet outputs silence forever. Disconnect it before routing
      // original audio around it, so fallback can never duplicate playback.
      try {this.mixer?.disconnect();} catch { /* also attempt remaining cleanup */ }
      try {this.original?.disconnect();} catch { /* original reconnect below */ }
      try {
        if (this.original) {
          this.fallback = this.context.createGain();
          this.fallback.gain.value = 0;
          this.original.connect(this.fallback);
          this.fallback.connect(this.context.destination);
          this.restoreFallback();
        }
      } catch { /* still report the fault so the owner retires the paid route */ }
    }
    this.publish();
    try {this.options.onFault?.(fault);} catch { /* observer cannot own teardown */ }
  }

  private restoreFallback(): void {
    if (!this.fallback) return;
    const gain = this.fallback.gain, now = this.context.currentTime;
    gain.cancelScheduledValues(now);
    if (this.state.settings.originalMuted) gain.setValueAtTime(0, now);
    else {
      gain.setValueAtTime(gain.value, now);
      gain.linearRampToValueAtTime(this.state.settings.originalVolume * DUCKING_PROFILE.headroom, now + DUCKING_PROFILE.releaseMs / 1000);
    }
  }

  private publish(): void { try {this.options.onState?.(this.getState());} catch { /* observer failures never affect playout */ } }

  /** Retires all owned nodes/context; borrowed tracks remain under the caller. */
  dispose(): Promise<void> {
    if (this.disposal) return this.disposal;
    this.disposed = true;
    this.state = {...this.state, ducking: false, translationEnabled: false, rendererReady: false, translatedSignalSeen: false, renderedSignalSeen: false, status: 'disposed'};
    let failed = false;
    const attempt = (action: () => void) => {try {action();} catch {failed = true;}};
    attempt(() => this.rejectRenderer?.()); this.rejectRenderer = undefined;
    if (this.renderer) {
      const renderer = this.renderer; this.renderer = undefined;
      attempt(() => {renderer.onerror = null;});
      attempt(() => renderer.pause()); attempt(() => {renderer.srcObject = null;}); attempt(() => renderer.remove());
    }
    attempt(() => this.translatedGate?.gain.setValueAtTime(0, this.context.currentTime));
    attempt(() => {this.context.onstatechange = null;});
    for (const remove of this.removers.splice(0)) attempt(remove);
    if (this.removeTranslatedListener) attempt(this.removeTranslatedListener);
    this.removeTranslatedListener = undefined;
    if (this.mixer) {
      attempt(() => {this.mixer!.onprocessorerror = null; this.mixer!.port.onmessage = null;});
      attempt(() => this.mixer!.port.postMessage({type: 'stop'}));
      attempt(() => this.mixer!.port.close());
    }
    for (const node of [this.translated, this.translatedGate, this.original, this.mixer, this.fallback]) if (node) attempt(() => node.disconnect());
    this.publish();
    this.disposal = (async () => {
      try {await deadline(this.context.close(), 1_000, 'private_audio_close_timeout');} catch {failed = true;}
      if (failed || this.context.state !== 'closed') throw new Error('private_audio_cleanup_incomplete');
    })();
    return this.disposal;
  }
}
