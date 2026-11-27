import test from 'node:test';
import assert from 'node:assert/strict';
import {PrivateListening, parsePrivateStatus} from '../.test-build/webrtc/private-listening.js';
import {groupCaptionEntries} from '../.test-build/webrtc/caption-groups.js';
import {exportPrivateObservations, retainPrivateConfigurations} from '../.test-build/webrtc/private-observations.js';
import {EMPTY_TRANSPORT_EVIDENCE, UNAVAILABLE_TRANSPORT_EVIDENCE} from '../.test-build/webrtc/private-transport.js';
const policy = {schema_version: 'audio-ducking.v1', original_gain: 1, ducked_gain: .2, attack_ms: 25, release_ms: 180, hold_ms: 180, rms_on: .008, rms_off: .004, volume_ramp_ms: 20, headroom: .5};
const session = {generation: 1, csrf: 'operator-csrf', providers_enabled: true, continuous: true};
const configuration = {schema_version: 'interpretation.v1', mode: 'auto_to_target', target: 'ja'};
const emptyAudioEvidence = {receivedPackets: 0, receivedSignal: false, status: 'pending', rendererReady: null, translatedSignalSeen: null, renderedSignalSeen: null, transport: EMPTY_TRANSPORT_EVIDENCE};
const flush = async () => {for (let i = 0; i < 10; i++) await new Promise(resolve => setImmediate(resolve));};
const deferred = () => {let resolve, reject; const promise = new Promise((yes, no) => {resolve = yes; reject = no;}); return {promise, resolve, reject};};
class Track extends EventTarget {
  kind = 'audio'; enabled = true; readyState = 'live'; stops = 0;
  constructor(id = 'original') {super(); this.id = id;}
  getSettings() {return this.kind === 'video' ? {displaySurface: 'browser'} : {suppressLocalAudioPlayback: true};}
  clone() {return new Track(`${this.id}-clone`);}
  stop() {this.stops++; this.readyState = 'ended';}
}
class Stream {
  constructor(tracks = [new Track()]) {this.tracks = tracks;}
  getAudioTracks() {return this.tracks.filter(track => track.kind === 'audio');}
  getVideoTracks() {return this.tracks.filter(track => track.kind === 'video');}
  getTracks() {return this.tracks;}
}
function meetingTab() {
  const video = new Track('conference tab');
  video.kind = 'video';
  return new Stream([new Track(), video]);
}
globalThis.MediaStream = Stream;
class Peer extends EventTarget {
  iceGatheringState = 'complete'; connectionState = 'new'; localDescription = null; channel = {onmessage: null, onclose: null, onerror: null, close() {}};
  addTrack(track) {this.input = track;}
  createDataChannel(label, settings) {assert.equal(label, 'oai-events'); assert.equal(settings.ordered, true); return this.channel;}
  async createOffer() {return {type: 'offer', sdp: 'v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n'};}
  async setLocalDescription(value) {this.localDescription = value;}
  async setRemoteDescription(value) {this.remote = value;}
  async getStats() {return new Map();}
  close() {this.connectionState = 'closed'; this.closes = (this.closes ?? 0) + 1;}
  event(value) {this.channel.onmessage?.({data: JSON.stringify(value)});}
}
function harness() {
  let now = 0, nonce = 0, claimed = false, claims = 0, authority = true, state = 'ready', requestId, requestedVoice;
  const clocks = new Set(), calls = [], peers = [], streams = [], contexts = [], mixes = [], ownerListeners = new Set();
  let override, selection, mixGate, replacementGate;
  const status = () => ({enabled: true, state, generation: 1, ...(requestId ? {request_id: requestId, requested_voice: requestedVoice} : {}), closure_confirmed: ['ready', 'closed'].includes(state), usage_confirmed: state === 'closed', usage_seconds: state === 'closed' ? 3.5 : null, allocated_microusd: nonce * 250000, warning_threshold_microusd: 5_000_000, duration_seconds: 300, allocation_duration_seconds: 335, ducking: policy});
  const clock = (fn, ms, repeat = false) => {const timer = {fn, at: now + ms, ms, repeat}; clocks.add(timer); return () => clocks.delete(timer);};
  const owner = {session: () => session, claim: (amount, seconds) => {assert.equal(amount, 279167); assert.equal(seconds, 335); claims++; if (claimed || !authority) return false; claimed = true; return true;}, connect: async () => {}, release: () => {claimed = false;}, available: () => !claimed && authority, current: () => authority,
    subscribe: fn => {ownerListeners.add(fn); return () => ownerListeners.delete(fn);}, authorityLost: () => {authority = false; for (const fn of ownerListeners) fn();}};
  const controller = new PrivateListening(owner, {
    now: () => now, nonce: () => (++nonce).toString(16).padStart(64, '0'), interval: (fn, ms) => clock(fn, ms, true), after: (fn, ms) => clock(fn, ms),
    select: source => {const stream = source === 'tab' ? meetingTab() : new Stream(); streams.push(stream); return selection ?? Promise.resolve(stream);},
    context: () => {const context = {resumes: 0, closes: 0, resume: async () => {context.resumes++;}, close: async () => {context.closes++;}}; contexts.push(context); return context;},
    peer: () => {const peer = new Peer(); peers.push(peer); return peer;},
    playback: async options => {const mix = {options, suspended: 0, disposed: 0, suspendTranslation() {this.suspended++;}, resumeTranslation() {this.resumed = (this.resumed ?? 0) + 1;}, async replaceTranslation(stream) {this.translated = stream; if (replacementGate) await replacementGate;}, detachTranslation() {this.translated = null;}, async dispose() {this.disposed++; await options.context.close();}, updateSettings(value) {this.settings = value;}}; mixes.push(mix); if (mixGate) await mixGate; return mix;},
    fetch: async (path, options) => {
      const body = options.body ? JSON.parse(options.body) : null; calls.push({path, options, body});
      const custom = override?.(path, options, body); if (custom !== undefined) return custom;
      let value;
      if (path.endsWith('/offer')) {requestId = body.request_id; requestedVoice = body.voice; state = 'connected'; value = {generation: 1, request_id: requestId, requested_voice: requestedVoice, session: {id: 'live-test'}, transport: {type: 'webrtc', sdp: 'v=0\r\nanswer'}, duration_seconds: 300, ducking: policy};}
      else if (path.endsWith('/stop')) {state = 'closed'; value = requestId ? status() : {...status(), request_id: body.request_id, closure_basis: 'cancelled_before_admission', usage_confirmed: false, usage_seconds: null};}
      else value = status();
      return new Response(JSON.stringify(value), {headers: {'Content-Type': 'application/json'}});
    },
  });
  return {controller, peers, streams, contexts, mixes, calls, clocks, status, owner,
    setOverride: fn => {override = fn;}, setSelection: value => {selection = value;}, setMixGate: value => {mixGate = value;}, setReplacementGate: value => {replacementGate = value;},
    setState: value => {state = value;}, claimed: () => claimed, claims: () => claims,
    async advance(ms) {now += ms; for (const timer of [...clocks]) if (timer.at <= now && clocks.has(timer)) {if (timer.repeat) timer.at = now + timer.ms; else clocks.delete(timer); timer.fn();} await flush();},
    async start(source = 'tab', voice = 'marin') {await controller.refresh(); await controller.start(source, configuration, true, 279167, voice);},
    async ready() {const peer = peers.at(-1); peer.connectionState = 'connected'; peer.onconnectionstatechange?.(); peer.event({type: 'session.started'}); peer.ontrack?.({track: new Track('translation')}); await flush();},
  };
}

