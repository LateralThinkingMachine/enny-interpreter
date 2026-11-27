import assert from "node:assert/strict";
import test from "node:test";
import {createElement} from "react";
import {renderToStaticMarkup} from "react-dom/server";
import {App} from "../.test-build/App.js";
import {PilotClient} from "../.test-build/client.js";
import {SessionTransition} from "../.test-build/SessionTransition.js";
import {budgetFixture, disabledBudget, geminiBudget} from "./budget-fixture.mjs";

const route = (route_id, live_mode, source = "remote_mix") => ({route_id, source, route: "live", live_mode, language: "hi", state: "ready", code: null, expires_in_seconds: 60});
const fragment = (key, live_mode, provenance = "live_output") => ({key, route_id: `former_${key}`, source: "remote_mix", route: "live", live_mode, language: "hi", provenance, text: `private generated ${key}`, final: false, item: null});
const session = (overrides = {}) => ({role: "operator", csrf: "synthetic", generation: 1, stopped: false, providers_enabled: true, expires_in_seconds: 7200, capabilities: {source_captions: true, private_translation: true, live_assistant: true, live_interpreter: true, prepared_public_speech: false}, limitations: ["Synthetic fixture, no devices or provider"], ...overrides});
function render(overrides = {}, appProps = {}) {
  // Rendering must not call a port or mutate application state. All positive
  // methods intentionally fail if a component invokes them while rendering.
  let sideEffects = 0;
  const fail = () => {sideEffects++; throw new Error("Unexpected render side effect");};
  const initial = new PilotClient("operator", {}).snapshot();
  const view = {...initial, message: "Synthetic render fixture", session: session(), budget: budgetFixture(), budgetFresh: true, connected: true, ...overrides};
  const client = new Proxy({privateListening: null, snapshot: () => view, subscribe: () => fail}, {get(target, property) {return property in target ? target[property] : fail;}});
  const html = renderToStaticMarkup(createElement(App, {...appProps, client}));
  assert.equal(sideEffects, 0);
  return html;
}

test("operator renders explicit model choices, independent sound controls and an inert Live interpretation cost plan", () => {
  const html = render();
  assert.match(html, /Dedicated translation · gpt-realtime-translate/);
  assert.match(html, /GPT-Live-1 · interpreter or assistant/);
  assert.match(html, /value="live_interpretation"/);
  assert.match(html, /Enable GPT-Live-1 interpreter sound/);
  assert.match(html, /Enable GPT-Live-1 assistant sound/);
  assert.match(html, /Enable dedicated translation sound/);
  assert.match(html, /Compare a meeting plan · starts nothing/);
  assert.match(html, /not measured charges or an account-wide billing cap/);
  assert.doesNotMatch(html, /type="password"|name="OPENAI_API_KEY"/);
});

test("typed-context selector lists only assistant routes even alongside a ready interpreter", () => {
  const html = render({routes: [route("interpreter_id", "interpretation"), route("assistant_id", "assistant", "self")]});
  const select = html.match(/<select id="live-route"[^>]*>(.*?)<\/select>/s)?.[1];
  assert.ok(select); assert.match(select, /value="assistant_id"/); assert.doesNotMatch(select, /interpreter_id/);
  assert.match(html, /GPT-Live-1 interpreter<\/strong>/);
  assert.match(html, /Typed questions and context go only to assistant routes/);
  const interpreterOnly = render({routes: [route("interpreter_id", "interpretation")]});
  assert.match(interpreterOnly, /No ready assistant route/);
  assert.match(interpreterOnly, /<button type="submit" disabled="">Send privately<\/button>/);
});

test("retained generated fragments preserve their exact mode and Live input is not labeled source ASR", () => {
  const html = render({fragments: [fragment(1, "interpretation"), fragment(2, "assistant"), fragment(3, "interpretation", "live_input")], routes: []});
  assert.match(html, /GPT-Live-1 interpreter · generated output · requested Hindi/);
  assert.match(html, /GPT-Live-1 assistant · generated output · requested Hindi/);
  assert.match(html, /GPT-Live-1 interpreter · input transcript · not source ASR/);
  assert.match(html, /Unverified generated output/);
});

test("public rendering omits private interpretation, assistant selectors and cost metadata", () => {
  const html = render({role: "public", routes: [route("private_route", "interpretation")], fragments: [fragment(1, "interpretation")], notes: "private typed notes"});
  for (const privateValue of ["private generated 1", "private typed notes", "private_route", 'id="live-route"', 'id="budget-state"', "Approved allowance"]) assert.ok(!html.includes(privateValue));
  assert.match(html, /Only a new statement|new statement requires a new|No approved statement yet/);
  assert.match(html, /Approved public screen/); assert.doesNotMatch(html, /Keep this page private/);
});

const button = (html, id) => {
  const markup = html.match(new RegExp(`<button id="${id}"[^>]*>`))?.[0];
  assert.ok(markup, `Expected button ${id}`);
  return markup;
};
const readyPreview = {meetPhase: "preview", captureConfirmed: true, meetSignalSeen: true, captureStatus: {remote_mix: {level: 0.2, frames: 100, muted: false}, self: null}};

function feedbackArticle(html, id) {
  const article = html.match(new RegExp(`<article id="${id}"[^>]*>(.*?)</article>`, "s"))?.[1];
  assert.ok(article, `${id} must be present`);
  return article;
}

