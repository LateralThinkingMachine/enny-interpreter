import assert from "node:assert/strict";
import test from "node:test";
import {PilotClient} from "../.test-build/client.js";
import {parseSession, parseEvent} from "../.test-build/protocol.js";
import {parseBudget, meetReservation} from "../.test-build/cost.js";
import {SourceArchive} from "../.test-build/source-archive.js";
import {budgetFixture, disabledBudget} from "./budget-fixture.mjs";

const json = (value, status = 200) => new Response(JSON.stringify(value), {status, headers: {"Content-Type": "application/json"}});
const flush = async () => {for (let i = 0; i < 12; i++) await new Promise(resolve => setImmediate(resolve));};
const deferred = () => {let resolve; const promise = new Promise(done => {resolve = done;}); return {promise, resolve};};
const session = () => ({role: "operator", csrf: "fixture_csrf", generation: 1, stopped: false, continuous: true, providers_enabled: true, expires_in_seconds: null, capabilities: {source_captions: true, private_translation: false, live_assistant: false, live_interpreter: true, prepared_public_speech: false}, limitations: []});
function budget() {
  const row = budgetFixture({amounts: null, remaining_seconds: null, route_duration_seconds: 300, continuous: {warning_threshold_microusd: 100_000, allocated_microusd: 50_000_000, windows_started: 200}, stream_reservations: {used: 0, limit: 2}, tts_reservations: {used: 0, limit: 0}});
  row.routes = row.routes.map(quote => ({...quote, quote_seconds: quote.route === "translation" ? null : 300, quote_microusd: quote.route === "translation" ? null : quote.per_minute_microusd * 5, configured: quote.route !== "translation", can_reserve: quote.route !== "translation"}));
  row.tts = {...row.tts, configured: false, can_reserve_maximum: false};
  return row;
}
function harness({notifications, source = "remote_mix"} = {}) {
  let now = 0, tick = () => {}, respond;
  const calls = [], sockets = [], effects = {capture: 0, stopped: 0, mute: [], private: []};
  const audio = {prepareCapture: async () => {}, cancelPreparedCapture: async () => {}, activateCapture: () => effects.capture++, startCapture: async () => {}, stopCapture: () => {}, stopAll: () => effects.stopped++, resumePlayback: async () => {}, setPrivateMuted: (...args) => effects.mute.push(args), playPrivate: (...args) => effects.private.push(args), stopClip: () => {}, playClip: async () => {}};
  const client = new PilotClient("operator", {costNotifications: notifications, audio, now: () => now, interval: callback => {tick = callback; return () => {tick = () => {};};}, broadcastStop: () => {}, verifyClip: async () => false, openPublicWindow: () => {},
    fetch: async (path, options = {}) => {
      const body = options.body ? JSON.parse(options.body) : undefined; calls.push({path, body, options});
      const response = respond?.(path, body, options); if (response !== undefined) return response;
      if (path === "/api/session") return json(session());
      if (path === "/api/budget") return json(budget());
      if (path === "/api/routes") return json({state: "starting", generation: 1, route_id: body.route});
      if (path === "/api/source-gap") return json({state: "closed", generation: 1});
      if (path === "/api/stop") return json({state: "stopped", generation: 2});
      if (path === "/api/diagnostics") return json({version: 1, generation: 2, stopped: true, stop: {code: "operator_stop", elapsed_seconds: 0}, last_issue: null});
      throw new Error("Unexpected synthetic request");
    }, socket: () => {const socket = {readyState: 1, bufferedAmount: 0, sent: [], closed: false, onmessage: null, send(value) {this.sent.push(value);}, close() {this.closed = true;}}; sockets.push(socket); return socket;},
  });
  const event = row => sockets.at(-1).onmessage?.({data: JSON.stringify({generation: 1, ...row})});
  const window = (route, number, state) => event({type: "window", route_id: route, source, route, window: number, state});
  const route = name => event({type: "route", route_id: name, source, route: name, live_mode: name === "live" ? "interpretation" : null, language: "en", state: "ready", code: null, expires_in_seconds: 300});
  const transcript = (window, {text = "fictional source", final = true, revision = 1, id = "same_id", sequence = 1} = {}) => event({type: "transcript", route_id: "transcription", source, route: "transcription", provenance: "source_asr", text, final, item: {id, revision, sequence}, window});
  return {client, calls, sockets, effects, audio, event, window, route, transcript, respond: value => {respond = value;},
    elapse(ms) {now += ms;},
    recovering(name, number, retry = 1) {event({type: "window", route_id: name, source, route: name, window: number, state: "recovering", code: "provider_connection_lost", retry_in_seconds: retry});},
    advance(ms) {const until = now + ms; while (now < until) {now = Math.min(until, now + 1000); event({type: "heartbeat"}); tick();}},
    async start() {
      await client.initialize(null); client.setMeetSource(source); client.confirmCapture(true); await client.selectMeetAudio(); client.captureStatus(source, {level: .1, frames: 50, muted: false});
      const start = client.startMeet("en", 335000); await flush(); event({type: "hello", role: "operator", stopped: false}); await flush();
      route("transcription"); window("transcription", 1, "ready"); await flush(); route("live"); window("live", 1, "ready"); await start;
      assert.equal(client.snapshot().meetPhase, "listening");
    },
    renew(name, next) {window(name, next - 1, "draining"); window(name, next, "renewing"); route(name); window(name, next, "ready");},
  };
}

test("private browser ownership invalidates legacy cost snapshots and release refreshes without starting work", async () => {
  const h = harness(); await h.client.initialize(null);
  const owner = h.client.privateListeningOwner(), first = deferred(), stale = deferred(), latest = deferred();
  let reads = 0;
  h.respond(path => path === "/api/budget" ? [first.promise, stale.promise, latest.promise][reads++] : undefined);
  for (const [amount, seconds] of [[250000, 335], [279167, 300], [undefined, 335]]) {
    assert.equal(owner.claim(amount, seconds), false);
    assert.equal(h.client.snapshot().browserListening, false);
  }
  assert.equal(owner.claim(279167, 335), true); assert.equal(h.client.snapshot().browserListening, true);
  assert.equal(h.client.snapshot().budgetFresh, false);
  first.resolve(json(budget())); await flush(); assert.equal(h.client.snapshot().budgetFresh, true);
  const oldRefresh = h.client.refreshBudget(); await flush();
  owner.release(); assert.equal(h.client.snapshot().browserListening, false); assert.equal(h.client.snapshot().budgetFresh, false);
  stale.resolve(json(budget())); await oldRefresh; await flush();
  assert.equal(reads, 3); assert.equal(h.client.snapshot().budgetFresh, false, "pre-release response cannot make retained cost current");
  const next = budget(); next.continuous.allocated_microusd += 279167; next.continuous.windows_started++;
  latest.resolve(json(next)); await flush();
  assert.equal(h.client.snapshot().budgetFresh, true); assert.equal(h.client.snapshot().budget.continuous.allocated_microusd, 50_279_167);
  assert.equal(h.calls.some(call => ["/api/routes", "/api/webrtc/session", "/api/stop"].includes(call.path)), false);
  assert.equal(h.sockets.length, 0); assert.equal(h.effects.capture, 0);
  h.respond(path => path === "/api/budget" ? json({}, 503) : undefined);
  owner.release(); await flush(); assert.equal(h.client.snapshot().budgetFresh, false);
  assert.equal(h.client.snapshot().budget.continuous.allocated_microusd, 50_279_167, "failed refresh preserves a visibly stale amount");
  h.client.dispose();
});