test('viewing availability never selects media; setup pins server configuration and gates input until playback and started', async () => {
  const h = harness(); await h.controller.refresh(); assert.equal(h.streams.length, 0);
  await h.controller.start('tab', configuration, false, 279167); assert.equal(h.peers.length, 0);
  await h.start(); const peer = h.peers[0]; assert.equal(peer.input.enabled, false); assert.equal(h.streams[0].getAudioTracks()[0].enabled, true);
  assert.equal(h.calls.find(row => row.path.endsWith('/offer')).options.headers['X-Pilot-CSRF'], session.csrf);
  assert.equal(h.calls.find(row => row.path.endsWith('/offer')).body.voice, 'marin');
  await h.ready(); assert.equal(peer.input.enabled, true); assert.equal(h.controller.snapshot().phase, 'listening');
  assert.equal(h.mixes[0].options.original, h.streams[0]); assert.notEqual(peer.input, h.streams[0].getAudioTracks()[0]);
  await h.controller.stop(); assert.equal(peer.input.readyState, 'ended'); assert.equal(h.controller.snapshot().phase, 'closed'); assert.equal(h.claimed(), false);
  h.controller.dispose();
});

test('original-only immediately gates local translation while input, captions, provider and other listener stay active', async () => {
  const h = harness(), other = harness(); await h.start(); await h.ready(); await other.start(); await other.ready();
  const peer = h.peers[0], graph = h.mixes[0], suspended = graph.suspended;
  const status = h.controller.snapshot().status, calls = h.calls.length, otherResumes = other.mixes[0].resumed;
  h.controller.updateSettings({originalVolume: .65, originalMuted: true, translatedVolume: .8, translatedMuted: true});
  const settings = h.controller.snapshot().settings;
  h.controller.setListeningMode('original_only');
  assert.equal(graph.suspended, suspended + 1, 'local fence runs synchronously before returning');
  assert.equal(h.controller.snapshot().listeningMode, 'original_only');
  assert.equal(h.controller.snapshot().phase, 'listening'); assert.equal(h.controller.snapshot().status, status);
  assert.equal(h.controller.snapshot().settings, settings, 'stored volume and explicit mute are not overwritten');
  assert.equal(peer.input.enabled, true); assert.equal(h.streams[0].getAudioTracks()[0].enabled, true);
  for (const [type, delta] of [['session.input_transcript.delta', 'Keep the original.'], ['session.output_transcript.delta', '原文を保持します。']]) peer.event({type, delta, start_ms: 0, end_ms: 100});
  assert.deepEqual(h.controller.snapshot().entries.map(row => row.text), ['Keep the original.', '原文を保持します。']);
  assert.match(h.controller.snapshot().message, /off for me.*paid GPT-Live-1 processing continue/);
  assert.equal(h.calls.length, calls, 'mode toggle/captions do not send a provider or control request');
  assert.equal(other.controller.snapshot().listeningMode, 'interpretation'); assert.equal(other.mixes[0].resumed, otherResumes);
  const resumed = graph.resumed; h.controller.setListeningMode('interpretation');
  assert.equal(graph.resumed, resumed + 1); assert.equal(h.controller.snapshot().settings, settings);
  assert.equal(h.calls.length, calls); assert.equal(peer.input.enabled, true);
  await h.controller.stop(); await other.controller.stop(); h.controller.dispose(); other.controller.dispose();
});

test('original-only survives pending renderer and renewal, with On gated until successor readiness', async () => {
  const h = harness(), attach = deferred(); h.setReplacementGate(attach.promise); await h.start();
  await h.ready(); assert.equal(h.controller.snapshot().phase, 'connecting');
  h.controller.setListeningMode('original_only');
  attach.resolve(); await flush();
  assert.equal(h.controller.snapshot().phase, 'listening'); assert.equal(h.mixes[0].resumed ?? 0, 0);
  assert.equal(h.peers[0].input.enabled, true);
  h.setReplacementGate(undefined); await h.advance(255000); await h.ready();
  assert.equal(h.controller.snapshot().listeningMode, 'original_only'); assert.equal(h.mixes[0].resumed ?? 0, 0);
  assert.equal(h.controller.snapshot().windows, 2); assert.equal(h.peers[1].input.enabled, true);
  await h.advance(255000);
  const successor = deferred(); h.setReplacementGate(successor.promise); await h.ready();
  h.controller.setListeningMode('interpretation');
  assert.equal(h.controller.snapshot().phase, 'renewing'); assert.equal(h.mixes[0].resumed ?? 0, 0);
  assert.equal(h.peers[2].input.enabled, false, 'On does not bypass current-session renderer readiness');
  successor.resolve(); await flush();
  assert.equal(h.controller.snapshot().phase, 'listening'); assert.equal(h.mixes[0].resumed, 1);
  assert.equal(h.peers[2].input.enabled, true); assert.equal(h.calls.filter(row => row.path.endsWith('/offer')).length, 3);
  await h.controller.stop(); h.controller.dispose();
});

test('original-only chosen during initial graph setup survives late graph publication', async () => {
  const h = harness(), gate = deferred(); h.setMixGate(gate.promise); await h.controller.refresh();
  const starting = h.controller.start('tab', configuration, true, 279167); await flush();
  h.controller.setListeningMode('original_only'); gate.resolve(); await starting; await h.ready();
  assert.equal(h.mixes[0].resumed ?? 0, 0); assert.equal(h.controller.snapshot().phase, 'listening');
  assert.equal(h.controller.snapshot().listeningMode, 'original_only'); assert.equal(h.peers[0].input.enabled, true);
  await h.controller.stop(); h.controller.dispose();
});

test('original-only survives explicit new Start and cannot revive a stopped route', async () => {
  const h = harness(); await h.start(); await h.ready(); h.controller.setListeningMode('original_only');
  const graph = h.mixes[0], resumed = graph.resumed; await h.controller.stop();
  h.controller.setListeningMode('interpretation'); assert.equal(graph.resumed, resumed);
  assert.equal(h.controller.snapshot().phase, 'closed');
  h.controller.setListeningMode('original_only'); await h.start('microphone'); await h.ready();
  assert.equal(h.mixes[1].options.original, null); assert.equal(h.mixes[1].resumed ?? 0, 0);
  assert.equal(h.peers[1].input.enabled, true); assert.equal(h.controller.snapshot().listeningMode, 'original_only');
  h.controller.setListeningMode('interpretation'); assert.equal(h.mixes[1].resumed, 1);
  await h.controller.stop(); h.controller.dispose();
  h.controller.setListeningMode('original_only'); assert.equal(h.mixes[1].resumed, 1);
});

test('invalid listener mode changes no state or playback, and failure while gating retains off preference', async () => {
  const h = harness(); await h.start(); await h.ready(); const prior = h.controller.snapshot(), graph = h.mixes[0];
  for (const value of [null, true, 'off', 'ORIGINAL_ONLY', {original_only: null}]) assert.throws(() => h.controller.setListeningMode(value), /Invalid private listening mode/);
  assert.equal(h.controller.snapshot(), prior);
  graph.suspendTranslation = () => {throw new Error('synthetic native failure');};
  h.controller.setListeningMode('original_only'); await flush();
  assert.equal(h.controller.snapshot().listeningMode, 'original_only'); assert.equal(h.controller.snapshot().phase, 'unconfirmed');
  assert.equal(h.controller.snapshot().failure.stage, 'playback_setup'); assert.equal(h.peers[0].input.enabled, false);
  h.controller.dispose();
});

test('invalid voice or interpretation cannot claim ownership, select audio or allocate a provider', async () => {
  const h = harness(); await h.controller.refresh(); const calls = h.calls.length;
  for (const voice of [null, '', 'custom_voice', 'Marin', {marin: null}, ['quartz'], 4]) await h.controller.start('tab', configuration, true, 279167, voice);
  await h.controller.start('tab', {...configuration, target: 'unsupported'}, true, 279167, 'marin');
  await h.controller.start('speaker', configuration, true, 279167, 'marin');
  assert.equal(h.claims(), 0); assert.equal(h.streams.length, 0); assert.equal(h.contexts.length, 0); assert.equal(h.peers.length, 0);
  assert.equal(h.calls.length, calls); assert.equal(h.controller.snapshot().available, true);
  assert.match(h.controller.snapshot().message, /supported language, voice and audio source/); h.controller.dispose();
});

