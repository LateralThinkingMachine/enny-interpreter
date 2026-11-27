import assert from 'node:assert/strict';
import test from 'node:test';
import {createElement} from 'react';
import {renderToStaticMarkup} from 'react-dom/server';
import {App} from '../.test-build/App.js';
import {PilotClient} from '../.test-build/client.js';
import {PrivateListeningPanel} from '../.test-build/webrtc/PrivateListeningPanel.js';
import {CostNotices} from '../.test-build/CostNotices.js';
import {CostPanel} from '../.test-build/CostPanel.js';
import {DEFAULT_DUCKING_SETTINGS} from '../.test-build/audio/ducking.js';
import {budgetFixture} from './budget-fixture.mjs';

const entry = (sequence, text, provenance = 'gpt_live_output', window = 1) => Object.freeze({sequence, text, provenance, window, start_ms: 0, end_ms: 100, participants: Object.freeze([]), model: 'gpt-live-1'});

const activePhases = ['selecting', 'connecting', 'listening', 'renewing', 'closing', 'unconfirmed'];
const defaultStatus = Object.freeze({enabled: true, state: 'ready', generation: 1, closure_confirmed: true, usage_confirmed: false, usage_seconds: null, allocated_microusd: 279167, warning_threshold_microusd: 5_000_000, duration_seconds: 300, allocation_duration_seconds: 335});
function fixture(patch = {}) {
  const view = Object.freeze({phase: 'idle', message: 'Private listening is off. No WebRTC session is open.', available: true, source: 'tab', listeningMode: 'interpretation', status: defaultStatus, settings: DEFAULT_DUCKING_SETTINGS, playback: null, entries: [], configurations: [], discarded: 0, windows: 0, gaps: 0, costNotice: null, ...patch});
  let sideEffects = 0;
  const forbidden = () => {sideEffects++; throw new Error('Rendering must not acquire audio, start providers, export, or change settings');};
  const controller = new Proxy({snapshot: () => view, subscribe: () => forbidden, isActive: () => activePhases.includes(view.phase)}, {get: (target, key) => key in target ? target[key] : forbidden});
  return {controller, view, check: () => assert.equal(sideEffects, 0)};
}
function render(patch = {}, blocked = false) {
  const f = fixture(patch);
  const budget = {continuous: {}, state: "active", pricing_version: "pilot_audio_2026_09_13_v1", routes: [{route: "live", quote_seconds: 300, per_minute_microusd: 50000, can_reserve: true}]};
  const html = renderToStaticMarkup(createElement(PrivateListeningPanel, {controller: f.controller, blocked, budget}));
  f.check();
  return html;
}
function button(html, id) {
  const tag = html.match(new RegExp(`<button\\b[^>]*id="${id}"[^>]*>`))?.[0];
  assert.ok(tag, `Expected ${id} button`);
  return tag;
}
function activity(html) {
  const text = html.match(/<div class="private-listening-state" role="status">(.*?)<\/div>/s)?.[1];
  assert.ok(text, 'The primary activity/cost status must be visible');
  return text;
}
test('the private listening start shows its single live window and the OpenAI payer before consent', () => {
  const html = render();
  const beforeStart = html.split('id="start-private-listening"')[0];
  assert.match(beforeStart, /Estimated US\$0\.28 per 5-minute connection/);
  assert.match(beforeStart, /OpenAI can bill the account that owns your API key/);
  assert.match(beforeStart, /no total spending cap/);
  assert.match(button(html, 'start-private-listening'), /aria-describedby="private-listening-estimate"/);
  assert.match(html, /Start private listening · billed by your own OpenAI account/);
  assert.match(render({status: null}), /Private listening estimate unavailable/);
});