test("private listener ownership replaces manual-route guidance without hiding terminal Stop", async () => {
  const h = harness(); await h.client.initialize(null);
  const owner = h.client.privateListeningOwner(); assert.equal(owner.claim(279167, 335), true);
  assert.match(h.client.snapshot().message, /Private listening is selected/);
  await h.client.connect(); h.event({type: "hello", role: "operator", stopped: false});
  assert.match(h.client.snapshot().message, /private listening panel shows current audio, connection and cost status/);
  assert.doesNotMatch(h.client.snapshot().message, /Open a route|select its audio source/);
  owner.release(); assert.match(h.client.snapshot().message, /released the local audio/); await flush();
  assert.equal(owner.claim(279167, 335), true, "an already connected control stream still gets private-listener guidance");
  assert.match(h.client.snapshot().message, /Private listening is selected/);
  await h.client.stop(); const stoppedMessage = h.client.snapshot().message;
  owner.release(); assert.equal(h.client.snapshot().message, stoppedMessage);
  h.client.dispose();
  const manual = harness(); await manual.client.initialize(null); await manual.client.connect(); manual.event({type: "hello", role: "operator", stopped: false});
  assert.match(manual.client.snapshot().message, /Open a route, then explicitly select its audio source/);
  manual.client.dispose();
});

test("guided microphone recovery returns to Listening with the same source and healthy sibling", async () => {
  const h = harness({source: "self"}); await h.start();
  assert.deepEqual(h.calls.filter(call => call.path === "/api/routes").map(call => call.body.source), ["self", "self"]);
  h.recovering("transcription", 2);
  assert.equal(h.client.snapshot().meetPhase, "recovering"); assert.equal(h.client.snapshot().capture.self, "capturing");
  assert.equal(h.client.snapshot().capture.remote_mix, "idle"); assert.equal(h.client.setMeetSource("remote_mix"), false);
  assert.deepEqual(h.client.snapshot().coverageGaps, ["self"]);
  h.event({type: "live_transcript", route_id: "live", source: "self", route: "live", provenance: "live_output", text: "The blue box", start_ms: 0, end_ms: 500, window: 1});
  assert.equal(h.client.snapshot().liveCaptions[0].text, "The blue box");
  h.route("transcription"); h.window("transcription", 2, "ready"); await flush();
  assert.equal(h.client.snapshot().meetPhase, "listening"); assert.equal(h.effects.capture, 1);
  assert.equal(h.effects.stopped, 0); assert.equal(h.client.snapshot().meetSource, "self");
  h.transcript(2, {text: "青い箱"}); assert.equal(h.client.snapshot().sourceFragments[0].source, "self");
  assert.equal(h.client.snapshot().sourceFragments[0].text, "青い箱"); h.client.dispose();
});

test("terminal guided microphone failure fences self only and retains its first source cause", async () => {
  const h = harness({source: "self"}); await h.start(); h.transcript(1, {text: "青い箱"});
  h.event({type: "gap", source: "self", code: "usage_mismatch"}); await flush();
  assert.equal(h.client.snapshot().aiPaused, true); assert.equal(h.client.snapshot().stopped, false);
  assert.equal(h.client.snapshot().meetSource, "self"); assert.equal(h.client.setMeetSource("remote_mix"), false);
  assert.deepEqual(h.client.snapshot().coverageGaps, ["self"]);
  assert.deepEqual(h.calls.filter(call => call.path === "/api/source-gap").map(call => call.body.source), ["self"]);
  assert.match(h.client.snapshot().terminalMessage, /usage inconsistent/);
  assert.equal(h.client.snapshot().sourceFragments[0].text, "青い箱");
  assert.equal(h.effects.stopped, 1); h.client.dispose();
});

test("Stop during microphone recovery prevents a late Ready from restoring capture or Listen", async () => {
  const h = harness({source: "self"}); await h.start(); h.recovering("live", 2);
  await h.client.stop(); h.route("live"); h.window("live", 2, "ready");
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().capture.self, "closed");
  assert.notEqual(h.client.snapshot().meetPhase, "listening"); assert.equal(h.effects.capture, 1);
  assert.equal(h.client.snapshot().muted.live_interpretation, true); h.client.dispose();
});

test("continuous session lifetime is explicit and never inferred from missing or malformed expiry", () => {
  assert.equal(parseSession(session(), "operator").expires_in_seconds, null);
  const legacy = {...session(), expires_in_seconds: 300}; delete legacy.continuous;
  assert.equal(parseSession(legacy, "operator").continuous, false);
  for (const changed of [{continuous: false}, {continuous: "true"}, {expires_in_seconds: 300}]) assert.throws(() => parseSession({...session(), ...changed}, "operator"));
  for (const state of ["draining", "renewing", "ready"]) {
    const row = {type: "window", generation: 1, route_id: "transcription", source: "remote_mix", route: "transcription", window: 1, state};
    assert.equal(parseEvent(JSON.stringify(row), "operator").state, state);
    assert.throws(() => parseEvent(JSON.stringify(row), "public"));
    assert.throws(() => parseEvent(JSON.stringify({...row, window: 0}), "operator"));
  }
});

