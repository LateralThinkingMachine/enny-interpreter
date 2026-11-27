import assert from "node:assert/strict";
import test from "node:test";
import {tabSession} from "../.test-build/session-transport.js";
import {PilotClient, takeEntry} from "../.test-build/client.js";
import {budgetFixture, disabledBudget} from "./budget-fixture.mjs";

const session = role => ({role, csrf: "synthetic_csrf", generation: 1, stopped: false, providers_enabled: true, expires_in_seconds: 7200, capabilities: {source_captions: role === "operator", private_translation: role === "operator", live_assistant: role === "operator", live_interpreter: role === "operator", prepared_public_speech: true}, limitations: ["Synthetic test only"]});
const preview = overrides => ({id: "preview_one", generation: 1, text: "Exact reviewed sentence.", language: "en", digest: "a".repeat(64), audio_path: "/api/previews/preview_one/audio", duration_ms: 100, auditioned: false, state: "prepared", ...overrides});
const delivery = overrides => ({type: "delivery", generation: 1, delivery_id: "delivery_one", text: "Exact public sentence.", language: "en", digest: "a".repeat(64), audio_path: "/api/public/audio/delivery_one", duration_ms: 100, ...overrides});
const deferred = () => {let resolve; const promise = new Promise(done => {resolve = done;}); return {promise, resolve};};
const flush = async () => {for (let i = 0; i < 8; i++) await new Promise(resolve => setImmediate(resolve));};
const json = (value, status = 200) => new Response(JSON.stringify(value), {status, headers: {"Content-Type": "application/json"}});
const wav = () => new Response(new Uint8Array(44), {headers: {"Content-Type": "audio/wav"}});

function harness(role = "operator") {
  let now = 0, tick = () => {}, respond;
  const calls = [], sockets = [], effects = {captures: [], sourceStops: [], allStops: 0, private: [], clip: 0, broadcast: 0, resume: 0, mutes: []};
  const audio = {startCapture: async source => {effects.captures.push(source);}, stopCapture: source => effects.sourceStops.push(source), stopAll: () => {effects.allStops++;}, resumePlayback: async () => {effects.resume++;}, setPrivateMuted: (...args) => effects.mutes.push(args), playPrivate: (...args) => effects.private.push(args), stopClip: () => {}, playClip: async (_bytes, callbacks) => {assert.equal(callbacks.isCurrent(), true); effects.clip++; callbacks.started(); callbacks.ended();}};
  const dependencies = {audio, now: () => now, interval: callback => {tick = callback; return () => {tick = () => {};};}, broadcastStop: () => {effects.broadcast++;}, verifyClip: async () => true, openPublicWindow: url => effects.opened = url,
    fetch: async (path, options = {}) => {calls.push({path, body: options.body && JSON.parse(options.body), options}); if (respond) {const answer = respond(path, options); if (answer !== undefined) return answer;} if (path.endsWith("/session") || path.endsWith("/bootstrap")) return json(session(role)); if (path === "/api/budget") return json(budgetFixture()); if (path === "/api/stop" || path.endsWith("/logout")) return json({state: "stopped", generation: 2}); if (path === "/api/public/receipt") return json({state: "recorded", generation: 1}); if (path === "/api/source-gap") return json({state: "closed", generation: 1}); throw new Error("Unexpected synthetic route");},
    socket: url => {const socket = {url, readyState: 1, bufferedAmount: 0, onopen: null, onmessage: null, onclose: null, onerror: null, sent: [], closed: false, send(data) {this.sent.push(data);}, close() {this.closed = true;}}; sockets.push(socket); return socket;},
  };
  const client = new PilotClient(role, dependencies);
  let itemSequence = 0;
  const event = value => {if (value.type === "route" && !Object.hasOwn(value, "live_mode")) value = {...value, live_mode: value.route === "live" ? "assistant" : null}; if (value.type === "transcript" && !Object.hasOwn(value, "item")) value = {...value, item: value.provenance === "source_asr" ? {id: `item_${++itemSequence}`, revision: 1, sequence: itemSequence} : null}; sockets.at(-1).onmessage?.({data: JSON.stringify(value)});};
  return {client, calls, sockets, effects, audio, dependencies, event, respond: callback => {respond = callback;}, advance: milliseconds => {now += milliseconds; tick();}, elapse: milliseconds => {now += milliseconds;}, async ready() {await client.initialize(null); await client.connect(); event({type: "hello", generation: 1, role, stopped: false}); client.confirmCapture(true);}, route(route = "transcription", source = "remote_mix", route_id = "route_one", live_mode = route === "live" ? "assistant" : null) {event({type: "route", generation: 1, route_id, source, route, live_mode, language: "en", state: "ready", code: null, expires_in_seconds: 600});}};
}

test("operator allowance initialization is read-only; public never requests or retains private budget", async () => {
  const operator = harness(); await operator.client.initialize(null);
  assert.equal(operator.client.snapshot().budgetFresh, true);
  const request = operator.calls.find(row => row.path === "/api/budget");
  assert.equal(request.options.method, "GET"); assert.equal(request.options.body, undefined);
  assert.equal(request.options.redirect, "error"); assert.equal(request.options.cache, "no-store");
  assert.equal(operator.sockets.length, 0); assert.deepEqual(operator.effects.captures, []); operator.client.dispose();
  const viewer = harness("public"); await viewer.ready(); await viewer.client.refreshBudget();
  await viewer.client.openRoute("remote_mix", "live", "ja", "assistant"); await viewer.client.prepare("secret", "en");
  assert.equal(viewer.calls.some(row => ["/api/budget", "/api/routes", "/api/previews"].includes(row.path)), false);
  assert.equal(viewer.client.snapshot().budget, null); viewer.client.dispose();
});

test("disabled allowance preserves inspection but refuses every paid action", async () => {
  const h = harness(); h.respond(path => path === "/api/budget" ? json(disabledBudget()) : undefined); await h.ready();
  await h.client.openRoute("remote_mix", "transcription", "ja"); await h.client.prepare("hello", "en");
  assert.equal(h.client.snapshot().stopped, false); assert.equal(h.client.snapshot().budget.state, "disabled");
  assert.equal(h.calls.some(row => ["/api/routes", "/api/previews"].includes(row.path)), false); h.client.dispose();
});

test("pending or malformed allowance disables paid work while keeping verified captured state separate", async () => {
  const h = harness(), pending = deferred(); await h.ready(); h.client.setNotes("private notes");
  h.respond(path => path === "/api/budget" ? pending.promise : undefined);
  const refresh = h.client.refreshBudget(); await flush();
  assert.equal(h.client.snapshot().budgetFresh, false); await h.client.openRoute("self", "live", "hi", "assistant");
  pending.resolve(json({generation: 1, amounts: {approved_microusd: 1000000}})); await refresh;
  assert.equal(h.client.snapshot().budgetFresh, false); assert.equal(h.client.snapshot().notes, "private notes");
  await h.client.prepare("No allowance", "en"); assert.equal(h.calls.some(row => ["/api/routes", "/api/previews"].includes(row.path)), false); h.client.dispose();
});

test("budget authorization loss clears private state; Stop aborts delayed allowance and prevents recovery", async () => {
  const denied = harness(); await denied.ready(); denied.client.setNotes("private"); denied.respond(path => path === "/api/budget" ? json({}, 403) : undefined);
  await denied.client.refreshBudget(); assert.equal(denied.client.snapshot().stopped, true); assert.equal(denied.client.snapshot().budget, null); assert.equal(denied.client.snapshot().notes, "");
  const h = harness(), pending = deferred(); await h.ready(); h.respond(path => path === "/api/budget" ? pending.promise : undefined);
  const action = h.client.refreshBudget(); await flush(); const request = h.calls.filter(row => row.path === "/api/budget").at(-1);
  await h.client.stop(); assert.equal(request.options.signal.aborted, true); pending.resolve(json(budgetFixture())); await action;
  assert.equal(h.client.snapshot().budget, null); assert.equal(h.client.snapshot().budgetFresh, false);
  const count = h.calls.length; await h.client.refreshBudget(); assert.equal(h.calls.length, count);
});