test("guided source choice explains own voice and is locked while selected audio is owned", () => {
  const remote = render();
  assert.match(remote, /Open your call in a <strong>browser tab/);
  assert.match(remote, /Select that tab and turn on <strong>Share tab audio/);
  assert.doesNotMatch(remote, /Google Meet|\bMeet tab\b/);
  assert.match(remote, /The meeting tab captures <strong>other people’s voices/);
  assert.match(remote, /To interpret your own voice, choose <strong>My microphone/);
  const self = render({meetSource: "self"});
  assert.match(self, /<option value="self" selected="">My microphone · my voice/);
  assert.match(self, /Press Start, allow microphone access, then speak/);
  assert.match(self, /Your meeting app keeps its own mute setting/);
  assert.doesNotMatch(self.match(/<select id="meet-source"[^>]*>/)[0], /disabled/);
  for (const state of [{meetPhase: "preview"}, {meetSourceLocked: true}, {busy: true}, {finishing: true}, {aiPaused: true}]) {
    const html = render({meetSource: "self", ...state});
    assert.match(html.match(/<select id="meet-source"[^>]*>/)[0], /disabled/);
  }
});

test("unresolved native selection offers cleanup without another device or AI start", () => {
  const html = render({meetSource: "self", meetSourceLocked: true, meetPhase: "idle"});
  assert.match(button(html, "start-meet"), /disabled/);
  assert.match(button(html, "choose-meet-tab"), /disabled/);
  assert.doesNotMatch(button(html, "retry-meet-release"), /disabled/);
  assert.match(html, /Dismiss any open browser permission or sharing prompt/);
  assert.match(button(render({meetSource: "self", meetSourceLocked: true, busy: true}), "retry-meet-release"), /disabled/);
  assert.doesNotMatch(render({meetSource: "self", meetSourceLocked: true, aiPaused: true}), /id="retry-meet-release"/);
});

test("visible microphone feedback separates sound, local send, source words and generated text", () => {
  const base = {meetSource: "self", meetPhase: "listening", captureStatus: {remote_mix: null, self: {level: .3, frames: 50, muted: false}}, sentFrames: {remote_mix: 70, self: 0}};
  const sound = render(base);
  assert.ok(sound.indexOf('id="meet-feedback"') < sound.indexOf('<details class="meet-details">'));
  assert.match(sound, /Microphone audio detected/);
  assert.match(sound, /Audio not sent yet/);
  assert.match(sound, /Captions waiting/);
  assert.match(sound, /Your original words will appear here/);
  const draft = {source: "self", text: "これはテストです。<script>unsafe</script>", retired: false, route_id: "captions", item_id: "one", revision: 1};
  const received = render({...base, sentFrames: {remote_mix: 70, self: 1}, sourceDrafts: [draft], fragments: [{...fragment(1, "interpretation"), source: "self", language: "en", text: "This is a test."}]});
  assert.match(received, /Audio sent to local service this session/);
  assert.match(received, /Captions received this session/);
  assert.match(received, /Interpretation received this session/);
  assert.match(feedbackArticle(received, "meet-caption-preview"), /これはテストです。&lt;script&gt;unsafe&lt;\/script&gt;/);
  assert.match(feedbackArticle(received, "meet-caption-preview"), /Live draft · may change · not yet confirmed/);
  assert.match(feedbackArticle(received, "meet-interpretation-preview"), /This is a test/);
  assert.doesNotMatch(feedbackArticle(received, "meet-caption-preview"), /aria-live|role="status"|<script>/);
});

test("speech feedback never promotes another source, Live input or another output language", () => {
  const html = render({meetSource: "self", meetPhase: "listening", sourceDrafts: [{source: "remote_mix", text: "other-source-draft", retired: false}],
    sourceFragments: [{...fragment(1, null), source: "remote_mix", route: "transcription", provenance: "source_asr", text: "other-source-caption"}],
    fragments: [{...fragment(2, "interpretation", "live_input"), source: "self", language: "en", text: "live-input-only"}, {...fragment(3, "interpretation"), source: "self", language: "hi", text: "wrong-output-language"}]});
  const original = feedbackArticle(html, "meet-caption-preview"), translated = feedbackArticle(html, "meet-interpretation-preview");
  assert.match(original, /Your original words will appear here/);
  assert.match(translated, /Your translation will appear here/);
  assert.doesNotMatch(original + translated, /other-source|live-input-only|wrong-output-language/);
});

test("paused feedback zeros stale levels and labels retained text while previews stay bounded", () => {
  const text = "前".repeat(900) + "最後";
  const base = {meetSource: "self", meetPhase: "paused", aiPaused: true, captureStatus: {remote_mix: null, self: {level: .9, frames: 1000, muted: false}}, sentFrames: {self: 80, remote_mix: 0},
    sourceFragments: [{...fragment(1, null), source: "self", route: "transcription", provenance: "source_asr", text, incomplete: true}]};
  const html = render(base);
  assert.match(html, /<meter id="meet-audio-level" min="0" max="1" value="0"/);
  assert.match(html, /Processing is inactive. Any text below is retained/);
  assert.match(html, /Captions received this session/);
  const original = feedbackArticle(html, "meet-caption-preview");
  assert.match(original, /incomplete at interruption/);
  assert.match(original, /…前+最後/);
  assert.doesNotMatch(original, new RegExp("前".repeat(900)));
  const recovering = render({...base, aiPaused: false, meetPhase: "recovering"});
  assert.match(recovering, /text may be from before the interruption/);
  assert.doesNotMatch(render({...base, stopped: true}), /id="meet-feedback"/);
  assert.doesNotMatch(render({...base, role: "public", session: session({role: "public"})}), /id="meet-feedback"/);
});