test("recovery retains capture, notes and interrupted ASR, rejects old output and keeps sibling interpretation", async () => {
  const h = harness(); await h.start(); h.client.setNotes("Retain me"); h.transcript(1, {final: false});
  h.recovering("transcription", 2);
  assert.equal(h.client.snapshot().meetPhase, "recovering"); assert.equal(h.client.snapshot().aiPaused, false);
  assert.equal(h.client.snapshot().sourceFragments[0].incomplete, true);
  h.transcript(1, {text: "late old text", revision: 2});
  const audio = {type: "audio", route_id: "live", source: "remote_mix", route: "live", sample_rate: 24000, pcm16_base64: "AAA=", window: 1}; h.event(audio);
  assert.equal(h.effects.private.length, 1);
  const frame = new ArrayBuffer(646); new DataView(frame).setUint8(0, 1); assert.equal(h.client.sendFrame("remote_mix", frame), true);
  for (let number = 3; number <= 6; number++) {h.advance(25000); h.recovering("transcription", number, 30); await flush();}
  h.route("transcription"); h.window("transcription", 6, "ready"); h.transcript(6, {text: "Speech returned"});
  assert.equal(h.client.snapshot().meetPhase, "listening"); assert.equal(h.client.snapshot().notes, "Retain me");
  assert.deepEqual(h.client.snapshot().sourceFragments.map(row => row.text), ["fictional source", "Speech returned"]);
  assert.equal(h.effects.capture, 1); assert.equal(h.effects.stopped, 0); assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2);
  assert.equal(h.calls.some(row => ["/api/stop", "/api/source-gap"].includes(row.path)), false); h.client.dispose();
});

test("initial recovery before first Ready survives repeated backoff with one Start and no stale-window acceptance", async () => {
  const h = harness(); await h.client.initialize(null); h.client.confirmCapture(true);
  const start = h.client.startMeetSession("en", 335000); await flush(); h.event({type: "hello", role: "operator", stopped: false}); await flush();
  assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2);
  h.route("live"); h.window("live", 1, "ready");
  h.recovering("transcription", 2); h.advance(13000); await flush();
  h.recovering("transcription", 3, 30); h.advance(29000); await flush();
  assert.equal(h.client.snapshot().aiPaused, false); assert.equal(h.effects.capture, 0);
  h.route("transcription"); h.window("transcription", 3, "ready"); await flush();
  await start;
  assert.equal(h.client.snapshot().meetPhase, "listening"); assert.equal(h.effects.capture, 1); h.client.dispose();
});

test("recovery of one starting service cannot extend the independent silent sibling's Ready deadline", async () => {
  for (const recovering of ["transcription", "live"]) {
    const h = harness(); await h.client.initialize(null); h.client.confirmCapture(true);
    const start = h.client.startMeetSession("en", 335000); await flush(); h.event({type: "hello", role: "operator", stopped: false}); await flush();
    h.recovering(recovering, 2); h.advance(13000); await flush(); await start;
    assert.equal(h.client.snapshot().aiPaused, true); assert.equal(h.effects.capture, 0);
    assert.match(h.client.snapshot().terminalMessage, /did not become ready in time/);
    assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2);
    assert.equal(h.calls.filter(row => row.path === "/api/source-gap").length, 1);
    h.route("transcription"); h.route("live"); await flush();
    assert.equal(h.effects.capture, 0); h.client.dispose();
  }
});

test("continuous heartbeat acknowledges network delivery directly after timer throttling; prolonged liveness loss still pauses", async () => {
  const h = harness(); await h.start(); const socket = h.sockets[0], before = socket.sent.length;
  h.elapse(15000); h.event({type: "heartbeat"});
  assert.equal(socket.sent.length, before + 1); assert.equal(h.client.snapshot().aiPaused, false);
  const frame = new ArrayBuffer(646); new DataView(frame).setUint8(0, 1); assert.equal(h.client.sendFrame("remote_mix", frame), true);
  h.elapse(30001); h.event({type: "heartbeat"}); await flush(); assert.equal(h.client.snapshot().aiPaused, true); h.client.dispose();
});

test("captions recovering while interpretation opens cannot activate PCM until both current windows are Ready", async () => {
  const h = harness(); await h.client.initialize(null); h.client.confirmCapture(true);
  const start = h.client.startMeetSession("en", 335000); await flush(); h.event({type: "hello", role: "operator", stopped: false}); await flush();
  h.route("transcription"); h.window("transcription", 1, "ready"); await flush();
  h.recovering("transcription", 2); h.route("live"); h.window("live", 1, "ready"); await flush();
  assert.equal(h.effects.capture, 0); assert.equal(h.client.snapshot().meetPhase, "recovering");
  assert.doesNotMatch(h.client.snapshot().message, /^Listening/);
  h.route("transcription"); h.window("transcription", 2, "ready"); await start;
  assert.equal(h.effects.capture, 1); assert.equal(h.client.snapshot().meetPhase, "listening"); h.client.dispose();
});

test("Stop during recovery clears work and late Ready never resumes sound or capture", async () => {
  const h = harness(); await h.start(); h.recovering("live", 2); await h.client.stop();
  h.route("live"); h.window("live", 2, "ready"); await flush();
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.effects.capture, 1);
  assert.equal(h.effects.mute.filter(([, muted]) => !muted).length, 1); assert.equal(h.effects.stopped, 1); h.client.dispose();
});

test("missing or out-of-order recovery evidence cannot silently extend a connection", async () => {
  for (const failure of ["no_update", "skipped", "duplicate"]) {
    const h = harness(); await h.start(); h.recovering("live", 2);
    if (failure === "no_update") h.advance(60001);
    if (failure === "skipped") h.recovering("live", 4);
    if (failure === "duplicate") h.recovering("live", 2);
    await flush(); assert.equal(h.client.snapshot().aiPaused, true, failure); h.client.dispose();
  }
});

test("warning-only allocations above the threshold do not block current bounded windows", () => {
  const row = parseBudget(budget(), 1, true); assert.equal(meetReservation(row), 335000);
  assert.equal(row.amounts, null); assert.equal(row.remaining_seconds, null);
  assert.throws(() => parseBudget(budget(), 1, false));
  for (const change of [{continuous: {...budget().continuous, allocated_microusd: Number.MAX_SAFE_INTEGER + 1}}, {amounts: budgetFixture().amounts}, {remaining_seconds: 900}, {stream_reservations: {used: 0, limit: 16}}]) assert.throws(() => parseBudget({...budget(), ...change}, 1, true));
  assert.equal(meetReservation(parseBudget({...budget(), stream_reservations: {used: 1, limit: 2}}, 1, true)), null);
  assert.equal(parseBudget({...disabledBudget(), remaining_seconds: null, continuous: null}, 1, true).state, "disabled");
});