test("reentrant Stop before allowance I/O or paid dispatch issues no new positive request", async () => {
  const h = harness(); await h.ready(); const initial = h.calls.filter(row => row.path === "/api/budget").length;
  h.client.subscribe(() => {if (h.client.snapshot().budgetRefreshing && !h.client.snapshot().stopped) void h.client.stop();});
  await h.client.refreshBudget(); assert.equal(h.calls.filter(row => row.path === "/api/budget").length, initial); assert.equal(h.client.snapshot().budget, null);
  const paid = harness(); await paid.ready(); paid.client.subscribe(() => {if (!paid.client.snapshot().budgetFresh && !paid.client.snapshot().stopped) void paid.client.stop();});
  await paid.client.openRoute("self", "live", "en", "assistant"); assert.equal(paid.calls.some(row => row.path === "/api/routes"), false);
});

test("15-second refresh admits no timer-driven paid work or automatic retry after refused allocation", async () => {
  const h = harness(); await h.ready(); let reads = 0;
  h.respond(path => {if (path === "/api/routes") return json({}, 429); if (path === "/api/budget") {reads++; return json(budgetFixture());}});
  await h.client.openRoute("remote_mix", "transcription", "ja");
  assert.equal(reads, 1); assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 1); assert.equal(h.client.snapshot().routes.length, 0);
  for (let i = 0; i < 15; i++) {h.event({type: "heartbeat", generation: 1}); h.advance(1000); await flush();}
  assert.equal(reads, 2); assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 1); h.client.dispose();
});

test("a stale allowance cannot dispatch paid work when timer callbacks are delayed but WS heartbeat is current", async () => {
  const h = harness(); await h.ready(); h.respond(path => path === "/api/routes" ? json({state: "starting", generation: 1, route_id: "late_quote"}) : undefined);
  for (let i = 0; i < 8; i++) {h.elapse(2000); h.event({type: "heartbeat", generation: 1});}
  assert.equal(h.client.snapshot().stopped, false); assert.equal(h.client.snapshot().connected, true);
  await h.client.openRoute("self", "live", "en", "assistant");
  assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 0); h.client.dispose();
});

test("TTS admission uses actual UTF-8 affordability and refreshes allowance even after refusal", async () => {
  const h = harness(); let prepares = 0, reads = 0;
  const low = budgetFixture({amounts: {approved_microusd: 20_000_000, reserved_microusd: 19_999_700, unallocated_microusd: 300}});
  low.routes = low.routes.map(row => ({...row, can_reserve: false})); low.tts.can_reserve_maximum = false;
  h.respond(path => {if (path === "/api/budget") {reads++; return json(low);} if (path === "/api/previews") {prepares++; return json({}, 429);}});
  await h.ready(); await h.client.prepare("あह😀a", "ja"); assert.equal(prepares, 0);
  await h.client.prepare("あह😀", "ja"); assert.equal(prepares, 1); assert.equal(reads, 2);
  assert.equal(h.client.snapshot().preview, null); assert.equal(h.client.snapshot().stopped, false); await flush(); assert.equal(prepares, 1); h.client.dispose();
});

test("budget refresh after successful route admission displays the new reserved amount", async () => {
  const h = harness(); await h.ready(); const after = budgetFixture({amounts: {approved_microusd: 20_000_000, reserved_microusd: 170000, unallocated_microusd: 19_830_000}, stream_reservations: {used: 1, limit: 16}});
  h.respond(path => path === "/api/routes" ? json({state: "starting", generation: 1, route_id: "paid_one"}) : path === "/api/budget" ? json(after) : undefined);
  await h.client.openRoute("self", "transcription", "en"); assert.equal(h.client.snapshot().budgetFresh, true); assert.equal(h.client.snapshot().budget.amounts.reserved_microusd, 170000);
  assert.equal(h.client.snapshot().routes[0].route_id, "paid_one"); h.client.dispose();
});

test("a pre-allocation read cannot become a fresh balance after a later paid admission completes", async () => {
  const h = harness(), admission = deferred(), oldRead = deferred(); await h.ready();
  let accepted = false, overlapRead = false;
  const after = budgetFixture({amounts: {approved_microusd: 20_000_000, reserved_microusd: 170000, unallocated_microusd: 19_830_000}, stream_reservations: {used: 1, limit: 16}});
  h.respond(path => {
    if (path === "/api/routes") return admission.promise;
    if (path === "/api/budget") {
      if (!accepted) {overlapRead = true; return oldRead.promise;}
      return json(after);
    }
  });
  const action = h.client.openRoute("self", "transcription", "en"); await flush();
  const read = h.client.refreshBudget(); await flush(); accepted = true;
  admission.resolve(json({state: "starting", generation: 1, route_id: "reserved_after_read"}));
  // Release the old read after the paid admission; a valid implementation may
  // discard/refetch it or refuse overlap entirely, but cannot call it current.
  await flush(); oldRead.resolve(json(budgetFixture())); await Promise.all([read, action]); await flush();
  assert.equal(h.client.snapshot().routes[0].route_id, "reserved_after_read");
  assert.ok(!h.client.snapshot().budgetFresh || h.client.snapshot().budget.amounts.reserved_microusd === 170000,
    `pre-allocation balance was published fresh; overlapping read admitted=${overlapRead}`);
  h.client.dispose();
});

test("route allocation notifications invalidate the allowance before reentrant paid actions", async () => {
  const h = harness(); await h.ready(); let reacted = false;
  h.respond(path => path === "/api/routes" ? json({state: "starting", generation: 1, route_id: "reentrant_paid"}) : undefined);
  h.client.subscribe(() => {
    if (!reacted && h.client.snapshot().routes.length > 0) {reacted = true; void h.client.openRoute("self", "live", "en", "assistant");}
  });
  h.route(); await flush();
  assert.equal(h.client.snapshot().stopped, false);
  assert.equal(h.calls.filter(row => row.path === "/api/routes").length, 0); h.client.dispose();
});

test("source interim updates replace in place and exact final appears only once in export", async () => {
  const h = harness(); await h.ready(); h.route();
  const value = {type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "Ken mee", final: false, item: {id: "source_one", revision: 1, sequence: 1}};
  h.event(value); h.event({...value, text: "Ken meets Asha.", final: true, item: {...value.item, revision: 2}});
  assert.equal(h.client.snapshot().sourceFragments.length, 1); assert.equal(h.client.snapshot().sourceFragments[0].text, "Ken meets Asha.");
  assert.equal(h.client.exportText().split("Ken meets Asha.").length, 2); assert.doesNotMatch(h.client.exportText(), /Ken mee\n/);
  await h.client.stop(); h.event({...value, text: "late private text", item: {...value.item, revision: 3}}); assert.deepEqual(h.client.snapshot().sourceFragments, []);
});
test("explicit route closure preserves unresolved item as incomplete and ignores its late final", async () => {
  const h = harness(); await h.ready(); h.route(); h.respond(path => path === "/api/routes/close" ? json({state: "closed", generation: 1}) : undefined);
  const value = {type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "unfinished thought", final: false, item: {id: "source_one", revision: 1, sequence: 1}};
  h.event(value); await h.client.closeRoute("route_one"); h.event({...value, final: true, item: {...value.item, revision: 2}});
  assert.equal(h.client.snapshot().stopped, false); assert.equal(h.client.snapshot().sourceFragments[0].incomplete, true); assert.equal(h.client.snapshot().sourceFragments[0].final, false);
  assert.match(h.client.exportText(), /incomplete at closure/); h.client.dispose();
});
test("reentrant Stop during expiry mute cannot restore route or source text", async () => {
  const h = harness(); await h.ready(); h.route("live");
  h.client.subscribe(() => {if (h.client.snapshot().muted.live && !h.client.snapshot().stopped) void h.client.stop();});
  h.advance(600001); assert.equal(h.client.snapshot().stopped, true); assert.deepEqual(h.client.snapshot().routes, []); assert.deepEqual(h.client.snapshot().sourceFragments, []);
});
test("capture requires explicit current attestation and withdrawal closes pending or active capture", async () => {
  const h = harness(); await h.ready(); h.route(); h.client.confirmCapture(false);
  await h.client.capture("remote_mix"); assert.deepEqual(h.effects.captures, []);
  h.client.confirmCapture(true); await h.client.capture("remote_mix"); assert.deepEqual(h.effects.captures, ["remote_mix"]);
  h.client.confirmCapture(false); assert.equal(h.client.snapshot().capture.remote_mix, "closed"); assert.deepEqual(h.effects.sourceStops, ["remote_mix"]);
  await h.client.stop(); h.client.confirmCapture(true); assert.equal(h.client.snapshot().captureConfirmed, false);
});

