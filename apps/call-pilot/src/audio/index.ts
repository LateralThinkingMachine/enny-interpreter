import {decodePCM, decodePreparedWav, encodeCaptureFrame} from './framing.js';
import {requestMeetingTab, requireMeetingTab} from './meeting-tab.js';
import type {AudioSource, PrivateKind} from './framing.js';

export type {AudioSource, PrivateKind};
export interface CaptureStatus {level: number; frames: number; muted: boolean}
export interface AudioEngineOptions {
  onCaptureStatus?(source: AudioSource, status: CaptureStatus): void;
  onFrame(source: AudioSource, frame: ArrayBuffer): boolean;
  onGap(source: AudioSource, code: string): void;
  onPlaybackFault?(kind: PrivateKind | 'clip', code: string): void;
}
export interface ClipCallbacks { started(): void; ended(): void; failed(): void; isCurrent?(): boolean }

interface Capture {
  sequence: number;
  active: boolean;
  releasing: boolean;
  prepared: boolean;
  selectionPending: boolean;
  activated: boolean;
  frames: number;
  level: number;
  signalObserved: boolean;
  lastStatus: number;
  forwardingAfter: number;
  stream?: MediaStream;
  context?: AudioContext;
  node?: AudioWorkletNode;
  input?: MediaStreamAudioSourceNode;
  previewTimer?: ReturnType<typeof setTimeout>;
  frameTimer?: ReturnType<typeof setTimeout>;
  cancellations: Set<() => void>;
  listeners: Array<() => void>;
  cleanup?: Promise<void>;
}
interface PlaybackLane {muted: boolean; next: number; nodes: Set<AudioBufferSourceNode>}

