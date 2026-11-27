import assert from "node:assert/strict";
import test from "node:test";
import {PilotClient} from "../.test-build/client.js";
import {parseDiagnostics, stopMessage} from "../.test-build/diagnostics.js";
import {captureMessage, providerMessage} from "../.test-build/feedback.js";
import {disabledBudget} from "./budget-fixture.mjs";

const diagnostic = (extra = {}) => ({version: 1, generation: 2, stopped: true, stop: {code: "operator_disconnected", elapsed_seconds: 180}, last_issue: {code: "text_limit", source: "remote_mix", route: "live", elapsed_seconds: 179}, ...extra});
const json = (value, status = 200) => new Response(JSON.stringify(value), {status, headers: {"Content-Type": "application/json"}});
const deferred = () => {let resolve; const promise = new Promise(done => {resolve = done;}); return {promise, resolve};};
const flush = async () => {for (let i = 0; i < 8; i++) await new Promise(resolve => setImmediate(resolve));};
function harness({role = "operator", stopped = false} = {}) {
  const calls = [], sockets = [], effects = {stop: 0, captures: 0, sound: 0};
  let respond;
  const session = {role, csrf: "synthetic_csrf", generation: stopped ? 2 : 1, stopped, providers_enabled: false, expires_in_seconds: 7200, capabilities: {source_captions: false, private_translation: false, live_assistant: false, live_interpreter: false, prepared_public_speech: false}, limitations: []};
  const client = new PilotClient(role, {
    now: () => 0, interval: () => () => {}, broadcastStop: () => {},
    audio: {stopAll: () => effects.stop++, stopClip: () => {}, stopCapture: () => {}, resumePlayback: async () => effects.sound++, setPrivateMuted: () => {}, prepareCapture: async () => effects.captures++, startCapture: async () => effects.captures++, activateCapture: () => effects.captures++, cancelPreparedCapture: async () => {}, playPrivate: () => effects.sound++, playClip: async () => effects.sound++},
    socket: () => {const socket = {readyState: 1, bufferedAmount: 0, send: () => {}, close: () => {}, onmessage: null, onclose: null}; sockets.push(socket); return socket;},
    fetch: async (path, options) => {
      calls.push({path, options});
      const result = respond?.(path, options); if (result !== undefined) return result;
      if (path.endsWith("/session")) return json(session);
      if (path === "/api/budget") return json(disabledBudget());
      if (path === "/api/diagnostics") return json(diagnostic());
      if (path === "/api/stop" || path.endsWith("/logout")) return json({state: "stopped", generation: 2});
      throw new Error("Unexpected fixture request");
    }, verifyClip: async () => false, openPublicWindow: () => {},
  });
  return {client, calls, sockets, effects, respond: fn => respond = fn,
    async connected() {await client.initialize(null); await client.connect(); sockets[0].onmessage({data: JSON.stringify({type: "hello", role, generation: 1, stopped: false})});},
  };
}

test("diagnostics accept only versioned bounded metadata and reject raw payloads, stale generations", () => {
  assert.deepEqual(parseDiagnostics(diagnostic(), 1), diagnostic());
  for (const row of [diagnostic({version: 2}), diagnostic({generation: 0}), diagnostic({stopped: false}), diagnostic({stop: null}), diagnostic({raw: "secret"}), diagnostic({stop: {code: "secret", elapsed_seconds: 1}}), diagnostic({stop: {code: "deadline", elapsed_seconds: -1}}), diagnostic({stop: {code: "deadline", elapsed_seconds: Number.MAX_SAFE_INTEGER + 1}}), diagnostic({last_issue: {code: "text_limit", source: "remote_mix", route: "live", elapsed_seconds: 1, text: "secret"}})]) {
    assert.throws(() => parseDiagnostics(row, 1));
  }
  assert.equal(parseDiagnostics(diagnostic({last_issue: {code: "text_limit", source: "remote_mix", route: "live", elapsed_seconds: 181}}), 1).last_issue.elapsed_seconds, 181);
  assert.equal(parseDiagnostics(diagnostic({stop: null, last_issue: null, stopped: false, generation: 1}), 1).stopped, false);
});

