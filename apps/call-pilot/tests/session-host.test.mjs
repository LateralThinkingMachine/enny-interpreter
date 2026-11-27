import assert from "node:assert/strict";
import test from "node:test";
import {PilotClient} from "../.test-build/client.js";
import {SessionHost} from "../.test-build/session-host.js";
import {parseSession} from "../.test-build/protocol.js";
import {disabledBudget} from "./budget-fixture.mjs";

const session = (generation = 1, changes = {}) => ({role: "operator", csrf: generation.toString(16).padStart(64, "0"), generation, stopped: false, providers_enabled: false, continuous: true, repeatable_sessions: true, expires_in_seconds: null, capabilities: {source_captions: false, private_translation: false, live_assistant: false, live_interpreter: false, prepared_public_speech: false}, limitations: [], ...changes});
const json = (value, status = 200) => new Response(JSON.stringify(value), {status, headers: {"Content-Type": "application/json"}});
const deferred = () => {let resolve; const promise = new Promise(done => {resolve = done;}); return {resolve, promise};};
const flush = async () => {for (let i = 0; i < 8; i++) await new Promise(resolve => setImmediate(resolve));};
function harness(initial = session(), preferences) {
  const calls = [], owners = [];
  let custom, nonce = 0;
  const host = new SessionHost(() => {
    const effects = {disposed: 0, captures: 0, sockets: 0, frames: 0, clocks: 0};
    const audio = {stopAll() {}, stopCapture() {}, stopClip() {}, setPrivateMuted() {}, resumePlayback: async () => {}, startCapture: async () => {effects.captures++;}, playPrivate() {}, playClip: async () => {}};
    const client = new PilotClient("operator", {audio, now: () => 0, interval: () => {effects.clocks++; return () => {effects.clocks--;};}, broadcastStop() {}, openPublicWindow() {}, verifyClip: async () => true,
      socket: () => {effects.sockets++; throw new Error("No session transition may connect");},
      fetch: async (path, options = {}) => {
        calls.push({path, options});
        const answer = custom?.(path, options); if (answer !== undefined) return answer;
        if (path === "/api/session") return json(initial);
        if (path === "/api/budget") return json(disabledBudget());
        if (path === "/api/diagnostics") return json({}, 503);
        if (path === "/api/stop" || path === "/api/logout") return json({state: "stopped", generation: 2});
        if (path === "/api/session/new") {const body = JSON.parse(options.body); return json({state: "ready", ...body, session: session(body.previous_generation + 2)});}
        throw new Error("Unexpected local request");
      }});
    const owned = {client, effects, audio, dispose: () => {effects.disposed++; client.dispose();}};
    owners.push(owned); return owned;
  }, () => (++nonce).toString(16).padStart(64, "0"), preferences);
  return {host, calls, owners, respond: fn => {custom = fn;}, async ready() {await host.snapshot().client.initialize(null);}};
}

test("repeatable capability is explicit, operator-only and continuous-only", () => {
  assert.equal(parseSession(session(), "operator").repeatable_sessions, true);
  const legacy = session(); delete legacy.repeatable_sessions;
  assert.equal(parseSession(legacy, "operator").repeatable_sessions, false);
  for (const change of [{session_generation: 2}, {session_generation: 0}, {session_generation: 1, repeatable_sessions: false}, {session_generation: 1, generation: 2}]) assert.throws(() => parseSession(session(1, change), "operator"));
  assert.equal(parseSession(session(2, {stopped: true, session_generation: 1}), "operator").session_generation, 1);
  for (const value of [1, "true", null]) assert.throws(() => parseSession(session(1, {repeatable_sessions: value}), "operator"));
  assert.throws(() => parseSession(session(1, {continuous: false, expires_in_seconds: 100}), "operator"));
  assert.throws(() => parseSession(session(1, {role: "public"}), "public"));
});

test("two ended recordings create independent clients, retain language and never start media", async () => {
  const h = harness(); await h.ready(); h.host.setLanguage("hi");
  for (let i = 0; i < 2; i++) {
    const previous = h.host.snapshot().client; previous.setNotes("Private previous notes");
    await previous.stop(); await h.host.create(previous);
    const next = h.host.snapshot();
    assert.notEqual(next.client, previous); assert.equal(next.key, i + 2); assert.equal(next.language, "hi");
    assert.equal(next.client.snapshot().stopped, false); assert.equal(previous.snapshot().stopped, true);
    assert.equal(next.client.snapshot().notes, ""); assert.equal(next.client.snapshot().captureConfirmed, false);
    assert.deepEqual(next.client.snapshot().routes, []); assert.equal(next.client.snapshot().session.generation, 3 + 2 * i);
    await h.host.create(previous); assert.equal(h.host.snapshot().key, i + 2);
  }
  assert.ok(h.owners.every(row => row.effects.captures === 0 && row.effects.sockets === 0));
  assert.deepEqual(h.owners.map(row => row.effects.disposed), [1, 1, 0]);
  assert.deepEqual(h.owners.map(row => row.effects.clocks), [0, 0, 1]);
  h.host.dispose(); h.host.dispose(); assert.deepEqual(h.owners.map(row => row.effects.disposed), [1, 1, 1]);
});

test("double-click is singleflight before React can rerender", async () => {
  const h = harness(); await h.ready(); const old = h.host.snapshot().client; await old.stop();
  const gate = deferred(); h.respond(path => path === "/api/session/new" ? gate.promise : undefined);
  const work = h.host.create(old); await flush(); await h.host.create(old);
  assert.equal(h.calls.filter(row => row.path === "/api/session/new").length, 1);
  const call = h.calls.at(-1); gate.resolve(json({state: "ready", ...JSON.parse(call.options.body), session: session(3)})); await work;
  assert.equal(h.owners.length, 2); h.host.dispose();
});