test("entry capability is scrubbed before validation, role does not follow arbitrary fragments", () => {
  const calls = [];
  assert.deepEqual(takeEntry({origin: "http://127.0.0.1:5182", pathname: "/public", hash: "#ticket=one_use_ticket"}, {replaceState: (...args) => calls.push(args)}), {role: "public", ticket: "one_use_ticket"});
  assert.deepEqual(calls, [[null, "", "/public"]]);
  assert.throws(() => takeEntry({origin: "https://attacker.invalid", pathname: "/", hash: "#bootstrap=secret"}, {replaceState: (...args) => calls.push(args)}));
  assert.equal(calls.length, 2); assert.throws(() => takeEntry({origin: "http://127.0.0.1:5182", pathname: "/public", hash: "#bootstrap=operator_secret"}, {replaceState: () => {}}));
});

test("session restore never connects sockets, opens devices or enables playback", async () => {
  const h = harness(); await h.client.initialize(null);
  assert.equal(h.sockets.length, 0); assert.deepEqual(h.effects.captures, []); assert.equal(h.effects.resume, 0); assert.equal(h.client.snapshot().connected, false); assert.deepEqual(h.client.snapshot().muted, {translation: true, live: true, live_interpretation: true}); h.client.dispose();
});

test("Stop reentered during initial busy notification admits no bootstrap request", async () => {
  const h = harness(); h.client.subscribe(() => {if (h.client.snapshot().busy && !h.client.snapshot().stopped) void h.client.stop();});
  await h.client.initialize("must_not_redeem"); assert.equal(h.calls.length, 0); assert.equal(h.client.snapshot().session, null); assert.equal(h.client.snapshot().stopped, true);
});

test("Stop latches before reentrant subscribers and prevents new positive network work", async () => {
  const h = harness(); await h.ready(); h.client.setNotes("private draft");
  h.client.subscribe(() => {if (h.client.snapshot().stopped) {void h.client.prepare("must not send", "en"); void h.client.connect(); h.client.setNotes("must not restore");}});
  await h.client.stop();
  assert.equal(h.effects.allStops, 1); assert.equal(h.client.snapshot().notes, ""); assert.equal(h.client.snapshot().stopped, true); assert.equal(h.sockets.length, 1); assert.equal(h.sockets[0].closed, true); assert.equal(h.calls.filter(call => call.path === "/api/previews").length, 0); assert.equal(h.effects.broadcast, 1);
});

test("Stop aborts delayed preview preparation and never restores its wording", async () => {
  const h = harness(), pending = deferred(); await h.ready(); h.respond(path => path === "/api/previews" ? pending.promise : undefined);
  const action = h.client.prepare("Exact reviewed sentence.", "en"); await flush();
  const request = h.calls.find(call => call.path === "/api/previews"); await h.client.stop(); assert.equal(request.options.signal.aborted, true);
  pending.resolve(json(preview())); await action;
  assert.equal(h.client.snapshot().preview, null); assert.equal(h.client.snapshot().stopped, true);
});

test("ready route and explicit capture are both required; private sound starts muted", async () => {
  const h = harness(); await h.ready(); await h.client.capture("remote_mix"); assert.deepEqual(h.effects.captures, []);
  h.route(); await h.client.capture("remote_mix"); assert.deepEqual(h.effects.captures, ["remote_mix"]);
  const frame = new ArrayBuffer(646); const header = new DataView(frame); header.setUint8(0, 1); assert.equal(h.client.sendFrame("remote_mix", frame), true);
  h.route("translation", "remote_mix", "translation_one");
  const audio = {type: "audio", generation: 1, route_id: "translation_one", source: "remote_mix", route: "translation", pcm16_base64: "AAA=", sample_rate: 24000};
  h.event(audio); assert.equal(h.effects.private.length, 0); await h.client.mute("translation", false); h.event(audio); assert.equal(h.effects.private.length, 1);
  await h.client.mute("translation", true); h.event(audio); assert.equal(h.effects.private.length, 1); assert.deepEqual(h.client.snapshot().muted, {translation: true, live: true, live_interpretation: true}); h.client.dispose();
});

test("source backpressure stops only its source and never relabels a microphone frame", async () => {
  const h = harness(); await h.ready(); h.route(); await h.client.capture("remote_mix");
  const wrong = new ArrayBuffer(646); const view = new DataView(wrong); view.setUint8(0, 1); view.setUint8(1, 1);
  assert.equal(h.client.sendFrame("remote_mix", wrong), false); assert.equal(h.client.snapshot().capture.remote_mix, "closed"); assert.equal(h.client.snapshot().capture.self, "idle");
  await h.client.capture("remote_mix"); assert.equal(h.effects.captures.length, 1); await flush(); assert.equal(h.calls.filter(call => call.path === "/api/source-gap").length, 1); h.client.dispose();
});

test("public role refuses a private transcript even under a valid generation", async () => {
  const h = harness("public"); await h.ready();
  h.event({type: "transcript", generation: 1, route_id: "private_route", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "must never display", final: true});
  assert.equal(h.client.snapshot().stopped, true); assert.deepEqual(h.client.snapshot().fragments, []); assert.equal(h.effects.clip, 0);
});

test("source ASR survives beyond500 fragments and generated provenance cannot masquerade as source", async () => {
  const h = harness(); await h.ready(); h.route();
  for (let i = 0; i < 550; i++) h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: `fragment${i}`, final: i % 2 === 0});
  assert.equal(h.client.snapshot().sourceFragments.length, 550); assert.equal(h.client.snapshot().sourceFragments[0].text, "fragment0"); assert.equal(h.client.snapshot().fragments.length, 0);
  h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "live_output", text: "not source", final: true}); assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().fragments.length, 0);
});

test("heartbeat expiry is a sticky audio fence, no reconnect or public replay", async () => {
  const h = harness("public"); await h.ready(); h.advance(3001);
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.effects.allStops, 1); await h.client.connect(); assert.equal(h.sockets.length, 1);
});

test("late public digest verification after Stop cannot play or restore text", async () => {
  const h = harness("public"), verified = deferred(); h.dependencies.verifyClip = () => verified.promise; await h.ready(); h.respond(path => path.startsWith("/api/public/audio/") ? wav() : undefined);
  h.event(delivery()); await flush(); await h.client.stop(); verified.resolve(true); await flush(); assert.equal(h.effects.clip, 0); assert.equal(h.client.snapshot().delivery, null);
});

test("public clip uses exact authorized path and receipts; duplicate delivery never replays", async () => {
  const h = harness("public"); await h.ready(); h.respond(path => path === "/api/public/audio/delivery_one" ? wav() : undefined);
  h.event(delivery()); await flush(); assert.equal(h.effects.clip, 1); assert.equal(h.client.snapshot().delivery?.state, "playback_ended");
  assert.deepEqual(h.calls.filter(call => call.path === "/api/public/receipt").map(call => call.body.state), ["accepted", "playback_started", "playback_ended"]);
  h.event(delivery()); await flush(); assert.equal(h.effects.clip, 1); assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().delivery, null);
});

test("mismatched public digest cannot display or play approved-looking words", async () => {
  const h = harness("public"); h.dependencies.verifyClip = async () => false; await h.ready(); h.respond(path => path.startsWith("/api/public/audio/") ? wav() : undefined);
  h.event(delivery()); await flush(); assert.equal(h.effects.clip, 0); assert.equal(h.client.snapshot().delivery, null); assert.equal(h.client.snapshot().stopped, true);
});

test("approval needs separate full audition and acknowledgement of the same immutable clip", async () => {
  const h = harness(); await h.ready(); let acknowledged = false;
  h.respond(path => {if (path === "/api/previews") return json(preview()); if (path.endsWith("/audio")) return wav(); if (path.endsWith("/audition")) {acknowledged = true; return json(preview({auditioned: true}));} if (path.endsWith("/approve")) return json({state: "offered", generation: 1, delivery_id: "delivery_one"});});
  await h.client.prepare("Exact reviewed sentence.", "en"); await h.client.approve(); assert.equal(h.calls.filter(call => call.path.endsWith("/approve")).length, 0);
  await h.client.acknowledge(); assert.equal(acknowledged, false); await h.client.audition(); assert.equal(h.client.snapshot().auditionComplete, true);
  await h.client.approve(); assert.equal(h.calls.filter(call => call.path.endsWith("/approve")).length, 0);
  await h.client.acknowledge(); await h.client.approve(); await h.client.approve(); assert.equal(h.calls.filter(call => call.path.endsWith("/approve")).length, 1); assert.equal(h.client.snapshot().preview.state, "offered"); h.client.dispose();
});