test("first-run Meet has one actionable Start with visible agreement and optional checks collapsed", () => {
  const html = render({connected: false});
  assert.match(html, /Interpret your meeting/);
  assert.match(html, /<select id="meet-language"[^>]*><option value="en" selected="">English/);
  assert.equal((html.match(/id="capture-agreement"/g) ?? []).length, 1);
  assert.match(button(html, "choose-meet-tab"), /disabled=""/);
  assert.doesNotMatch(button(html, "start-meet"), /disabled/);
  assert.match(button(html, "start-meet"), /aria-describedby="meet-start-estimate meet-start-agreement meet-start-help"/);
  assert.match(html, /By starting, I confirm everyone has agreed to sending this audio to OpenAI/);
  assert.match(html, /Interpretation will play through my headphones/);
  assert.match(html, /Start live interpretation · billed by your own OpenAI account<\/button>/);
  const beforeStart = html.split('id="start-meet"')[0];
  assert.match(beforeStart, /Estimated US\$0\.67 for up to 10 min of captions and interpretation/);
  assert.match(beforeStart, /Enny charges no fee/);
  assert.match(beforeStart, /OpenAI can bill the account that owns your API key/);
  assert.match(beforeStart, /Your local estimate limit is US\$20\.00/);
  assert.ok(html.indexOf('id="start-meet"') < html.indexOf('id="capture-agreement"'));
  for (const summary of ["Audio &amp; connection details", "Advanced audio &amp; manual routes", "Cost &amp; limits", "Public presentation", "Private assistant context"]) {
    assert.match(html, new RegExp(`<details[^>]*><summary>${summary}</summary>`));
  }
  assert.doesNotMatch(html, /<details[^>]* open/);
  assert.ok(html.indexOf('id="meet-heading"') < html.indexOf("Private captions</h2>"));
  assert.ok(html.indexOf("Private captions</h2>") < html.indexOf("Advanced audio &amp; manual routes</summary>"));
  assert.match(html, /id="finish-export"/);
  assert.match(html, /■ Stop pilot/);
});

test("provider-disabled preview is available without a stream connection and clearly cannot start AI", () => {
  const free = {connected: false, session: session({providers_enabled: false}), budget: disabledBudget()};
  const initial = render(free);
  assert.match(initial, /Free rehearsal · US\$0\.00/);
  assert.match(initial, /Start free rehearsal<\/button>/);
  assert.match(button(initial, "choose-meet-tab"), /aria-describedby="meet-start-estimate"/);
  const html = render({...readyPreview, ...free});
  assert.doesNotMatch(button(html, "choose-meet-tab"), /disabled/);
  assert.doesNotMatch(html, /id="start-meet"|Press Start|By starting, I confirm/);
  assert.match(html, /Live captions and interpretation require a new session approved with your own provider API key/);
  assert.equal((html.match(/id="choose-meet-tab"/g) ?? []).length, 1);
  assert.equal((html.match(/id="capture-agreement"/g) ?? []).length, 1);
  assert.ok(html.indexOf('id="choose-meet-tab"') < html.indexOf('Audio &amp; connection details'));
  assert.match(html, /Everyone agrees to this local audio check/);
  assert.doesNotMatch(html, /no total spending cap/);
  assert.match(html, /Local check · nothing sent/);
  assert.match(html, /Tab audio detected/);
  assert.match(html, /<meter id="meet-audio-level" min="0" max="1" value="0.2"/);
});

test("Start does not require a preview, signal, separate checkbox or manual quote refresh", () => {
  for (const allowed of [
    {}, {meetPhase: "idle", captureConfirmed: false}, {meetSignalSeen: false},
    {captureStatus: {remote_mix: null, self: null}},
    {captureStatus: {remote_mix: {level: 0, frames: 100, muted: false}, self: null}},
    {captureStatus: {remote_mix: {level: 0.005, frames: 100, muted: false}, self: null}},
  ]) assert.doesNotMatch(button(render({...readyPreview, ...allowed}), "start-meet"), /disabled/);
  for (const blocked of [
    {budgetFresh: false}, {budgetRefreshing: true}, {budget: null},
    {budget: budgetFixture({stream_reservations: {used: 15, limit: 16}})},
    {budget: budgetFixture({amounts: {approved_microusd: 500000, reserved_microusd: 0, unallocated_microusd: 500000}})},
    {busy: true}, {finishing: true}, {aiPaused: true}, {capture: {remote_mix: "closed", self: "idle"}},
    {session: session({capabilities: {...session().capabilities, live_interpreter: false}})},
    {session: session({capabilities: {...session().capabilities, source_captions: false}})},
  ]) assert.match(button(render({...readyPreview, ...blocked}), "start-meet"), /disabled/);
  const html = render(readyPreview);
  assert.match(html, /Reserve up to \$0\.67 for both services/);
  assert.match(html, /90:00 remains in this activation/);
  assert.match(html, /Two stream admissions/);
  assert.doesNotMatch(html.split('<div class="workspace-grid">')[0], /Refresh allowance|Start becomes available after/);
});

test("capture, sent audio and received AI output remain distinct observations", () => {
  const missing = render({...readyPreview, meetPhase: "listening", sentFrames: {remote_mix: 4, self: 0}});
  assert.match(missing, /Audio sent/);
  assert.match(missing, /Captions waiting/);
  assert.match(missing, /Interpretation waiting/);
  assert.match(missing, /The meter shows sound, not recognized speech/);
  assert.doesNotMatch(missing, /Local check · nothing sent/);
  const liveInput = render({...readyPreview, meetPhase: "listening", fragments: [fragment(1, "interpretation", "live_input")]});
  assert.match(liveInput, /Interpretation waiting/);
  const delivered = render({...readyPreview, meetPhase: "listening", sourceFragments: [{...fragment(2, null), provenance: "source_asr", route: "transcription"}], fragments: [{...fragment(1, "interpretation"), language: "en"}]});
  assert.match(delivered, /Captions received/);
  assert.match(delivered, /Interpretation received/);
  const spent = render({...readyPreview, meetPhase: "listening", budget: budgetFixture({stream_reservations: {used: 2, limit: 2}})});
  assert.match(spent, /This test is running/);
  assert.doesNotMatch(spent, /Cannot reserve both services/);
});

