import assert from "node:assert/strict";
import test from "node:test";
import {createElement} from "react";
import {renderToStaticMarkup} from "react-dom/server";
import {PilotClient} from "../.test-build/client.js";
import {SessionHost} from "../.test-build/session-host.js";
import {MeetSetup} from "../.test-build/MeetSetup.js";
import {parseEvent, parseInterpretation, parseInterpretationLifecycle} from "../.test-build/protocol.js";
import {budgetFixture} from "./budget-fixture.mjs";

const pair = {schema_version: "interpretation.v1", mode: "two_way", first: "ja", second: "en"};
const auto = {schema_version: "interpretation.v1", mode: "auto_to_target", target: "en"};
const json = (value, status = 200) => new Response(JSON.stringify(value), {status, headers: {"Content-Type": "application/json"}});
const flush = async () => {for (let i = 0; i < 12; i++) await new Promise(resolve => setImmediate(resolve));};
const deferred = () => {let resolve; const promise = new Promise(done => {resolve = done;}); return {resolve, promise};};
function session(generation = 1) {return {role: "operator", csrf: String(generation).padStart(64, "0"), generation, stopped: false, continuous: true, repeatable_sessions: true, providers_enabled: true, expires_in_seconds: null, capabilities: {source_captions: true, private_translation: false, live_assistant: false, live_interpreter: true, prepared_public_speech: false}, limitations: []};}
function budget(generation) {
  const row = budgetFixture({generation, amounts: null, remaining_seconds: null, route_duration_seconds: 300, continuous: {warning_threshold_microusd: 5_000_000, allocated_microusd: 10_000_000, windows_started: 20}, stream_reservations: {used: 0, limit: 2}, tts_reservations: {used: 0, limit: 0}});
  row.routes = row.routes.map(quote => ({...quote, quote_seconds: quote.route === "translation" ? null : 300, quote_microusd: quote.route === "translation" ? null : quote.per_minute_microusd * 5, configured: quote.route !== "translation", can_reserve: quote.route !== "translation"}));
  row.tts = {...row.tts, configured: false, can_reserve_maximum: false}; return row;
}
const lifecycle = (generation = 1, state = "paused") => ({schema_version: "interpretation-lifecycle.v1", generation, state, local_retired: state === "paused", provider_closed_confirmed: state === "paused", resume_allowed: state === "paused"});
function harness() {
  let currentGeneration = 1, respond, paused = false;
  const calls = [], owners = [];
  const host = new SessionHost(() => {
    const generation = currentGeneration;
    let socket;
    const effects = {prepare: 0, activate: 0, stop: 0, private: []};
    const audio = {prepareCapture: async () => {effects.prepare++;}, cancelPreparedCapture: async () => {}, activateCapture: () => effects.activate++, startCapture: async () => {}, stopCapture() {}, stopAll: () => effects.stop++, retire: async () => true, resumePlayback: async () => {}, setPrivateMuted() {}, playPrivate: (...args) => effects.private.push(args), stopClip() {}, playClip: async () => {}};
    const event = data => socket?.onmessage?.({data: JSON.stringify({generation, ...data})});
    const client = new PilotClient("operator", {audio, now: () => 0, interval: () => () => {}, broadcastStop() {}, verifyClip: async () => false, openPublicWindow() {}, socket() {socket = {readyState: 1, bufferedAmount: 0, sent: [], send(value) {this.sent.push(value);}, close() {}}; queueMicrotask(() => event({type: "hello", role: "operator", stopped: false})); return socket;}, fetch: async (path, options = {}) => {
      const body = options.body && JSON.parse(options.body); calls.push({path, body, options});
      const result = respond?.(path, body, options); if (result !== undefined) return result;
      if (path === "/api/session") return json(session(generation));
      if (path === "/api/budget") return json(budget(generation));
      if (path === "/api/routes") {
        const route_id = `g${generation}_${body.route}`;
        queueMicrotask(() => {
          event({type: "route", route_id, source: body.source, route: body.route, live_mode: body.live_mode, language: body.language, ...(body.interpretation ? {interpretation: body.interpretation} : {}), ...(body.delivery ? {delivery: body.delivery} : {}), state: "ready", code: null, expires_in_seconds: 300});
          event({type: "window", route_id, source: body.source, route: body.route, window: 1, state: "ready"});
        });
        return json({state: "starting", generation, route_id});
      }
      if (path === "/api/interpretation/pause") {paused = true; return json(lifecycle(generation));}
      if (path === "/api/interpretation/state") return json(lifecycle(generation, paused ? "paused" : "active"));
      if (path === "/api/interpretation/resume") {currentGeneration = generation + 1; paused = false; return json({state: "ready", ...body, session: session(currentGeneration)});}
      if (path === "/api/stop") return json({state: "stopped", generation: generation + 1});
      if (path === "/api/source-gap") return json({state: "closed", generation});
      if (path === "/api/diagnostics") return json({}, 503);
      return json({}, 404);
    }});
    const owned = {client, audio, effects, event, generation, socket: () => socket, dispose: () => client.dispose()}; owners.push(owned); return owned;
  }, () => "b".repeat(64));
  return {host, owners, calls, respond: callback => {respond = callback;}, get client() {return host.snapshot().client;}, get owner() {return owners.at(-1);}, async start(policy = pair) {await this.client.initialize(null); this.client.setInterpretation(policy); this.client.confirmCapture(true); await this.client.startMeetSession("en", 335000); assert.equal(this.client.snapshot().meetPhase, "listening");}};
}