test('private voice offers only built-in choices and setup stays disabled until an active attempt closes', () => {
  const idle = render();
  const selector = idle.match(/<select id="private-listening-voice">(.*?)<\/select>/s)?.[1];
  assert.ok(selector); assert.match(selector, /<option value="marin" selected="">Marin<\/option>/);
  for (const voice of ['Quartz', 'Gleam', 'Meridian']) assert.match(selector, new RegExp(`<option value="${voice.toLowerCase()}">${voice}<\\/option>`));
  assert.match(idle, /not language-specific native-speaker presets/);
  for (const phase of activePhases) {
    const html = render({phase, status: {...defaultStatus, requested_voice: 'quartz'}});
    assert.match(html, /<fieldset class="advanced-controls" disabled=""><legend class="sr-only">Private listening setup/);
    assert.match(html, /Current or last requested voice: quartz/);
    assert.match(html, /not a verified voice-quality result/);
  }
});
test('private setup exposes personal and two-way choices without promising delivery to another participant', () => {
  const html = render();
  assert.match(html, /<option value="auto_to_target" selected="">Personal target language · this device<\/option>/);
  assert.match(html, /<option value="two_way">Two-way language pair · this device<\/option>/);
  assert.match(html, /each participant needs a separately authorized Companion listener on their own device/);
  assert.match(html, /Audio and captions are not sent to other meeting participants/);
  assert.match(html, /Changing a target or language pair does not change what other participants hear/);

  assert.match(html, /selected audio goes to the interpretation provider/);
  assert.match(html, /does not provide remote access/);
  assert.match(html, /id="private-original-words">Original/);
  assert.match(html, /id="private-interpreted-words">Interpretation/);
  assert.match(html, /No original words received in the retained view/);
  assert.match(html, /No interpretation words received in the retained view/);
  assert.doesNotMatch(html, /<a[^>]*href=/, 'No remote-listener link');
});


test('private Start requires fresh agreement and rendering cannot start audio or a provider', () => {
  for (const blocked of [false, true]) {
    const html = render({}, blocked);
    assert.match(button(html, 'start-private-listening'), /disabled=""/);
    const agreement = html.match(/<input type="checkbox"[^>]*>/)?.[0];
    assert.ok(agreement);
    assert.doesNotMatch(agreement, /checked/);
    assert.match(html, /Everyone agrees to AI audio processing/);
    assert.match(html, /selected tab has no shared AI interpreter speaking/);
    assert.match(activity(html), /Private interpretation off/);
    assert.doesNotMatch(activity(html), /paid connection open/);
    assert.doesNotMatch(html, /<audio|<video|type="password"|OPENAI_API_KEY/);
  }
});

test('paid listening and connection setup stay distinct from muted headphone playback', () => {
  const html = render({phase: 'listening', message: 'Silence and muted playback do not stop cost.', settings: {...DEFAULT_DUCKING_SETTINGS, originalMuted: true, translatedMuted: true}, status: {...defaultStatus, state: 'connected', closure_confirmed: false}});
  assert.match(activity(html), /Interpretation on · paid connection open/);
  assert.match(activity(html), /muted playback do not stop cost/);
  assert.doesNotMatch(button(html, 'stop-private-listening'), /disabled/);
  assert.match(html, /aria-pressed="true">Unmute original/);
  assert.match(html, /aria-pressed="true">Unmute interpretation/);
  assert.match(html, /Muting does not stop processing or cost/);
  assert.match(html, /Final usage is not confirmed/);
  for (const phase of ['connecting', 'renewing']) {
    const setup = render({phase, status: {...defaultStatus, closure_confirmed: false}});
    assert.match(activity(setup), /Connecting · may incur cost/);
    assert.doesNotMatch(activity(setup), /Interpretation on|Private interpretation off/);
    assert.doesNotMatch(button(setup, 'stop-private-listening'), /disabled/);
  }
});