test("silent and muted previews remain ordinary audio states and processing prevents mixed manual workflows", () => {
  const silent = render({...readyPreview, meetSignalSeen: false, captureStatus: {remote_mix: {level: 0, frames: 100, muted: false}, self: null}});
  assert.match(silent, /Quiet right now/);
  assert.doesNotMatch(button(silent, "start-meet"), /disabled/);
  assert.match(silent, /check Share tab audio, your meeting’s speaker output and whether you chose the right tab/);
  assert.match(silent, /This does not capture your microphone/);
  const muted = render({...readyPreview, captureStatus: {remote_mix: {level: 0, frames: 100, muted: true}, self: null}});
  assert.match(muted, /Waiting for tab audio/);
  for (const meetPhase of ["preview", "connecting", "starting_captions", "starting_interpreter", "listening"]) {
    const html = render({...readyPreview, meetPhase});
    assert.equal((html.match(/<fieldset class="advanced-controls" disabled="">/g) ?? []).length, 3);
    assert.match(html, /Manual controls are paused during meeting setup and processing/);
    assert.doesNotMatch(html.match(/<input id="capture-agreement"[^>]*>/)?.[0] ?? "missing", /disabled/);
  }
  assert.match(silent, /Release selected tab/);
  assert.doesNotMatch(silent, /Choose a different meeting tab|Cancel audio check/);
});

test("manual processing cannot be presented as a no-send preview or start a conflicting guided setup", () => {
  const html = render({captureConfirmed: true, routes: [route("manual", "interpretation")]});
  assert.match(button(html, "choose-meet-tab"), /disabled/);
  assert.match(html, /Manual audio routes are already in use/);
  assert.doesNotMatch(html, /Local check · nothing sent/);
});

test("failed guided setup cannot invite a paid retry and manual route errors use fixed actionable wording", () => {
  const failed = render({...readyPreview, meetPhase: "failed", meetIssue: "The test stopped after its stream limit was reached."});
  assert.match(button(failed, "choose-meet-tab"), /disabled/);
  assert.match(button(failed, "start-meet"), /disabled/);
  assert.match(failed, /The test stopped after its stream limit was reached/);
  assert.match(render({routes: [{...route("failed", "interpretation"), state: "failed", code: "provider_budget"}]}), /A local spending or resource limit was reached, but this record does not identify which one or prove billed spend/);
  const unknown = render({routes: [{...route("failed", "interpretation"), state: "failed", code: "untrusted-provider-detail"}]});
  assert.match(unknown, /The speech route ended or failed/);
  assert.doesNotMatch(unknown, /untrusted-provider-detail/);
});

test("caption rendering remains limited to the latest 100 fragments while reporting the full retained count", () => {
  const sourceFragments = Array.from({length: 101}, (_, index) => ({...fragment(index, null), route: "transcription", provenance: "source_asr", text: `caption-entry-${index}__`}));
  const html = render({sourceFragments});
  assert.doesNotMatch(html, /caption-entry-0__/);
  assert.match(html, /caption-entry-1__/);
  assert.match(html, /caption-entry-100__/);
  assert.equal((html.split('<div class="workspace-grid">')[1].match(/caption-entry-\d+__/g) ?? []).length, 100);
  assert.match(html, /latest 100 of 101 retained source fragments/);
});

test("quiet speech never blocks Start and active listening does not ask the user to start again", () => {
  const html = render({...readyPreview, meetSignalSeen: false, captureStatus: {remote_mix: {level: 0.005, frames: 100, muted: false}, self: null}});
  assert.match(html, /Quiet audio never blocks Start/);
  assert.doesNotMatch(button(html, "start-meet"), /disabled/);
  assert.doesNotMatch(html.split('<div class="workspace-grid">')[0], /too quiet|stronger audio level|Refresh allowance/);
  const quiet = render({...readyPreview, meetPhase: "listening", captureStatus: {remote_mix: {level: 0, frames: 100, muted: false}, self: null}});
  assert.match(quiet, /<strong>Listening<\/strong>/);
  assert.match(quiet, /Waiting for the other person’s speech/);
  assert.match(quiet, /Quiet right now/);
  assert.doesNotMatch(quiet, /id="start-meet"/);
  assert.doesNotMatch(button(quiet, "stop-meet"), /disabled/);
  const recovering = render({...readyPreview, meetPhase: "recovering"});
  assert.match(recovering, /Reconnecting…/);
  assert.match(recovering, /audio during this gap may be missed/);
  assert.doesNotMatch(recovering, /AI paused|id="start-meet"/);
  assert.doesNotMatch(button(recovering, "stop-meet"), /disabled/);
});

test("the stopped page retains server and cleanup uncertainty in its visible global status", () => {
  const html = render({stopped: true, meetPhase: "failed", meetIssue: "Interpretation could not start.", message: "Stopped locally. Server Stop is unconfirmed; keep this page stopped. Some local resource cleanup was unconfirmed. Interpretation could not start."});
  assert.match(html, /class="status stopped" role="status" aria-live="polite">Stopped locally\. Server Stop is unconfirmed/);
  assert.match(html, /Some local resource cleanup was unconfirmed/);
  assert.match(html, /Interpretation could not start/);
  assert.match(html, /class="panel stopped-panel"/);
  assert.doesNotMatch(html, /id="meet-heading"/);
});