test("server cannot replace preview wording during acknowledgement", async () => {
  const h = harness(); await h.ready(); h.respond(path => path === "/api/previews" ? json(preview()) : path.endsWith("/audio") ? wav() : path.endsWith("/audition") ? json(preview({text: "changed words", auditioned: true})) : undefined);
  await h.client.prepare("Exact reviewed sentence.", "en"); await h.client.audition(); await h.client.acknowledge(); assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().preview, null); assert.equal(h.calls.filter(call => call.path.endsWith("/approve")).length, 0);
});

test("public ticket is one-use UI state, expires boundedly and is cleared by Stop", async () => {
  const h = harness(); await h.ready(); h.respond(path => path === "/api/public/tickets" ? json({ticket: "secret_viewer_ticket", expires_in_seconds: 120, path: "/public", origin: "http://127.0.0.1:5182"}) : undefined);
  await h.client.issuePublicTicket(); assert.equal(h.client.snapshot().publicTicketReady, true); assert.equal(JSON.stringify(h.client.snapshot()).includes("secret_viewer_ticket"), false);
  assert.equal(h.client.takePublicUrl(), "http://127.0.0.1:5182/public#ticket=secret_viewer_ticket"); assert.equal(h.client.takePublicUrl(), null);
  await h.client.issuePublicTicket(); await h.client.stop(); assert.equal(h.client.takePublicUrl(), null);
});

test("request options never send operator keys, follow redirects or cache private content", async () => {
  const h = harness(); await h.client.initialize("one_use_operator_ticket"); const call = h.calls[0]; assert.equal(call.path, "/api/bootstrap"); assert.deepEqual(call.body, {ticket: "one_use_operator_ticket"}); assert.equal(call.options.redirect, "error"); assert.equal(call.options.credentials, "omit"); assert.equal(call.options.cache, "no-store"); assert.equal(Object.keys(call.options.headers).some(key => /authorization|api.key/i.test(key)), false); assert.equal(JSON.stringify(h.client.snapshot()).includes("one_use_operator_ticket"), false); h.client.dispose();
});

test("delayed enable cannot undo a later private mute or playback fault", async () => {
  const h = harness(), enabled = deferred(); await h.ready(); h.audio.resumePlayback = () => enabled.promise;
  const action = h.client.mute("translation", false); await h.client.mute("translation", true); enabled.resolve(); await action;
  assert.equal(h.client.snapshot().muted.translation, true); assert.equal(h.effects.mutes.some(([, muted]) => !muted), false); h.client.dispose();
});

test("source Stop ignores already queued fragments and does not resurrect ready routes", async () => {
  const h = harness(); await h.ready(); h.route(); h.client.gap("remote_mix"); h.route();
  h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "late source", final: true});
  assert.equal(h.client.snapshot().routes[0].state, "closed"); assert.equal(h.client.snapshot().fragments.length, 0); assert.equal(h.client.snapshot().stopped, false); await flush(); h.client.dispose();
});

test("short-clip receipts are serialized despite delayed started acknowledgement", async () => {
  const h = harness("public"), started = deferred(); await h.ready();
  h.respond((path, options) => {if (path.startsWith("/api/public/audio/")) return wav(); if (path === "/api/public/receipt" && JSON.parse(options.body).state === "playback_started") return started.promise;});
  h.event(delivery()); await flush(); assert.deepEqual(h.calls.filter(call => call.path === "/api/public/receipt").map(call => call.body.state), ["accepted", "playback_started"]);
  started.resolve(json({state: "recorded", generation: 1})); await flush(); assert.deepEqual(h.calls.filter(call => call.path === "/api/public/receipt").map(call => call.body.state), ["accepted", "playback_started", "playback_ended"]); h.client.dispose();
});

test("reentrant Stop during ticket consumption opens no public window", async () => {
  const h = harness(); await h.ready(); h.respond(path => path === "/api/public/tickets" ? json({ticket: "secret_viewer_ticket", expires_in_seconds: 120, path: "/public", origin: "http://127.0.0.1:5182"}) : undefined);
  await h.client.issuePublicTicket(); h.client.subscribe(() => {if (!h.client.snapshot().publicTicketReady && !h.client.snapshot().stopped) void h.client.stop();}); h.client.openPublicScreen(); assert.equal(h.effects.opened, undefined); assert.equal(h.client.snapshot().stopped, true);
});

test("normal route expiry closes that route only and never automatically renews it", async () => {
  const h = harness(); await h.ready(); h.event({type: "route", generation: 1, route_id: "short", source: "remote_mix", route: "transcription", language: "en", state: "ready", code: null, expires_in_seconds: 1});
  await h.client.capture("remote_mix"); h.advance(1001); assert.equal(h.client.snapshot().routes[0].state, "closed"); assert.equal(h.client.snapshot().stopped, false);
  const frame = new ArrayBuffer(646); new DataView(frame).setUint8(0, 1); assert.equal(h.client.sendFrame("remote_mix", frame), true);
  assert.equal(h.calls.filter(call => call.path === "/api/routes").length, 0); h.client.dispose();
});

test("operator and public sockets use separate exact origins", async () => {
  const operator = harness(), viewer = harness("public"); await operator.ready(); await viewer.ready();
  assert.equal(operator.sockets[0].url, "ws://127.0.0.1:5180/api/operator/events"); assert.equal(viewer.sockets[0].url, "ws://127.0.0.1:5182/api/public/events");
  assert.throws(() => takeEntry({origin: "http://127.0.0.1:5180", pathname: "/public", hash: "#ticket=viewer"}, {replaceState: () => {}})); operator.client.dispose(); viewer.client.dispose();
});

test("UTF-8 Live and TTS limits reject multibyte input before any provider action request", async () => {
  const h = harness(); await h.ready(); h.route("live");
  await h.client.liveText("route_one", "あ".repeat(134)); await h.client.prepare("ह".repeat(667), "hi");
  assert.equal(h.calls.filter(call => call.path === "/api/live/text" || call.path === "/api/previews").length, 0); assert.equal(h.client.snapshot().stopped, false); h.client.dispose();
});

test("generated rolling-window eviction never evicts the retained source archive", async () => {
  const h = harness(); await h.ready(); h.route(); h.route("live", "self", "assistant");
  h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "first source remains", final: true});
  for (let i = 0; i < 550; i++) h.event({type: "transcript", generation: 1, route_id: "assistant", source: "self", route: "live", provenance: "live_output", text: `generated${i}`, final: false});
  assert.equal(h.client.snapshot().sourceFragments[0].text, "first source remains"); assert.equal(h.client.snapshot().fragments.length, 500); assert.equal(h.client.snapshot().generatedDropped, 50);
  const text = h.client.exportText(); assert.ok(text.includes("first source remains")); assert.ok(text.includes("50 older generated fragments")); assert.equal(text.includes("generated549"), false); h.client.dispose();
});

test("source archive UTF-8 capacity closes affected processing without dropping earlier source", async () => {
  const h = harness(); await h.ready(); h.route(); const text = "あ".repeat(4096);
  for (let i = 0; i < 400; i++) h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text, final: true});
  const view = h.client.snapshot(); assert.equal(view.capture.remote_mix, "closed"); assert.equal(view.capture.self, "idle"); assert.ok(view.sourceFragments.length > 0); assert.equal(view.sourceFragments[0].text, text);
  assert.ok(view.sourceFragments.reduce((total, item) => total + new TextEncoder().encode(item.text).length, 0) <= 4 * 1024 * 1024); assert.deepEqual(view.coverageGaps, ["remote_mix"]); assert.match(view.message, /capacity/); await flush(); h.client.dispose();
});

