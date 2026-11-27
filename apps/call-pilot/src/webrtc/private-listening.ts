import {requestMeetingTab, requireMeetingTab} from '../audio/meeting-tab.js';
import type {Interpretation, Session} from '../protocol.js';
import {parseInterpretation} from '../protocol.js';
import {BrowserDuckingPlayback, DEFAULT_DUCKING_SETTINGS, validateDuckingSettings} from '../audio/ducking.js';
import type {DuckingPlaybackOptions, DuckingPlaybackState, DuckingSettings} from '../audio/ducking.js';
import type {CaptionEntry} from './caption-data.js';
import {parsePrivateVoice, retainPrivateConfigurations, type PrivateVoice, type PrivateWindowConfiguration} from './private-observations.js';
import {EMPTY_TRANSPORT_EVIDENCE, UNAVAILABLE_TRANSPORT_EVIDENCE, PrivateTransportSampler, type PrivateTransportEvidence} from './private-transport.js';

export type PrivateInput = 'tab' | 'microphone';
export type PrivateListeningMode = 'interpretation' | 'original_only';
export type PrivatePhase = 'idle' | 'selecting' | 'connecting' | 'listening' | 'renewing' | 'closing' | 'closed' | 'unconfirmed' | 'failed';
const FAILURE_MESSAGES = {
  audio_selection: 'Audio selection failed. Choose a supported audio source and check browser sharing permissions.',
  playback_setup: 'Private headphone playback could not be prepared.',
  operator_connection: 'The local operator connection could not be established.',
  peer_setup: 'The browser speech connection could not be prepared.',
  local_offer: 'The browser could not prepare its speech connection offer.',
  ice_gathering: 'The browser could not complete speech network discovery.',
  service_admission: 'The local service could not confirm admission of the private speech connection.',
  remote_answer: 'The browser could not validate or apply the speech connection answer.',
  session_start: 'The speech connection did not become ready in time.',
  control_stream: 'Speech control disconnected. No previous audio is replayed.',
  media_connection: 'The WebRTC audio connection ended unexpectedly.',
  provider_event: 'The speech connection returned an invalid event.',
  provider_closed: 'The provider closed this speech session unexpectedly.',
  provider_error: 'The speech provider reported an error.',
  lease_renewal: 'The private listening lease could not be renewed.',
  provider_status: 'The service ended this private speech connection or could not confirm its state.',
  audio_ended: 'Selected audio ended. No automatic source replacement.',
  renewal_cleanup: 'Audio cleanup could not be confirmed during renewal.',
  renewal_closure: 'Speech renewal stopped because predecessor closure was not confirmed.',
} as const;
export type PrivateListeningFailure = Readonly<{stage: keyof typeof FAILURE_MESSAGES; message: string; httpStatus: number | null}>;
export type PrivateAudioEvidence = Readonly<{
  receivedPackets: number; receivedSignal: boolean; status: 'pending' | 'observed' | 'unavailable';
  rendererReady: boolean | null; translatedSignalSeen: boolean | null; renderedSignalSeen: boolean | null;
  transport: PrivateTransportEvidence;
}>;
const EMPTY_AUDIO_EVIDENCE: PrivateAudioEvidence = Object.freeze({receivedPackets: 0, receivedSignal: false, status: 'pending', rendererReady: null, translatedSignalSeen: null, renderedSignalSeen: null, transport: EMPTY_TRANSPORT_EVIDENCE});
class PrivateRequestFailure extends Error {
  constructor(readonly status: number) {super('Private request failed');}
}
type ServerStatus = Readonly<{enabled: boolean; state: string; generation: number; request_id?: string; requested_voice?: PrivateVoice; closure_basis?: 'cancelled_before_admission'; issue: string | null; closure_confirmed: boolean; usage_confirmed: boolean; usage_seconds: number | null; allocated_microusd: number | null; warning_threshold_microusd: number | null; duration_seconds: number; allocation_duration_seconds: number | null}>;
export type PrivateListeningView = Readonly<{
  phase: PrivatePhase; message: string; available: boolean; source: PrivateInput; listeningMode: PrivateListeningMode;
  status: ServerStatus | null; settings: DuckingSettings; playback: DuckingPlaybackState | null;
  entries: readonly CaptionEntry[]; discarded: number; windows: number; gaps: number;
  configurations: readonly PrivateWindowConfiguration[];
  costNotice: string | null;
  failure: PrivateListeningFailure | null;
  audioEvidence: PrivateAudioEvidence;
}>;
export interface PrivateListeningOwner {
  session(): Session | null;
  claim(displayedEstimate: number, allocationSeconds: number | null): boolean;
  connect(): Promise<void>;
  release(): void;
  available(): boolean;
  current(): boolean;
  subscribe(listener: () => void): () => void;
  authorityLost(): void;
}
export interface PrivateListeningPorts {
  fetch: typeof fetch;
  select(source: PrivateInput): Promise<MediaStream>;
  context(): AudioContext;
  peer(): RTCPeerConnection;
  playback(options: DuckingPlaybackOptions): Promise<BrowserDuckingPlayback>;
  nonce(): string;
  now(): number;
  interval(callback: () => void, ms: number): () => void;
  after(callback: () => void, ms: number): () => void;
}
const ACTIVE: readonly PrivatePhase[] = ['selecting', 'connecting', 'listening', 'renewing', 'closing', 'unconfirmed'];
const STATES = ['ready', 'connecting', 'connected', 'closing', 'closed', 'closure_uncertain', 'busy'];
const ISSUES = ['creation_rejected', 'creation_unconfirmed', 'start_cancelled', 'invalid_provider_answer', 'sideband_unavailable', 'closure_timeout', 'sideband_protocol', 'sideband_identity', 'usage_unconfirmed', 'provider_error', 'text_capacity', 'delegation_unavailable', 'sideband_disconnected'];
const MAX_TEXT_BYTES = 1_048_576, MAX_ENTRIES = 2000;
const encoder = new TextEncoder();
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid private listening response');
  return value as Record<string, unknown>;
}
function counter(value: unknown): number {if (!Number.isSafeInteger(value) || (value as number) < 0) throw new Error('Invalid counter'); return value as number;}
function validatePolicy(value: unknown): void {
  const row = record(value);
  const expected = {schema_version: 'audio-ducking.v1', original_gain: 1, ducked_gain: .20, attack_ms: 25, release_ms: 180, hold_ms: 180, rms_on: .008, rms_off: .004, volume_ramp_ms: 20, headroom: .5};
  if (Object.keys(row).length !== Object.keys(expected).length || Object.entries(expected).some(([key, val]) => row[key] !== val)) throw new Error('Unsupported playback policy');
}
export function parsePrivateStatus(value: unknown, generation: number): ServerStatus {
  const row = record(value); validatePolicy(row.ducking);
  if (row.generation !== generation || typeof row.enabled !== 'boolean' || !STATES.includes(String(row.state))
      || typeof row.closure_confirmed !== 'boolean' || typeof row.usage_confirmed !== 'boolean'
      || row.duration_seconds !== 300 || (row.request_id !== undefined && (typeof row.request_id !== 'string' || !/^[a-f0-9]{64}$/.test(row.request_id)))) throw new Error('Invalid private status');
  const usage = row.usage_seconds === undefined || row.usage_seconds === null ? null : Number(row.usage_seconds);
  if (usage !== null && (!Number.isFinite(usage) || usage < 0 || usage > 360)) throw new Error('Invalid usage');
  const issue = row.issue === undefined || row.issue === null ? null : row.issue;
  if (issue !== null && (typeof issue !== 'string' || !ISSUES.includes(issue))) throw new Error('Invalid private issue');
  const voice = row.requested_voice === undefined ? undefined : parsePrivateVoice(row.requested_voice);
  if (row.closure_basis !== undefined && (row.closure_basis !== 'cancelled_before_admission' || row.state !== 'closed' || !row.closure_confirmed || row.usage_confirmed || usage !== null || row.session_id != null)) throw new Error('Invalid cancellation receipt');
  return Object.freeze({enabled: row.enabled, state: String(row.state), generation, issue,
    ...(row.request_id === undefined ? {} : {request_id: row.request_id as string}),
    ...(voice === undefined ? {} : {requested_voice: voice}),
    ...(row.closure_basis === undefined ? {} : {closure_basis: 'cancelled_before_admission' as const}),
    closure_confirmed: row.closure_confirmed, usage_confirmed: row.usage_confirmed, usage_seconds: usage,
    allocated_microusd: row.allocated_microusd === null ? null : counter(row.allocated_microusd),
    warning_threshold_microusd: row.warning_threshold_microusd === null ? null : counter(row.warning_threshold_microusd), duration_seconds: 300,
    allocation_duration_seconds: row.allocation_duration_seconds === 335 ? 335 : null});
}