test("stopped operator page separates local observation, first server trigger and last issue from billing or closure proof", () => {
  const diagnostics = {version: 1, generation: 2, stopped: true, stop: {code: "operator_disconnected", elapsed_seconds: 180}, last_issue: {code: "text_limit", source: "remote_mix", route: "live", elapsed_seconds: 179}};
  const html = render({stopped: true, diagnostics, diagnosticsState: "ready", terminalMessage: "Audio delivery fell behind.", message: "Stop recorded by the local server."});
  assert.match(html, /Why the test stopped/); assert.match(html, /This page observed:/); assert.match(html, /Audio delivery fell behind/);
  assert.match(html, /operator connection closed or lost its heartbeat/); assert.match(html, /3m 0s after the local process started/);
  assert.match(html, /Last service issue:/); assert.match(html, /text-processing limit was reached/);
  assert.match(html, /not evidence that the dollar allowance was spent/); assert.match(html, /does not confirm provider closure/);
  assert.doesNotMatch(html, /id="start-meet"|id="meet-heading"/);
  const publicHtml = render({role: "public", stopped: true, diagnostics, diagnosticsState: "ready", terminalMessage: "Private local observation"});
  assert.doesNotMatch(publicHtml, /Why the test stopped|text-processing limit|Private local observation|3m 0s/);
});

test("stopped operator UI explains unavailable diagnostics without inventing a deadline or spent allowance", () => {
  const html = render({stopped: true, diagnostics: null, diagnosticsState: "unavailable", terminalMessage: null});
  assert.match(html, /stop reason is unavailable/); assert.match(html, /No reason is inferred/);
  assert.doesNotMatch(html, /approved local session time limit was reached|allowance was spent|Server stop:/);
});

test("guided listening keeps private interpretation mute available outside disabled manual controls", () => {
  for (const muted of [false, true]) {
    const html = render({...readyPreview, meetPhase: "listening", muted: {translation: true, live: true, live_interpretation: muted}});
    const toggle = button(html, "meet-sound-toggle");
    assert.match(toggle, new RegExp(`aria-pressed="${!muted}"`));
    assert.doesNotMatch(toggle, /disabled/);
    assert.match(html, muted ? /Enable interpretation sound/ : /Mute interpretation sound/);
    assert.ok(html.indexOf('id="meet-sound-toggle"') < html.indexOf('class="advanced-controls"'));
    assert.match(html, /Muting discards sound; processing and reservations continue/);
    assert.match(html, /Enabling sound never replays the muted interval/);
  }
  for (const meetPhase of ["idle", "preview", "connecting", "starting_captions", "starting_interpreter", "failed"]) assert.doesNotMatch(render({...readyPreview, meetPhase}), /id="meet-sound-toggle"/);
  assert.match(button(render({...readyPreview, meetPhase: "listening", finishing: true}), "meet-sound-toggle"), /disabled/);
});

test("continuous spending warnings stay visible without a cap or a disabled Start", () => {
  const raw = budgetFixture({amounts: null, remaining_seconds: null, continuous: {warning_threshold_microusd: 1_000_000, allocated_microusd: 3_000_000, windows_started: 4}, stream_reservations: {used: 0, limit: 2}, route_duration_seconds: 300});
  raw.routes = raw.routes.map(row => ({...row, quote_seconds: 300, quote_microusd: row.per_minute_microusd * 5}));
  const html = render({...readyPreview, session: session({continuous: true, expires_in_seconds: null}), budget: raw});
  assert.doesNotMatch(button(html, "start-meet"), /disabled/);
  assert.match(html.split('id="start-meet"')[0], /Estimated US\$0\.34 for each 5 min/);
  assert.match(html, /Runs until you stop/); assert.match(html, /no total spending cap/);
  assert.doesNotMatch(html, /prepare a provider activation with a budget/);
  assert.match(html, /Cost notices every US\$1\.00/);
  assert.match(html, /Next notice at US\$4\.00/);
  assert.match(html, /Estimated speech allocations since this local service started/); assert.doesNotMatch(html, /nulls|undefined|remains in this activation/);
});

test("continuous AI pause retains readable captions and editable notes with export and explicit Stop", () => {
  const html = render({aiPaused: true, meetPhase: "paused", connected: false, session: session({continuous: true, expires_in_seconds: null}), notes: "Keep this draft", sourceDiscarded: 15, renewalCount: 3, sourceFragments: [{...fragment(1, null), route: "transcription", provenance: "source_asr", text: "Retained caption"}], meetIssue: "The service connection closed."});
  assert.match(html, /AI paused · session retained/); assert.match(html, /Retained caption/); assert.match(html, /Keep this draft/);
  assert.match(html, /15 sealed earlier-window items discarded/); assert.match(html, /Transition gaps are included in the export/);
  assert.doesNotMatch(button(html, "finish-export"), /disabled/); assert.match(html, /Export retained captions &amp; notes/);
  assert.match(button(html, "start-meet"), /disabled/); assert.match(button(html, "choose-meet-tab"), /disabled/);
  assert.doesNotMatch(html, /class="panel stopped-panel"/); assert.match(html, /■ Stop pilot/);
});