test("versioned policy and pause schemas reject ambiguous authority and language metadata", () => {
  assert.deepEqual(parseInterpretation(pair), pair); assert.deepEqual(parseInterpretation({...auto, target: "vi"}), {...auto, target: "vi"});
  for (const value of [{...pair, second: "ja"}, {...auto, target: "xx"}, {...pair, extra: "instruction"}, {...auto, schema_version: "interpretation.v2"}]) assert.throws(() => parseInterpretation(value));
  for (const value of [{...lifecycle(), generation: 2}, {...lifecycle(), local_retired: false}, {...lifecycle(), provider_closed_confirmed: false}, {...lifecycle(), state: "active"}, {...lifecycle(), extra: true}]) assert.throws(() => parseInterpretationLifecycle(value, 1));
  assert.equal(parseInterpretationLifecycle(lifecycle(), 1).resume_allowed, true);
});

test("pair and Auto each admit just one caption and one interpreter route with immutable mode metadata", async () => {
  for (const policy of [pair, auto, {...pair, first: "vi", second: "es"}]) {
    const h = harness(); await h.start(policy);
    const routes = h.calls.filter(call => call.path === "/api/routes").map(call => call.body);
    assert.equal(routes.length, 2); assert.deepEqual(routes[1].interpretation, policy); assert.equal(routes[0].interpretation, undefined);
    assert.deepEqual(routes[0].transcription.languages, policy.mode === "two_way" ? [policy.first, policy.second] : ["ja", "en", "hi", "es", "vi"]);
    assert.equal(h.client.setInterpretation(auto), false); assert.equal(h.client.snapshot().interpretation.mode, policy.mode);
    h.host.dispose();
  }
});

test("manual Pause immediately fences media and survives late output while preserving notes and source text", async () => {
  const h = harness(); await h.start(); h.client.setNotes("fictional retained notes");
  h.owner.event({type: "transcript", route_id: "g1_transcription", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "青い箱", final: true, item: {id: "source1", revision: 1, sequence: 1}, window: 1});
  const gate = deferred(); h.respond(path => path === "/api/interpretation/pause" ? gate.promise : undefined);
  const pausing = h.client.pauseInterpretation();
  assert.equal(h.client.snapshot().manualPause.state, "pausing"); assert.equal(h.client.snapshot().aiPaused, false); assert.equal(h.owner.effects.stop, 1);
  assert.equal(h.client.sendFrame("remote_mix", new ArrayBuffer(646)), false); assert.equal(h.client.canResumeInterpretation(), false);
  h.owner.event({type: "audio", route_id: "g1_live", source: "remote_mix", route: "live", interpretation: pair, pcm16_base64: "AAA=", sample_rate: 16000, window: 1});
  h.owner.event({type: "window", route_id: "g1_live", source: "remote_mix", route: "live", window: 2, state: "recovering", code: "provider_connection_lost", retry_in_seconds: 1});
  assert.equal(h.owner.effects.private.length, 0); assert.equal(h.client.snapshot().renewalCount, 0);
  gate.resolve(json(lifecycle())); await pausing;
  assert.equal(h.client.canResumeInterpretation(), true); assert.equal(h.client.snapshot().notes, "fictional retained notes"); assert.equal(h.client.snapshot().sourceFragments[0].text, "青い箱");
  assert.equal(h.calls.some(call => call.path === "/api/source-gap"), false); h.host.dispose();
});