test("Finish retains late source finals and waits for the ordered marker before Stop and export", async () => {
  const h = harness(), stopped = deferred(); await h.ready(); h.route(); h.client.setNotes("private final notes");
  h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "unfinished", final: false, item: {id: "tail", sequence: 1, revision: 1}});
  h.respond(path => path === "/api/stop" ? stopped.promise : undefined);
  const pending = h.client.finishExport(); await flush();
  h.event({type: "finishing", generation: 1});
  assert.equal(h.client.snapshot().stopped, false); assert.equal(h.client.snapshot().finishing, true); assert.equal(h.effects.allStops, 1);
  assert.equal(h.client.sendFrame("remote_mix", new ArrayBuffer(646)), false);
  h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "retained final source", final: true, item: {id: "tail", sequence: 1, revision: 2}});
  h.event({type: "route", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", language: "en", state: "closed", code: null, expires_in_seconds: null});
  assert.equal(h.client.snapshot().stopped, false);
  h.event({type: "finished", generation: 1, source_completion: "complete"});
  const exported = await pending; assert.match(exported, /private final notes/); assert.match(exported, /retained final source/); assert.doesNotMatch(exported, /unfinished/); assert.match(exported, /Source-ASR items from PCM frames sent before Finish finalized/);
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().notes, ""); assert.equal(h.client.snapshot().sourceFragments.length, 0); assert.equal(h.client.exportText(), null);
  stopped.resolve(json({state: "stopped", generation: 2})); await flush(); assert.equal(h.client.snapshot().sourceFragments.length, 0);
});

test("Finish follows earlier PCM on the same socket and refuses another request", async () => {
  const h = harness(); await h.ready(); h.route(); await h.client.capture("remote_mix");
  const frame = new ArrayBuffer(646); new DataView(frame).setUint8(0, 1);
  assert.equal(h.client.sendFrame("remote_mix", frame), true);
  const pending = h.client.finishExport();
  assert.equal(await h.client.finishExport(), null);
  const sent = h.sockets[0].sent;
  assert.equal(sent.at(-2), frame); assert.deepEqual(JSON.parse(sent.at(-1)), {type: "finish", generation: 1});
  assert.equal(h.client.sendFrame("remote_mix", frame), false);
  assert.equal(h.calls.some(c => c.path === "/api/finish"), false);
  h.event({type: "finishing", generation: 1}); h.event({type: "finished", generation: 1, source_completion: "no_active_asr"});
  assert.match(await pending, /No active source-ASR route/); assert.equal(h.client.snapshot().stopped, true);
});

test("Stop cancels pending Finish immediately and late receipts never export or restore data", async () => {
  const h = harness(); await h.ready(); h.client.setNotes("must disappear");
  const pending = h.client.finishExport(); await h.client.stop(); assert.equal(await pending, null);
  h.event({type: "finishing", generation: 1}); h.event({type: "finished", generation: 1, source_completion: "complete"}); await flush();
  assert.equal(h.client.snapshot().notes, ""); assert.equal(h.client.snapshot().stopped, true);
  assert.equal(h.sockets[0].sent.filter(c => typeof c === "string" && JSON.parse(c).type === "finish").length, 1);
});

test("Finish deadline is absolute, exports incomplete source, and is never extended by heartbeats", async () => {
  const h = harness(); await h.ready(); h.route();
  h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "tail pending", final: false});
  const pending = h.client.finishExport(); await flush();
  for (let i = 0; i < 35; i++) {h.event({type: "heartbeat", generation: 1}); h.advance(1000);}
  const exported = await pending; assert.match(exported, /finalization was incomplete/); assert.match(exported, /incomplete at closure/); assert.match(exported, /tail pending/); assert.equal(h.client.snapshot().stopped, true);
});

test("Unordered, stale or duplicate Finish admission stops without exporting", async () => {
  for (const events of [
    [{type: "finished", generation: 1, source_completion: "complete"}],
    [{type: "finishing", generation: 2}],
    [{type: "finishing", generation: 1}, {type: "finishing", generation: 1}],
  ]) {
    const h = harness(); await h.ready(); h.client.setNotes("must disappear");
    const pending = h.client.finishExport(); for (const event of events) h.event(event);
    assert.equal(await pending, null); assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().notes, "");
  }
});

test("Finish send failure produces an explicit incomplete export without retry", async () => {
  const h = harness(); await h.ready(); h.client.setNotes("retained draft");
  h.sockets[0].send = () => {throw new Error("transport write");};
  const exported = await h.client.finishExport(); assert.match(exported, /retained draft/); assert.match(exported, /finalization was incomplete/);
  assert.equal(h.client.snapshot().stopped, true);
});

test("Stop reentered from Finish notification admits no finish request or export", async () => {
  const h = harness(); await h.ready();
  h.client.subscribe(() => {if (h.client.snapshot().finishing && !h.client.snapshot().stopped) void h.client.stop();});
  assert.equal(await h.client.finishExport(), null);
  assert.equal(h.sockets[0].sent.some(c => typeof c === "string" && JSON.parse(c).type === "finish"), false);
});

test("Socket loss, sign-out and expiry cancel pending Finish without a late download", async () => {
  for (const cancel of [h => h.sockets[0].onclose(), h => h.client.logout(), h => h.advance(7200001)]) {
    const h = harness(); await h.ready(); h.client.setNotes("private draft"); const pending = h.client.finishExport();
    await cancel(h); assert.equal(await pending, null); assert.equal(h.client.snapshot().notes, ""); assert.equal(h.client.snapshot().stopped, true);
  }
});

test("Finish supersedes pending preparation while the existing private socket keeps receiving", async () => {
  const h = harness(), preparation = deferred(); await h.ready(); h.route();
  h.respond(path => path === "/api/previews" ? preparation.promise : undefined);
  const preparing = h.client.prepare("Exact reviewed sentence.", "en"); await flush();
  const pending = h.client.finishExport(); await flush(); preparation.resolve(json(preview())); await preparing;
  await h.client.openRoute("self", "live", "en", "assistant"); await h.client.mute("live", false); await h.client.capture("self");
  assert.equal(h.client.snapshot().preview, null); assert.equal(h.effects.resume, 0); assert.equal(h.calls.filter(c => c.path === "/api/routes").length, 0); assert.deepEqual(h.effects.captures, []);
  h.event({type: "finishing", generation: 1}); h.event({type: "finished", generation: 1, source_completion: "incomplete"}); assert.match(await pending, /finalization was incomplete/);
});

test("provider route closure does not wait behind ordinary speech preparation", async () => {
  const h = harness(), preparation = deferred(); await h.ready(); h.route("translation");
  await flush(); assert.equal(h.client.snapshot().budgetFresh, true);
  h.respond(path => path === "/api/previews" ? preparation.promise : path === "/api/routes/close" ? json({state: "closed", generation: 1}) : undefined);
  const pending = h.client.prepare("Exact reviewed sentence.", "en"); await flush(); assert.equal(h.client.snapshot().busy, true);
  await h.client.closeRoute("route_one"); assert.equal(h.client.snapshot().routes[0].state, "closed"); assert.equal(h.client.snapshot().muted.translation, true); assert.equal(h.calls.filter(call => call.path === "/api/routes/close").length, 1);
  preparation.resolve(json(preview())); await pending; assert.equal(h.client.snapshot().preview.text, "Exact reviewed sentence."); h.client.dispose();
});

test("Finish fences pending audition callbacks and unsolicited preview events", async () => {
  const h = harness(), playback = deferred(); let callbacks; await h.ready();
  h.respond(path => path === "/api/previews" ? json(preview()) : path === preview().audio_path ? wav() : undefined);
  await h.client.prepare(preview().text, "en");
  h.audio.playClip = async (_bytes, value) => {callbacks = value; await playback.promise;};
  const audition = h.client.audition(); await flush(); assert.equal(h.client.snapshot().auditioning, true);
  const pending = h.client.finishExport(); assert.equal(callbacks.isCurrent(), false); callbacks.ended(); callbacks.failed();
  h.event({type: "preview", generation: 1, preview: preview()}); playback.resolve(); await audition;
  assert.equal(h.client.snapshot().preview, null); assert.equal(h.client.snapshot().auditionComplete, false); assert.equal(h.client.snapshot().stopped, false);
  h.event({type: "finishing", generation: 1}); h.event({type: "finished", generation: 1, source_completion: "no_active_asr"});
  assert.match(await pending, /No active source-ASR route/);
});

test("Audio cleanup exceptions cannot retain private state, socket authority or block server Stop", async () => {
  const h = harness(); await h.ready(); h.route(); h.client.setNotes("private sentinel");
  h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "private source", final: true});
  h.audio.stopAll = () => {throw new Error("synthetic audio teardown error");};
  await h.client.stop();
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().notes, ""); assert.deepEqual(h.client.snapshot().sourceFragments, []);
  assert.equal(h.sockets[0].closed, true); assert.equal(h.sockets[0].onmessage, null); assert.equal(h.effects.broadcast, 1);
  assert.equal(h.calls.filter(c => c.path === "/api/stop").length, 1); assert.match(h.client.snapshot().message, /resource cleanup was unconfirmed/);
});