test("recurring cost milestones are accessible and stay visible when browser notifications are blocked", () => {
  const raw = budgetFixture({amounts: null, remaining_seconds: null, continuous: {warning_threshold_microusd: 5_000_000, allocated_microusd: 11_000_000, windows_started: 70}, stream_reservations: {used: 0, limit: 2}, route_duration_seconds: 300});
  const html = render({...readyPreview, session: session({continuous: true, expires_in_seconds: null}), budget: raw, costNotice: {milestone_microusd: 10_000_000, crossed_intervals: 2}, costNotificationState: "denied"});
  assert.match(html, /Cost notices every US\$5\.00/); assert.match(html, /Next notice at US\$15\.00/);
  assert.match(html, /id="cost-milestone-notice" role="status" aria-live="polite" aria-atomic="true">Estimated speech allocations since this local service started reached US\$10\.00/);
  assert.match(html, /2 warning intervals crossed/); assert.match(html, /not measured account spend/i);
  assert.match(html, /Browser notifications are blocked/); assert.match(html, /No total spending cap/);
  assert.match(html, /Estimates and notice milestones reset when the local service restarts/);
  assert.match(html, /Starting a new session keeps these allocations/);
  assert.match(html, /A new session or page reload starts from the current estimate without replaying old notices/);
  assert.match(html, /Speech allocations since this service started/);
  assert.match(html, /Speech windows since this service started/);
  assert.doesNotMatch(html, /Estimated session allocations|Estimated session cost/);
  assert.match(html, /Notices are amount-only and silent/);
  const publicHtml = render({role: "public", costNotice: {milestone_microusd: 10_000_000, crossed_intervals: 2}, budget: raw});
  assert.doesNotMatch(publicHtml, /Cost notices|cost-milestone-notice|Estimated speech allocations since this local service started|10\.00/);
});


test("retained cost notices do not claim AI remains active during pause or Finish", () => {
  const raw = budgetFixture({amounts: null, remaining_seconds: null, continuous: {warning_threshold_microusd: 5_000_000, allocated_microusd: 5_100_000, windows_started: 30}, stream_reservations: {used: 0, limit: 2}, route_duration_seconds: 300});
  for (const inactive of [{aiPaused: true, meetPhase: "paused"}, {finishing: true}]) {
    const html = render({...inactive, session: session({continuous: true, expires_in_seconds: null}), budget: raw, costNotice: {milestone_microusd: 5_000_000, crossed_intervals: 1}, costNotificationState: "requesting"});
    assert.match(html, /Warnings do not stop AI or cap spending/);
    assert.match(html, /Browser cost notices are closed while AI is paused or finishing/);
    assert.doesNotMatch(html, /Processing continues until you stop|Speech starts independently|speech can start|Next notice at/);
  }
});


test("paused diagnostics show first local observation separately from service, source and time without restarting", () => {
  const diagnostics = {version: 1, generation: 1, stopped: false, stop: null, last_issue: {code: "provider_closed", source: "remote_mix", route: "live", elapsed_seconds: 42}};
  const html = render({aiPaused: true, meetPhase: "paused", terminalMessage: "First local interruption.", meetIssue: "First local interruption.", diagnostics, diagnosticsState: "ready", session: session({continuous: true, expires_in_seconds: null}), notes: "Preserve this note"});
  assert.match(html, /id="pause-reason-heading">Why AI paused/); assert.match(html, /This page observed:<\/strong> First local interruption/);
  assert.match(html, /Last service issue:<\/strong> The speech service connection closed/);
  assert.match(html, /Selected tab · remote mix · GPT-Live-1 · interpreter or assistant; 0m 42s/);
  assert.match(html, /may differ from the first interruption observed on this page/);
  assert.match(html, /has not recorded a global Stop/); assert.match(html, /does not permit a restart/);
  assert.match(html, /Export retained captions and notes before restarting/); assert.match(html, /Generated interpretation is not included/);
  assert.match(html, /Preserve this note/); assert.doesNotMatch(button(html, "finish-export"), /disabled/); assert.match(button(html, "start-meet"), /disabled/);
  assert.equal((html.match(/First local interruption/g) ?? []).length, 1);
  const missing = render({aiPaused: true, meetPhase: "paused", terminalMessage: "First local interruption.", diagnosticsState: "unavailable"});
  assert.match(missing, /Server service details are unavailable/); assert.doesNotMatch(missing, /Last service issue:/);
  const publicHtml = render({role: "public", aiPaused: true, diagnostics, diagnosticsState: "ready", terminalMessage: "First local interruption."});
  assert.doesNotMatch(publicHtml, /Why AI paused|First local interruption|0m 42s|Last service issue:/);
});

test("ASR duration details render exactly and privately only in paused or stopped diagnostics", () => {
  const diagnostics = {version: 1, generation: 1, stopped: false, stop: null, last_issue: {code: "usage_mismatch", source: "remote_mix", route: "transcription", elapsed_seconds: 42, asr_usage: {submitted_ms: 2400, reported_ms: 4001, allowed_ms: 3400}}};
  const paused = {aiPaused: true, meetPhase: "paused", terminalMessage: "First local interruption.", diagnostics, diagnosticsState: "ready", session: session({continuous: true, expires_in_seconds: null})};
  for (const overrides of [paused, {...paused, aiPaused: false, stopped: true}]) {
    const html = render(overrides);
    assert.match(html, /Duration check for this source-caption item/);
    assert.match(html, /<dt>Submitted duration<\/dt><dd>2\.400 seconds<\/dd>/);
    assert.match(html, /<dt>Provider-reported duration<\/dt><dd>4\.001 seconds \(rounded up to a whole millisecond\)<\/dd>/);
    assert.match(html, /<dt>Local allowed upper bound<\/dt><dd>3\.400 seconds<\/dd>/);
    assert.match(html, /extra 1\.000 second is this application’s compatibility policy/);
    assert.match(html, /not documented OpenAI rounding behavior or a record of billed usage/);
    assert.match(html, /This page observed:<\/strong> First local interruption/);
    assert.doesNotMatch(html, /asr_usage|submitted_ms|reported_ms|allowed_ms/);
  }
  const withReported = reported_ms => ({...paused, diagnostics: {...diagnostics, last_issue: {...diagnostics.last_issue, asr_usage: {...diagnostics.last_issue.asr_usage, reported_ms}}}});
  assert.match(render(withReported(Number.MAX_SAFE_INTEGER)), /9007199254740\.991 seconds \(rounded up to a whole millisecond\)/);
  const zeroSubmitted = {...paused, diagnostics: {...diagnostics, last_issue: {...diagnostics.last_issue, asr_usage: {submitted_ms: 0, reported_ms: 1001, allowed_ms: 1000}}}};
  assert.match(render(zeroSubmitted), /<dt>Submitted duration<\/dt><dd>0\.000 seconds/);
  const beyond = render(withReported(null));
  assert.match(beyond, /Provider-reported duration<\/dt><dd>Beyond the supported numeric range/);
  assert.doesNotMatch(beyond, /NaN|Infinity|undefined/);
  for (const overrides of [{...paused, role: "public"}, {...paused, session: null}, {...paused, aiPaused: false, meetPhase: "listening"}]) {
    assert.doesNotMatch(render(overrides), /Duration check for this source-caption item|4\.001 seconds|Submitted duration|Local allowed upper bound/);
  }
  const oldRecord = {...diagnostics, last_issue: {code: "usage_mismatch", source: "remote_mix", route: "transcription", elapsed_seconds: 42}};
  assert.doesNotMatch(render({...paused, diagnostics: oldRecord}), /Duration check for this source-caption item|Submitted duration/);
});