test("unconfirmed provider closure and browser cleanup independently refuse Resume; explicit status retry grants no capture", async () => {
  const h = harness(); await h.start(); h.owner.audio.retire = async () => false;
  h.respond(path => path === "/api/interpretation/pause" ? json(lifecycle(1, "closure_unconfirmed")) : undefined);
  await h.client.pauseInterpretation(); assert.equal(h.client.canResumeInterpretation(), false);
  h.respond(path => path === "/api/interpretation/state" ? json(lifecycle()) : undefined);
  await h.client.checkInterpretationPause(); assert.equal(h.client.canResumeInterpretation(), false);
  h.owner.audio.retire = async () => true; await h.client.checkInterpretationPause(); assert.equal(h.client.canResumeInterpretation(), true);
  assert.equal(h.owner.effects.activate, 1); assert.equal(h.calls.filter(call => call.path === "/api/routes").length, 2); h.host.dispose();
});

test("Resume rotates authority and device owner, retains labeled source segment and notes, never starts itself", async () => {
  const h = harness(); await h.start(); const old = h.client;
  old.setNotes("fictional note"); h.owner.event({type: "transcript", route_id: "g1_transcription", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "秘密ではない例", final: true, item: {id: "s", revision: 1, sequence: 1}, window: 1});
  await old.pauseInterpretation(); const before = h.calls.filter(call => call.path === "/api/routes").length;
  await h.host.resume(old); await flush();
  assert.notEqual(h.client, old); assert.equal(h.client.snapshot().session.generation, 2); assert.notEqual(h.client.snapshot().session.csrf, old.snapshot().session.csrf);
  assert.equal(h.client.snapshot().captureConfirmed, false); assert.equal(h.owner.effects.prepare, 0); assert.equal(h.owner.effects.activate, 0); assert.equal(h.calls.filter(call => call.path === "/api/routes").length, before);
  assert.equal(h.client.snapshot().notes, "fictional note"); assert.equal(h.client.snapshot().earlierSegments.length, 1); assert.match(h.client.exportText(), /秘密ではない例/); assert.match(h.client.exportText(), /PAUSED GAP/);
  assert.deepEqual(h.client.snapshot().interpretation, pair); assert.equal(h.client.snapshot().budget.continuous.allocated_microusd, 10_000_000);
  h.host.dispose(); assert.ok(h.owner.effects.stop > 0);
});

test("Stop dominates an in-flight Pause result and never restores Resume", async () => {
  const h = harness(); await h.start(); const gate = deferred(); h.respond(path => path === "/api/interpretation/pause" ? gate.promise : undefined);
  const pause = h.client.pauseInterpretation(); await h.client.stop(); gate.resolve(json(lifecycle())); await pause;
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().manualPause, null); assert.equal(h.client.canResumeInterpretation(), false); assert.ok(h.owner.effects.stop > 0); h.host.dispose();
});

test("Pause during outstanding native selection fences late completion before any provider admission", async () => {
  const h = harness(); await h.client.initialize(null); h.client.setInterpretation(pair); h.client.confirmCapture(true);
  const gate = deferred(); h.owner.audio.prepareCapture = () => gate.promise;
  const start = h.client.startMeetSession("en", 335000); await h.client.pauseInterpretation(); gate.resolve(); await start;
  assert.equal(h.calls.filter(call => call.path === "/api/routes").length, 0); assert.equal(h.owner.effects.activate, 0); assert.equal(h.client.snapshot().manualPause.state, "paused"); h.host.dispose();
});