test("ASR mismatch duration metadata is optional, exact, bounded and scoped to source transcription", () => {
  const issue = {code: "usage_mismatch", source: "remote_mix", route: "transcription", elapsed_seconds: 42};
  assert.deepEqual(parseDiagnostics(diagnostic({last_issue: issue}), 1).last_issue, issue);
  for (const submitted_ms of [0, 1, 999, 120000]) {
    for (const reported_ms of [submitted_ms + 1001, Number.MAX_SAFE_INTEGER, null]) {
      const asr_usage = {submitted_ms, reported_ms, allowed_ms: submitted_ms + 1000};
      const row = diagnostic({last_issue: {...issue, asr_usage}});
      assert.deepEqual(parseDiagnostics(row, 1), row);
    }
  }
  const usage = {submitted_ms: 2400, reported_ms: 4001, allowed_ms: 3400};
  const invalidUsage = [null, undefined, [], {}, "raw_provider_detail", {...usage, text: "raw_provider_detail"},
    {submitted_ms: 2400, allowed_ms: 3400}, {...usage, allowed_ms: 3401}, {...usage, allowed_ms: 3399}, {...usage, reported_ms: 3400}, {...usage, reported_ms: 3399}, {...usage, reported_ms: 0}];
  for (const key of ["submitted_ms", "reported_ms", "allowed_ms"]) {
    for (const value of [-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, "2400", true, undefined]) invalidUsage.push({...usage, [key]: value});
  }
  invalidUsage.push({...usage, submitted_ms: 120001, allowed_ms: 121001}, {...usage, submitted_ms: null}, {...usage, allowed_ms: null});
  for (const asr_usage of invalidUsage) assert.throws(() => parseDiagnostics(diagnostic({last_issue: {...issue, asr_usage}}), 1));
  for (const route of [null, "translation", "live"]) assert.throws(() => parseDiagnostics(diagnostic({last_issue: {...issue, route, asr_usage: usage}}), 1));
  for (const code of ["unknown", "provider_closed", "source_gap", "input_audio_limit"]) assert.throws(() => parseDiagnostics(diagnostic({last_issue: {...issue, code, asr_usage: usage}}), 1));
  assert.throws(() => parseDiagnostics(diagnostic({last_issue: {...issue, asr_usage: usage, provider_message: "raw_provider_detail"}}), 1));
  assert.throws(() => parseDiagnostics(diagnostic({generation: 1, last_issue: {...issue, asr_usage: usage}}), 2));
});

test("stopped reload reads duration metadata without provider work or copying raw diagnostic fields", async () => {
  const h = harness({stopped: true});
  const row = diagnostic({last_issue: {code: "usage_mismatch", source: "self", route: "transcription", elapsed_seconds: 42, asr_usage: {submitted_ms: 2400, reported_ms: null, allowed_ms: 3400}}});
  h.respond(path => path === "/api/diagnostics" ? json(row) : undefined);
  await h.client.initialize(null); await flush();
  assert.deepEqual(h.client.snapshot().diagnostics, row);
  assert.deepEqual(h.calls.map(call => call.path), ["/api/session", "/api/diagnostics", "/api/stop-receipt"]);
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.sockets.length, 0);
  assert.equal(h.effects.captures, 0); assert.equal(h.effects.sound, 0); h.client.dispose();
});

test("fixed messages never display unknown codes or prototype properties and deadline wording covers active streams", () => {
  for (const code of ["constructor", "__proto__", "raw_secret_payload"]) {
    for (const render of [captureMessage, providerMessage, stopMessage]) {const message = render(code); assert.equal(typeof message, "string"); assert.equal(message.includes(code), false);}
  }
  assert.doesNotMatch(providerMessage("deadline"), /did not become ready/);
  assert.match(providerMessage("text_limit"), /not evidence that the dollar allowance was spent/);
  assert.doesNotMatch(stopMessage("process_shutdown"), /time limit|allowance/);
  assert.equal(parseDiagnostics(diagnostic({stop: {code: "process_shutdown", elapsed_seconds: 180}}), 1).stop.code, "process_shutdown");
});