test('voice and cloned language policy freeze before picker await and remain pinned through renewal', async () => {
  const h = harness(), picker = deferred(), mutable = {...configuration}; await h.controller.refresh(); h.setSelection(picker.promise);
  const start = h.controller.start('tab', mutable, true, 279167, 'quartz');
  mutable.target = 'en';
  await h.controller.start('microphone', {...configuration, target: 'es'}, true, 279167, 'gleam');
  assert.equal(h.claims(), 1); assert.equal(h.streams.length, 1);
  picker.resolve(meetingTab()); await start; await h.ready();
  const initial = h.controller.snapshot().configurations[0];
  assert.deepEqual(initial, {window: 1, model: 'gpt-live-1', source: 'tab', interpretation: configuration, requested_voice: 'quartz'});
  assert.equal(Object.isFrozen(initial), true); assert.equal(Object.isFrozen(initial.interpretation), true);
  assert.throws(() => {initial.interpretation.target = 'en';}, TypeError);
  await h.advance(255000); await h.ready();
  const offers = h.calls.filter(row => row.path.endsWith('/offer'));
  assert.equal(offers.length, 2);
  for (const offer of offers) {assert.equal(offer.body.voice, 'quartz'); assert.deepEqual(offer.body.interpretation, configuration);}
  assert.deepEqual(h.controller.snapshot().configurations.map(row => row.window), [2], 'empty prior windows do not accumulate');
  await h.controller.stop(); h.controller.dispose();
});

test('answer must match the requested built-in voice before remote SDP or media activation', async () => {
  for (const voice of [undefined, null, 'gleam', 'custom_voice', {quartz: null}]) {
    const h = harness();
    h.setOverride((path, _options, body) => {
      if (path.endsWith('/offer')) return new Response(JSON.stringify({generation: 1, request_id: body.request_id, requested_voice: voice,
        session: {id: 'one'}, transport: {type: 'webrtc', sdp: 'v=0\r\nanswer'}, duration_seconds: 300, ducking: policy}), {headers: {'Content-Type': 'application/json'}});
      if (path.endsWith('/stop')) return new Response(JSON.stringify({...h.status(), state: 'closed', request_id: body.request_id, requested_voice: 'quartz', closure_confirmed: true}), {headers: {'Content-Type': 'application/json'}});
    });
    await h.start('tab', 'quartz');
    assert.equal(h.peers[0].remote, undefined); assert.equal(h.peers[0].input.enabled, false);
    assert.equal(h.controller.snapshot().failure.stage, 'remote_answer'); assert.equal(h.controller.snapshot().phase, 'closed');
    assert.equal(h.calls.filter(row => row.path.endsWith('/offer')).length, 1); h.controller.dispose();
  }
});

test('owned status cannot omit or change its voice; only validated pre-admission cancellation may omit it', async () => {
  for (const voice of [undefined, 'gleam', null, {quartz: null}]) {
    const h = harness(); await h.start('tab', 'quartz'); await h.ready();
    h.setOverride(path => path.endsWith('/heartbeat') ? new Response(JSON.stringify({...h.status(), requested_voice: voice}), {headers: {'Content-Type': 'application/json'}}) : undefined);
    await h.advance(5000);
    assert.equal(h.controller.snapshot().failure.stage, 'lease_renewal'); assert.equal(h.peers[0].input.enabled, false);
    assert.equal(h.controller.snapshot().phase, 'closed'); h.controller.dispose();
  }
  const h = harness();
  assert.equal(parsePrivateStatus(h.status(), 1).requested_voice, undefined, 'unrelated ready status remains compatible');
  for (const voice of [null, 'custom_voice', {marin: null}]) assert.throws(() => parsePrivateStatus({...h.status(), requested_voice: voice}, 1));
  assert.throws(() => parsePrivateStatus({...h.status(), closure_basis: 'cancelled_before_admission'}, 1));
  h.controller.dispose();
});

test('Stop and new Start keep exact original fragments with their own language, source and requested voice in v2 export', async () => {
  const h = harness(); await h.start('tab', 'quartz'); await h.ready();
  h.peers[0].event({type: 'session.input_transcript.delta', delta: 'Original source ', start_ms: 100, end_ms: 200});
  h.peers[0].event({type: 'session.output_transcript.delta', delta: '訂正は25です。', start_ms: 150, end_ms: 260});
  const original = h.controller.snapshot().entries; const originalBytes = JSON.stringify(original);
  await h.controller.stop(); await h.controller.refresh();
  await h.controller.start('microphone', {...configuration, target: 'en'}, true, 279167, 'meridian'); await h.ready();
  h.peers[1].event({type: 'session.input_transcript.delta', delta: '別の発言', start_ms: 0, end_ms: 100});
  const view = h.controller.snapshot(), exported = exportPrivateObservations(view);
  assert.equal(exported.schema_version, 'private-live-observations.v2'); assert.equal(exported.fragments, view.entries);
  assert.deepEqual(exported.configurations.map(row => [row.window, row.source, row.interpretation.target, row.requested_voice]), [[1, 'tab', 'ja', 'quartz'], [2, 'microphone', 'en', 'meridian']]);
  assert.equal(exported.fragments[0], original[0]); assert.equal(exported.fragments[1], original[1]); assert.equal(JSON.stringify(original), originalBytes);
  assert.equal(Object.isFrozen(exported), true); assert.equal(Object.isFrozen(exported.configurations), true);
  assert.match(exported.configuration_meaning, /effective provider voice are not verified/);
  const roundTrip = JSON.parse(JSON.stringify(exported)); assert.deepEqual(roundTrip.fragments, view.entries);
  for (let i = 0; i < 2000; i++) h.peers[1].event({type: 'session.input_transcript.delta', delta: ' bounded', start_ms: i, end_ms: i + 1});
  assert.deepEqual(h.controller.snapshot().configurations.map(row => row.window), [2]);
  assert.equal(exported.configurations.length, 2, 'earlier exported snapshot remains immutable after eviction');
  await h.controller.stop(); assert.equal(h.controller.snapshot().configurations.length, 1);
  h.controller.dispose(); assert.equal(h.controller.snapshot().configurations.length, 0);
});

test('configuration retention cannot accumulate empty windows or omit a retained fragment window', () => {
  const configs = Object.freeze(Array.from({length: 2001}, (_, n) => Object.freeze({window: n + 1, model: 'gpt-live-1', requested_voice: 'marin', source: 'tab', interpretation: Object.freeze({...configuration})})));
  const entries = [{window: 1}, {window: 1999}];
  const retained = retainPrivateConfigurations(configs, entries, 2001);
  assert.deepEqual(retained.map(row => row.window), [1, 1999, 2001]); assert.equal(retained[0], configs[0]); assert.equal(Object.isFrozen(retained), true);
  assert.equal(configs.length, 2001);
});