const nextSession = (overrides = {}) => ({available: true, creating: false, error: null, create() {throw new Error("Rendering must never create a session");}, ...overrides});

test("stopped operator offers an explicit fresh session with terminal diagnostics collapsed", () => {
  const html = render({stopped: true, stopAcknowledged: true, message: "Stop recorded by the local server. Start a new session when ready."}, {sessionActions: nextSession()});
  assert.match(html, /Session ended/);
  assert.match(html, /aria-labelledby="stopped-heading"/);
  assert.match(html, /<h1 id="stopped-heading" tabindex="-1">Session ended\.<\/h1>/);
  assert.doesNotMatch(html, /autofocus/);
  assert.doesNotMatch(button(html, "new-session"), /disabled/);
  assert.match(html, /Your language choice stays the same/);
  assert.match(html, /<details class="meet-details session-diagnostics-details"><summary>Session details<\/summary>/);
  assert.doesNotMatch(html, /Retry server Stop|id="start-meet"|This page cannot resume itself/);
  const repeatable = render({stopped: true, message: "Session ended. Start a new session when you are ready.", diagnosticsState: "unavailable"}, {sessionActions: nextSession()});
  assert.match(repeatable, /Retry server Stop/); // Display text alone cannot acknowledge Stop.
  const uncertain = render({stopped: true, message: "Stopped locally. Server Stop is unconfirmed."}, {sessionActions: nextSession()});
  assert.match(uncertain, /<button class="quiet">Retry server Stop<\/button>/);
  assert.doesNotMatch(button(uncertain, "new-session"), /disabled/);
  const pending = render({stopped: true}, {sessionActions: nextSession({creating: true})});
  assert.match(button(pending, "new-session"), /disabled/);
  assert.match(pending, /Preparing new session…/);
});

test("fresh session controls require host availability and authenticated operator context", () => {
  for (const [overrides, actions] of [
    [{stopped: true, role: "public"}, nextSession()],
    [{stopped: true, session: null}, nextSession()],
    [{stopped: true}, nextSession({available: false})],
    [{stopped: true}, undefined],
    [{meetPhase: "listening"}, nextSession()],
  ]) assert.doesNotMatch(render(overrides, {sessionActions: actions}), /id="new-session"|id="export-new-session"|id="discard-new-session"/);
});

test("paused fresh session foregrounds export and makes discarding a separate inline choice", () => {
  const html = render({aiPaused: true, meetPhase: "paused", notes: "Keep these notes", sourceFragments: [{...fragment(1, null), route: "transcription", provenance: "source_asr", text: "Retained source"}]}, {sessionActions: nextSession()});
  assert.match(html, /Keep these notes|Retained source/);
  assert.doesNotMatch(button(html, "export-new-session"), /disabled/);
  assert.match(html, /Export &amp; new session/);
  assert.match(button(html, "discard-new-session"), /aria-expanded="false" aria-controls="discard-session-choice"/);
  assert.match(html, /id="discard-session-choice" class="discard-session-choice" hidden=""/);
  assert.match(html, /will discard the captions, interpretation and notes retained on this page/);
  assert.match(html, /Your browser starts a download\. Check Downloads/);
  assert.doesNotMatch(html, /id="new-session"/);
  const pending = render({aiPaused: true, meetPhase: "paused"}, {sessionActions: nextSession({creating: true})});
  for (const id of ["export-new-session", "discard-new-session", "confirm-discard-new-session"]) assert.match(button(pending, id), /disabled/);
});

test("export dispatch blocks session replacement and failures remain explicit without automatic creation", () => {
  const exportRetained = () => {throw new Error("Rendering must never export");};
  const pending = renderToStaticMarkup(createElement(SessionTransition, {actions: nextSession(), paused: true, exporting: true, exportRetained}));
  assert.match(pending, /aria-busy="true"/);
  for (const id of ["export-new-session", "discard-new-session", "confirm-discard-new-session"]) assert.match(button(pending, id), /disabled/);
  const stoppedPending = renderToStaticMarkup(createElement(SessionTransition, {actions: nextSession(), exporting: true}));
  assert.match(button(stoppedPending, "new-session"), /disabled/);
  const failed = renderToStaticMarkup(createElement(SessionTransition, {actions: nextSession({error: "The local service is unavailable."}), paused: true, exportError: "The export could not be prepared.", exportRetained}));
  assert.match(failed, /role="alert">The local service is unavailable/);
  assert.match(failed, /role="alert">The export could not be prepared/);
  assert.doesNotMatch(button(failed, "export-new-session"), /disabled/);
});