test("planned drain keeps final ASR and the selected capture, while window scope rejects old text and audio", async () => {
  const h = harness(); await h.start(); h.client.setNotes("Keep my notes");
  h.transcript(1, {text: "provisional", final: false}); h.window("transcription", 1, "draining");
  h.transcript(1, {text: "final old window", final: true, revision: 2}); h.window("transcription", 2, "renewing");
  const frame = new ArrayBuffer(646); new DataView(frame).setUint8(0, 1);
  assert.equal(h.client.sendFrame("remote_mix", frame), true);
  h.transcript(1, {text: "stale old overwrite", revision: 3});
  h.route("transcription"); h.window("transcription", 2, "ready"); h.transcript(2, {text: "new window same ID"});
  h.window("live", 1, "draining"); h.window("live", 2, "renewing");
  const audio = {type: "audio", route_id: "live", source: "remote_mix", route: "live", sample_rate: 24000, pcm16_base64: "AAA="};
  h.event({...audio, window: 1}); h.route("live"); h.window("live", 2, "ready"); h.event({...audio, window: 1}); h.event({...audio, window: 2});
  assert.equal(h.effects.private.length, 1); assert.equal(h.effects.capture, 1); assert.equal(h.effects.stopped, 0);
  assert.equal(h.client.snapshot().stopped, false); assert.equal(h.client.snapshot().aiPaused, false);
  assert.deepEqual(h.client.snapshot().sourceFragments.map(row => row.text), ["final old window", "new window same ID"]);
  assert.match(h.client.exportText(), /Keep my notes/); assert.match(h.client.exportText(), /Rolling source retention/); assert.match(h.client.exportText(), /Sources with known gaps\/closure: remote_mix/);
  assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2); h.client.dispose();
});

test("renewed continuous connections remain active beyond the legacy two-hour session limit", async () => {
  const h = harness(); await h.start();
  for (let window = 2; window <= 30; window++) {h.advance(250000); h.renew("transcription", window); h.renew("live", window); await flush();}
  assert.equal(h.client.snapshot().stopped, false); assert.equal(h.client.snapshot().aiPaused, false);
  assert.equal(h.effects.capture, 1); assert.equal(h.calls.some(row => row.path === "/api/stop"), false); h.client.dispose();
});

test("renewal timeout and provider errors pause both AI routes without deleting captions or notes", async () => {
  for (const failure of ["timeout", "provider", "unsealed", "duplicate_drain", "future_audio"]) {
    const h = harness(); await h.start(); h.transcript(1); h.client.setNotes("Private retained note");
    if (failure === "timeout") {h.window("live", 1, "draining"); h.window("live", 2, "renewing"); h.advance(35001);}
    if (failure === "provider") h.event({type: "gap", source: "remote_mix", code: "source_incomplete"});
    if (failure === "unsealed") {h.transcript(1, {id: "pending", sequence: 2, final: false}); h.window("transcription", 1, "draining"); h.window("transcription", 2, "renewing");}
    if (failure === "duplicate_drain") {h.window("live", 1, "draining"); h.window("live", 1, "draining");}
    if (failure === "future_audio") h.event({type: "audio", route_id: "live", source: "remote_mix", route: "live", sample_rate: 24000, pcm16_base64: "AAA=", window: 2});
    await flush(); assert.equal(h.client.snapshot().stopped, false, failure); assert.equal(h.client.snapshot().aiPaused, true, failure);
    assert.equal(h.client.snapshot().notes, "Private retained note"); assert.match(h.client.exportText(), /fictional source/);
    assert.equal(h.effects.stopped, 1); assert.equal(h.calls.some(row => row.path === "/api/stop"), false);
    assert.equal(h.calls.filter(row => row.path === "/api/source-gap").length, 1);
    if (["timeout", "provider", "unsealed"].includes(failure)) {assert.equal(h.sockets[0].closed, false); h.advance(1000); assert.ok(h.sockets[0].sent.some(value => typeof value === "string" && value.includes("heartbeat")));}
    const before = h.calls.length; await h.client.startMeet("en", 335000); await h.client.openRoute("remote_mix", "live", "en", "interpretation"); assert.equal(h.calls.length, before);
    await h.client.stop(); assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().notes, ""); assert.deepEqual(h.client.snapshot().sourceFragments, []); h.client.dispose();
  }
});

test("late pause closure cannot overwrite explicit Stop and reentrant Stop prevents pause network requests", async () => {
  const h = harness(), pending = deferred(); await h.start();
  h.respond(path => path === "/api/source-gap" ? pending.promise : undefined);
  h.event({type: "gap", source: "remote_mix", code: "provider_closed"}); await h.client.stop(); const snapshot = h.client.snapshot();
  pending.resolve(json({state: "closed", generation: 1})); await flush(); assert.deepEqual(h.client.snapshot(), snapshot); h.client.dispose();
  const other = harness(); await other.start(); other.client.subscribe(() => {if (other.client.snapshot().aiPaused && !other.client.snapshot().stopped) void other.client.stop();});
  other.event({type: "gap", source: "remote_mix", code: "provider_closed"}); await flush();
  assert.equal(other.calls.some(row => row.path === "/api/source-gap"), false); assert.equal(other.client.snapshot().stopped, true); other.client.dispose();
});

const fragment = (window, sequence, text = "x") => ({key: window * 4096 + sequence, window, route_id: "source", source: "remote_mix", route: "transcription", live_mode: null, language: "en", provenance: "source_asr", text, final: true, item: {id: `item_${sequence}`, revision: 1, sequence}});
test("rolling source retention bounds every identity map and cannot recreate evicted old-window records", () => {
  const archive = new SourceArchive(); archive.continuous();
  for (let window = 1; window <= 3; window++) {archive.advanceWindow("source", window); for (let item = 1; item <= 4096; item++) assert.equal(archive.apply(fragment(window, item)), "changed");}
  assert.equal(archive.items().length, 10000); assert.equal(archive.discardedCount(), 2288);
  assert.equal(archive.records.size, 10000); assert.equal(archive.sequences.size, 10000); assert.equal(archive.windows.size, 1); assert.equal(archive.routeOrder.size, 1);
  assert.equal(archive.apply(fragment(1, 1, "late replay")), "unchanged"); assert.equal(archive.items().length, 10000);
  assert.equal(archive.byteLength(), 10000); archive.clear(); assert.equal(archive.discardedCount(), 0); assert.equal(archive.sequences.size, 0);
});

test("byte retention evicts only sealed earlier windows and never silently drops the current window", () => {
  const archive = new SourceArchive(); archive.continuous(); archive.advanceWindow("source", 1);
  for (let item = 1; item <= 4096; item++) archive.apply(fragment(1, item, "a".repeat(1024)));
  assert.equal(archive.byteLength(), 4 * 1024 * 1024);
  assert.equal(archive.apply(fragment(1, 4097, "more")), "capacity"); assert.equal(archive.discardedCount(), 0);
  archive.advanceWindow("source", 2); assert.equal(archive.apply(fragment(2, 1, "あ".repeat(1024))), "changed");
  assert.equal(archive.discardedCount(), 3); assert.equal(archive.byteLength(), 4 * 1024 * 1024);
});