test('canceling a new voice picker preserves the previous closed receipt and permits a later explicit Start', async () => {
  const h = harness(); await h.start('tab', 'quartz'); await h.ready();
  h.peers[0].event({type: 'session.input_transcript.delta', delta: 'Retained original.', start_ms: 0, end_ms: 100});
  await h.controller.stop(); await h.controller.refresh();
  const prior = h.controller.snapshot().status, configuration = h.controller.snapshot().configurations[0];
  const picker = deferred(); h.setSelection(picker.promise);
  const next = h.controller.start('microphone', {schema_version: 'interpretation.v1', mode: 'auto_to_target', target: 'en'}, true, 279167, 'gleam');
  await h.controller.stop(); picker.reject(new Error('synthetic picker canceled')); await next; await flush();
  assert.equal(h.controller.snapshot().phase, 'closed');
  assert.equal(h.controller.snapshot().status, prior); assert.equal(prior.requested_voice, 'quartz');
  assert.equal(h.controller.snapshot().configurations[0], configuration);
  assert.equal(h.calls.filter(row => row.path.endsWith('/offer')).length, 1);
  await h.controller.refresh(); assert.equal(h.controller.snapshot().available, true);
  h.setSelection(undefined); await h.controller.start('microphone', {schema_version: 'interpretation.v1', mode: 'auto_to_target', target: 'en'}, true, 279167, 'meridian');
  assert.equal(h.calls.filter(row => row.path.endsWith('/offer')).at(-1).body.voice, 'meridian');
  await h.controller.stop(); h.controller.dispose();
});

test('private tab selection refuses missing audio, non-tab surfaces and unverified suppression before admission', async () => {
  for (const change of [
    tab => {tab.tracks.shift();},
    tab => {tab.getVideoTracks()[0].getSettings = () => ({displaySurface: 'window'});},
    tab => {tab.getVideoTracks()[0].readyState = 'ended';},
    tab => {tab.getAudioTracks()[0].getSettings = () => ({});},
  ]) {
    const h = harness(), tab = meetingTab();
    change(tab); h.setSelection(Promise.resolve(tab));
    await h.start('tab');
    assert.equal(h.controller.snapshot().failure?.stage, 'audio_selection');
    assert.equal(h.peers.length, 0);
    assert.equal(h.calls.some(row => row.path.endsWith('/offer')), false);
    assert.ok(tab.getTracks().every(track => track.stops === 1));
    h.controller.dispose();
  }
});

test('pending audio selection gives guidance for the selected source without opening a provider', async () => {
  for (const source of ['microphone', 'tab']) {
    const h = harness(), chooser = deferred(); h.setSelection(chooser.promise); await h.controller.refresh();
    const start = h.controller.start(source, configuration, true, 279167);
    assert.equal(h.controller.snapshot().phase, 'selecting');
    assert.match(h.controller.snapshot().message, source === 'microphone' ? /Allow microphone access in your browser/ : /Choose the browser tab and share its audio/);
    assert.match(h.controller.snapshot().message, /Nothing is sent during selection/);
    assert.equal(h.calls.some(row => row.path.endsWith('/offer')), false);
    await h.controller.stop(); chooser.resolve(new Stream()); await start; h.controller.dispose();
  }
});

test('late picker after Stop is discarded; no provider request can appear', async () => {
  const h = harness(), chooser = deferred(); h.setSelection(chooser.promise); await h.controller.refresh();
  const start = h.controller.start('tab', configuration, true, 279167); await h.controller.stop();
  assert.equal(h.controller.snapshot().phase, 'unconfirmed'); assert.equal(h.claimed(), true);
  await h.controller.start('tab', configuration, true, 279167); assert.equal(h.streams.length, 1);
  const stream = new Stream(); chooser.resolve(stream); await start;
  assert.equal(stream.getTracks()[0].stops, 1); assert.equal(h.peers.length, 0); assert.equal(h.calls.some(row => row.path.endsWith('/offer')), false);
  h.controller.dispose();
});

test('Stop during an admitted but unanswered offer silences locally and closes exact request without replay', async () => {
  const h = harness(), offer = deferred(); h.setOverride(path => {
    if (path.endsWith('/offer')) return offer.promise;
    if (path.endsWith('/stop') || path.endsWith('/status') && h.calls.some(row => row.path.endsWith('/stop'))) return new Response(JSON.stringify({...h.status(), state: 'closed', request_id: h.calls.find(row => row.path.endsWith('/offer')).body.request_id, requested_voice: 'marin', closure_confirmed: true}), {headers: {'Content-Type': 'application/json'}});
  });
  await h.controller.refresh(); const start = h.controller.start('tab', configuration, true, 279167); await flush();
  await h.controller.stop(); assert.equal(h.peers[0].connectionState, 'closed'); assert.equal(h.streams[0].getTracks()[0].readyState, 'ended');
  offer.resolve(new Response(JSON.stringify({transport: {type: 'webrtc', sdp: 'late'}}), {headers: {'Content-Type': 'application/json'}})); await start;
  assert.equal(h.peers[0].remote, undefined); assert.equal(h.controller.snapshot().phase, 'closed'); h.controller.dispose();
});

test('Stop before admission accepts only its exact cancellation receipt without waiting for a nonexistent owner', async () => {
  const h = harness(), offer = deferred(); h.setOverride((path, _options, body) => {
    if (path.endsWith('/offer')) return offer.promise;
    if (path.endsWith('/stop')) return new Response(JSON.stringify({...h.status(), state: 'closed', request_id: body.request_id, closure_confirmed: true, closure_basis: 'cancelled_before_admission', usage_confirmed: false, usage_seconds: null}), {headers: {'Content-Type': 'application/json'}});
  });
  await h.controller.refresh(); const start = h.controller.start('tab', configuration, true, 279167); await flush();
  await h.controller.stop(); assert.equal(h.controller.snapshot().phase, 'closed'); assert.equal(h.claimed(), false);
  assert.equal(h.controller.snapshot().status.usage_confirmed, false);
  assert.equal(h.calls.filter(row => row.path.endsWith('/status')).length, 1);
  offer.resolve(new Response('{}', {status: 409, headers: {'Content-Type': 'application/json'}})); await start; h.controller.dispose();
});

test('late playback creation after Stop is disposed; source and translation never regain authority', async () => {
  const h = harness(), gate = deferred(); h.setMixGate(gate.promise); await h.start('microphone');
  const peer = h.peers[0]; peer.connectionState = 'connected'; peer.event({type: 'session.started'}); peer.ontrack({track: new Track('translation')});
  await h.controller.stop(); assert.equal(peer.input.enabled, false); gate.resolve(); await flush();
  assert.equal(h.mixes[0].disposed, 1); assert.equal(h.controller.snapshot().phase, 'closed'); h.controller.dispose();
});

test('WebRTC deltas become literal paragraphs while original audit fragments and interval order stay intact', async () => {
  const h = harness(); await h.start(); await h.ready();
  const peer = h.peers[0];
  ['ノー', 'トは', '20', 'ドル', 'です。'].forEach((delta, i) => peer.event({type: 'session.output_transcript.delta', delta, start_ms: (4 - i) * 5, end_ms: 30}));
  const entries = h.controller.snapshot().entries;
  assert.deepEqual(entries.map(row => row.text), ['ノー', 'トは', '20', 'ドル', 'です。']);
  assert.equal(groupCaptionEntries(entries)[0].text, 'ノートは20ドルです。');
  assert.equal(Object.isFrozen(entries), true); assert.equal(Object.isFrozen(entries[0]), true);
  await h.controller.stop(); assert.equal(h.controller.snapshot().entries.length, 5);
  h.controller.dispose(); assert.equal(h.controller.snapshot().entries.length, 0);
});