/** Each page owns its engine; shutdown never waits for a network operation. */
export function createAudioEngine(options: AudioEngineOptions) {
  let stopped = false;
  let cleanupFailed = false;
  const contextClosures = new Set<Promise<void>>();
  const captures = new Map<AudioSource, Capture>();
  let playback: AudioContext | undefined;
  let playbackArmed = false;
  let clip: {nodes: Set<AudioBufferSourceNode>; timer?: ReturnType<typeof setTimeout>; fail: () => void} | undefined;
  const lanes: Record<PrivateKind, PlaybackLane> = {
    live: {muted: true, next: 0, nodes: new Set()},
    live_interpretation: {muted: true, next: 0, nodes: new Set()},
    translation: {muted: true, next: 0, nodes: new Set()},
  };

  // Browser resources may already be detached or closed. One failed release
  // must not prevent the remaining devices, nodes or callbacks from retiring.
  function release(action: () => void) { try { action(); } catch { cleanupFailed = true; } }
  function stopNode(node: AudioBufferSourceNode) {
    release(() => { node.onended = null; });
    release(() => node.stop());
    release(() => node.disconnect());
  }
  function stopTracks(stream: MediaStream) {
    release(() => { for (const track of stream.getTracks()) release(() => track.stop()); });
  }
  function closeContext(context: AudioContext): Promise<void> {
    release(() => { context.onstatechange = null; });
    // Context shutdown is asynchronous. A pending/failed close must never allow
    // a new selection to claim that the previous device check was released.
    const closed = new Promise<void>(resolve => {
      let settled = false;
      const complete = (failed: boolean) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        if (failed) cleanupFailed = true;
        resolve();
      };
      const timer = setTimeout(() => complete(true), 1000);
      try { void context.close().then(() => complete(context.state !== 'closed'), () => complete(true)); }
      catch { complete(true); }
    });
    contextClosures.add(closed);
    void closed.then(() => contextClosures.delete(closed));
    return closed;
  }

  function clearLane(kind: PrivateKind) {
    const lane = lanes[kind];
    const nodes = [...lane.nodes];
    lane.nodes.clear();
    lane.next = 0;
    for (const node of nodes) stopNode(node);
  }

  function stopClip() {
    if (!clip) return;
    const previous = clip;
    clip = undefined;
    clearTimeout(previous.timer);
    const nodes = [...previous.nodes];
    previous.nodes.clear();
    for (const node of nodes) stopNode(node);
    release(previous.fail);
  }

  function status(source: AudioSource, capture: Capture, force = false) {
    const now = capture.context?.currentTime ?? 0;
    if (!force && now - capture.lastStatus < 0.1) return;
    capture.lastStatus = now;
    const audio = capture.stream?.getAudioTracks()[0];
    release(() => options.onCaptureStatus?.(source, {
      level: capture.active ? capture.level : 0,
      frames: capture.frames,
      muted: capture.active && Boolean(audio?.muted || audio?.enabled === false),
    }));
  }

  function stopCapture(source: AudioSource) {
    const capture = captures.get(source);
    if (!capture || capture.cleanup || capture.releasing) return;
    capture.releasing = true;
    capture.active = false; // Fence permission promises and queued messages first.
    capture.prepared = false;
    clearTimeout(capture.previewTimer);
    clearTimeout(capture.frameTimer);
    for (const cancel of capture.cancellations) cancel();
    capture.cancellations.clear();
    for (const remove of capture.listeners) release(remove);
    capture.listeners = [];
    release(() => { if (capture.node) { capture.node.onprocessorerror = null; capture.node.port.onmessage = null; } });
    release(() => capture.node?.port.postMessage('stop'));
    release(() => capture.node?.port.close());
    release(() => capture.node?.disconnect());
    release(() => capture.input?.disconnect());
    if (capture.stream) stopTracks(capture.stream);
    capture.level = 0;
    status(source, capture, true);
    capture.cleanup = (capture.context ? closeContext(capture.context) : Promise.resolve()).then(() => {
      if (cleanupFailed) release(() => options.onGap(source, 'audio_cleanup_unconfirmed'));
      // Only local checks may be selected again. Activated sources remain
      // permanently closed, including when no PCM reached the server yet.
      if (!capture.activated && !capture.selectionPending && !cleanupFailed && captures.get(source) === capture) captures.delete(source);
    });
  }

  function gap(source: AudioSource, code: string) {
    if (!captures.get(source)?.active) return;
    stopCapture(source);
    release(() => options.onGap(source, code));
  }

  function bounded<T>(capture: Capture, operation: Promise<T>, milliseconds: number, code: string): Promise<T> {
    return new Promise((resolve, reject) => {
      let settled = false;
      const finish = (work: () => void) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        capture.cancellations.delete(cancel);
        work();
      };
      const cancel = () => finish(() => reject(new Error('capture_closed')));
      const timer = setTimeout(() => finish(() => reject(new Error(code))), milliseconds);
      capture.cancellations.add(cancel);
      void operation.then(value => finish(() => resolve(value)), error => finish(() => reject(error)));
    });
  }

  async function prepareCapture(source: AudioSource) {
    if (stopped || cleanupFailed) throw new Error('capture_closed');
    if (captures.has(source)) throw new Error('capture_closed');
    if (!navigator.mediaDevices || typeof AudioWorkletNode !== 'function' || typeof AudioContext !== 'function') throw new Error('capture_unavailable');
    const capture: Capture = {sequence: 0, active: true, releasing: false, prepared: false, selectionPending: false, activated: false, frames: 0, level: 0, signalObserved: false, lastStatus: -Infinity, forwardingAfter: Infinity, cancellations: new Set(), listeners: []};
    captures.set(source, capture);
    status(source, capture, true);
    try {
      if (stopped || !capture.active) throw new Error('capture_closed');
      // Permission is requested synchronously in the click stack. No route,
      // provider operation or PCM emission is needed to inspect local audio.
      const request = source === 'self'
        ? navigator.mediaDevices.getUserMedia({audio: {channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: false}, video: false})
        : requestMeetingTab();
      capture.selectionPending = true;
      const selection = request.then(stream => {
        if (stopped || !capture.active) {
          stopTracks(stream);
          if (cleanupFailed) release(() => options.onGap(source, 'audio_cleanup_unconfirmed'));
          throw new Error('capture_closed');
        }
        capture.stream = stream;
        return stream;
      }).finally(() => {
        capture.selectionPending = false;
        if (!capture.active && !capture.activated && !cleanupFailed && !capture.context && captures.get(source) === capture) captures.delete(source);
      });
      const stream = await bounded(capture, selection, 30_000, 'capture_selection_timeout');
      if (stopped || !capture.active) throw new Error('capture_closed');
      const audioTracks = stream.getAudioTracks();
      if (source === 'remote_mix') requireMeetingTab(stream);
      else if (audioTracks.length !== 1 || audioTracks[0].readyState !== 'live') throw new Error('tab_audio_missing');
      for (const track of stream.getTracks()) {
        const ended = () => gap(source, 'capture_track_ended');
        const muted = () => {
          if (capture.active) { capture.level = 0; status(source, capture, true); }
        };
        track.addEventListener('ended', ended);
        track.addEventListener('mute', muted);
        track.addEventListener('unmute', muted);
        capture.listeners.push(() => { track.removeEventListener('ended', ended); track.removeEventListener('mute', muted); track.removeEventListener('unmute', muted); });
      }
      const context = new AudioContext({latencyHint: 'interactive'});
      capture.context = context;
      await bounded(capture, (async () => {
        await context.audioWorklet.addModule(new URL('./capture-worklet.js', import.meta.url).href);
        if (stopped || !capture.active) throw new Error('capture_closed');
        await context.resume();
        if (context.state !== 'running' || stopped || !capture.active) throw new Error('capture_unavailable');
      })(), 10_000, 'capture_setup_timeout');
      if (stopped || !capture.active) throw new Error('capture_closed');
      const input = context.createMediaStreamSource(new MediaStream(audioTracks));
      capture.input = input;
      const node = new AudioWorkletNode(context, 'pilot-pcm-capture', {numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [1], channelCount: 1, channelCountMode: 'explicit'});
      capture.node = node;
      node.onprocessorerror = () => gap(source, 'capture_processor_failed');
      const frameDeadline = () => { clearTimeout(capture.frameTimer); capture.frameTimer = setTimeout(() => gap(source, 'capture_no_frames'), 3000); };
      node.port.onmessage = ({data}: MessageEvent<unknown>) => {
        if (stopped || !capture.active) return;
        if (typeof data !== 'object' || data === null) { gap(source, 'invalid_capture_frame'); return; }
        const message = data as {type?: string; pcm?: unknown; at?: number};
        if (message.type === 'gap') { gap(source, 'capture_backpressure'); return; }
        if (message.type !== 'pcm' || !(message.pcm instanceof ArrayBuffer) || message.pcm.byteLength !== 640 || typeof message.at !== 'number' || !Number.isFinite(message.at) || context.currentTime - message.at > 0.3 || message.at > context.currentTime + 0.02) { gap(source, 'capture_stale'); return; }
        const samples = new Int16Array(message.pcm);
        try {
          let sum = 0;
          for (const sample of samples) sum += (sample / 32768) ** 2;
          capture.level = Math.min(1, Math.sqrt(sum / samples.length) * 4);
          capture.frames = Math.min(Number.MAX_SAFE_INTEGER, capture.frames + 1);
          frameDeadline();
          const firstSignal = !capture.signalObserved && capture.level > 0;
          capture.signalObserved ||= firstSignal;
          status(source, capture, firstSignal || capture.frames === 1);
          if (stopped || !capture.active) return;
          // Preview frames, queued port messages and a partial pre-Start DSP
          // frame are discarded. A conservative 40ms sample-time boundary also
          // excludes FIR history. Sequence starts only with the first emission.
          if (capture.activated && message.at > capture.forwardingAfter) {
            const frame = encodeCaptureFrame(source, capture.sequence, message.pcm);
            if (!options.onFrame(source, frame)) { gap(source, 'capture_backpressure'); return; }
            capture.sequence++;
          }
          if (capture.active) node.port.postMessage('ack');
        } catch { gap(source, 'invalid_capture_frame'); }
        finally { samples.fill(0); }
      };
      context.onstatechange = () => { if (capture.active && context.state !== 'running') gap(source, 'capture_suspended'); };
      input.connect(node);
      // The processor writes silence; meeting/microphone audio is not monitored.
      node.connect(context.destination);
      capture.prepared = true;
      capture.previewTimer = setTimeout(() => gap(source, 'capture_preview_timeout'), 120_000);
      frameDeadline();
      status(source, capture, true);
    } catch (error) {
      stopCapture(source);
      await capture.cleanup;
      const known = ['capture_closed', 'tab_audio_missing', 'select_browser_tab', 'capture_unavailable', 'capture_selection_timeout', 'capture_setup_timeout'];
      const code = cleanupFailed ? 'audio_cleanup_unconfirmed'
        : error instanceof DOMException && error.name === 'NotAllowedError' ? 'capture_permission_denied'
        : error instanceof Error && known.includes(error.message) ? error.message : 'capture_permission_or_device_failed';
      // eslint-disable-next-line preserve-caught-error -- Device/permission diagnostics are intentionally replaced with fixed safe codes.
      throw new Error(code);
    }
  }

  function activateCapture(source: AudioSource) {
    const capture = captures.get(source);
    if (stopped || cleanupFailed || !capture?.active || !capture.prepared || capture.activated || capture.context?.state !== 'running' || capture.stream?.getAudioTracks()[0]?.readyState !== 'live' || (source === 'remote_mix' && capture.stream?.getVideoTracks()[0]?.readyState !== 'live')) throw new Error('capture_not_prepared');
    capture.forwardingAfter = capture.context.currentTime + 0.04;
    capture.activated = true;
    clearTimeout(capture.previewTimer);
  }

  async function cancelPreparedCapture(source: AudioSource): Promise<void> {
    const capture = captures.get(source);
    if (capture?.activated) return;
    // Stop remains immediate; the acknowledgement waits only for local cleanup.
    stopCapture(source);
    await capture?.cleanup;
    if (cleanupFailed) throw new Error('audio_cleanup_unconfirmed');
    // A web page cannot dismiss Chrome's picker. No second selection is safe
    // until the operator cancels it or its late result has been released.
    if (capture?.selectionPending) throw new Error('capture_selection_pending');
  }

  async function startCapture(source: AudioSource) {
    await prepareCapture(source);
    activateCapture(source);
  }

  async function resumePlayback() {
    if (stopped) throw new Error('playback_closed');
    playback ??= new AudioContext({latencyHint: 'interactive'});
    const current = playback;
    await current.resume();
    if (stopped || playback !== current || current.state !== 'running') throw new Error('playback_unavailable');
    playbackArmed = true;
    current.onstatechange = () => {
      if (stopped || current.state === 'running') return;
      playbackArmed = false;
      const active = (['translation', 'live', 'live_interpretation'] as const).filter(kind => !lanes[kind].muted);
      lanes.translation.muted = true;
      lanes.live.muted = true;
      lanes.live_interpretation.muted = true;
      for (const kind of ['translation', 'live', 'live_interpretation'] as const) {
        clearLane(kind);
        if (active.includes(kind)) release(() => options.onPlaybackFault?.(kind, 'playback_suspended'));
      }
      if (clip) { stopClip(); release(() => options.onPlaybackFault?.('clip', 'playback_suspended')); }
    };
  }

  function setPrivateMuted(kind: PrivateKind, muted: boolean) {
    lanes[kind].muted = stopped || muted;
    clearLane(kind);
  }

  function playPrivate(kind: PrivateKind, pcm: Uint8Array, rate: number) {
    const lane = lanes[kind];
    if (stopped || lane.muted) return;
    try {
      if (!playback || !playbackArmed || playback.state !== 'running') throw new Error('playback_not_armed');
      const samples = decodePCM(pcm, rate);
      const now = playback.currentTime;
      // The initial/underrun cushion absorbs ordinary packet jitter. Applying
      // that cushion to every packet would insert gaps while a predecessor is
      // still safely scheduled. Preserve its exact end without changing speed.
      const schedulingLead = Math.max(0.005, 256 / playback.sampleRate);
      const start = lane.next >= now + schedulingLead ? lane.next : now + 0.04;
      const end = start + samples.length / rate;
      if (end - now > 1.25 || lane.nodes.size >= 64) throw new Error('playback_backpressure');
      const buffer = playback.createBuffer(1, samples.length, rate);
      buffer.copyToChannel(samples, 0);
      const node = playback.createBufferSource();
      node.buffer = buffer;
      node.connect(playback.destination);
      lane.nodes.add(node);
      node.onended = () => { lane.nodes.delete(node); release(() => node.disconnect()); };
      lane.next = end;
      node.start(start);
    } catch (error) {
      lane.muted = true;
      clearLane(kind);
      options.onPlaybackFault?.(kind, error instanceof Error ? error.message : 'playback_failed');
    }
  }

  function playClip(bytes: ArrayBuffer, callbacks: ClipCallbacks): Promise<void> {
    stopClip();
    return new Promise((resolve, reject) => {
      let settled = false;
      const fail = () => { if (!settled) { settled = true; release(callbacks.failed); reject(new Error('clip_stopped_or_failed')); } };
      try {
        if (stopped || !playbackArmed || !playback || playback.state !== 'running') throw new Error('playback_not_armed');
        const {samples, rate} = decodePreparedWav(bytes);
        const context = playback;
        const current = {nodes: new Set<AudioBufferSourceNode>(), timer: undefined as ReturnType<typeof setTimeout> | undefined, fail};
        clip = current;
        let offset = 0;
        let next = context.currentTime + 0.02;
        let lastTick = performance.now();
        let started = false;
        const tick = () => {
          if (clip !== current || settled) return;
          const now = performance.now();
          // A delayed timer is a loss of authority, not permission to catch up/replay.
          if (stopped || !playbackArmed || context.state !== 'running' || now - lastTick > 400 || callbacks.isCurrent?.() === false) { stopClip(); return; }
          lastTick = now;
          if (offset === samples.length && current.nodes.size === 0) {
            settled = true;
            clip = undefined;
            callbacks.ended();
            resolve();
            return;
          }
          while (offset < samples.length && next - context.currentTime < 0.15) {
            if (stopped || clip !== current || settled || callbacks.isCurrent?.() === false) { stopClip(); return; }
            // At most 190ms is scheduled. A frozen page cannot play an entire approved clip.
            const end = Math.min(samples.length, offset + Math.floor(rate * 0.04));
            const buffer = context.createBuffer(1, end - offset, rate);
            buffer.copyToChannel(samples.subarray(offset, end), 0);
            const node = context.createBufferSource();
            node.buffer = buffer;
            node.connect(context.destination);
            current.nodes.add(node);
            node.onended = () => { current.nodes.delete(node); release(() => node.disconnect()); };
            next = Math.max(next, context.currentTime + 0.01);
            node.start(next);
            next += (end - offset) / rate;
            offset = end;
            if (!started) { started = true; callbacks.started(); }
          }
          if (clip === current && !settled) current.timer = setTimeout(() => { try { tick(); } catch { stopClip(); } }, 40);
        };
        tick();
      } catch { stopClip(); fail(); }
    });
  }

  function stopAll() {
    if (stopped) { if (cleanupFailed) throw new Error('audio_cleanup_unconfirmed'); return; }
    stopped = true;
    playbackArmed = false;
    lanes.translation.muted = true;
    lanes.live.muted = true;
    lanes.live_interpretation.muted = true;
    for (const capture of captures.values()) capture.active = false;
    for (const source of captures.keys()) stopCapture(source);
    for (const kind of ['translation', 'live', 'live_interpretation'] as const) clearLane(kind);
    stopClip();
    if (playback) void closeContext(playback);
    release(() => window.removeEventListener('pagehide', stopAll));
    // Report only after all releases were attempted. The client still clears
    // private state and requests server Stop when local cleanup is unconfirmed.
    if (cleanupFailed) throw new Error('audio_cleanup_unconfirmed');
  }
  async function retire(): Promise<boolean> {
    // Stop stays synchronous. A new recording additionally waits for every
    // owned capture/playback context's existing one-second close deadline.
    release(stopAll);
    await Promise.all([...contextClosures]);
    // Chrome's native chooser cannot be dismissed by this page. A late result
    // remains fenced and releases its tracks; it cannot justify a new owner
    // until the chooser has actually completed or been cancelled.
    return !cleanupFailed && ![...captures.values()].some(capture => capture.selectionPending);
  }
  window.addEventListener('pagehide', stopAll);

  return {prepareCapture, activateCapture, cancelPreparedCapture, startCapture, stopCapture, stopAll, retire, resumePlayback, setPrivateMuted, playPrivate, playClip, stopClip};
}