test("capture errors give source-specific guidance without exposing unrecognized error data", () => {
  assert.doesNotMatch(captureMessage("unknown_raw_error"), /unknown_raw_error/);
  assert.match(captureMessage("tab_audio_missing"), /Share tab audio/);
  assert.match(captureMessage("capture_permission_denied", "self"), /microphone access/i);
});

const preciseIssues = [
  ["stream_limit", /approved number of speech connections/],
  ["input_audio_limit", /incoming audio volume limit/],
  ["output_audio_limit", /generated audio volume limit/],
  ["input_pacing", /arrived faster than real time/],
  ["speech_boundary_limit", /natural speech boundary within 120 seconds/],
  ["source_ledger_limit", /source-caption tracking limit/],
  ["transcript_item_limit", /caption exceeded its text or revision limit/],
  ["usage_mismatch", /audio usage inconsistent with the audio submitted/],
  ["source_response_deadline", /did not confirm an audio receipt or final caption/],
  ["protocol_capacity_limit", /local event or output queue limit/],
];

test("precise resource issues have distinct fixed explanations and retain a strict metadata vocabulary", () => {
  const messages = new Set();
  for (const [code, meaning] of preciseIssues) {
    const row = diagnostic({last_issue: {code, source: "remote_mix", route: "transcription", elapsed_seconds: 126}});
    assert.equal(parseDiagnostics(row, 1).last_issue.code, code);
    const message = providerMessage(code); assert.match(message, meaning); assert.match(message, /Processing stopped/);
    assert.doesNotMatch(message, /allowance was spent|retry automatically|reconnect automatically/);
    messages.add(message);
    for (const unexpected of [code.toUpperCase(), code + "_raw_secret", code + " ", "provider_" + code]) {
      assert.throws(() => parseDiagnostics({...row, last_issue: {...row.last_issue, code: unexpected}}, 1));
      assert.equal(providerMessage(unexpected).includes(unexpected), false);
    }
    assert.throws(() => parseDiagnostics({...row, last_issue: {...row.last_issue, provider_message: "raw_secret"}}, 1));
    assert.throws(() => parseDiagnostics({...row, stop: {code, elapsed_seconds: 126}}, 1));
  }
  assert.equal(messages.size, preciseIssues.length);
  for (const code of ["budget_exhausted", "provider_budget"]) {
    assert.match(providerMessage(code), /does not identify which one or prove billed spend/);
  }
  assert.match(providerMessage("usage_mismatch"), /not a record of billed spend/);
});

test("precise last-service issues survive stopped-session reload without retrying any processing", async () => {
  for (const [code, meaning] of preciseIssues) {
    const h = harness({stopped: true});
    h.respond(path => path === "/api/diagnostics" ? json(diagnostic({last_issue: {code, source: "remote_mix", route: "transcription", elapsed_seconds: 126}})) : undefined);
    await h.client.initialize(null); await flush();
    assert.equal(h.client.snapshot().stopped, true);
    assert.equal(h.client.snapshot().diagnostics.last_issue.code, code);
    assert.match(providerMessage(h.client.snapshot().diagnostics.last_issue.code), meaning);
    assert.equal(h.client.snapshot().diagnostics.stop.code, "operator_disconnected");
    assert.deepEqual(h.calls.map(call => call.path), ["/api/session", "/api/diagnostics", "/api/stop-receipt"]);
    assert.equal(h.sockets.length, 0); assert.equal(h.effects.captures, 0); assert.equal(h.effects.sound, 0);
    h.client.dispose();
  }
});