test('overlapping input and output captions leave full-duplex input and translated playback active', async () => {
  for (const source of ['microphone', 'tab']) {
    const h = harness(); await h.start(source); await h.ready();
    const peer = h.peers[0], mix = h.mixes[0], original = h.streams[0].getAudioTracks()[0];
    const resumed = mix.resumed, suspended = mix.suspended, calls = h.calls.length;
    const observations = [
      ['session.output_transcript.delta', '価格は', 0, 500],
      ['session.input_transcript.delta', 'Actually, ', 300, 600],
      ['session.output_transcript.delta', '20ドル', 400, 900],
      ['session.input_transcript.delta', 'twenty-five dollars.', 550, 1300],
      ['session.output_transcript.delta', '訂正します。25ドルです。', 1000, 2000],
    ];
    for (const [type, delta, start_ms, end_ms] of observations) {
      peer.event({type, delta, start_ms, end_ms});
      assert.equal(h.controller.snapshot().phase, 'listening');
      assert.equal(peer.input.enabled, true, 'input remains continuous while generated speech is arriving');
      assert.equal(original.enabled, true, 'caption activity does not alter the borrowed source');
      assert.equal(peer.input.readyState, 'live'); assert.equal(original.readyState, 'live');
      assert.equal(mix.suspended, suspended, 'source text is not a playback-cutoff signal');
      assert.equal(mix.resumed, resumed, 'captions do not create new playback turns');
      assert.equal(mix.disposed, 0);
    }
    assert.deepEqual(h.controller.snapshot().entries.map(row => row.text), observations.map(row => row[1]));
    assert.deepEqual(h.controller.snapshot().entries.map(row => row.provenance), ['gpt_live_output', 'gpt_live_input', 'gpt_live_output', 'gpt_live_input', 'gpt_live_output']);
    assert.equal(h.calls.length, calls, 'interleaved captions issue no additional provider control request');
    await h.controller.stop(); h.controller.dispose();
  }
});

test('bounded caption retention marks discarded words and rejects malformed event timestamps', async () => {
  const h = harness(); await h.start(); await h.ready(); const peer = h.peers[0];
  for (let i = 0; i < 2100; i++) peer.event({type: 'session.input_transcript.delta', delta: ' word', start_ms: i, end_ms: i + 1});
  assert.equal(h.controller.snapshot().entries.length, 2000); assert.equal(h.controller.snapshot().discarded, 100);
  peer.event({type: 'session.input_transcript.delta', delta: 'invalid', start_ms: -1, end_ms: 4}); await flush();
  assert.equal(peer.input.enabled, false); assert.equal(h.controller.snapshot().entries.at(-1).text, ' word'); h.controller.dispose();
});

test('renewal waits for exact predecessor closure, uses new request and never replays transition capture', async () => {
  const h = harness(); await h.start(); await h.ready(); const first = h.peers[0], original = h.streams[0].getTracks()[0];
  await h.advance(255000); await flush();
  assert.equal(h.peers.length, 2); assert.equal(first.connectionState, 'closed'); assert.equal(original.readyState, 'live');
  assert.equal(h.mixes.length, 1); assert.equal(h.mixes[0].disposed, 0); assert.equal(h.contexts.length, 1);
  assert.equal(h.peers[1].input.enabled, false); assert.equal(h.controller.snapshot().gaps, 1);
  const offers = h.calls.filter(row => row.path.endsWith('/offer')); assert.notEqual(offers[0].body.request_id, offers[1].body.request_id);
  await h.ready(); assert.equal(h.controller.snapshot().phase, 'listening'); await h.controller.stop(); h.controller.dispose();
});

test('expected predecessor teardown cannot stop renewal or reenable transition input; successor faults remain terminal', async () => {
  for (const fault of ['peer', 'channel', 'provider']) {
    const h = harness(), closing = deferred(); await h.start(); await h.ready();
    const first = h.peers[0], mix = h.mixes[0], firstStatus = h.status();
    h.setOverride((path, options, body) => path.endsWith('/stop') && body.request_id === firstStatus.request_id ? closing.promise : undefined);
    await h.advance(255000);
    assert.equal(h.controller.snapshot().phase, 'renewing');
    assert.equal(first.input.enabled, false);
    assert.equal(mix.translated, null, 'detach before remote track-ended can be treated as a playback fault');
    const resumed = mix.resumed;
    first.event({type: 'session.output_transcript.delta', delta: 'old final words', start_ms: 100, end_ms: 101});
    assert.equal(first.input.enabled, false, 'late transcript must not reactivate retiring input');
    assert.equal(mix.resumed, resumed);
    first.connectionState = 'closed'; first.onconnectionstatechange(); first.channel.onclose(); first.event({type: 'session.closed'});
    assert.equal(h.controller.snapshot().phase, 'renewing');
    assert.equal(h.calls.filter(row => row.path.endsWith('/stop')).length, 1);
    assert.equal(h.peers.length, 1, 'new offer still waits for authoritative closure');
    closing.resolve(new Response(JSON.stringify({...firstStatus, state: 'closed', closure_confirmed: true, usage_confirmed: true, usage_seconds: 3.5}), {headers: {'Content-Type': 'application/json'}}));
    await flush(); assert.equal(h.peers.length, 2); const successor = h.peers[1];
    assert.equal(h.controller.snapshot().phase, 'renewing');
    if (fault === 'peer') {successor.connectionState = 'failed'; successor.onconnectionstatechange();}
    else if (fault === 'channel') successor.channel.onclose();
    else successor.event({type: 'session.closed'});
    await flush();
    assert.equal(h.controller.snapshot().phase, 'closed', `successor ${fault} failure cannot inherit predecessor closure exemption`);
    assert.equal(successor.input.enabled, false); assert.equal(h.streams[0].getTracks()[0].readyState, 'ended');
    h.controller.dispose();
  }
});

test('availability reports an existing unclosed provider connection without claiming or resuming it', async () => {
  for (const state of ['connected', 'busy', 'closure_uncertain']) {
    const h = harness(); h.setState(state); await h.controller.refresh();
    const view = h.controller.snapshot();
    assert.equal(view.phase, 'idle'); assert.equal(view.available, false); assert.equal(view.status.closure_confirmed, false);
    assert.match(view.message, /may still incur cost/); assert.doesNotMatch(view.message, /Ready for private listening/);
    await h.controller.start('tab', configuration, true, 279167);
    assert.equal(h.streams.length, 0); assert.equal(h.claimed(), false); h.controller.dispose();
  }
  const h = harness(); assert.equal(h.controller.snapshot().status, null); assert.match(h.controller.snapshot().message, /not been verified/);
  await h.controller.refresh(); assert.equal(h.controller.snapshot().status.closure_confirmed, true);
  h.setOverride(() => Promise.reject(new Error('connection unavailable'))); await h.controller.refresh();
  assert.equal(h.controller.snapshot().status, null); assert.equal(h.controller.snapshot().available, false);
  assert.match(h.controller.snapshot().message, /cost is unknown/); h.controller.dispose();
});

test('status exposes only fixed backend issue codes and keeps absent diagnostics null', () => {
  const h = harness();
  assert.equal(parsePrivateStatus(h.status(), 1).issue, null);
  assert.equal(parsePrivateStatus({...h.status(), issue: null}, 1).issue, null);
  assert.equal(parsePrivateStatus({...h.status(), issue: 'sideband_unavailable'}, 1).issue, 'sideband_unavailable');
  for (const issue of ['raw provider failure with secret', 'unknown_issue', 3, {}]) assert.throws(() => parsePrivateStatus({...h.status(), issue}, 1));
  h.controller.dispose();
});

test('first setup HTTP failure remains separate from confirmed Stop and later backend cleanup diagnostics', async () => {
  const h = harness();
  h.setOverride((path, _options, body) => {
    if (path.endsWith('/offer')) return new Response('{"detail":"untrusted provider secret and transcript"}', {status: 409, headers: {'Content-Type': 'application/json'}});
    if (path.endsWith('/stop')) return new Response(JSON.stringify({...h.status(), state: 'closed', request_id: body.request_id, closure_confirmed: true, closure_basis: 'cancelled_before_admission', usage_confirmed: false, usage_seconds: null, issue: 'start_cancelled'}), {headers: {'Content-Type': 'application/json'}});
  });
  await h.start();
  const failure = h.controller.snapshot().failure;
  assert.deepEqual(failure, {stage: 'service_admission', message: 'The local service could not confirm admission of the private speech connection.', httpStatus: 409});
  assert.equal(Object.isFrozen(failure), true);
  assert.equal(h.controller.snapshot().phase, 'closed');
  assert.match(h.controller.snapshot().message, /provider closure confirmed/);
  assert.equal(h.controller.snapshot().status.issue, 'start_cancelled');
  assert.doesNotMatch(JSON.stringify(h.controller.snapshot()), /untrusted provider|secret and transcript/);
  await h.controller.stop(); await h.controller.refresh();
  assert.equal(h.controller.snapshot().failure, failure);
  h.setOverride(() => undefined); await h.start();
  assert.equal(h.controller.snapshot().failure, null);
  await h.controller.stop(); assert.equal(h.controller.snapshot().failure, null); h.controller.dispose();
});