test("host language preference survives a fresh session without carrying participant agreement", () => {
  for (const generation of [1, 4]) {
    const html = render({session: session({generation}), captureConfirmed: false}, {preferredLanguage: "hi", onLanguageChange() {throw new Error("Rendering must not change preference");}});
    assert.match(html, /<option value="hi" selected="">Hindi/);
    assert.doesNotMatch(html.match(/<input id="capture-agreement"[^>]*>/)?.[0] ?? "missing", /checked/);
    assert.match(html, /By starting, I confirm everyone has agreed/);
  }
});

test("private early captions are visibly provisional, escaped and absent from public and stopped UI", () => {
  const sourceDrafts = [{route_id: "captions", source: "remote_mix", route: "transcription", item_id: "early", revision: 1, text: "<img src=x onerror=alert(1)>", retired: false}];
  const html = render({sourceDrafts});
  assert.match(html, /Live draft/); assert.match(html, /Awaiting source confirmation/);
  assert.match(html, /not included in exports or data lookups/);
  assert.match(html, /&lt;img src=x onerror=alert\(1\)&gt;/); assert.doesNotMatch(html, /<img/);
  for (const overrides of [{role: "public"}, {stopped: true}]) assert.doesNotMatch(render({sourceDrafts, ...overrides}), /onerror|Live draft/);
});

test("Gemini consent names the audio recipient and offers only supported finite controls", () => {
  const capabilities = {source_captions: true, live_interpreter: true, private_translation: false, live_assistant: false, prepared_public_speech: false};
  const html = render({session: session({provider_vendor: "gemini", continuous: false, capabilities}), budget: geminiBudget()});
  assert.match(html, /sending this audio to Google Gemini/);
  assert.match(html, /Start live interpretation · billed by your own Google account/);
  assert.match(html.split('id="start-meet"')[0], /Estimated local reservation US\$2\.00 for up to 10 min/);
  assert.match(html, /The reservation uses a preview rate/);
  assert.doesNotMatch(html, /billed by your own OpenAI account/);
  assert.match(html, /gemini-3.5-transcribe-live/);
  assert.match(html, /Gemini Live Translate/);
  assert.match(html, /id="spoken-language"/);
  assert.doesNotMatch(html, /GPT-Live|gpt-live-transcribe|gpt-realtime-translate|id="live-mode"|id="speech-terms"|id="private-listening-title"|id="public-text"|id="live-text"/i);
});

test("guided caption reader excludes generated output from another interpretation policy with the same target", () => {
  const base = {schema_version: "interpretation.v1", mode: "two_way", first: "en", second: "ja"};
  const generated = {...fragment(7, "interpretation"), language: "en", interpretation: {...base, second: "hi"}, text: "different-language-pair"};
  const html = render({interpretation: base, fragments: [generated]});
  assert.doesNotMatch(feedbackArticle(html, "meet-interpretation-preview"), /different-language-pair/);
  const accepted = render({interpretation: base, fragments: [{...generated, interpretation: base, text: "same-language-pair"}]});
  assert.match(feedbackArticle(accepted, "meet-interpretation-preview"), /same-language-pair/);
  assert.match(accepted, /Caption size/);
  assert.doesNotMatch(render({stopped: true}), /aria-label="Caption reader"/);
  assert.doesNotMatch(render({role: "public", session: session({role: "public"})}), /aria-label="Caption reader"/);
});

test("Stop receipt separates local retirement, native evidence and unavailable billing on the private operator only", () => {
  const receipt = {version: 1, session_generation: 1, generation: 2, provider_activity: "unconfirmed", reserved_microusd: 125000, invoice: "unavailable"};
  const view = {stopped: true, stopAcknowledged: true, stopReceipt: receipt, stopReceiptState: "ready", localAudioRetirement: "confirmed", rtcAudioRetirement: "unconfirmed"};
  const html = render(view);
  for (const marker of ["Guided audio handles retired on this page", "Private WebRTC audio retirement unconfirmed", "Stop acknowledged by the server", "Provider closure unconfirmed", "$0.125 reserved locally", "Actual charges unavailable"]) assert.ok(html.includes(marker), marker);
  assert.doesNotMatch(render({...view, role: "public", session: session({role: "public"})}), /Stop receipt|\$0\.125|WebRTC audio retirement/);
  assert.doesNotMatch(render({...view, session: null}), /Stop receipt|\$0\.125/);
});

test("fixed-target interpretation rows distinguish timed and untimed speech from source, assistant, other inputs and policies", () => {
  const fixed = {source: "self", language: "ja", live_mode: "interpretation", provenance: "live_output"};
  const distractors = [
    {source: "remote_mix"}, {language: "en"}, {live_mode: "assistant"}, {provenance: "live_input"},
    {interpretation: {schema_version: "interpretation.v1", mode: "two_way", first: "ja", second: "en"}},
  ];
  for (const timed of [true, false]) {
    const entries = [{}, ...distractors].map((override, i) => ({
      ...fragment(i + 1, "interpretation"), ...fixed, ...override, text: i === 0 ? "requested target" : "different content " + i,
    }));
    if (!timed) entries.push({...entries[0], key: 99, route: "translation", live_mode: null, provenance: "translation", text: "dedicated translation"});
    const fixture = timed ? {liveCaptions: entries} : {fragments: entries};
    const html = render({...fixture, sourceFragments: [{...entries[0], key: 100, route: "transcription", live_mode: null, provenance: "source_asr", text: "source words"}]});
    const selected = [...html.matchAll(/<li\b([^>]*data-interpretation-mode="fixed"[^>]*)>[\s\S]*?<p>([\s\S]*?)<\/p>/g)]
      .filter(([, attributes]) => attributes.includes('data-interpretation-source="self"') && attributes.includes('data-interpretation-target="ja"'))
      .map(match => match[2]);
    assert.deepEqual(selected, ["requested target"]);
  }
});