test("Finish cleanup exceptions cancel export and still retire private state and server authority", async () => {
  for (const broken of ["audio", "public"]) {
    const h = harness(); await h.ready(); h.client.setNotes("private sentinel");
    if (broken === "audio") h.audio.stopAll = () => {throw new Error("audio teardown");};
    else h.dependencies.broadcastStop = () => {throw new Error("public child unavailable");};
    assert.equal(await h.client.finishExport(), null); await flush();
    assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().finishing, false); assert.equal(h.client.snapshot().notes, "");
    assert.equal(h.sockets[0].closed, true); assert.equal(h.calls.filter(c => c.path === "/api/stop").length, 1);
    assert.equal(h.sockets[0].sent.some(c => typeof c === "string" && JSON.parse(c).type === "finish"), false);
  }
});

test("A final rejected by local archive capacity cannot inherit the provider complete footer", async () => {
  const h = harness(); await h.ready(); h.route(); const full = "あ".repeat(4096);
  for (let i = 1; i <= 341; i++) h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: full, final: true, item: {id: `complete_${i}`, sequence: i, revision: 1}});
  const tail = {type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "tail pending", final: false, item: {id: "tail", sequence: 342, revision: 1}};
  h.event(tail); assert.equal(h.client.snapshot().coverageGaps.length, 0);
  const pending = h.client.finishExport(); h.event({type: "finishing", generation: 1});
  h.event({...tail, text: full, final: true, item: {...tail.item, revision: 2}});
  assert.deepEqual(h.client.snapshot().coverageGaps, ["remote_mix"]);
  h.event({type: "finished", generation: 1, source_completion: "complete"});
  const exported = await pending; assert.match(exported, /incomplete at closure/); assert.match(exported, /finalization was incomplete/);
  assert.doesNotMatch(exported, /frames sent before Finish finalized/);
});

test("Live mode is an explicit request and capabilities gate interpreter independently", async () => {
  for (const enabled of ["interpretation", "assistant"]) {
    const h = harness();
    h.respond(path => path === "/api/session" ? json({...session("operator"), capabilities: {...session("operator").capabilities, live_interpreter: enabled === "interpretation", live_assistant: enabled === "assistant"}}) : path === "/api/routes" ? json({state: "starting", generation: 1, route_id: "new_live"}) : undefined);
    await h.ready();
    await h.client.openRoute("remote_mix", "live", "hi");
    await h.client.openRoute("remote_mix", "live", "hi", enabled === "assistant" ? "interpretation" : "assistant");
    await h.client.openRoute("remote_mix", "translation", "hi", "interpretation");
    assert.equal(h.calls.some(row => row.path === "/api/routes"), false);
    await h.client.openRoute("remote_mix", "live", "hi", enabled);
    assert.deepEqual(h.calls.find(row => row.path === "/api/routes").body, {generation: 1, source: "remote_mix", route: "live", language: "hi", live_mode: enabled});
    assert.equal(h.client.snapshot().routes[0].live_mode, enabled);
    assert.deepEqual(h.effects.captures, []); assert.deepEqual(h.effects.private, []);
    h.client.dispose();
  }
});

test("live mode changes on existing route identity stop before accepting relabeled output", async () => {
  const h = harness(); await h.ready(); h.route("live", "remote_mix", "same", "interpretation");
  h.event({type: "transcript", generation: 1, route_id: "same", source: "remote_mix", route: "live", provenance: "live_output", text: "private interpretation", final: false});
  assert.equal(h.client.snapshot().fragments[0].live_mode, "interpretation");
  h.route("live", "remote_mix", "same", "assistant");
  assert.equal(h.client.snapshot().stopped, true); assert.deepEqual(h.client.snapshot().fragments, []); assert.equal(h.effects.broadcast, 1);
});

test("a route WS event racing HTTP admission must match the exact requested mode", async () => {
  for (const observed of ["interpretation", "assistant"]) {
    const h = harness(), pending = deferred(); await h.ready();
    h.respond(path => path === "/api/routes" ? pending.promise : undefined);
    const open = h.client.openRoute("remote_mix", "live", "en", "interpretation"); await flush();
    h.route("live", "remote_mix", "race", observed);
    pending.resolve(json({state: "starting", generation: 1, route_id: "race"})); await open;
    assert.equal(h.client.snapshot().stopped, observed !== "interpretation");
    if (observed === "interpretation") assert.equal(h.client.snapshot().routes[0].live_mode, observed);
    h.client.dispose();
  }
});

test("typed context is refused for interpreter routes and accepted only for ready assistants", async () => {
  const h = harness(); await h.ready(); h.route("live", "remote_mix", "interpreter", "interpretation"); h.route("live", "self", "assistant", "assistant");
  h.respond(path => path === "/api/live/text" ? json({state: "accepted", generation: 1}) : undefined);
  await h.client.liveText("interpreter", "Do not send this to an interpreter");
  assert.equal(h.calls.some(row => row.path === "/api/live/text"), false);
  await h.client.liveText("assistant", "A private question");
  assert.deepEqual(h.calls.find(row => row.path === "/api/live/text").body, {generation: 1, route_id: "assistant", text: "A private question"});
  h.client.dispose();
});

test("interpreter sound cannot enable assistant sound; muted intervals and source gaps never replay", async () => {
  const h = harness(); await h.ready(); h.route("live", "remote_mix", "interpreter", "interpretation"); h.route("live", "self", "assistant", "assistant");
  const audio = (route_id, source) => h.event({type: "audio", generation: 1, route_id, source, route: "live", pcm16_base64: "AAA=", sample_rate: 16000});
  audio("interpreter", "remote_mix"); audio("assistant", "self"); assert.deepEqual(h.effects.private, []);
  await h.client.mute("live_interpretation", false); assert.deepEqual(h.effects.private, []);
  audio("interpreter", "remote_mix"); audio("assistant", "self");
  assert.deepEqual(h.effects.private.map(row => row[0]), ["live_interpretation"]);
  await h.client.mute("live", false); audio("assistant", "self");
  h.client.gap("remote_mix");
  assert.equal(h.client.snapshot().muted.live_interpretation, true); assert.equal(h.client.snapshot().muted.live, false);
  audio("interpreter", "remote_mix"); audio("assistant", "self");
  assert.deepEqual(h.effects.private.map(row => row[0]), ["live_interpretation", "live", "live"]);
  await h.client.stop(); assert.deepEqual(h.client.snapshot().muted, {translation: true, live: true, live_interpretation: true});
});

test("generated mode provenance survives route history eviction and remains absent from source exports", async () => {
  const h = harness(); await h.ready(); h.route("live", "remote_mix", "first", "interpretation");
  for (const provenance of ["live_input", "live_output"]) h.event({type: "transcript", generation: 1, route_id: "first", source: "remote_mix", route: "live", provenance, text: "generated private words", final: false});
  for (let index = 0; index < 8; index++) h.event({type: "route", generation: 1, route_id: index === 0 ? "first" : `closed_${index}`, source: "remote_mix", route: "live", live_mode: index === 0 ? "interpretation" : "assistant", language: "en", state: "closed", code: null, expires_in_seconds: null});
  assert.equal(h.client.snapshot().routes.some(row => row.route_id === "first"), false);
  assert.deepEqual(h.client.snapshot().fragments.map(row => row.live_mode), ["interpretation", "interpretation"]);
  assert.doesNotMatch(h.client.exportText(), /generated private words/); h.client.dispose();
});

test("normal Finish suppresses interpreter output immediately and Stop cancels its pending export", async () => {
  const h = harness(); await h.ready(); h.route("live", "remote_mix", "interpreter", "interpretation");
  await h.client.mute("live_interpretation", false);
  const finish = h.client.finishExport();
  h.event({type: "audio", generation: 1, route_id: "interpreter", source: "remote_mix", route: "live", pcm16_base64: "AAA=", sample_rate: 16000});
  h.event({type: "transcript", generation: 1, route_id: "interpreter", source: "remote_mix", route: "live", provenance: "live_output", text: "late interpretation", final: false});
  assert.deepEqual(h.effects.private, []); assert.deepEqual(h.client.snapshot().fragments, []);
  await h.client.stop(); assert.equal(await finish, null);
});