test('local setup failures expose their fixed stage and never the original browser error', async () => {
  const h = harness(); h.owner.connect = async () => {throw new Error('sensitive browser exception');};
  await h.start();
  assert.deepEqual(h.controller.snapshot().failure, {stage: 'operator_connection', message: 'The local operator connection could not be established.', httpStatus: null});
  assert.equal(h.controller.snapshot().phase, 'closed');
  assert.doesNotMatch(JSON.stringify(h.controller.snapshot()), /sensitive browser/);
  assert.equal(h.calls.some(call => call.path.endsWith('/offer')), false);
  h.controller.dispose();
});

test('HTTP authorization failure is retained before owner loss, while explicit Stop creates no failure', async () => {
  const h = harness();
  h.setOverride((path, _options, body) => {
    if (path.endsWith('/offer')) return new Response('do not expose', {status: 403});
    if (path.endsWith('/stop')) return new Response(JSON.stringify({...h.status(), state: 'closed', request_id: body.request_id, closure_confirmed: true, closure_basis: 'cancelled_before_admission', usage_confirmed: false, usage_seconds: null}), {headers: {'Content-Type': 'application/json'}});
  });
  await h.start(); await flush();
  assert.equal(h.controller.snapshot().failure.stage, 'service_admission');
  assert.equal(h.controller.snapshot().failure.httpStatus, 403);
  assert.equal(h.controller.snapshot().phase, 'closed'); h.controller.dispose();
  const normal = harness(); await normal.start(); await normal.ready(); await normal.controller.stop();
  assert.equal(normal.controller.snapshot().failure, null); normal.controller.dispose();
});

test('unconfirmed closure prevents new paid admission and never claims zero future cost', async () => {
  const h = harness(); await h.start(); await h.ready();
  h.setOverride(path => path.endsWith('/stop') || path.endsWith('/status') ? new Response(JSON.stringify({...h.status(), state: 'closure_uncertain', closure_confirmed: false}), {headers: {'Content-Type': 'application/json'}}) : undefined);
  await h.controller.stop(); assert.equal(h.controller.snapshot().phase, 'unconfirmed'); assert.equal(h.claimed(), true); assert.match(h.controller.snapshot().message, /cost is unknown/);
  await h.controller.start('tab', configuration, true, 279167); assert.equal(h.peers.length, 1); h.controller.dispose();
});

test('operator authority loss and dispose immediately silence every owned media resource', async () => {
  const h = harness(); await h.start(); await h.ready(); h.owner.authorityLost();
  assert.equal(h.peers[0].input.enabled, false); assert.ok(h.mixes[0].suspended >= 1); assert.equal(h.streams[0].getTracks()[0].readyState, 'ended');
  await flush(); h.controller.dispose(); assert.equal(h.clocks.size, 0);
});

test('server playback policy and generation must match exactly', () => {
  const h = harness(); assert.equal(parsePrivateStatus(h.status(), 1).duration_seconds, 300);
  assert.throws(() => parsePrivateStatus({...h.status(), generation: 2}, 1));
  assert.throws(() => parsePrivateStatus({...h.status(), ducking: {...policy, ducked_gain: 1}}, 1));
  assert.throws(() => parsePrivateStatus({...h.status(), allocated_microusd: -1}, 1)); h.controller.dispose();
});

test('reentrant Stop at selecting cannot open a native chooser or audio context', async () => {
  const h = harness(); await h.controller.refresh();
  h.controller.subscribe(() => {if (h.controller.snapshot().phase === 'selecting') void h.controller.stop();});
  await h.controller.start('tab', configuration, true, 279167); await flush();
  assert.equal(h.streams.length, 0); assert.equal(h.contexts.length, 0); assert.equal(h.calls.some(row => row.path.endsWith('/offer')), false); h.controller.dispose();
});

test('original playback begins before provider response and remains one graph through interpretation', async () => {
  const h = harness(), gate = deferred(); h.setOverride(path => path.endsWith('/offer') ? gate.promise : undefined);
  await h.controller.refresh(); const start = h.controller.start('tab', configuration, true, 279167); await flush();
  assert.equal(h.mixes.length, 1); assert.equal(h.mixes[0].options.original, h.streams[0]); assert.equal(h.mixes[0].options.translated, null);
  const requestId = h.calls.find(row => row.path.endsWith('/offer')).body.request_id;
  gate.resolve(new Response(JSON.stringify({generation: 1, request_id: requestId, requested_voice: 'marin', session: {id: 'one'}, transport: {type: 'webrtc', sdp: 'v=0\r\nanswer'}, duration_seconds: 300, ducking: policy}), {headers: {'Content-Type': 'application/json'}}));
  await start; await h.ready(); assert.equal(h.mixes.length, 1); assert.equal(h.mixes[0].translated.getAudioTracks()[0].id, 'translation'); h.controller.dispose();
});

test('duplicate output tracks are refused while microphone playback creation is pending', async () => {
  const h = harness(), gate = deferred(); h.setMixGate(gate.promise); await h.start('microphone');
  const peer = h.peers[0]; peer.connectionState = 'connected'; peer.event({type: 'session.started'});
  peer.ontrack({track: new Track('one')}); peer.ontrack({track: new Track('two')});
  assert.equal(h.mixes.length, 1); assert.equal(peer.input.enabled, false); gate.resolve(); await flush();
  assert.equal(h.mixes[0].disposed, 1); h.controller.dispose();
});

test('one failing native close does not skip remaining cleanup or remote Stop', async () => {
  const h = harness(); await h.start(); await h.ready();
  h.peers[0].channel.close = () => {throw new Error('native close failed');};
  await h.controller.stop(); assert.equal(h.streams[0].getTracks()[0].readyState, 'ended'); assert.equal(h.mixes[0].disposed, 1);
  assert.ok(h.calls.some(row => row.path.endsWith('/stop'))); assert.equal(h.controller.snapshot().phase, 'unconfirmed');
  await h.controller.stop(); assert.equal(h.controller.snapshot().phase, 'unconfirmed'); h.controller.dispose();
});

test('negative server status retires local media even if peer connection still says connected', async () => {
  const h = harness(); await h.start(); await h.ready(); h.setState('closing'); await h.advance(5000);
  assert.equal(h.peers[0].input.enabled, false); assert.equal(h.streams[0].getTracks()[0].readyState, 'ended'); h.controller.dispose();
});

test('late predecessor heartbeat cannot stop a renewed connection', async () => {
  const h = harness(), old = deferred(); await h.start(); await h.ready();
  const oldStatus = h.status(); h.setOverride(path => path.endsWith('/heartbeat') ? old.promise : undefined);
  await h.advance(5000); await h.advance(250000); assert.equal(h.peers.length, 2); await h.ready();
  old.resolve(new Response(JSON.stringify(oldStatus), {headers: {'Content-Type': 'application/json'}})); await flush();
  assert.equal(h.controller.snapshot().phase, 'listening'); assert.equal(h.peers[1].input.enabled, true); h.controller.dispose();
});