const privatePlayback = {selectiveDuckingAvailable: true, status: 'playing', ducking: true};
test('original-only private mode keeps captions and cost visible while original volume remains available', () => {
  const html = render({phase: 'listening', listeningMode: 'original_only', playback: {...privatePlayback, status: 'translation_suspended', ducking: false},
    status: {...defaultStatus, state: 'connected', closure_confirmed: false}, entries: [entry(1, 'Delivered original words.', 'gpt_live_input'), entry(2, '届いた解釈。')]});
  assert.match(activity(html), /Interpretation off for me · paid connection open/);
  assert.doesNotMatch(activity(html), /Private interpretation off|zero cost|processing stopped|no cost/);
  assert.match(button(html, 'private-hear-original'), /aria-pressed="true"/);
  assert.match(button(html, 'private-hear-interpretation'), /aria-pressed="false"/);
  assert.match(html, /Original audio returns to your selected volume; original mute still applies/);
  assert.match(html, /Live captions continue in either mode/);
  assert.match(html, /GPT-Live-1 remains running and billable until you select Stop private listening/);
  assert.match(html, /speech already present in the tab audio remains audible/);
  assert.match(html, /Delivered original words/); assert.match(html, /届いた解釈/);
  assert.doesNotMatch(html.match(/<input id="private-original-volume"[^>]*>/)?.[0] ?? '', /disabled/);
  assert.match(html.match(/<input id="private-interpretation-volume"[^>]*>/)?.[0] ?? '', /disabled=""/);
  assert.doesNotMatch(button(html, 'stop-private-listening'), /disabled/);
});

test('original-only controls require the active separate tab graph, not microphone or failed/retained playback', () => {
  for (const phase of ['connecting', 'listening', 'renewing']) {
    assert.match(render({phase, playback: privatePlayback}), /id="private-hear-original"/);
  }
  for (const patch of [{phase: 'listening', playback: null}, {phase: 'listening', playback: {...privatePlayback, selectiveDuckingAvailable: false}},
    {phase: 'listening', source: 'microphone', playback: privatePlayback},
    ...['idle', 'selecting', 'closing', 'closed', 'failed', 'unconfirmed'].map(phase => ({phase, playback: privatePlayback})),
    ...['failed', 'disposed'].map(status => ({phase: 'listening', playback: {...privatePlayback, status}}))]) {
    assert.doesNotMatch(render(patch), /id="private-hear-original"/);
  }
});

test('a retained off preference allows explicit microphone recovery without claiming original microphone monitoring', () => {
  const html = render({phase: 'listening', source: 'microphone', listeningMode: 'original_only', playback: {...privatePlayback, selectiveDuckingAvailable: false}});
  assert.doesNotMatch(html, /id="private-hear-original"/);
  assert.match(html, /Your microphone is not played back/);
  assert.match(html, /Live captions and paid GPT-Live-1 processing continue until Stop/);
  assert.doesNotMatch(button(html, 'private-enable-microphone-interpretation'), /disabled/);
  assert.match(html.match(/<input id="private-original-volume"[^>]*>/)?.[0] ?? '', /disabled=""/);
});

test('legacy cost displays mark retained totals unverified after private listener release until fresh budget arrives', () => {
  const view = {...new PilotClient('operator', {}).snapshot(), role: 'operator', budgetFresh: false, budget: budgetFixture({continuous: {warning_threshold_microusd: 5_000_000, allocated_microusd: 0, windows_started: 0}}), session: {continuous: true, providers_enabled: true}};
  const notices = renderToStaticMarkup(createElement(CostNotices, {view}));
  const panel = renderToStaticMarkup(createElement(CostPanel, {view, client: {}}));
  assert.match(notices, /current total unverified/); assert.match(notices, /does not show current processing or cost closure/);
  assert.doesNotMatch(notices, /Next notice at|Estimated speech allocations since/);
  assert.match(panel, /Last observed speech allocations · current total unverified/);
  assert.equal(renderToStaticMarkup(createElement(CostNotices, {view: {...view, browserListening: true}})), '');
});