test("evicted terminal route identities cannot be revived or changed to another Live mode", async () => {
  for (const changedMode of [false, true]) {
    const h = harness(); await h.ready(); h.route("live", "remote_mix", "retired", "interpretation");
    for (let index = 0; index < 8; index++) h.event({type: "route", generation: 1, route_id: index === 0 ? "retired" : `closed_${index}`, source: "remote_mix", route: "live", live_mode: "interpretation", language: "en", state: "closed", code: null, expires_in_seconds: null});
    assert.equal(h.client.snapshot().routes.some(row => row.route_id === "retired"), false);
    h.route("live", "remote_mix", "retired", changedMode ? "assistant" : "interpretation");
    assert.equal(h.client.snapshot().stopped, changedMode);
    assert.equal(h.client.snapshot().routes.some(row => row.route_id === "retired"), false);
    h.client.dispose();
  }
});

test("activation identity memory refuses a seventeenth route instead of forgetting immutable pins", async () => {
  const h = harness(); await h.ready();
  for (let index = 0; index < 17; index++) {
    h.event({type: "route", generation: 1, route_id: `pin_${index}`, source: "remote_mix", route: "live", live_mode: "interpretation", language: "en", state: "closed", code: null, expires_in_seconds: null});
    assert.equal(h.client.snapshot().stopped, index === 16);
  }
});