test("natural source draining can exceed 35 seconds while renewal keeps its separate bounded grace", async () => {
  const h = harness(); await h.start(); h.transcript(1, {final: false}); h.client.setNotes("Still retained");
  h.window("transcription", 1, "draining"); h.advance(100000);
  assert.equal(h.client.snapshot().aiPaused, false);
  h.transcript(1, {final: true, revision: 2});
  assert.equal(h.client.snapshot().sourceFragments[0].final, true);
  h.window("transcription", 2, "renewing"); h.advance(35001); await flush();
  assert.equal(h.client.snapshot().aiPaused, true); assert.equal(h.client.snapshot().stopped, false);
  assert.equal(h.client.snapshot().notes, "Still retained"); assert.equal(h.sockets[0].closed, false);
  assert.equal(h.calls.some(row => row.path === "/api/stop"), false); h.client.dispose();
});

test("a lost admission receipt is fenced by its source before a late result can restore startup", async () => {
  const h = harness(), pending = deferred();
  await h.client.initialize(null); h.client.setNotes("Admission note"); h.client.confirmCapture(true); await h.client.selectMeetTab(); h.client.captureStatus("remote_mix", {level: .1, frames: 50, muted: false});
  h.respond(path => path === "/api/routes" ? pending.promise : undefined);
  const startup = h.client.startMeet("en", 335000); await flush(); h.event({type: "hello", role: "operator", stopped: false}); await flush();
  assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 1);
  h.client.captureGap("remote_mix", "capture_track_ended"); await flush();
  assert.equal(h.calls.filter(row => row.path === "/api/source-gap").length, 1);
  assert.equal(h.client.snapshot().aiPaused, true); assert.equal(h.client.snapshot().notes, "Admission note");
  pending.resolve(json({state: "starting", generation: 1, route_id: "late_admitted_route"})); await startup; await flush();
  assert.equal(h.effects.capture, 0); assert.equal(h.client.snapshot().aiPaused, true);
  assert.equal(h.calls.some(row => row.path === "/api/stop"), false); assert.equal(h.client.snapshot().routes.length, 0); h.client.dispose();
});

test("an explicit operator Stop received from the server remains terminal across continuous clients", async () => {
  const h = harness(); await h.start(); h.transcript(1); h.client.setNotes("Unsaved private notes");
  h.event({type: "stopped", generation: 2, reason: "operator_stop"}); await flush();
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().notes, "");
  assert.deepEqual(h.client.snapshot().sourceFragments, []); assert.equal(h.sockets[0].closed, true); h.client.dispose();
});


function notificationFixture(permission = "granted") {
  const effects = {requests: 0, messages: [], closed: 0};
  const port = {status: () => permission, requestPermission: async () => {effects.requests++; return permission;}, notify: text => {effects.messages.push(text);}, close: () => {effects.closed++;}};
  return {port, effects};
}
function noticeBudget(allocated) {const row = budget(); row.continuous = {warning_threshold_microusd: 5_000_000, allocated_microusd: allocated, windows_started: 0}; return row;}

test("continuous cost notifications repeat at each new interval and coalesce suspended-page catchup", async () => {
  const notices = notificationFixture(), h = harness({notifications: notices.port}); let currentBudget = noticeBudget(0);
  h.respond(path => path === "/api/budget" ? json(currentBudget) : undefined);
  await h.start(); await flush(); assert.equal(notices.effects.requests, 0);
  for (const amount of [4_999_999, 5_000_000, 5_000_000, 10_000_000, 27_000_000, 20_000_000, 27_000_000]) {
    currentBudget = noticeBudget(amount); await h.client.refreshBudget();
  }
  assert.equal(notices.effects.messages.length, 3);
  assert.match(notices.effects.messages[0], /US\$5\.00/); assert.match(notices.effects.messages[1], /US\$10\.00/);
  assert.match(notices.effects.messages[2], /US\$25\.00/); assert.match(notices.effects.messages[2], /3 warning intervals crossed/);
  assert.deepEqual(h.client.snapshot().costNotice, {milestone_microusd: 25_000_000, crossed_intervals: 3});
  assert.equal(h.client.snapshot().aiPaused, false); assert.equal(h.client.snapshot().stopped, false);
  assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2);
  h.client.dispose(); assert.equal(notices.effects.closed, 1);
});

test("the initial budget baseline does not replay historical milestones and permission does not replay missed notices", async () => {
  const notices = notificationFixture("default"), h = harness({notifications: notices.port}), permission = deferred();
  notices.port.requestPermission = () => {notices.effects.requests++; return permission.promise;};
  await h.start(); await flush(); assert.equal(notices.effects.requests, 1); assert.equal(h.client.snapshot().costNotificationState, "requesting");
  // Initial fixture already has $50 in allocations. Its first snapshot is only a baseline.
  assert.equal(notices.effects.messages.length, 0); assert.equal(h.client.snapshot().costNotice, null);
  h.respond(path => path === "/api/budget" ? json({...budget(), continuous: {...budget().continuous, allocated_microusd: 50_100_000}}) : undefined);
  await h.client.refreshBudget(); assert.equal(h.client.snapshot().costNotice.milestone_microusd, 50_100_000);
  assert.equal(notices.effects.messages.length, 0);
  notices.port.status = () => "granted"; permission.resolve("granted"); await flush();
  assert.equal(h.client.snapshot().costNotificationState, "granted"); assert.equal(notices.effects.messages.length, 0);
  await h.client.enableCostNotifications(); assert.equal(notices.effects.requests, 1); h.client.dispose();
});

test("denied, unsupported and failed browser notifications never block processing or the in-app milestone", async () => {
  for (const state of ["denied", "unavailable", "throws"]) {
    const notices = notificationFixture(state === "throws" ? "granted" : state), h = harness({notifications: notices.port});
    if (state === "throws") notices.port.notify = () => {throw new Error("Synthetic notification failure");};
    let currentBudget = noticeBudget(0); h.respond(path => path === "/api/budget" ? json(currentBudget) : undefined);
    await h.start(); await flush(); currentBudget = noticeBudget(5_000_000); await h.client.refreshBudget();
    assert.equal(h.client.snapshot().meetPhase, "listening", state); assert.equal(h.client.snapshot().costNotice.milestone_microusd, 5_000_000, state);
    assert.equal(notices.effects.requests, 0); assert.equal(notices.effects.messages.length, 0); h.client.dispose();
  }
});