/** One private listener, bound to one authenticated Pilot. React is not a media owner.
 * Media travels only on WebRTC. Data-channel text is a bounded display observation;
 * the Rust sideband remains authoritative for lifecycle, configuration and usage. */
export class PrivateListening {
  private view: PrivateListeningView = {phase: 'idle', message: 'Private listening availability and provider closure have not been verified.', available: false, source: 'tab', listeningMode: 'interpretation', status: null, settings: DEFAULT_DUCKING_SETTINGS, playback: null, entries: [], configurations: Object.freeze([]), discarded: 0, windows: 0, gaps: 0, costNotice: null, failure: null, audioEvidence: EMPTY_AUDIO_EVIDENCE};
  private requestedVoice: PrivateVoice | null = null;
  private setupStage: PrivateListeningFailure['stage'] = 'audio_selection';
  private listeners = new Set<() => void>();
  private epoch = 0;
  private disposed = false;
  private capture: MediaStream | null = null;
  private input: MediaStreamTrack | null = null;
  private peer: RTCPeerConnection | null = null;
  private retiringPeer: RTCPeerConnection | null = null;
  private channel: RTCDataChannel | null = null;
  private playback: BrowserDuckingPlayback | null = null;
  private context: AudioContext | null = null;
  private session: Session | null = null;
  private requestId: string | null = null;
  private offered = false;
  private started = false;
  private pending = new Set<AbortController>();
  private cancelHeartbeat: (() => void) | null = null;
  private cancelRenewal: (() => void) | null = null;
  private cancelStartup: (() => void) | null = null;
  private cancelIce: (() => void) | null = null;
  private cancelAudioEvidence: (() => void) | null = null;
  private unsubscribe: () => void;
  private heartbeatPending = false;
  private sequence = 0;
  private textBytes = 0;
  private thresholdMultiple: number | null = null;
  private refreshing = false;
  private stopping: Promise<void> | null = null;
  private retiredContexts: Promise<unknown>[] = [];
  private selectionPending = false;
  private playbackPending = 0;
  private playbackAbort: AbortController | null = null;
  private outputTrack: MediaStreamTrack | null = null;
  private cleanupFailed = false;
  constructor(private readonly owner: PrivateListeningOwner, private readonly ports: PrivateListeningPorts) {
    this.unsubscribe = owner.subscribe(() => {
      if (ACTIVE.includes(this.view.phase) && !owner.current()) void this.stop('Private listening stopped because its operator session ended.');
    });
  }
  snapshot = (): PrivateListeningView => this.view;
  subscribe = (listener: () => void): (() => void) => {this.listeners.add(listener); return () => this.listeners.delete(listener);};
  private set(update: Partial<PrivateListeningView>): void {if (this.disposed) return; this.view = {...this.view, ...update}; for (const listener of this.listeners) listener();}
  private current(epoch: number): boolean {return !this.disposed && this.epoch === epoch && this.owner.current();}
  private rememberFailure(stage: PrivateListeningFailure['stage'], error?: unknown): void {
    if (this.disposed || this.view.failure || !this.isActive() || ['closing', 'unconfirmed'].includes(this.view.phase)) return;
    // Retain fixed local metadata before Stop/authority-loss cleanup can obscure it.
    // Publication happens with Stop's latch; raw browser/provider errors are excluded.
    this.view = {...this.view, failure: Object.freeze({stage, message: FAILURE_MESSAGES[stage], httpStatus: error instanceof PrivateRequestFailure ? error.status : null})};
  }
  private fail(stage: PrivateListeningFailure['stage'], error?: unknown): Promise<void> {
    this.rememberFailure(stage, error); return this.stop(FAILURE_MESSAGES[stage]);
  }
  isActive(): boolean {return ACTIVE.includes(this.view.phase);}
  async refresh(): Promise<void> {
    if (this.disposed || this.refreshing || this.isActive()) return;
    const session = this.owner.session(); if (!session) return;
    this.refreshing = true; const epoch = this.epoch;
    try {
      const status = parsePrivateStatus(await this.request('/api/webrtc/status', undefined, session), session.generation);
      if (this.disposed || epoch !== this.epoch || this.owner.session() !== session) return;
      this.observeStatus(status);
      this.set({available: status.enabled && status.closure_confirmed && this.owner.available(), message: !status.closure_confirmed ? 'An existing speech connection has not confirmed closure and may still incur cost. Check availability again after it closes.' : status.enabled ? 'Ready for private listening. Choose audio and press Start. Provider initialization can incur cost before audio connects.' : 'Private WebRTC listening requires an enabled continuous local GPT-Live-1 activation.'});
    } catch {if (!this.disposed && epoch === this.epoch) this.set({available: false, status: null, message: 'Private listening availability and provider closure could not be verified; ongoing cost is unknown.'});}
    finally {this.refreshing = false;}
  }
  async start(source: PrivateInput, interpretation: Interpretation, agreed: boolean, displayedEstimate: number, voice: PrivateVoice = 'marin'): Promise<void> {
    if (!agreed || this.disposed || this.isActive() || !this.view.available) return;
    let pinned: Interpretation, selectedVoice: PrivateVoice;
    try {
      if (source !== 'tab' && source !== 'microphone') throw new Error('Unsupported source');
      // Clone/freeze before claim, native picker or any await. The caller's
      // mutable settings cannot change this Start or its later renewals.
      pinned = parseInterpretation(interpretation); selectedVoice = parsePrivateVoice(voice);
    } catch {this.set({message: 'Choose a supported language, voice and audio source before starting.'}); return;}
    if (!this.view.status || !this.owner.claim(displayedEstimate, this.view.status.allocation_duration_seconds)) return;
    if (this.disposed || !this.owner.current()) {this.owner.release(); return;}
    this.session = this.owner.session(); if (!this.session) {this.owner.release(); return;}
    this.requestedVoice = selectedVoice;
    const epoch = ++this.epoch;
    this.setupStage = 'audio_selection';
    this.set({phase: 'selecting', source, message: source === 'microphone' ? 'Allow microphone access in your browser. Nothing is sent during selection.' : 'Choose the browser tab and share its audio. Nothing is sent during selection.', available: false, failure: null});
    if (!this.current(epoch)) return;
    let selected: Promise<MediaStream> | undefined;
    try {
      // Both browser permission and audio unlock begin within the explicit gesture.
      this.selectionPending = true;
      selected = this.ports.select(source);
      this.context = this.ports.context();
      void this.context.resume().catch(error => {if (this.current(epoch)) void this.fail('playback_setup', error);});
    } catch (error) {
      if (selected) void selected.then(stream => this.stopTracks(stream), () => {}).finally(() => {this.selectionPending = false; this.pendingRetired();});
      else this.selectionPending = false;
      await this.fail('audio_selection', error); return;
    }
    try {
      const stream = await selected;
      this.selectionPending = false;
      if (!this.current(epoch)) {this.stopTracks(stream); this.pendingRetired(); return;}
      this.capture = stream;
      if (source === 'tab') {
        requireMeetingTab(stream);
        const settings = stream.getAudioTracks()[0].getSettings() as MediaTrackSettings & {suppressLocalAudioPlayback?: boolean};
        if (settings.suppressLocalAudioPlayback !== true) throw new Error('Tab playback suppression unavailable');
      } else if (stream.getAudioTracks().length !== 1 || stream.getAudioTracks()[0].readyState !== 'live') throw new Error('Missing audio');
      for (const track of stream.getTracks()) track.addEventListener('ended', () => {if (this.current(epoch)) void this.fail('audio_ended');}, {once: true});
      this.setupStage = 'playback_setup';
      if (source === 'tab') await this.createPlayback(epoch, null);
      if (!this.current(epoch)) return;
      this.setupStage = 'operator_connection';
      await this.owner.connect(); if (!this.current(epoch)) return;
      await this.connect(epoch, pinned, selectedVoice);
    } catch (error) {this.selectionPending = false; if (this.current(epoch)) await this.fail(this.setupStage, error); else this.pendingRetired();}
  }
  private async connect(epoch: number, interpretation: Interpretation, voice: PrivateVoice): Promise<void> {
    if (!this.current(epoch) || !this.capture || !this.session) return;
    this.setupStage = 'peer_setup';
    this.requestId = this.ports.nonce(); this.offered = false; this.started = false;
    const requestId = this.requestId;
    const peer = this.ports.peer(); this.peer = peer;
    const input = this.capture.getAudioTracks()[0].clone(); input.enabled = false; this.input = input;
    peer.addTrack(input, new MediaStream([input]));
    const channel = peer.createDataChannel('oai-events', {ordered: true}); this.channel = channel;
    const window = this.view.windows + 1;
    const configuration: PrivateWindowConfiguration = Object.freeze({window, model: 'gpt-live-1', interpretation, requested_voice: voice, source: this.view.source});
    const configurations = retainPrivateConfigurations([...this.view.configurations, configuration], this.view.entries, window);
    const valid = () => this.current(epoch) && this.peer === peer && this.requestId === requestId;
    this.set({phase: window > 1 ? 'renewing' : 'connecting', message: 'Connecting to GPT-Live-1. A provider session may incur cost during setup.', windows: window, configurations, playback: null, audioEvidence: EMPTY_AUDIO_EVIDENCE});
    if (!valid()) return;
    channel.onmessage = event => {if (valid()) {try {this.event(event.data, window, peer); this.activate();} catch {void this.fail('provider_event');}}};
    channel.onerror = channel.onclose = () => {if (valid() && this.retiringPeer !== peer && this.view.phase !== 'closing') void this.fail('control_stream');};
    peer.onconnectionstatechange = () => {
      if (!valid()) return;
      if (['failed', 'disconnected', 'closed'].includes(peer.connectionState)) {if (this.retiringPeer !== peer) void this.fail('media_connection');}
      else this.activate();
    };
    peer.ontrack = event => {
      if (!valid() || this.retiringPeer === peer || event.track.kind !== 'audio') return;
      if (this.outputTrack || this.playbackPending) {void this.fail('provider_event'); return;}
      this.outputTrack = event.track;
      if (this.playback) {
        const playback = this.playback; this.playbackPending++;
        void (async () => {
          let attached = false;
          try {
            await playback.replaceTranslation(new MediaStream([event.track]));
            attached = true;
          } catch {if (valid() && this.retiringPeer !== peer) void this.fail('playback_setup');}
          finally {this.playbackPending--; this.pendingRetired();}
          if (attached && valid() && this.retiringPeer !== peer && this.playback === playback) this.activate();
        })();
      } else void this.createPlayback(epoch, new MediaStream([event.track])).then(() => {if (valid()) this.activate();}, () => {if (valid()) void this.fail('playback_setup');});
    };
    this.startAudioEvidence(epoch, peer);
    this.cancelStartup?.(); this.cancelStartup = this.ports.after(() => {if (valid() && this.view.phase !== 'listening') void this.fail('session_start');}, 45_000);
    this.setupStage = 'local_offer';
    const offer = await peer.createOffer(); if (!valid()) return;
    await peer.setLocalDescription(offer); if (!valid()) return;
    this.setupStage = 'ice_gathering';
    await this.gatherIce(peer, valid); if (!valid()) return;
    const sdp = peer.localDescription?.sdp;
    if (!sdp || encoder.encode(sdp).length > 65_536) throw new Error('Invalid local offer');
    this.offered = true;
    this.startHeartbeat(epoch);
    this.setupStage = 'service_admission';
    const answer = record(await this.request('/api/webrtc/offer', {generation: this.session.generation, request_id: requestId, sdp, interpretation, voice, participant_agreement: true, headphones: true}, this.session, 30_000));
    if (!valid()) return;
    this.setupStage = 'remote_answer';
    validatePolicy(answer.ducking);
    const transport = record(answer.transport);
    if (answer.generation !== this.session.generation || answer.request_id !== requestId || parsePrivateVoice(answer.requested_voice) !== voice || answer.duration_seconds !== 300 || transport.type !== 'webrtc' || typeof transport.sdp !== 'string' || encoder.encode(transport.sdp).length > 65_536) throw new Error('Invalid answer');
    await peer.setRemoteDescription({type: 'answer', sdp: transport.sdp}); if (!valid()) return;
    this.setupStage = 'session_start';
    this.cancelRenewal?.(); this.cancelRenewal = this.ports.after(() => {if (valid()) void this.renew(epoch, interpretation, voice);}, (answer.duration_seconds as number - 45) * 1000);
  }
  private activate(): void {
    if (!this.started || this.playbackPending || !this.playback || !this.outputTrack || !this.input || this.retiringPeer === this.peer || this.peer?.connectionState !== 'connected' || !this.owner.current() || !['connecting', 'renewing'].includes(this.view.phase)) return;
    this.cancelStartup?.(); this.cancelStartup = null;
    if (this.view.listeningMode === 'original_only') this.playback.suspendTranslation();
    else this.playback.resumeTranslation();
    this.input.enabled = true;
    this.set({phase: 'listening', message: this.listeningMessage()});
  }
  private listeningMessage(): string {
    return this.view.listeningMode === 'original_only'
      ? 'Interpretation off for me · captions and paid GPT-Live-1 processing continue. Stop ends processing.'
      : 'Interpreting privately · paid GPT-Live-1 connection open. Silence and muted playback do not stop cost.';
  }
  private gatherIce(peer: RTCPeerConnection, valid: () => boolean): Promise<void> {
    if (peer.iceGatheringState === 'complete') return Promise.resolve();
    return new Promise((resolve, reject) => {
      let finished = false;
      const finish = (error?: Error) => {
        if (finished) return; finished = true;
        cancelTimer(); peer.removeEventListener('icegatheringstatechange', changed);
        if (this.cancelIce === cancelled) this.cancelIce = null;
        if (error) reject(error); else resolve();
      };
      const changed = () => {if (!valid()) finish(new Error('ICE cancelled')); else if (peer.iceGatheringState === 'complete') finish();};
      const cancelled = () => finish(new Error('ICE cancelled'));
      const cancelTimer = this.ports.after(() => finish(new Error('ICE gathering timeout')), 10_000);
      this.cancelIce = cancelled;
      peer.addEventListener('icegatheringstatechange', changed); changed();
    });
  }
  private event(data: unknown, window: number, peer: RTCPeerConnection): void {
    if (typeof data !== 'string' || encoder.encode(data).length > 65_536) throw new Error('Invalid data channel message');
    const event = record(JSON.parse(data));
    if (event.type === 'session.started') {if (this.started) throw new Error('Duplicate start'); this.started = true; return;}
    if (event.type === 'session.closed') {if (this.retiringPeer !== peer && this.view.phase !== 'closing') void this.fail('provider_closed'); return;}
    if (event.type === 'error' || event.type === 'session.error') {void this.fail('provider_error'); return;}
    if (!['session.input_transcript.delta', 'session.output_transcript.delta'].includes(String(event.type))) return;
    if (!this.started || typeof event.delta !== 'string' || event.delta.includes('\0') || encoder.encode(event.delta).length > 16_384
        || typeof event.start_ms !== 'number' || typeof event.end_ms !== 'number' || !Number.isFinite(event.start_ms) || !Number.isFinite(event.end_ms)
        || event.start_ms < 0 || event.end_ms < event.start_ms || event.end_ms > 360_000) throw new Error('Invalid transcript delta');
    if (!event.delta) return;
    const entry: CaptionEntry = Object.freeze({sequence: ++this.sequence, window, provenance: event.type === 'session.input_transcript.delta' ? 'gpt_live_input' : 'gpt_live_output', text: event.delta,
      start_ms: event.start_ms, end_ms: event.end_ms, participants: Object.freeze([]), model: 'gpt-live-1'});
    const entries = [...this.view.entries, entry]; this.textBytes += encoder.encode(entry.text).length; let discarded = this.view.discarded;
    while (entries.length > MAX_ENTRIES || this.textBytes > MAX_TEXT_BYTES) {this.textBytes -= encoder.encode(entries.shift()!.text).length; discarded++;}
    this.set({entries: Object.freeze(entries), configurations: retainPrivateConfigurations(this.view.configurations, entries, this.view.windows), discarded});
  }
  private startHeartbeat(epoch: number): void {
    this.cancelHeartbeat?.();
    const requestId = this.requestId;
    this.cancelHeartbeat = this.ports.interval(() => {
      if (this.heartbeatPending || !this.current(epoch) || !this.session || !this.requestId) return;
      this.heartbeatPending = true;
      void this.request('/api/webrtc/heartbeat', {generation: this.session.generation, request_id: requestId}, this.session, 5000)
        .then(value => {if (this.current(epoch) && this.requestId === requestId && this.view.phase !== 'renewing' && this.view.phase !== 'closing') this.observeStatus(parsePrivateStatus(value, this.session!.generation));})
        .catch(error => {if (this.current(epoch) && this.requestId === requestId && this.view.phase !== 'renewing' && this.view.phase !== 'closing') void this.fail('lease_renewal', error);})
        .finally(() => {this.heartbeatPending = false;});
    }, 5000);
  }
  private startAudioEvidence(epoch: number, peer: RTCPeerConnection): void {
    this.cancelAudioEvidence?.();
    let cancelled = false, pending = false;
    let cancelDeadline: (() => void) | null = null;
    const transport = new PrivateTransportSampler();
    const valid = () => !cancelled && this.current(epoch) && this.peer === peer && this.retiringPeer !== peer;
    const unavailable = () => {
      transport.reset();
      if (valid()) this.set({audioEvidence: Object.freeze({...this.view.audioEvidence, status: 'unavailable', transport: UNAVAILABLE_TRANSPORT_EVIDENCE})});
    };
    const cancelInterval = this.ports.interval(() => {
      if (pending || !valid()) return;
      pending = true;
      let expired = false;
      // A stalled browser request stays single-flight, but cannot leave stale
      // transport values displayed as a current measurement indefinitely.
      cancelDeadline = this.ports.after(() => {expired = true; unavailable();}, 2500);
      void (async () => {
        try {
          const report = await peer.getStats();
          if (!valid() || expired) return;
          // Inspect only bounded inbound-audio numeric fields. Never retain raw
          // reports, transport addresses, device labels, track IDs or content.
          let packets = 0, signal = false, observed = false, count = 0;
          for (const row of report.values()) {
            if (++count > 256) break;
            if (row.type !== 'inbound-rtp' || (row.kind ?? row.mediaType) !== 'audio') continue;
            const received: unknown = row.packetsReceived;
            if (typeof received === 'number' && Number.isSafeInteger(received) && received >= 0) {
              packets = Math.min(Number.MAX_SAFE_INTEGER, packets + received); observed = true;
            }
            const energy: unknown = row.totalAudioEnergy, level: unknown = row.audioLevel;
            const energyValid = typeof energy === 'number' && Number.isFinite(energy) && energy >= 0 && energy <= Number.MAX_SAFE_INTEGER;
            const levelValid = typeof level === 'number' && Number.isFinite(level) && level >= 0 && level <= 1;
            observed ||= energyValid || levelValid;
            signal ||= energyValid && energy > 0 || levelValid && level > 0;
          }
          const prior = this.view.audioEvidence;
          this.set({audioEvidence: Object.freeze({...prior, receivedPackets: Math.max(prior.receivedPackets, packets), receivedSignal: prior.receivedSignal || signal, status: observed || prior.status === 'observed' ? 'observed' : 'pending', transport: transport.sample(report)})});
        } catch {
          unavailable();
        } finally {cancelDeadline?.(); cancelDeadline = null; pending = false;}
      })();
    }, 1000);
    this.cancelAudioEvidence = () => {cancelled = true; cancelInterval(); cancelDeadline?.(); cancelDeadline = null; transport.reset();};
  }
  private observeStatus(status: ServerStatus): void {
    if (this.requestId && status.request_id !== this.requestId) throw new Error('Changed session owner');
    if (this.requestId && status.requested_voice !== this.requestedVoice && !(status.requested_voice === undefined && status.closure_basis === 'cancelled_before_admission')) throw new Error('Changed requested voice');
    let costNotice = this.view.costNotice;
    if (status.allocated_microusd !== null && status.warning_threshold_microusd && status.warning_threshold_microusd > 0) {
      const multiple = Math.floor(status.allocated_microusd / status.warning_threshold_microusd);
      if (this.thresholdMultiple !== null && multiple > this.thresholdMultiple) costNotice = `Local estimated allocations reached US$${(status.allocated_microusd / 1_000_000).toFixed(2)} since service start. This is not an invoice or spending cap.`;
      this.thresholdMultiple = multiple;
    }
    this.set({status, costNotice});
    if (['closure_uncertain', 'busy'].includes(status.state) && this.isActive() && this.view.phase !== 'closing') void this.fail('provider_status');
    else if (['listening', 'connecting'].includes(this.view.phase) && (!status.enabled || ['closing', 'closed'].includes(status.state))) void this.fail('provider_status');
  }
  private async closeProvider(): Promise<boolean> {
    if (!this.offered || !this.session || !this.requestId) return true;
    const session = this.session, requestId = this.requestId;
    try {
      const stopped = parsePrivateStatus(await this.request('/api/webrtc/stop', {generation: session.generation, request_id: requestId}, session, 5000), session.generation);
      if (stopped.request_id !== requestId) return false;
      this.observeStatus(stopped);
      if (stopped.closure_confirmed) return true;
      const deadline = this.ports.now() + 25_000;
      for (let attempt = 0; attempt < 50 && !this.disposed && this.ports.now() < deadline; attempt++) {
        const status = parsePrivateStatus(await this.request('/api/webrtc/status', undefined, session, 2500), session.generation);
        if (status.request_id !== requestId) return false;
        this.observeStatus(status);
        if (status.closure_confirmed) return true;
        if (status.state === 'closure_uncertain') return false;
        await new Promise<void>(resolve => this.ports.after(resolve, 500));
      }
    } catch { /* local retirement does not establish remote closure */ }
    return false;
  }
  private cleanup(work: () => void): void {try {work();} catch {this.cleanupFailed = true;}}
  private stopTracks(stream: MediaStream): void {for (const track of stream.getTracks()) this.cleanup(() => track.stop());}
  private pendingRetired(): void {
    if (!this.disposed && !this.selectionPending && !this.playbackPending && this.view.phase === 'unconfirmed' && !this.stopping) void this.stop('Audio selection and playback setup have retired; rechecking closure.');
  }
  private async createPlayback(epoch: number, translated: MediaStream | null): Promise<void> {
    const context = this.context; if (!context || this.playbackPending || this.playback) throw new Error('Playback ownership unavailable');
    const abort = new AbortController(); this.playbackAbort = abort; this.playbackPending++;
    this.context = null;
    try {
      const playback = await this.ports.playback({original: this.view.source === 'tab' ? this.capture : null, translated, context, settings: this.view.settings, signal: abort.signal,
        onState: state => {
          if (!this.current(epoch)) return;
          let audioEvidence = this.view.audioEvidence;
          if (this.peer && this.outputTrack && this.retiringPeer !== this.peer) {
            audioEvidence = Object.freeze({...audioEvidence,
              rendererReady: typeof state.rendererReady === 'boolean' ? state.rendererReady : audioEvidence.rendererReady,
              translatedSignalSeen: typeof state.translatedSignalSeen === 'boolean' ? audioEvidence.translatedSignalSeen === true || state.translatedSignalSeen : audioEvidence.translatedSignalSeen,
              renderedSignalSeen: typeof state.renderedSignalSeen === 'boolean' ? audioEvidence.renderedSignalSeen === true || state.renderedSignalSeen : audioEvidence.renderedSignalSeen,
            });
          }
          this.set({playback: state, audioEvidence});
        },
        onFault: () => {if (this.current(epoch)) void this.fail('playback_setup');}});
      if (!this.current(epoch) || abort.signal.aborted) {await playback.dispose(); return;}
      this.playback = playback;
      // No generated playout until both this graph and session.started are ready.
      playback.suspendTranslation();
    } catch (error) {
      if (context.state !== 'closed') try {await context.close();} catch {this.cleanupFailed = true;}
      throw error;
    } finally {
      this.playbackPending--; if (this.playbackAbort === abort) this.playbackAbort = null; this.pendingRetired();
    }
  }
  private retireMedia(keepCapture: boolean): void {
    this.cancelAudioEvidence?.(); this.cancelAudioEvidence = null;
    this.cancelStartup?.(); this.cancelStartup = null; this.cancelRenewal?.(); this.cancelRenewal = null;
    this.cleanup(() => this.cancelIce?.()); this.cancelIce = null;
    this.cleanup(() => this.playbackAbort?.abort());
    const input = this.input; this.input = null;
    if (input) {this.cleanup(() => {input.enabled = false;}); this.cleanup(() => input.stop());}
    this.cleanup(() => this.playback?.suspendTranslation()); this.cleanup(() => this.playback?.detachTranslation());
    this.outputTrack = null;
    const channel = this.channel; this.channel = null;
    if (channel) {channel.onmessage = channel.onclose = channel.onerror = null; this.cleanup(() => channel.close());}
    const peer = this.peer; this.peer = null;
    if (peer) {peer.ontrack = peer.onconnectionstatechange = null; this.cleanup(() => peer.close());}
    if (this.retiringPeer === peer) this.retiringPeer = null;
    if (!keepCapture) {
      const playback = this.playback; this.playback = null;
      if (playback) this.cleanup(() => {this.retiredContexts.push(playback.dispose());});
      const context = this.context; this.context = null;
      if (context) this.cleanup(() => {this.retiredContexts.push(context.close());});
      const capture = this.capture; this.capture = null;
      if (capture) this.cleanup(() => this.stopTracks(capture));
    }
    for (const work of this.retiredContexts) void work.catch(() => {});
  }
  private async renew(epoch: number, interpretation: Interpretation, voice: PrivateVoice): Promise<void> {
    if (!this.current(epoch) || this.view.phase !== 'listening') return;
    // Fence this exact predecessor before notifying subscribers or asking the
    // provider to close. Its final events may still arrive during retirement.
    this.retiringPeer = this.peer;
    this.cancelAudioEvidence?.(); this.cancelAudioEvidence = null;
    this.set({phase: 'renewing', gaps: this.view.gaps + 1, message: 'Refreshing the bounded speech connection. Transition audio is not replayed; captions have a coverage gap.'});
    if (!this.current(epoch)) return;
    this.cancelHeartbeat?.(); this.cancelHeartbeat = null;
    // Disable only the owned sender clone; the original capture track is untouched.
    this.cleanup(() => {if (this.input) this.input.enabled = false;});
    this.cleanup(() => this.playback?.suspendTranslation());
    this.cleanup(() => this.playback?.detachTranslation());
    if (this.cleanupFailed) {await this.fail('renewal_cleanup'); return;}
    const closed = await this.closeProvider();
    if (!this.current(epoch)) return;
    this.retireMedia(true);
    const retired = await Promise.allSettled(this.retiredContexts.splice(0));
    if (!this.current(epoch)) return;
    if (!closed || this.cleanupFailed || retired.some(result => result.status === 'rejected')) {await this.fail('renewal_closure'); return;}
    try {
      await this.connect(epoch, interpretation, voice);
    } catch (error) {if (this.current(epoch)) await this.fail(this.setupStage, error);}
  }
  async stop(message = 'Private listening stopped locally. Checking provider closure…'): Promise<void> {
    if (this.disposed) return;
    if (this.stopping) return this.stopping;
    ++this.epoch; this.cancelHeartbeat?.(); this.cancelHeartbeat = null;
    for (const request of this.pending) request.abort(); this.pending.clear();
    this.retireMedia(false);
    const work = (async () => {
      await Promise.resolve(); // publish the singleflight owner before notifying UI subscribers
      const confirmed = await this.closeProvider();
      const retired = await Promise.allSettled(this.retiredContexts.splice(0));
      if (this.disposed) return;
      if (retired.some(row => row.status === 'rejected')) this.cleanupFailed = true;
      const clean = !this.cleanupFailed && !this.selectionPending && !this.playbackPending && retired.every(row => row.status === 'fulfilled');
      this.set({phase: confirmed && clean ? 'closed' : 'unconfirmed', message: confirmed && clean ? 'Private listening is off · provider closure confirmed. No new audio is sent. Earlier usage may still be billed. The meeting tab controls its original sound again.' : this.selectionPending ? 'Audio is stopped. Cancel the open browser sharing picker to finish cleanup; a second selection is blocked.' : 'Audio is stopped locally. Provider closure or device cleanup remains unconfirmed; additional cost is unknown.'});
      if (confirmed && clean) {this.requestId = null; this.offered = false; this.owner.release(); this.set({available: this.owner.available()});}
    })();
    this.stopping = work;
    this.set({phase: 'closing', message, playback: null, available: false});
    try {await work;} finally {if (this.stopping === work) this.stopping = null;}
  }
  updateSettings(update: Partial<DuckingSettings>): void {
    if (this.disposed) return;
    const settings = validateDuckingSettings({...this.view.settings, ...update, version: 'private-ducking.v1' as const});
    this.playback?.updateSettings(settings); this.set({settings});
  }
  /** Listener-local playback preference, separate from provider/session state.
   * The live track keeps advancing while gated; there is no catch-up queue.
   * Retain this preference through late setup and renewal before any resume. */
  setListeningMode(mode: PrivateListeningMode): void {
    if (mode !== 'interpretation' && mode !== 'original_only') throw new Error('Invalid private listening mode');
    if (this.disposed || mode === this.view.listeningMode) return;
    // Latch before touching native playback or publishing callbacks. A graph
    // arriving later must consult this value, never its predecessor's state.
    this.view = {...this.view, listeningMode: mode};
    try {
      if (mode === 'original_only') this.playback?.suspendTranslation();
      else if (this.view.phase === 'listening' && this.started && !this.playbackPending
          && this.outputTrack && this.peer?.connectionState === 'connected'
          && this.retiringPeer !== this.peer && this.owner.current()) this.playback?.resumeTranslation();
    } catch {void this.fail('playback_setup'); return;}
    this.set(this.view.phase === 'listening' ? {message: this.listeningMessage()} : {});
  }
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true; ++this.epoch; this.unsubscribe(); this.cancelHeartbeat?.();
    for (const request of this.pending) request.abort(); this.pending.clear();
    this.retireMedia(false); this.listeners.clear(); this.requestedVoice = null; this.view = {...this.view, entries: [], configurations: Object.freeze([]), status: null, playback: null, failure: null, phase: 'closed', available: false};
    // The owning Pilot's Stop/logout/page-exit path closes the same Rust owner;
    // the independent 30-second lease covers lost terminal HTTP observations.
  }
  private async request(path: string, body: unknown | undefined, session: Session, timeout = 5000): Promise<unknown> {
    const abort = new AbortController(); this.pending.add(abort);
    const cancelTimeout = this.ports.after(() => abort.abort(), timeout);
    try {
      const payload = body === undefined ? undefined : JSON.stringify(body);
      if (payload && encoder.encode(payload).length > 98_304) throw new Error('Oversize request');
      const response = await this.ports.fetch(path, {method: body === undefined ? 'GET' : 'POST', credentials: 'omit', mode: 'same-origin', redirect: 'error', cache: 'no-store', signal: abort.signal,
        headers: body === undefined ? {} : {'Content-Type': 'application/json', 'X-Pilot-CSRF': session.csrf}, body: payload});
      if (!response.ok) {
        const failure = new PrivateRequestFailure(response.status);
        const current = !abort.signal.aborted && !this.disposed && this.owner.session() === session;
        const requestedId = typeof body === 'object' && body !== null && 'request_id' in body ? body.request_id : undefined;
        // Retain the actual HTTP observation before authority-loss cleanup. A
        // cancelled or predecessor request cannot supply a new terminal cause.
        if (current && requestedId === this.requestId && this.isActive() && !['closing', 'unconfirmed'].includes(this.view.phase)) {
          if (path === '/api/webrtc/offer') this.rememberFailure('service_admission', failure);
          else if (path === '/api/webrtc/heartbeat' && this.view.phase !== 'renewing') this.rememberFailure('lease_renewal', failure);
        }
        if ([401, 403].includes(response.status) && current) this.owner.authorityLost();
        throw failure;
      }
      if (response.headers.get('Content-Type')?.split(';')[0].trim() !== 'application/json') throw new Error('Invalid response type');
      const reader = response.body?.getReader(); if (!reader) throw new Error('Missing response body');
      let length = 0; const chunks: Uint8Array[] = [];
      try {for (;;) {const row = await reader.read(); if (abort.signal.aborted) throw new Error('Cancelled'); if (row.done) break; length += row.value.length; if (length > 98_304) throw new Error('Oversize response'); chunks.push(row.value);}} finally {await reader.cancel().catch(() => {});}
      const bytes = new Uint8Array(length); let offset = 0; for (const chunk of chunks) {bytes.set(chunk, offset); offset += chunk.length;}
      return JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(bytes));
    } finally {cancelTimeout(); this.pending.delete(abort);}
  }
}

export function browserPrivateListening(owner: PrivateListeningOwner, fetchApi: typeof fetch): PrivateListening {
  return new PrivateListening(owner, {
    fetch: fetchApi, context: () => new AudioContext({latencyHint: 'interactive'}), peer: () => new RTCPeerConnection(),
    playback: options => BrowserDuckingPlayback.create(options), now: () => performance.now(),
    nonce: () => Array.from(crypto.getRandomValues(new Uint8Array(32)), n => n.toString(16).padStart(2, '0')).join(''),
    interval: (fn, ms) => {const timer = setInterval(fn, ms); return () => clearInterval(timer);},
    after: (fn, ms) => {const timer = setTimeout(fn, ms); return () => clearTimeout(timer);},
    select: async source => {
      if (source === 'microphone') return navigator.mediaDevices.getUserMedia({audio: {channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: false}, video: false});
      return requestMeetingTab(true);
    },
  });
}