test("stopped reload only reads the session, diagnostics and private Stop receipt without rearming devices, sockets, routes or sound", async () => {
  const h = harness({stopped: true}); await h.client.initialize(null); await flush();
  assert.deepEqual(h.calls.map(call => call.path), ["/api/session", "/api/diagnostics", "/api/stop-receipt"]);
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().diagnostics.last_issue.code, "text_limit");
  assert.equal(h.client.snapshot().diagnosticsState, "ready"); assert.equal(h.sockets.length, 0); assert.equal(h.effects.captures, 0); assert.equal(h.effects.sound, 0);
  const options = h.calls[1].options;
  assert.equal(options.cache, "no-store"); assert.equal(options.credentials, "omit"); assert.equal(options.redirect, "error"); assert.equal(options.method, "GET");
  h.client.dispose();
});

test("a lost terminal WebSocket frame is recovered through the operator readback without reconnecting", async () => {
  const h = harness(); await h.connected(); h.sockets[0].onclose();
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.effects.stop, 1);
  await flush(); assert.equal(h.client.snapshot().diagnostics.stop.code, "operator_disconnected");
  await h.client.connect(); assert.equal(h.sockets.length, 1); h.client.dispose();
});

test("explicit Stop latches locally before waiting and reads diagnostics only after its HTTP result", async () => {
  const h = harness(), pending = deferred(); await h.client.initialize(null);
  h.respond(path => path === "/api/stop" ? pending.promise : undefined);
  const stopping = h.client.stop();
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.effects.stop, 1);
  await flush(); assert.equal(h.calls.some(call => call.path === "/api/diagnostics"), false);
  pending.resolve(json({state: "stopped", generation: 2})); await stopping;
  assert.equal(h.calls.filter(call => call.path === "/api/diagnostics").length, 1);
  assert.equal(h.client.snapshot().diagnosticsState, "ready"); h.client.dispose();
});

test("received stopped reason is fixed and preserved while delayed and repeated Stop responses change status", async () => {
  const h = harness(), pending = deferred(); await h.connected();
  h.sockets[0].onmessage({data: JSON.stringify({type: "stopped", generation: 2, reason: "backpressure"})});
  const reason = h.client.snapshot().terminalMessage; assert.match(reason, /could not keep up/);
  h.respond(path => path === "/api/stop" ? pending.promise : undefined);
  const stopping = h.client.stop(); await flush();
  assert.equal(h.client.snapshot().terminalMessage, reason);
  pending.resolve(json({state: "stopped", generation: 2})); await stopping; await h.client.stop(false);
  assert.equal(h.client.snapshot().terminalMessage, reason); assert.equal(h.calls.filter(call => call.path === "/api/diagnostics").length, 1);
  assert.equal(h.client.snapshot().diagnostics.last_issue.code, "text_limit"); h.client.dispose();
});

test("malformed, oversized, unauthorized and unavailable diagnostics never invent a reason or unlock processing", async () => {
  for (const answer of [() => json(diagnostic({raw: "synthetic_private_payload"})), () => json({padding: "x".repeat(2049)}), () => json({}, 401), () => Promise.reject(new Error("synthetic_private_payload"))]) {
    const h = harness({stopped: true}); h.respond(path => path === "/api/diagnostics" ? answer() : undefined);
    await h.client.initialize(null); await flush();
    assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().diagnostics, null); assert.equal(h.client.snapshot().diagnosticsState, "unavailable");
    assert.equal(JSON.stringify(h.client.snapshot()).includes("synthetic_private_payload"), false); assert.equal(h.sockets.length, 0); h.client.dispose();
  }
});