test('unconfirmed closure offers only closure checking and cannot claim zero ongoing cost', () => {
  const html = render({phase: 'unconfirmed', available: false, message: 'Audio is stopped locally. Provider closure or device cleanup remains unconfirmed; additional cost is unknown.', status: {...defaultStatus, state: 'closure_uncertain', closure_confirmed: false, allocated_microusd: null}});
  assert.match(activity(html), /Audio stopped · checking cost closure/);
  assert.match(activity(html), /additional cost is unknown/);
  assert.doesNotMatch(activity(html), /Private interpretation off|No WebRTC session is open|no cost|zero cost/i);
  assert.match(html, />Check closure again<\/button>/);
  assert.doesNotMatch(button(html, 'stop-private-listening'), /disabled/);
  assert.doesNotMatch(html, /id="start-private-listening"/);
  assert.match(html, /Local allocation total unavailable/);
});

test('confirmed off state retains previous costs and labels estimates separately from final provider usage', () => {
  const html = render({phase: 'closed', message: 'Private listening is off · provider closure confirmed. Earlier usage may still be billed.', status: {...defaultStatus, state: 'closed', usage_seconds: 12.5, usage_confirmed: true}, gaps: 2});
  assert.match(activity(html), /Private interpretation off/);
  assert.match(activity(html), /Earlier usage may still be billed/);
  assert.match(html, /US\$0\.28 estimated allocations since service start/);
  assert.match(html, /not invoice charges/);
  assert.match(html, /Provider observed 12\.5 seconds/);
  assert.match(html, /Final usage confirmed by the provider/);
  assert.match(html, /2 connection-transition gaps/);
  assert.match(button(html, 'start-private-listening'), /disabled=""/, 'SSR never inherits agreement from a previous run');
});

test('reload cannot describe an unconfirmed existing provider connection as off', () => {
  for (const state of ['connecting', 'connected', 'closing', 'closure_uncertain', 'busy']) {
    const html = render({phase: 'idle', available: false, message: 'An existing connection has not confirmed closure.', status: {...defaultStatus, state, closure_confirmed: false}});
    assert.match(activity(html), /Another connection may still incur cost/);
    assert.doesNotMatch(activity(html), /Private interpretation off/);
    assert.match(button(html, 'start-private-listening'), /disabled/);
  }
  assert.match(activity(render({status: null, available: false})), /Connection and cost status unverified/);
});

test('startup explanation stays visible beside confirmed closure', () => {
  const html = render({phase: 'closed', message: 'Private listening is off · provider closure confirmed.', failure: {stage: 'provider_admission', message: 'The local service could not admit private listening.', httpStatus: 409}, status: {...defaultStatus, state: 'closed'}});
  assert.match(activity(html), /provider closure confirmed/);
  assert.match(html, /role="alert">The local service could not admit private listening/);
  assert.match(html, /provider_admission/);
  assert.match(html, /local HTTP 409/);
  assert.match(html, /failure is retained separately from provider closure/);
});

test('live paragraphs join literal fragments without rewriting audit text or claiming complete source ASR', () => {
  const entries = Object.freeze([
    entry(1, 'ノー'), entry(2, 'トは'), entry(3, '20'), entry(4, 'ドル'), entry(5, 'です。'),
    entry(6, 'The blue ', 'gpt_live_input'), entry(7, 'notebook costs', 'gpt_live_input'),
    entry(8, '<script>untrusted</script>', 'gpt_live_output', 2),
  ]);
  const before = JSON.stringify(entries);
  const html = render({phase: 'listening', entries, discarded: 3, windows: 2, gaps: 1});
  assert.match(html, /<span class="private-caption-body">ノートは20ドルです。<\/span><span class="private-caption-tail"><\/span>/);
  assert.match(html, /<span class="private-caption-body">The blue notebook costs<\/span><span class="private-caption-tail"><\/span>/);
  assert.doesNotMatch(html, /<p>The blue notebook costs[.!?]/);
  for (const text of ['ノー', 'トは', '20', 'ドル', 'です。', 'The blue ', 'notebook costs']) assert.ok(html.includes(`<pre>${text}</pre>`), `Exact audit fragment ${text}`);
  assert.match(html, /&lt;script&gt;untrusted&lt;\/script&gt;/);
  assert.doesNotMatch(html, /<script>/);
  assert.match(html, /Original speech · unverified/);
  const originalLane = html.match(/<section class="private-caption-lane original"[^>]*>(.*?)<\/section>/s)?.[1];
  const translatedLane = html.match(/<section class="private-caption-lane interpretation"[^>]*>(.*?)<\/section>/s)?.[1];
  assert.ok(originalLane); assert.ok(translatedLane);
  assert.match(originalLane, /The blue notebook costs/); assert.doesNotMatch(originalLane, /ノートは20ドルです。/);
  assert.match(translatedLane, /ノートは20ドルです。/); assert.doesNotMatch(translatedLane, /The blue notebook costs/);
  assert.match(html, /not aligned sentence pairs or verified speaker turns/);
  assert.match(html, /unfinished speech stays unfinished/);
  assert.match(html, /not final source-ASR captions/);
  assert.match(html, /3 older raw fragments discarded/);
  assert.match(html, />Export original observations<\/button>/);
  assert.equal(JSON.stringify(entries), before);
});