test('incomplete ICE prevents admission until final candidates exist; Stop cancels gathering', async () => {
  for (const stop of [false, true]) {
    const h = harness(); await h.controller.refresh();
    h.controller.subscribe(() => {if (h.controller.snapshot().phase === 'connecting') h.peers.at(-1).iceGatheringState = 'gathering';});
    const start = h.controller.start('tab', configuration, true, 279167); await flush();
    assert.equal(h.calls.some(row => row.path.endsWith('/offer')), false);
    if (stop) {await h.controller.stop(); await start; assert.equal(h.calls.some(row => row.path.endsWith('/offer')), false);}
    else {
      h.peers[0].localDescription.sdp += 'a=candidate:synthetic\r\n'; h.peers[0].iceGatheringState = 'complete'; h.peers[0].dispatchEvent(new Event('icegatheringstatechange'));
      await start; assert.match(h.calls.find(row => row.path.endsWith('/offer')).body.sdp, /a=candidate:synthetic/);
    }
    h.controller.dispose();
  }
});

test('reentrant Stop notifications share one closure operation', async () => {
  const h = harness(); await h.start(); await h.ready();
  h.controller.subscribe(() => {if (h.controller.snapshot().phase === 'closing') void h.controller.stop();});
  await h.controller.stop(); assert.equal(h.calls.filter(row => row.path.endsWith('/stop')).length, 1); h.controller.dispose();
});

test('an aborted request returning a late authorization failure cannot revoke a newer browser action', async () => {
  const h = harness(), gate = deferred(); h.setOverride(path => path.endsWith('/offer') ? gate.promise : undefined);
  await h.controller.refresh(); const start = h.controller.start('tab', configuration, true, 279167); await flush();
  await h.controller.stop(); gate.resolve(new Response('{}', {status: 401, headers: {'Content-Type': 'application/json'}})); await start;
  assert.equal(h.owner.current(), true); h.controller.dispose();
});

test('RTP observation keeps only numeric inbound audio evidence and cannot authorize playback', async () => {
  const h = harness(); await h.start(); const peer = h.peers[0];
  peer.getStats = async () => new Map([
    ['audio-one', {type: 'inbound-rtp', kind: 'audio', packetsReceived: 10, totalAudioEnergy: .2, id: 'private-track', deviceLabel: 'private microphone', address: '192.0.2.1'}],
    ['audio-two', {type: 'inbound-rtp', mediaType: 'audio', packetsReceived: 5, audioLevel: 0}],
    ['outbound', {type: 'outbound-rtp', kind: 'audio', packetsReceived: 9000, totalAudioEnergy: 10}],
    ['video', {type: 'inbound-rtp', kind: 'video', packetsReceived: 9000, audioLevel: 1}],
    ['invalid', {type: 'inbound-rtp', kind: 'audio', packetsReceived: '20', audioLevel: Infinity, totalAudioEnergy: NaN}],
  ]);
  await h.advance(1000);
  assert.deepEqual(h.controller.snapshot().audioEvidence, {...emptyAudioEvidence, receivedPackets: 15, receivedSignal: true, status: 'observed', transport: UNAVAILABLE_TRANSPORT_EVIDENCE});
  assert.equal(Object.isFrozen(h.controller.snapshot().audioEvidence), true);
  assert.doesNotMatch(JSON.stringify(h.controller.snapshot().audioEvidence), /private|192\.0\.2|audio-one/);
  assert.equal(peer.input.enabled, false); assert.equal(h.controller.snapshot().phase, 'connecting');
  peer.getStats = async () => new Map([['audio', {type: 'inbound-rtp', kind: 'audio', packetsReceived: 0, totalAudioEnergy: 0}]]);
  await h.advance(1000); assert.equal(h.controller.snapshot().audioEvidence.receivedSignal, true);
  await h.controller.stop(); assert.equal(h.controller.snapshot().audioEvidence.receivedPackets, 15); h.controller.dispose();
});

test('invalid numeric RTC stats do not become signal or packet evidence and counts stay bounded', async () => {
  const h = harness(); await h.start(); const peer = h.peers[0];
  peer.getStats = async () => new Map([
    ['one', {type: 'inbound-rtp', kind: 'audio', packetsReceived: -1, audioLevel: 2, totalAudioEnergy: Infinity}],
    ['two', {type: 'inbound-rtp', kind: 'audio', packetsReceived: .5, audioLevel: '1', totalAudioEnergy: '5'}],
    ['three', {type: 'inbound-rtp', kind: 'audio', packetsReceived: Number.MAX_SAFE_INTEGER + 1, audioLevel: NaN, totalAudioEnergy: -1}],
  ]);
  await h.advance(1000); assert.deepEqual(h.controller.snapshot().audioEvidence, {...emptyAudioEvidence, transport: UNAVAILABLE_TRANSPORT_EVIDENCE});
  peer.getStats = async () => new Map([
    ['one', {type: 'inbound-rtp', kind: 'audio', packetsReceived: Number.MAX_SAFE_INTEGER}],
    ['two', {type: 'inbound-rtp', kind: 'audio', packetsReceived: 20}],
  ]);
  await h.advance(1000); assert.equal(h.controller.snapshot().audioEvidence.receivedPackets, Number.MAX_SAFE_INTEGER);
  h.controller.dispose();
});

test('RTC polling is singleflight; failed statistics remain diagnostic without stopping speech', async () => {
  const h = harness(), gate = deferred(); await h.start(); await h.ready(); const peer = h.peers[0]; let calls = 0;
  peer.getStats = () => {calls++; return gate.promise;};
  await h.advance(1000); await h.advance(3000); assert.equal(calls, 1);
  gate.reject(new Error('private transport failure')); await flush();
  assert.equal(h.controller.snapshot().audioEvidence.status, 'unavailable');
  assert.equal(h.controller.snapshot().phase, 'listening'); assert.equal(h.controller.snapshot().failure, null);
  assert.doesNotMatch(JSON.stringify(h.controller.snapshot()), /private transport failure/);
  peer.getStats = async () => new Map([['audio', {type: 'inbound-rtp', kind: 'audio', packetsReceived: 4, audioLevel: .4}]]);
  await h.advance(1000); assert.equal(h.controller.snapshot().audioEvidence.status, 'observed');
  h.controller.dispose();
});

test('transport diagnostics use current peer deltas and clear on a stalled report without opening or gating media', async () => {
  const h = harness(), gate = deferred(); await h.start(); await h.ready(); const peer = h.peers[0];
  let timestamp = 1000, emitted = 100, delay = 10;
  const report = () => new Map([
    ['audio', {id: 'audio', type: 'inbound-rtp', kind: 'audio', timestamp, jitterBufferDelay: delay, jitterBufferEmittedCount: emitted, jitter: .012, transportId: 'transport'}],
    ['transport', {id: 'transport', type: 'transport', selectedCandidatePairId: 'pair'}],
    ['pair', {id: 'pair', type: 'candidate-pair', state: 'succeeded', currentRoundTripTime: .08}],
  ]);
  peer.getStats = async () => report(); await h.advance(1000);
  assert.equal(h.controller.snapshot().audioEvidence.transport.jitterBufferDelayMs, null, 'first cumulative observation is no interval estimate');
  timestamp += 1000; emitted += 100; delay += 5; await h.advance(1000);
  assert.deepEqual(h.controller.snapshot().audioEvidence.transport, {status: 'observed', roundTripMs: 80, inboundJitterMs: 12, jitterBufferDelayMs: 50, sampleIntervalMs: 1000});
  let polls = 0; peer.getStats = () => {polls++; return gate.promise;}; await h.advance(1000); await h.advance(2500);
  assert.equal(polls, 1); assert.deepEqual(h.controller.snapshot().audioEvidence.transport, UNAVAILABLE_TRANSPORT_EVIDENCE);
  assert.equal(peer.input.enabled, true); assert.equal(h.controller.snapshot().phase, 'listening'); assert.equal(h.controller.snapshot().failure, null);
  assert.equal(h.calls.filter(call => call.path.endsWith('/offer')).length, 1);
  gate.resolve(report()); await flush(); assert.deepEqual(h.controller.snapshot().audioEvidence.transport, UNAVAILABLE_TRANSPORT_EVIDENCE, 'expired report never republishes a current estimate');
  timestamp += 3500; emitted += 100; delay += 5; peer.getStats = async () => report(); await h.advance(1000);
  assert.equal(h.controller.snapshot().audioEvidence.transport.jitterBufferDelayMs, null, 'recovery needs a new exact baseline');
  await h.controller.stop(); h.controller.dispose();
});