test("standalone audio refuses meeting delivery and source/output authority conflation", () => {
  const base = {type: "audio", generation: 1, route_id: "live1", source: "remote_mix", route: "live", interpretation: pair, delivery: {audience: "meeting"}, pcm16_base64: "AAA=", sample_rate: 16000, window: 1};
  assert.throws(() => parseEvent(JSON.stringify(base), "operator"));
  for (const value of [{...base, interpretation: undefined}, {...base, source: "self"}, {...base, route: "translation"}, {...base, detected_language: "ja"}]) assert.throws(() => parseEvent(JSON.stringify(value), "operator"));
});

test("rendered pair, Auto and Pause labels are factual and rendering opens no routes or devices", async () => {
  const h = harness(); await h.start(); await h.client.pauseInterpretation();
  const html = renderToStaticMarkup(createElement(MeetSetup, {view: h.client.snapshot(), client: h.client, preferredLanguage: "en", finish() {}, sessionActions: {available: true, creating: false, error: null, create() {}, resume() {}}}));
  assert.match(html, /Resume interpretation/); assert.match(html, /paid speech connections closed/); assert.match(html, /your browser requires a new permission gesture/); assert.doesNotMatch(html, /Detected language: Japanese/);
  assert.equal(h.calls.filter(call => call.path === "/api/routes").length, 2); h.host.dispose();
});

test("Pause during an admitted route with a lost HTTP reply fences the pair without depending on a route ID", async () => {
  const h = harness(); await h.client.initialize(null); h.client.setInterpretation(pair); h.client.confirmCapture(true);
  const gate = deferred(); h.respond(path => path === "/api/routes" ? gate.promise : undefined);
  const start = h.client.startMeetSession("en", 335000); await flush(); assert.equal(h.calls.filter(call => call.path === "/api/routes").length, 1);
  await h.client.pauseInterpretation(); gate.resolve(json({state: "starting", generation: 1, route_id: "lost_admission"})); await start;
  assert.equal(h.calls.filter(call => call.path === "/api/routes").length, 1); assert.equal(h.calls.filter(call => call.path === "/api/interpretation/pause").length, 1);
  assert.equal(h.owner.effects.activate, 0); assert.equal(h.client.canResumeInterpretation(), true); h.host.dispose();
});

test("recovering speech windows can be manually paused, but fault-paused work cannot be resumed", async () => {
  const h = harness(); await h.start();
  h.owner.event({type: "window", route_id: "g1_live", source: "remote_mix", route: "live", window: 2, state: "recovering", code: "provider_connection_lost", retry_in_seconds: 1});
  assert.equal(h.client.snapshot().meetPhase, "recovering"); await h.client.pauseInterpretation(); assert.equal(h.client.canResumeInterpretation(), true); h.host.dispose();
  const failed = harness(); await failed.start(); failed.owner.event({type: "gap", source: "remote_mix", code: "usage_mismatch"}); await flush();
  assert.equal(failed.client.snapshot().aiPaused, true); assert.equal(failed.client.canResumeInterpretation(), false); await failed.host.resume(failed.client);
  assert.equal(failed.calls.some(call => call.path === "/api/interpretation/resume"), false); failed.host.dispose();
});

test("lost Resume reply retries the same identity, double clicks singleflight and Stop fences late successors", async () => {
  const h = harness(); await h.start(); await h.client.pauseInterpretation(); const old = h.client;
  let failed = false; h.respond(path => {if (path === "/api/interpretation/resume" && !failed) {failed = true; throw new Error("lost reply");}});
  await h.host.resume(old); assert.match(h.host.snapshot().error, /not confirmed/); await h.host.resume(old);
  const requests = h.calls.filter(call => call.path === "/api/interpretation/resume"); assert.equal(requests.length, 2); assert.deepEqual(requests[0].body, requests[1].body); h.host.dispose();
  const stopped = harness(); await stopped.start(); await stopped.client.pauseInterpretation(); const paused = stopped.client, gate = deferred();
  stopped.respond(path => path === "/api/interpretation/resume" ? gate.promise : undefined);
  const resuming = stopped.host.resume(paused); await flush(); await stopped.host.resume(paused); assert.equal(stopped.calls.filter(call => call.path === "/api/interpretation/resume").length, 1);
  await paused.stop(); gate.resolve(json({state: "ready", previous_generation: 1, request_id: "b".repeat(64), session: session(2)})); await resuming;
  assert.equal(stopped.owners.length, 1); assert.equal(stopped.client.snapshot().stopped, true); stopped.host.dispose();
});