function renderApp(browserListening, role = 'operator') {
  const f = fixture({phase: 'listening', message: 'Private sentinel audio content.', entries: [entry(1, 'private sentinel caption')], status: {...defaultStatus, state: 'connected', closure_confirmed: false}});
  const initial = new PilotClient(role, {}).snapshot();
  const view = {...initial, role, browserListening, message: 'Synthetic render fixture', connected: true, budget: budgetFixture(), budgetFresh: true,
    session: {role, csrf: 'synthetic', generation: 1, stopped: false, providers_enabled: true, expires_in_seconds: null, continuous: true, repeatable_sessions: true,
      capabilities: {source_captions: true, private_translation: true, live_assistant: false, live_interpreter: true, prepared_public_speech: false}, limitations: ['Synthetic fixture']}};
  let sideEffects = 0;
  const forbidden = () => {sideEffects++; throw new Error('Operator rendering must be inert');};
  const client = new Proxy({privateListening: f.controller, snapshot: () => view, subscribe: () => forbidden}, {get: (target, key) => key in target ? target[key] : forbidden});
  const html = renderToStaticMarkup(createElement(App, {client}));
  assert.equal(sideEffects, 0); f.check();
  return html;
}

test('Operator keeps Stop available and disables competing capture/routes while private browser listening owns audio', () => {
  const html = renderApp(true);
  assert.match(html, /<fieldset[^>]*disabled=""[^>]*hidden=""[^>]*><legend class="sr-only">Source captions and existing interpretation workflow/);
  assert.match(html, /Private listening → Connection and cost details/);
  assert.doesNotMatch(html, /aria-label="Recurring cost notices"|aria-label="Continuous spending estimates"/);
  for (const legend of ['Source captions and existing interpretation workflow', 'Manual audio controls', 'Assistant context controls']) {
    assert.match(html, new RegExp(`<fieldset[^>]*disabled=""[^>]*><legend class="sr-only">${legend}</legend>`));
  }
  const globalStop = html.match(/<button[^>]*>■ Stop pilot<\/button>/)?.[0];
  assert.ok(globalStop); assert.doesNotMatch(globalStop, /disabled/);
  assert.doesNotMatch(button(html, 'stop-private-listening'), /disabled/);
  assert.match(button(html, 'notes-finish-export'), /disabled=""/, 'Legacy Finish cannot interrupt private browser ownership through a conflicting workflow');
  const idle = renderApp(false);
  assert.doesNotMatch(idle, /<fieldset[^>]*disabled=""[^>]*><legend class="sr-only">Manual audio controls<\/legend>/);
});

test('public screen cannot disclose private WebRTC words, sound settings, or usage', () => {
  const html = renderApp(true, 'public');
  assert.match(html, /Approved public screen/);
  for (const text of ['private sentinel caption', 'Private sentinel audio content.', 'Your private interpreter', 'start-private-listening', 'stop-private-listening', 'US$0.28']) assert.ok(!html.includes(text), `Private content leaked: ${text}`);
});