test("lost response uses the same receipt on explicit retry without another Stop", async () => {
  const h = harness(); await h.ready(); const old = h.host.snapshot().client; await old.stop();
  let failed = false; h.respond(path => {if (path === "/api/session/new" && !failed) {failed = true; throw new Error("Lost reply");}});
  await h.host.create(old); assert.match(h.host.snapshot().error, /not confirmed/);
  const stops = h.calls.filter(row => row.path === "/api/stop").length;
  await h.host.create(old);
  const requests = h.calls.filter(row => row.path === "/api/session/new");
  assert.equal(requests.length, 2); assert.equal(requests[0].options.body, requests[1].options.body);
  assert.equal(h.calls.filter(row => row.path === "/api/stop").length, stops);
  assert.equal(h.host.snapshot().key, 2); h.host.dispose();
});

for (const cancel of ["logout", "dispose"]) test(`${cancel} fences a late session reply`, async () => {
  const h = harness(); await h.ready(); const old = h.host.snapshot().client; await old.stop();
  const gate = deferred(); h.respond(path => path === "/api/session/new" ? gate.promise : undefined);
  const work = h.host.create(old); await flush(); const call = h.calls.at(-1);
  if (cancel === "logout") await old.logout(); else h.host.dispose();
  gate.resolve(json({state: "ready", ...JSON.parse(call.options.body), session: session(3)})); await work;
  assert.equal(h.owners.length, 1); assert.equal(h.host.snapshot().key, 1); h.host.dispose();
});

for (const changes of [{repeatable_sessions: false}, {continuous: false, repeatable_sessions: false, expires_in_seconds: 100}]) test(`one-use/finite session never sends New session (${JSON.stringify(changes)})`, async () => {
  const h = harness(session(1, changes)); await h.ready(); const old = h.host.snapshot().client; await old.stop(); await h.host.create(old);
  assert.equal(h.calls.some(row => row.path === "/api/session/new"), false); h.host.dispose();
});

test("reloading an authenticated ended repeatable session allows an explicit successor", async () => {
  const h = harness(session(2, {stopped: true, session_generation: 1})); await h.ready(); const old = h.host.snapshot().client;
  assert.equal(old.snapshot().stopped, true); assert.equal(h.host.snapshot().available, true);
  await h.host.create(old); assert.equal(h.host.snapshot().client.snapshot().session.generation, 3); h.host.dispose();
});

test("lost authorization hides renewal and rejects stale actions", async () => {
  const h = harness(); await h.ready(); const old = h.host.snapshot().client; await old.stop();
  h.respond(path => path === "/api/session/new" ? json({}, 401) : undefined);
  await h.host.create(old); assert.equal(h.host.snapshot().available, false);
  const count = h.calls.length; await h.host.create(old); assert.equal(h.calls.length, count); h.host.dispose();
});

test("successor response must bind receipt, generation and a fresh authority", async () => {
  for (const malformed of [{request_id: "b".repeat(64)}, {previous_generation: 8}, {session: session(1)}, {session: session(3, {csrf: session().csrf})}, {session: session(3, {stopped: true})}, {session: session(3, {repeatable_sessions: false})}]) {
    const h = harness(); await h.ready(); const old = h.host.snapshot().client; await old.stop();
    h.respond((path, options) => path === "/api/session/new" ? json({state: "ready", ...JSON.parse(options.body), session: session(3), ...malformed}) : undefined);
    await h.host.create(old); assert.equal(h.owners.length, 1); assert.ok(h.host.snapshot().error); h.host.dispose();
  }
});


test("new session waits for local audio retirement and does not dispatch on uncertainty", async () => {
  const h = harness(); await h.ready(); const old = h.host.snapshot().client; await old.stop();
  const gate = deferred(); h.owners[0].audio.retire = () => gate.promise;
  const action = h.host.create(old); await flush();
  assert.equal(h.calls.some(row => row.path === "/api/session/new"), false);
  gate.resolve(false); await action;
  assert.equal(h.calls.some(row => row.path === "/api/session/new"), false);
  assert.match(h.host.snapshot().error, /Audio cleanup/);
  h.owners[0].audio.retire = async () => true;
  await h.host.create(old); assert.equal(h.host.snapshot().key, 2); h.host.dispose();
});


test("saved preferences cross fresh session boundaries while capture consent and content do not", async () => {
  const saved = {version: 1, language: "hi"}, writes = [];
  const h = harness(session(), {read: () => saved, write: value => {writes.push(value); return true;}});
  await h.ready(); assert.equal(h.host.snapshot().language, "hi"); assert.equal(writes.length, 0);
  h.host.setLanguage("ja");
  const previous = h.host.snapshot().client; previous.setNotes("session-only note"); await previous.stop();
  await h.host.create(previous);
  assert.notEqual(h.host.snapshot().client, previous);
  assert.deepEqual(h.host.snapshot().preferences, {version: 1, language: "ja"});
  assert.equal(h.host.snapshot().client.snapshot().notes, "");
  assert.equal(h.host.snapshot().client.snapshot().captureConfirmed, false);
  assert.equal(writes.length, 1); h.host.dispose();
});
test("failed preference writes are visible without blocking session use", async () => {
  const h = harness(session(), {read() {throw new Error("storage blocked");}, write() {throw new Error("quota");}});
  await h.ready(); h.host.setLanguage("ja");
  assert.equal(h.host.snapshot().language, "ja"); assert.equal(h.host.snapshot().preferencesSaved, false);
  assert.equal(h.host.snapshot().client.snapshot().stopped, false); h.host.dispose();
});