test('transport values survive Stop as last observations but cannot cross actual peer renewal', async () => {
  const h = harness(), gate = deferred(); await h.start(); await h.ready(); const first = h.peers[0];
  const report = jitter => new Map([['audio', {id: 'same-id-in-each-peer', type: 'inbound-rtp', kind: 'audio', jitter}]]);
  first.getStats = async () => report(.02); await h.advance(1000);
  assert.equal(h.controller.snapshot().audioEvidence.transport.inboundJitterMs, 20);
  first.getStats = () => gate.promise; await h.advance(1000); await h.advance(253000); assert.equal(h.peers.length, 2);
  assert.deepEqual(h.controller.snapshot().audioEvidence.transport, EMPTY_TRANSPORT_EVIDENCE);
  gate.resolve(report(30)); await flush(); assert.deepEqual(h.controller.snapshot().audioEvidence.transport, EMPTY_TRANSPORT_EVIDENCE);
  h.peers[1].getStats = async () => report(.005); await h.advance(1000);
  const last = h.controller.snapshot().audioEvidence.transport; assert.equal(last.inboundJitterMs, 5);
  await h.controller.stop(); assert.equal(h.controller.snapshot().audioEvidence.transport, last); h.controller.dispose();
});

test('Stop cancels RTC polling immediately and retains the last observation while late results are ignored', async () => {
  const h = harness(), gate = deferred(); await h.start(); await h.ready(); const peer = h.peers[0]; let calls = 0;
  peer.getStats = async () => new Map([['audio', {type: 'inbound-rtp', kind: 'audio', packetsReceived: 7, audioLevel: 0}]]);
  await h.advance(1000); const observed = h.controller.snapshot().audioEvidence;
  peer.getStats = () => {calls++; return gate.promise;}; await h.advance(1000);
  await h.controller.stop(); assert.equal([...h.clocks].some(timer => timer.repeat && timer.ms === 1000), false);
  gate.resolve(new Map([['audio', {type: 'inbound-rtp', kind: 'audio', packetsReceived: 50, audioLevel: 1}]])); await flush();
  await h.advance(2000); assert.equal(calls, 1); assert.equal(h.controller.snapshot().audioEvidence, observed);
  assert.equal(h.controller.snapshot().phase, 'closed'); h.controller.dispose();
});

test('renewal resets RTC evidence and a late predecessor result cannot alter the successor', async () => {
  const h = harness(), gate = deferred(); await h.start(); await h.ready(); const first = h.peers[0];
  first.getStats = () => gate.promise; await h.advance(1000); await h.advance(254000); assert.equal(h.peers.length, 2);
  assert.deepEqual(h.controller.snapshot().audioEvidence, emptyAudioEvidence);
  gate.resolve(new Map([['audio', {type: 'inbound-rtp', kind: 'audio', packetsReceived: 99, audioLevel: 1}]])); await flush();
  assert.equal(h.controller.snapshot().audioEvidence.receivedPackets, 0);
  h.peers[1].getStats = async () => new Map([['audio', {type: 'inbound-rtp', kind: 'audio', packetsReceived: 2, totalAudioEnergy: 0}]]);
  await h.advance(1000); assert.equal(h.controller.snapshot().audioEvidence.receivedPackets, 2);
  assert.equal(h.controller.snapshot().audioEvidence.receivedSignal, false); h.controller.dispose();
});

test('translation renderer attachment must resolve before activation and late attachment cannot undo Stop', async () => {
  for (const stop of [false, true]) {
    const h = harness(), gate = deferred(); h.setReplacementGate(gate.promise); await h.start();
    const peer = h.peers[0]; peer.ontrack({track: new Track('translation')});
    peer.event({type: 'session.started'}); peer.connectionState = 'connected'; peer.onconnectionstatechange();
    peer.event({type: 'session.input_transcript.delta', delta: 'testing', start_ms: 0, end_ms: 100}); await flush();
    assert.equal(h.controller.snapshot().phase, 'connecting'); assert.equal(h.peers[0].input.enabled, false);
    assert.equal(h.mixes[0].resumed ?? 0, 0);
    if (stop) {await h.controller.stop(); assert.equal(h.controller.snapshot().phase, 'unconfirmed');}
    gate.resolve(); await flush();
    assert.equal(h.controller.snapshot().phase, stop ? 'closed' : 'listening');
    assert.equal(h.peers[0].input.enabled, !stop);
    if (stop) assert.equal(h.mixes[0].resumed ?? 0, 0);
    h.controller.dispose();
  }
});

test('a pending successor renderer cannot reactivate after duplicate-track failure', async () => {
  const h = harness(), gate = deferred(); await h.start(); await h.ready(); await h.advance(255000);
  h.setReplacementGate(gate.promise); await h.ready(); const next = h.peers[1], resumed = h.mixes[0].resumed;
  assert.equal(h.controller.snapshot().phase, 'renewing'); assert.equal(next.input.enabled, false);
  next.ontrack({track: new Track('duplicate')}); await flush();
  gate.resolve(); await flush();
  assert.equal(h.controller.snapshot().phase, 'closed'); assert.equal(next.input.enabled, false);
  assert.equal(h.mixes[0].resumed, resumed); h.controller.dispose();
});

test('graph observations survive Stop without claiming heard output and reset for a successor window', async () => {
  const h = harness(); await h.start(); await h.ready();
  const graph = {rendererReady: true, translatedSignalSeen: true, renderedSignalSeen: false};
  h.mixes[0].options.onState(graph);
  assert.deepEqual(h.controller.snapshot().audioEvidence, {...emptyAudioEvidence, ...graph});
  await h.advance(255000); assert.equal(h.peers.length, 2);
  assert.deepEqual(h.controller.snapshot().audioEvidence, emptyAudioEvidence);
  h.mixes[0].options.onState(graph); assert.deepEqual(h.controller.snapshot().audioEvidence, emptyAudioEvidence, 'no current remote track means no graph ownership');
  await h.ready(); h.mixes[0].options.onState({...graph, renderedSignalSeen: true});
  const observed = h.controller.snapshot().audioEvidence;
  await h.controller.stop(); assert.equal(h.controller.snapshot().audioEvidence, observed);
  h.mixes[0].options.onState({rendererReady: false, translatedSignalSeen: false, renderedSignalSeen: false});
  assert.equal(h.controller.snapshot().audioEvidence, observed, 'teardown and stale graph callbacks cannot erase the final observation');
  assert.equal(h.controller.snapshot().playback, null); h.controller.dispose();
});

test('a failure while retiring the closed predecessor prevents another paid window', async () => {
  const h = harness(); await h.start(); await h.ready();
  h.peers[0].close = () => {throw new Error('native peer cleanup failed');};
  await h.advance(255000); await flush();
  assert.equal(h.peers.length, 1); assert.equal(h.calls.filter(row => row.path.endsWith('/offer')).length, 1);
  assert.equal(h.controller.snapshot().phase, 'unconfirmed');
  assert.equal(h.controller.snapshot().failure.stage, 'renewal_closure');
  assert.equal(h.streams[0].getTracks()[0].readyState, 'ended'); h.controller.dispose();
});