test("late diagnostics and Stop replies after sign out or disposal cannot restore state", async () => {
  for (const action of ["logout", "dispose"]) {
    const h = harness({stopped: true}), pending = deferred();
    h.respond(path => path === "/api/diagnostics" ? pending.promise : undefined);
    await h.client.initialize(null); await flush(); assert.equal(h.client.snapshot().diagnosticsState, "loading");
    await h.client[action](); pending.resolve(json(diagnostic())); await flush();
    assert.equal(h.client.snapshot().diagnostics, null);
    if (action === "logout") {assert.equal(h.client.snapshot().session, null); assert.equal(h.client.snapshot().diagnosticsState, "idle");}
  }
  const h = harness(), pending = deferred(); await h.client.initialize(null);
  h.respond(path => path === "/api/stop" ? pending.promise : undefined);
  const stopping = h.client.stop(); await h.client.logout(); const state = h.client.snapshot();
  pending.resolve(json({state: "stopped", generation: 2})); await stopping;
  assert.deepEqual(h.client.snapshot(), state); assert.equal(h.calls.some(call => call.path === "/api/diagnostics"), false);
});

test("public stop and reload never request or retain operator diagnostics", async () => {
  const h = harness({role: "public", stopped: true}); await h.client.initialize(null); await h.client.stop(); await flush();
  assert.equal(h.calls.some(call => call.path === "/api/diagnostics"), false); assert.equal(h.client.snapshot().diagnostics, null); h.client.dispose();
});

test("reentrant logout when loading diagnostics prevents even the request from being sent", async () => {
  const h = harness({stopped: true}); h.client.subscribe(() => {if (h.client.snapshot().diagnosticsState === "loading") void h.client.logout();});
  await h.client.initialize(null); await flush(); assert.equal(h.calls.some(call => call.path === "/api/diagnostics"), false); assert.equal(h.client.snapshot().session, null);
});

test("diagnostic timeout aborts the read without resuming the stopped client", async () => {
  const h = harness({stopped: true}); let aborted = false;
  h.respond((path, options) => path === "/api/diagnostics" ? new Promise((_resolve, reject) => {
    options.signal.addEventListener("abort", () => {aborted = true; reject(new Error("Synthetic stalled read"));}, {once: true});
  }) : undefined);
  await h.client.initialize(null);
  await new Promise(resolve => setTimeout(resolve, 5100));
  assert.equal(aborted, true); assert.equal(h.client.snapshot().diagnosticsState, "unavailable");
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.sockets.length, 0); h.client.dispose();
});

test("continuous elapsed observations remain exact safe integers beyond two hours", () => {
  const row = diagnostic({stop: {code: "operator_stop", elapsed_seconds: Number.MAX_SAFE_INTEGER}, last_issue: {code: "source_incomplete", route: "transcription", source: "remote_mix", elapsed_seconds: 10000}});
  assert.equal(parseDiagnostics(row, 1).stop.elapsed_seconds, Number.MAX_SAFE_INTEGER);
  assert.equal(parseDiagnostics(row, 1).last_issue.code, "source_incomplete");
});


test("valid Live closure reasons remain exact fixed metadata without inferring user intent or blocked content", () => {
  const meanings = [
    ["live_closed_requested", /provider reported a close request/],
    ["live_expired", /provider reported that the GPT-Live-1 session expired/],
    ["live_content_closed", /provider ended the GPT-Live-1 session because of its content handling/],
    ["live_remote_hangup", /provider reported a remote hangup/],
    ["live_connection_lost", /provider reported a lost connection/],
  ];
  const messages = new Set();
  for (const [code, meaning] of meanings) {
    const row = diagnostic({last_issue: {code, source: "remote_mix", route: "live", elapsed_seconds: 42}});
    assert.equal(parseDiagnostics(row, 1).last_issue.code, code);
    const text = providerMessage(code); assert.match(text, meaning); assert.match(text, /no automatic retry/);
    assert.doesNotMatch(text, /you requested|unsafe speech|prohibited|bypass|account spend/); messages.add(text);
    assert.throws(() => parseDiagnostics({...row, last_issue: {...row.last_issue, code: `${code}_raw_detail`}}, 1));
    assert.equal(providerMessage(`${code}_raw_detail`).includes(`${code}_raw_detail`), false);
  }
  assert.equal(messages.size, 5);
});