test("Stop fences pending permission, pending budget and reentrant milestone delivery", async () => {
  const notices = notificationFixture("default"), h = harness({notifications: notices.port}), permission = deferred(), response = deferred();
  notices.port.requestPermission = () => {notices.effects.requests++; return permission.promise;};
  let currentBudget = noticeBudget(0); h.respond(path => path === "/api/budget" ? json(currentBudget) : undefined);
  await h.start(); await flush(); h.respond(path => path === "/api/budget" ? response.promise : undefined);
  const refresh = h.client.refreshBudget(); await h.client.stop(); const stopped = h.client.snapshot();
  notices.port.status = () => "granted"; permission.resolve("granted"); currentBudget = noticeBudget(5_000_000); response.resolve(json(currentBudget));
  await refresh; await flush(); assert.deepEqual(h.client.snapshot(), stopped); assert.equal(notices.effects.messages.length, 0); assert.equal(notices.effects.closed, 1);
  const second = notificationFixture(), other = harness({notifications: second.port}); let otherBudget = noticeBudget(0);
  other.respond(path => path === "/api/budget" ? json(otherBudget) : undefined); await other.start(); await flush();
  other.client.subscribe(() => {if (other.client.snapshot().costNotice && !other.client.snapshot().stopped) void other.client.stop();});
  otherBudget = noticeBudget(5_000_000); await other.client.refreshBudget(); await flush();
  assert.equal(other.client.snapshot().stopped, true); assert.equal(second.effects.messages.length, 0); assert.equal(other.client.snapshot().costNotice, null);
});

test("authority loss fences a late notification permission result and public clients never ask", async () => {
  const notices = notificationFixture("default"), h = harness({notifications: notices.port}), permission = deferred();
  notices.port.requestPermission = () => {notices.effects.requests++; return permission.promise;};
  await h.start(); await flush(); h.respond(path => path === "/api/budget" ? json({}, 403) : undefined);
  await h.client.refreshBudget(); await flush(); assert.equal(h.client.snapshot().stopped, true); const stopped = h.client.snapshot();
  permission.resolve("granted"); await flush(); assert.deepEqual(h.client.snapshot(), stopped); assert.equal(notices.effects.messages.length, 0);
  const publicClient = new PilotClient("public", {costNotifications: notices.port});
  await publicClient.enableCostNotifications(); assert.equal(notices.effects.requests, 1); assert.equal(publicClient.snapshot().costNotice, null);
});


test("AI pause and Finish close browser notices immediately and fence pending permission", async () => {
  for (const ending of ["pause", "finish"]) {
    const notices = notificationFixture(), h = harness({notifications: notices.port}); let currentBudget = noticeBudget(0);
    h.respond(path => path === "/api/budget" ? json(currentBudget) : undefined); await h.start(); await flush();
    currentBudget = noticeBudget(5_000_000); await h.client.refreshBudget(); assert.equal(notices.effects.messages.length, 1);
    let finished;
    if (ending === "pause") h.event({type: "gap", source: "remote_mix", code: "provider_closed"});
    else finished = h.client.finishExport();
    assert.equal(notices.effects.closed, 1, ending); assert.equal(h.client.snapshot().costNotice.milestone_microusd, 5_000_000);
    currentBudget = noticeBudget(10_000_000); await h.client.refreshBudget(); assert.equal(notices.effects.messages.length, 1);
    await h.client.stop(); if (finished) assert.equal(await finished, null);
    const waiting = notificationFixture("default"), other = harness({notifications: waiting.port}), permission = deferred();
    waiting.port.requestPermission = () => permission.promise; await other.start(); await flush();
    let otherFinish;
    if (ending === "pause") {other.event({type: "gap", source: "remote_mix", code: "provider_closed"}); await flush();}
    else otherFinish = other.client.finishExport();
    const paused = other.client.snapshot(); permission.resolve("granted"); await flush();
    assert.deepEqual(other.client.snapshot(), paused); assert.equal(waiting.effects.closed, 1); assert.equal(waiting.effects.messages.length, 0);
    await other.client.stop(); if (otherFinish) assert.equal(await otherFinish, null);
  }
});


const pausedDiagnostics = () => ({version: 1, generation: 1, stopped: false, stop: null, last_issue: {code: "provider_closed", source: "remote_mix", route: "live", elapsed_seconds: 42}});
const pausedUsageDiagnostics = () => ({...pausedDiagnostics(), last_issue: {code: "usage_mismatch", source: "remote_mix", route: "transcription", elapsed_seconds: 42, asr_usage: {submitted_ms: 2400, reported_ms: 4001, allowed_ms: 3400}}});

test("paused ASR duration observation preserves notes and first cause without retry or export disclosure", async () => {
  const h = harness(); await h.start(); await flush(); h.client.setNotes("Retained synthetic note"); h.transcript(1);
  h.respond(path => path === "/api/diagnostics" ? json(pausedUsageDiagnostics()) : undefined);
  h.event({type: "gap", source: "remote_mix", code: "usage_mismatch"}); const first = h.client.snapshot().terminalMessage;
  await flush();
  assert.deepEqual(h.client.snapshot().diagnostics, pausedUsageDiagnostics());
  assert.equal(h.client.snapshot().terminalMessage, first); assert.equal(h.client.snapshot().aiPaused, true);
  assert.match(h.client.exportText(), /Retained synthetic note/);
  assert.doesNotMatch(h.client.exportText(), /asr_usage|submitted_ms|reported_ms|allowed_ms/);
  await h.client.startMeet("en", 335000); await h.client.refreshBudget(); await flush();
  assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2);
  assert.equal(h.calls.filter(row => row.path === "/api/diagnostics").length, 1); h.client.dispose();
});

test("paused diagnostics retain the first local cause and report server service/timing once after fencing", async () => {
  const h = harness(); await h.start(); await flush(); h.client.setNotes("Private incident note"); h.transcript(1, {text: "Fictional source", final: false});
  h.respond(path => path === "/api/diagnostics" ? json(pausedDiagnostics()) : undefined);
  h.event({type: "gap", source: "remote_mix", code: "provider_closed"}); const firstCause = h.client.snapshot().terminalMessage;
  h.event({type: "gap", source: "remote_mix", code: "source_gap"}); await flush();
  assert.equal(h.client.snapshot().aiPaused, true); assert.equal(h.client.snapshot().stopped, false);
  assert.equal(h.client.snapshot().terminalMessage, firstCause); assert.equal(h.client.snapshot().meetIssue, firstCause);
  assert.deepEqual(h.client.snapshot().diagnostics, pausedDiagnostics()); assert.equal(h.client.snapshot().diagnosticsState, "ready");
  assert.deepEqual(h.client.snapshot().coverageGaps, ["remote_mix"]);
  assert.equal(h.client.snapshot().sourceFragments[0].incomplete, true);
  assert.match(h.client.exportText(), /Sources with known gaps\/closure: remote_mix/); assert.match(h.client.exportText(), /Private incident note/);
  const calls = h.calls.filter(row => row.path === "/api/diagnostics"); assert.equal(calls.length, 1);
  assert.equal(calls[0].options.method, "GET"); assert.equal(calls[0].options.credentials, "omit"); assert.equal(calls[0].body, undefined);
  assert.ok(h.calls.findIndex(row => row.path === "/api/source-gap") < h.calls.findIndex(row => row.path === "/api/diagnostics"));
  h.advance(60000); await h.client.refreshBudget(); await flush();
  assert.equal(h.calls.filter(row => row.path === "/api/diagnostics").length, 1); assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2); h.client.dispose();
});