const earlyDraft = (overrides = {}) => ({type: "source_draft", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", item_id: "early", revision: 1, text: "The revenue was not", retired: false, ...overrides});
test("early words display immediately but cannot enter source export or trigger data retrieval", async () => {
  const h = harness(); await h.ready(); h.route();
  const originalSource = h.client.snapshot().sourceFragments;
  h.event(earlyDraft());
  assert.equal(h.client.snapshot().stopped, false);
  assert.equal(h.client.snapshot().sourceDrafts[0].text, "The revenue was not");
  assert.equal(h.client.snapshot().sourceFragments, originalSource);
  assert.doesNotMatch(h.client.exportText(), /The revenue was not/);
  h.event(earlyDraft({revision: 2, text: "The revenue was not one million."}));
  h.event(earlyDraft({revision: 2, text: "", retired: true}));
  assert.equal(h.client.snapshot().sourceDrafts.length, 0);
  h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "The revenue was not one million.", final: true, item: {id: "early", sequence: 1, revision: 1}});
  assert.equal(h.client.snapshot().stopped, false);
  assert.equal(h.client.snapshot().sourceFragments.length, 1);
  assert.match(h.client.exportText(), /The revenue was not one million/);
  h.client.dispose();
});
test("Finish discards unbound drafts while receiving subsequent bound source finals", async () => {
  const h = harness(); await h.ready(); h.route(); h.event(earlyDraft());
  const pending = h.client.finishExport(); assert.deepEqual(h.client.snapshot().sourceDrafts, []);
  h.event({type: "finishing", generation: 1});
  h.event(earlyDraft({text: "", retired: true}));
  h.event({type: "transcript", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "Correct final.", final: true, item: {id: "early", sequence: 1, revision: 1}});
  h.event({type: "finished", generation: 1, source_completion: "complete"});
  const text = await pending; assert.match(text, /Correct final/); assert.doesNotMatch(text, /The revenue was not/);
  assert.deepEqual(h.client.snapshot().sourceDrafts, []);
});
test("Stop, source closure and invalid draft revisions never leave speculative text behind", async () => {
  for (const close of [h => h.client.stop(), h => h.event({...earlyDraft(), revision: 3}), h => h.event({type: "route", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", language: "en", state: "closed", code: null, expires_in_seconds: null})]) {
    const h = harness(); await h.ready(); h.route(); h.event(earlyDraft()); await close(h);
    assert.deepEqual(h.client.snapshot().sourceDrafts, []); h.client.dispose();
  }
});

test("unresolved drafts cannot inherit a complete Finish footer even after a closed-route marker", async () => {
  for (const retire of [false, true]) for (const close of [false, true]) {
    const h = harness(); await h.ready(); h.route(); h.event(earlyDraft());
    const pending = h.client.finishExport(); h.event({type: "finishing", generation: 1});
    if (retire) h.event(earlyDraft({retired: true, text: ""}));
    if (close) h.event({type: "route", generation: 1, route_id: "route_one", source: "remote_mix", route: "transcription", language: "en", state: "closed", code: null, expires_in_seconds: null});
    h.event({type: "finished", generation: 1, source_completion: "complete"});
    const text = await pending;
    assert.match(text, /Source finalization was incomplete/); assert.doesNotMatch(text, /The revenue was not/);
    assert.equal(h.client.snapshot().stopped, true);
  }
});

test("hint editing starts no I/O, binds only caption admission and freezes while a route is open", async () => {
  const h=harness(); await h.ready(); const initial=h.calls.length;
  h.client.setSpeechHints("ja","佐藤\nAC-42"); assert.equal(h.calls.length,initial);
  assert.doesNotMatch(h.client.exportText(),/佐藤|AC-42/);
  h.respond(path => path === "/api/routes" ? json({state:"starting",generation:1,route_id:"hint_route"}) : undefined);
  await h.client.openRoute("remote_mix","transcription","en");
  assert.deepEqual(h.calls.find(call=>call.path==="/api/routes").body.transcription,{languages:["ja"],keywords:["佐藤","AC-42"]});
  h.client.setSpeechHints("hi","changed"); assert.equal(h.client.snapshot().speechTerms,"佐藤\nAC-42");
  await h.client.stop(); assert.equal(h.client.snapshot().speechTerms,""); assert.equal(h.client.snapshot().spokenLanguage,"auto");
  h.client.setSpeechHints("hi","cannot restore"); assert.equal(h.client.snapshot().speechTerms,"");
  const live=harness(); await live.ready(); live.client.setSpeechHints("ja","private keyword");
  live.respond(path=>path==="/api/routes" ? json({state:"starting",generation:1,route_id:"live_route"}) : undefined);
  await live.client.openRoute("remote_mix","live","en","interpretation");
  assert.equal(Object.hasOwn(live.calls.find(call=>call.path==="/api/routes").body,"transcription"),false); live.client.dispose();
});
test("invalid optional hints do not cause a paid admission or a fallback to previous hints", async () => {
  const h=harness(); await h.ready(); h.client.setSpeechHints("en","x".repeat(81));
  assert.ok(h.client.snapshot().speechHintsError);
  await h.client.openRoute("remote_mix","transcription","en");
  assert.equal(h.calls.some(call=>call.path==="/api/routes"),false); h.client.dispose();
});
const timedLive=(overrides={})=>({type:"live_transcript",generation:1,route_id:"live_route",source:"remote_mix",route:"live",provenance:"live_output",text:"not ",start_ms:100.25,end_ms:200.5,...overrides});
test("timed interpretation grows immediately and never enters source export or data retrieval", async()=>{
  const h=harness(); await h.ready(); h.route("live","remote_mix","live_route","interpretation");
  h.event(timedLive()); const key=h.client.snapshot().liveCaptions[0].key;
  h.event(timedLive({text:"12,000",start_ms:200.5,end_ms:400}));
  assert.equal(h.client.snapshot().liveCaptions.length,1); assert.equal(h.client.snapshot().liveCaptions[0].key,key);
  assert.equal(h.client.snapshot().liveCaptions[0].text,"not 12,000");
  assert.equal(h.client.snapshot().sourceFragments.length,0); assert.equal(h.client.snapshot().fragments.length,0);
  assert.doesNotMatch(h.client.exportText(),/not 12,000/);
  const finish=h.client.finishExport(); h.event(timedLive({text:"late"}));
  assert.equal(h.client.snapshot().liveCaptions[0].text,"not 12,000");
  h.event({type:"finishing",generation:1}); h.event({type:"finished",generation:1,source_completion:"no_active_asr"});
  assert.doesNotMatch(await finish,/not 12,000|late/); assert.equal(h.client.snapshot().liveCaptions.length,0);
});

test("the viewer's Retry server Stop revokes the same session after a lost logout response", async () => {
  const h = harness("public");
  const network = h.dependencies.fetch;
  const origin = "http://127.0.0.1:5182";
  const tab = tabSession("public", origin, true, {
    storage: () => {throw new Error("Storage unavailable");},
    fetch: (url, init) => network(new URL(url).pathname, init),
    socket: h.dependencies.socket,
  });
  const transport = tab.openClient();
  h.dependencies.fetch = transport.fetch;
  h.dependencies.beginLogout = transport.beginLogout;
  let attempts = 0;
  const credential = "d".repeat(64);
  h.respond(path => {
    if (path === "/api/public/bootstrap") {
      const response = json(session("public")); response.headers.set("X-Pilot-Session", credential); return response;
    }
    if (path === "/api/public/logout" && ++attempts === 1) throw new Error("Transient connection failure");
  });
  await h.client.initialize("synthetic_ticket");
  assert.equal(h.client.snapshot().session?.role, "public");
  await h.client.stop();
  assert.match(h.client.snapshot().message, /unconfirmed/);
  await assert.rejects(transport.fetch("/api/public/session"));
  await h.client.stop();
  assert.match(h.client.snapshot().message, /Stop recorded/);
  const revocations = h.calls.filter(call => call.path === "/api/public/logout");
  assert.equal(revocations.length, 2);
  assert.ok(revocations.every(call => call.options.headers.get("Authorization") === "Bearer " + credential));
  h.client.dispose(); transport.dispose(); tab.leave();
});

const stopReport = (overrides = {}) => ({version: 1, session_generation: 1, generation: 2, provider_activity: "close_observed", reserved_microusd: 134000, invoice: "unavailable", ...overrides});

test("operator Stop samples native evidence without waiting for a chooser; trusted fields stay independent", async () => {
  const h = harness(), retirement = deferred();
  h.audio.retire = () => retirement.promise;
  h.respond(path => path === "/api/stop-receipt" ? json(stopReport()) : undefined);
  await h.ready(); await h.client.stop(); await flush();
  assert.equal(h.client.snapshot().localAudioRetirement, "checking");
  assert.equal(h.client.snapshot().stopAcknowledged, true);
  assert.equal(h.client.snapshot().stopReceipt.provider_activity, "close_observed");
  retirement.resolve(false); await flush();
  assert.equal(h.client.snapshot().localAudioRetirement, "unconfirmed");
  h.audio.retire = async () => true;
  await h.client.recheckStopEvidence();
  assert.equal(h.client.snapshot().localAudioRetirement, "confirmed");
  assert.deepEqual(h.calls.filter(call => call.path === "/api/stop-receipt").map(call => call.options.method), ["GET", "GET"]);
});

test("a lost or malformed Stop response cannot borrow acknowledgment from an authenticated receipt", async () => {
  for (const response of [() => {throw new Error("lost");}, () => json({state: "stopped", generation: 3}), () => json({ok: true})]) {
    const h = harness(); h.audio.retire = async () => true;
    h.respond(path => path === "/api/stop" ? response() : path === "/api/stop-receipt" ? json(stopReport({provider_activity: "none_admitted", reserved_microusd: 0})) : undefined);
    await h.ready(); await h.client.stop(); await flush();
    assert.equal(h.client.snapshot().stopAcknowledged, false);
    assert.equal(h.client.snapshot().stopReceipt.provider_activity, "none_admitted");
    assert.equal(h.client.snapshot().localAudioRetirement, "confirmed");
  }
});

test("Stop receipt rejects crossed generations, unauthorised and unknown metadata, and ignores a late signed-out result", async () => {
  for (const raw of [stopReport({session_generation: 2, generation: 3}), stopReport({generation: 22}), stopReport({invoice: "estimated"}), stopReport({reserved_microusd: -1}), stopReport({raw_provider_data: "fixture"})]) {
    const h = harness();
    h.respond(path => path === "/api/stop-receipt" ? json(raw) : undefined);
    await h.ready(); await h.client.stop(); await flush();
    assert.equal(h.client.snapshot().stopAcknowledged, true);
    assert.equal(h.client.snapshot().stopReceipt, null);
    assert.equal(h.client.snapshot().stopReceiptState, "unavailable");
  }
  const denied = harness();
  denied.respond(path => path === "/api/stop-receipt" ? json({}, 403) : undefined);
  await denied.ready(); await denied.client.stop(); await flush();
  assert.equal(denied.client.snapshot().stopReceiptState, "unauthorized");
  assert.equal(denied.client.snapshot().stopReceipt, null);
  assert.equal(denied.client.snapshot().stopAcknowledged, true);
  const h = harness(), late = deferred();
  h.respond(path => path === "/api/stop-receipt" ? late.promise : undefined);
  await h.ready(); await h.client.stop(); await h.client.logout();
  late.resolve(json(stopReport())); await flush();
  assert.equal(h.client.snapshot().session, null);
  assert.equal(h.client.snapshot().stopReceipt, null);
  assert.equal(h.client.snapshot().stopReceiptState, "idle");
});

test("terminal resampling observes delayed upstream closure; public Stop does not query a receipt", async () => {
  const h = harness(); let reads = 0;
  h.respond(path => path === "/api/stop-receipt" ? json(stopReport({provider_activity: ++reads === 1 ? "unconfirmed" : "close_observed"})) : undefined);
  await h.ready(); await h.client.stop(); await flush();
  assert.equal(h.client.snapshot().stopReceipt.provider_activity, "unconfirmed");
  await new Promise(resolve => setTimeout(resolve, 450));
  assert.equal(h.client.snapshot().stopReceipt.provider_activity, "close_observed");
  assert.equal(reads, 2);
  const viewer = harness("public");
  await viewer.ready(); await viewer.client.stop(); await viewer.client.recheckStopEvidence();
  assert.ok(viewer.calls.every(call => call.path !== "/api/stop-receipt"));
});

test("global Stop keeps private WebRTC retirement uncertain when stopping the separate browser owner", async () => {
  const h = harness();
  h.audio.retire = async () => true;
  await h.ready();
  let disposed = false;
  h.client.privateListening = {snapshot: () => ({phase: disposed ? "closed" : "listening"}), dispose: () => {disposed = true;}};
  h.respond(path => path === "/api/stop-receipt" ? json(stopReport({provider_activity: "unconfirmed", reserved_microusd: null})) : undefined);
  await h.client.stop(); await flush();
  assert.ok(disposed);
  assert.equal(h.client.snapshot().localAudioRetirement, "confirmed");
  assert.equal(h.client.snapshot().rtcAudioRetirement, "unconfirmed");
  assert.equal(h.client.snapshot().stopReceipt.reserved_microusd, null);
  await h.client.logout();
});

test("already-stopped operator reload reads the terminal generation without claiming a previous page's Stop response", async () => {
  const h = harness();
  h.respond(path => path === "/api/session" ? json({...session("operator"), stopped: true, generation: 8}) :
    path === "/api/stop-receipt" ? json(stopReport({session_generation: 7, generation: 8})) : undefined);
  await h.client.initialize(null); await flush();
  assert.equal(h.client.snapshot().stopAcknowledged, false);
  assert.equal(h.client.snapshot().stopReceipt.generation, 8);
  assert.equal(h.client.snapshot().localAudioRetirement, "unconfirmed"); // No retire port.
  assert.ok(h.calls.every(call => call.path !== "/api/stop"));
});


test("an exact acknowledgment from either overlapping Stop survives a timeout in either settlement order", async () => {
  for (const success of [0, 1]) for (const first of [0, 1]) {
    const h = harness(), gates = [deferred(), deferred()];
    let called = 0;
    h.respond(path => path === "/api/stop" ? gates[called++].promise : undefined);
    await h.ready();
    const requests = [h.client.stop(), h.client.stop()];
    for (const index of [first, 1 - first]) {
      gates[index].resolve(index === success ? json({state: "stopped", generation: 2}) : Promise.reject(new Error("timeout")));
      await requests[index];
    }
    assert.equal(h.client.snapshot().stopAcknowledged, true);
    assert.match(h.client.snapshot().message, /Stop recorded/);
  }
});

test("a delayed Stop acknowledgment cannot settle a signed-out or disposed predecessor", async () => {
  for (const leave of [client => client.logout(), client => client.dispose()]) {
    const h = harness(), receipt = deferred();
    h.respond(path => path === "/api/stop" ? receipt.promise : undefined);
    await h.ready();
    const stopping = h.client.stop();
    await leave(h.client);
    receipt.resolve(json({state: "stopped", generation: 2}));
    await stopping;
    assert.equal(h.client.snapshot().stopAcknowledged, false);
    assert.ok(h.calls.every(call => call.path !== "/api/stop-receipt"));
  }
});