test("provider-free paused reload reads lifecycle authority instead of showing a new Start", async () => {
  const h = harness(); h.respond(path => path === "/api/session" ? json({...session(), providers_enabled: false, capabilities: {source_captions: false, private_translation: false, live_assistant: false, live_interpreter: false, prepared_public_speech: false}}) : path === "/api/interpretation/state" ? json(lifecycle()) : undefined);
  await h.client.initialize(null); await flush();
  assert.equal(h.client.snapshot().manualPause.state, "paused"); assert.equal(h.client.canResumeInterpretation(), true); assert.equal(h.owner.effects.activate, 0); assert.equal(h.client.snapshot().notes, ""); h.host.dispose();
});

test("retained source segments remain bounded across repeated Pause and Resume without replay", async () => {
  const h = harness(); await h.start();
  for (let n = 0; n < 10; n++) {
    const old = h.client; old.setNotes(`fictional note ${n}`); await old.pauseInterpretation(); await h.host.resume(old); await flush();
    assert.equal(h.client.snapshot().captureConfirmed, false); h.client.confirmCapture(true); await h.client.startMeetSession("en", 335000);
    assert.equal(h.client.snapshot().meetPhase, "listening");
  }
  assert.equal(h.client.snapshot().earlierSegments.length, 8); assert.equal(h.client.snapshot().earlierSegmentsDropped, 2); assert.match(h.client.exportText(), /2 older segments discarded/);
  assert.equal(h.client.snapshot().notes, "fictional note 9"); assert.equal(h.calls.filter(call => call.path === "/api/routes").length, 22); h.host.dispose();
});

test("finite and legacy scopes cannot enable extended modes or Pause", async () => {
  const h = harness(); h.respond(path => path === "/api/session" ? json({...session(), repeatable_sessions: false}) : undefined);
  await h.client.initialize(null); assert.equal(h.client.setInterpretation(pair), false); h.client.confirmCapture(true); await h.client.startMeetSession("vi", 335000); await h.client.pauseInterpretation();
  assert.equal(h.calls.some(call => call.path === "/api/routes" || call.path === "/api/interpretation/pause"), false); h.host.dispose();
});

test("Finish after Resume never labels a meeting with a paused gap as complete", async () => {
  const h = harness(); await h.start(); const old = h.client; await old.pauseInterpretation(); await h.host.resume(old); await flush();
  h.client.confirmCapture(true); await h.client.startMeetSession("en", 335000); const finishing = h.client.finishExport();
  h.owner.event({type: "finishing"}); h.owner.event({type: "finished", source_completion: "complete"});
  const exported = await finishing; assert.match(exported, /PAUSED GAP/); assert.match(exported, /Source finalization was incomplete or unconfirmed/); h.host.dispose();
});

test("notes edited while Resume is pending survive the authority handoff", async () => {
  const h = harness(); await h.start(); await h.client.pauseInterpretation(); const old = h.client, gate = deferred();
  h.respond(path => path === "/api/interpretation/resume" ? gate.promise : undefined);
  old.setNotes("before request"); const resuming = h.host.resume(old); await flush(); old.setNotes("edited while waiting");
  gate.resolve(json({state: "ready", previous_generation: 1, request_id: "b".repeat(64), session: session(2)})); await resuming;
  assert.equal(h.client.snapshot().notes, "edited while waiting"); assert.equal(h.owner.effects.activate, 0); h.host.dispose();
});