test("Stop gets a separate diagnostic observation and fences the earlier pending pause result", async () => {
  const h = harness(), pending = deferred(); await h.start(); await flush(); let reads = 0;
  const stoppedRecord = {version: 1, generation: 2, stopped: true, stop: {code: "operator_stop", elapsed_seconds: 99}, last_issue: pausedDiagnostics().last_issue};
  h.respond(path => path === "/api/diagnostics" ? ++reads === 1 ? pending.promise : json(stoppedRecord) : undefined);
  h.event({type: "gap", source: "remote_mix", code: "provider_closed"}); await flush(); assert.equal(reads, 1);
  await h.client.stop(); const stopped = h.client.snapshot(); assert.deepEqual(stopped.diagnostics, stoppedRecord);
  pending.resolve(json(pausedUsageDiagnostics())); await flush(); assert.deepEqual(h.client.snapshot(), stopped);
  await h.client.stop(); assert.equal(reads, 2); assert.equal(h.client.snapshot().diagnosticsState, "ready");
});

test("paused diagnostic authorization loss clears private state without retrying or erasing the first interruption", async () => {
  for (const status of [401, 403]) {
    const h = harness(); await h.start(); await flush(); h.client.setNotes("Unsaved private note"); h.transcript(1);
    h.respond(path => path === "/api/diagnostics" ? json({}, status) : undefined);
    h.event({type: "gap", source: "remote_mix", code: "provider_closed"}); const firstCause = h.client.snapshot().terminalMessage; await flush();
    assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().notes, ""); assert.deepEqual(h.client.snapshot().sourceFragments, []);
    assert.equal(h.client.snapshot().terminalMessage, firstCause); assert.match(h.client.snapshot().message, /Diagnostic authorization was lost/);
    assert.equal(h.client.snapshot().diagnostics, null); assert.equal(h.client.snapshot().diagnosticsState, "unavailable");
    assert.equal(h.calls.filter(row => row.path === "/api/diagnostics").length, 1); assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2);
  }
});

test("unavailable or invalid paused diagnostics preserve export and cannot become positive authority", async () => {
  for (const result of [() => json({}, 503), () => json({...pausedDiagnostics(), raw_provider_error: "Untrusted detail"}), () => json({...pausedUsageDiagnostics(), last_issue: {...pausedUsageDiagnostics().last_issue, asr_usage: {...pausedUsageDiagnostics().last_issue.asr_usage, text: "Untrusted detail"}}}), () => json({padding: "x".repeat(3000)})]) {
    const h = harness(); await h.start(); await flush(); h.client.setNotes("Retain for export"); h.transcript(1);
    h.respond(path => path === "/api/diagnostics" ? result() : undefined);
    h.event({type: "gap", source: "remote_mix", code: "provider_closed"}); await flush();
    assert.equal(h.client.snapshot().aiPaused, true); assert.equal(h.client.snapshot().stopped, false); assert.equal(h.client.snapshot().diagnosticsState, "unavailable");
    assert.equal(h.client.snapshot().diagnostics, null); assert.match(h.client.exportText(), /Retain for export/);
    assert.doesNotMatch(JSON.stringify(h.client.snapshot()), /Untrusted detail/);
    assert.equal(h.calls.filter(row => row.path === "/api/diagnostics").length, 1);
    await h.client.startMeet("en", 335000); assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2); h.client.dispose();
  }
});

test("Finish, signout and page disposal fence late paused diagnostics", async () => {
  for (const ending of ["finish", "logout", "dispose"]) {
    const h = harness(), pending = deferred(); await h.start(); await flush(); h.client.setNotes("Retained note");
    h.respond(path => path === "/api/diagnostics" ? pending.promise : path === "/api/logout" ? json({state: "signed_out"}) : undefined);
    h.event({type: "gap", source: "remote_mix", code: "provider_closed"}); await flush();
    let finish;
    if (ending === "finish") finish = h.client.finishExport();
    else if (ending === "logout") await h.client.logout();
    else h.client.dispose();
    const snapshot = h.client.snapshot(); pending.resolve(json(pausedUsageDiagnostics())); await flush();
    assert.deepEqual(h.client.snapshot(), snapshot, ending);
    if (finish) {h.client.dispose(); assert.equal(await finish, null);} else assert.equal(snapshot.notes, "");
    assert.equal(h.calls.filter(row => row.path === "/api/diagnostics").length, 1);
  }
});


test("each valid Live terminal reason pauses the pair and survives fixed diagnostic readback without retry", async () => {
  for (const code of ["live_closed_requested", "live_expired", "live_content_closed", "live_remote_hangup", "live_connection_lost"]) {
    const h = harness(); await h.start(); await flush(); h.client.setNotes("Keep the incident note"); h.transcript(1);
    h.respond(path => path === "/api/diagnostics" ? json({...pausedDiagnostics(), last_issue: {...pausedDiagnostics().last_issue, code}}) : undefined);
    h.event({type: "gap", source: "remote_mix", code}); await flush();
    assert.equal(h.client.snapshot().aiPaused, true, code); assert.equal(h.client.snapshot().stopped, false, code);
    assert.equal(h.client.snapshot().diagnostics.last_issue.code, code); assert.match(h.client.snapshot().terminalMessage, /GPT-Live-1/);
    assert.match(h.client.exportText(), /Keep the incident note/); assert.match(h.client.exportText(), /known gaps\/closure: remote_mix/);
    await h.client.startMeet("en", 335000); assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2); h.client.dispose();
  }
});

test("a stalled paused diagnostic read aborts at its deadline and leaves a usable retained export", async () => {
  const h = harness(); await h.start(); await flush(); h.client.setNotes("Retained after diagnostic timeout"); let aborted = false;
  h.respond((path, _body, options) => path === "/api/diagnostics" ? new Promise((_resolve, reject) => {
    options.signal.addEventListener("abort", () => {aborted = true; reject(new Error("Synthetic stalled diagnostic"));}, {once: true});
  }) : undefined);
  h.event({type: "gap", source: "remote_mix", code: "provider_closed"}); await flush();
  await new Promise(resolve => setTimeout(resolve, 5100));
  assert.equal(aborted, true); assert.equal(h.client.snapshot().diagnosticsState, "unavailable");
  assert.equal(h.client.snapshot().aiPaused, true); assert.equal(h.client.snapshot().stopped, false);
  assert.match(h.client.exportText(), /Retained after diagnostic timeout/);
  assert.equal(h.calls.filter(row => row.path === "/api/diagnostics").length, 1); assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 2); h.client.dispose();
});