test('private audio details distinguish incoming packets, renderer preparation and gain-stage signals from hearing', () => {
  const retained = {receivedPackets: 25, receivedSignal: true, status: 'observed', rendererReady: true, translatedSignalSeen: true, renderedSignalSeen: false};
  const html = render({phase: 'closed', audioEvidence: retained});
  assert.match(html, /Current or last connection observations/); assert.match(html, /25 · browser reported/);
  assert.match(html, /Incoming audio energy<\/dt><dd>Observed/);
  assert.match(html, /Browser audio renderer<\/dt><dd>Prepared/);
  assert.match(html, /Translated signal reached the mixer<\/dt><dd>Observed/);
  assert.match(html, /Translated signal after playback gain<\/dt><dd>No positive signal observed/);
  assert.match(html, /do not confirm that you heard sound/); assert.match(html, /do not establish audible playback/);
  assert.doesNotMatch(html, /You heard|Playback confirmed|Audio successfully played/);
  const unavailable = render({audioEvidence: {...retained, receivedPackets: 0, receivedSignal: false, status: 'unavailable', rendererReady: null, translatedSignalSeen: null, renderedSignalSeen: null}});
  assert.match(unavailable, /0 · latest browser report unavailable/); assert.match(unavailable, /Browser audio renderer<\/dt><dd>Not observed/);
});

test('private transport details report observed component estimates without inventing end-to-end delay', () => {
  const transport = {status: 'observed', roundTripMs: 41.125, inboundJitterMs: 2, jitterBufferDelayMs: 18.75, sampleIntervalMs: 1000};
  const audioEvidence = {receivedPackets: 25, receivedSignal: true, status: 'observed', rendererReady: true, translatedSignalSeen: true, renderedSignalSeen: true, transport};
  const html = render({phase: 'closed', audioEvidence});
  assert.match(html, /Current or last browser transport estimates/);
  for (const [label, value] of [['Network round-trip time', '41.1'], ['Incoming audio jitter', '2.0'], ['Mean jitter-buffer delay', '18.8'], ['Jitter-buffer observation interval', '1000.0']]) {
    assert.ok(html.includes(`${label}</dt><dd>${value} ms`));
  }
  assert.match(html, /not model processing or end-to-end speech delay/);
  assert.match(html, /Round-trip time is not one-way delay/);
  assert.match(html, /None of these observations confirms another participant heard interpretation/);
  const missing = render({audioEvidence: {...audioEvidence, transport: {status: 'unavailable', roundTripMs: null, inboundJitterMs: null, jitterBufferDelayMs: null, sampleIntervalMs: null}}});
  assert.match(missing, /latest estimates unavailable/);
  assert.match(missing, /Network round-trip time<\/dt><dd>Unavailable/);
  assert.doesNotMatch(missing, /Network round-trip time<\/dt><dd>0/);
});

test('WebRTC caption reader takes the active window only and releases its display on closing', () => {
  const config = {window: 2, source: 'tab', model: 'gpt-live-1', requested_voice: 'marin',
    interpretation: {schema_version: 'interpretation.v1', mode: 'auto_to_target', target: 'en'}};
  const state = {windows: 2, phase: 'listening', configurations: [config],
    entries: [entry(1, 'old-original', 'gpt_live_input'), entry(2, 'current-generated', 'gpt_live_output', 2)]};
  const reader = render(state).match(/<section class="caption-reader".*?<\/section>/s)?.[0];
  assert.ok(reader);
  assert.match(reader, /Original speech · unverified/);
  assert.match(reader, /Waiting for original words/);
  assert.match(reader, /current-generated/);
  assert.doesNotMatch(reader, /old-original/);
  for (const phase of ['selecting', 'renewing', 'closing', 'closed', 'unconfirmed', 'failed']) {
    assert.doesNotMatch(render({...state, phase}), /aria-label="Caption reader"/);
  }
});