test("drafts follow continuous window scope, recovery clears them and stale drafts never reappear", async () => {
  const h = harness(); await h.start();
  const draft = window => ({type: "source_draft", route_id: "transcription", source: "remote_mix", route: "transcription", item_id: "hypothesis", revision: 1, text: "Fictional early words", retired: false, window});
  h.event(draft(1)); assert.equal(h.client.snapshot().sourceDrafts.length, 1);
  h.event({type: "window", route_id: "transcription", source: "remote_mix", route: "transcription", window: 2, state: "recovering", code: "provider_connection_lost", retry_in_seconds: 1});
  assert.equal(h.client.snapshot().sourceDrafts.length, 0);
  h.event(draft(1)); assert.equal(h.client.snapshot().sourceDrafts.length, 0);
  h.route("transcription"); h.window("transcription", 2, "ready"); h.event(draft(2));
  assert.equal(h.client.snapshot().sourceDrafts.length, 1);
  h.event({type: "gap", source: "remote_mix", code: "provider_protocol"});
  assert.equal(h.client.snapshot().aiPaused, true); assert.equal(h.client.snapshot().sourceDrafts.length, 0);
  h.event(draft(2)); assert.equal(h.client.snapshot().sourceDrafts.length, 0);
  h.client.dispose();
});

const continuousDraft = (overrides = {}) => ({type: "source_draft", route_id: "transcription", source: "remote_mix", route: "transcription", item_id: "unbound_speech", revision: 1, text: "Unconfirmed fictional words", retired: false, window: 1, ...overrides});

test("planned renewal waits for draft retirement and its bound source final without discarding the confirmed archive", async () => {
  const h = harness(); await h.start(); h.client.setNotes("Keep this note");
  h.transcript(1, {id: "confirmed", sequence: 1, text: "Earlier confirmed words"});
  h.event(continuousDraft()); h.window("transcription", 1, "draining");
  assert.equal(h.client.snapshot().aiPaused, false);
  assert.equal(h.client.snapshot().sourceDrafts[0].text, "Unconfirmed fictional words");
  h.event(continuousDraft({text: "", retired: true}));
  h.transcript(1, {id: "unbound_speech", sequence: 2, text: "Later confirmed words", final: true});
  h.window("transcription", 2, "renewing"); h.route("transcription"); h.window("transcription", 2, "ready");
  assert.equal(h.client.snapshot().aiPaused, false); assert.equal(h.client.snapshot().stopped, false);
  assert.deepEqual(h.client.snapshot().sourceFragments.map(fragment => fragment.text), ["Earlier confirmed words", "Later confirmed words"]);
  assert.deepEqual(h.client.snapshot().sourceDrafts, []);
  h.event(continuousDraft()); assert.deepEqual(h.client.snapshot().sourceDrafts, []);
  h.event(continuousDraft({window: 2, text: "New window draft"}));
  assert.equal(h.client.snapshot().sourceDrafts[0].text, "New window draft");
  assert.match(h.client.exportText(), /Keep this note/); assert.doesNotMatch(h.client.exportText(), /Unconfirmed fictional words|New window draft/);
  assert.equal(h.effects.capture, 1); assert.equal(h.effects.stopped, 0); h.client.dispose();
});

for (const unresolved of ["active", "retired_without_binding"]) {
  test(`planned renewal with ${unresolved === "active" ? "an active" : "a retired but unbound"} draft pauses independently of server claims and retains confirmed work`, async () => {
    const h = harness(); await h.start(); h.client.setNotes("Retain confirmed work");
    h.transcript(1, {id: "confirmed", sequence: 1, text: "Earlier confirmed words"});
    h.event(continuousDraft()); h.window("transcription", 1, "draining");
    if (unresolved === "retired_without_binding") h.event(continuousDraft({retired: true, text: ""}));
    h.window("transcription", 2, "renewing"); await flush();
    assert.equal(h.client.snapshot().aiPaused, true); assert.equal(h.client.snapshot().stopped, false);
    assert.deepEqual(h.client.snapshot().sourceDrafts, []);
    assert.deepEqual(h.client.snapshot().sourceFragments.map(fragment => fragment.text), ["Earlier confirmed words"]);
    assert.match(h.client.exportText(), /Earlier confirmed words/); assert.match(h.client.exportText(), /Retain confirmed work/);
    assert.doesNotMatch(h.client.exportText(), /Unconfirmed fictional words/);
    const effects = h.effects.capture;
    h.route("transcription"); h.window("transcription", 2, "ready"); h.event(continuousDraft({window: 2})); await flush();
    assert.equal(h.client.snapshot().aiPaused, true); assert.deepEqual(h.client.snapshot().sourceDrafts, []); assert.equal(h.effects.capture, effects);
    assert.equal(h.calls.some(call => call.path === "/api/stop"), false); assert.equal(h.effects.stopped, 1); h.client.dispose();
  });
}

for (const status of [401, 403]) {
  test(`drafts and confirmed work are cleared on operator authorization loss (HTTP ${status}), with late speech fenced`, async () => {
    const h = harness(); await h.start(); h.client.setNotes("Private note");
    h.transcript(1, {id: "confirmed", sequence: 1, text: "Confirmed private words"}); h.event(continuousDraft());
    const late = h.sockets[0].onmessage, admissions = h.calls.filter(call => call.path === "/api/routes").length;
    h.respond(path => path === "/api/budget" ? json({error: "fixture_unauthorized"}, status) : undefined);
    await h.client.refreshBudget(); await flush();
    assert.equal(h.client.snapshot().stopped, true); assert.deepEqual(h.client.snapshot().sourceDrafts, []);
    assert.deepEqual(h.client.snapshot().sourceFragments, []); assert.equal(h.client.snapshot().notes, ""); assert.equal(h.client.exportText(), null);
    late({data: JSON.stringify({generation: 1, ...continuousDraft({revision: 2, text: "Unconfirmed fictional words that arrived late"})})});
    late({data: JSON.stringify({generation: 1, type: "transcript", route_id: "transcription", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "Late final cannot restore", final: true, item: {id: "unbound_speech", revision: 1, sequence: 2}, window: 1})});
    assert.deepEqual(h.client.snapshot().sourceDrafts, []); assert.deepEqual(h.client.snapshot().sourceFragments, []);
    assert.equal(h.calls.filter(call => call.path === "/api/routes").length, admissions); assert.equal(h.effects.capture, 1); assert.equal(h.effects.stopped, 1); h.client.dispose();
  });
}
