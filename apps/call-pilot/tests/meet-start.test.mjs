import assert from "node:assert/strict";
import test from "node:test";
import {PilotClient} from "../.test-build/client.js";
import {parseBudget} from "../.test-build/cost.js";
import {captureMessage, providerMessage} from "../.test-build/feedback.js";
import {budgetFixture, disabledBudget} from "./budget-fixture.mjs";

const deferred = () => {let resolve, reject; const promise = new Promise((yes, no) => {resolve = yes; reject = no;}); return {promise, resolve, reject};};
const flush = async () => {for (let i = 0; i < 10; i++) await new Promise(resolve => setImmediate(resolve));};
const json = (value, status = 200) => new Response(JSON.stringify(value), {status, headers: {"Content-Type": "application/json"}});
const frame = (source = "remote_mix") => {const buffer = new ArrayBuffer(646), view = new DataView(buffer); view.setUint8(0, 1); view.setUint8(1, source === "self" ? 1 : 0); return buffer;};
const signal = {level: 0.1, frames: 12, muted: false};
function harness({enabled = true, source = "remote_mix"} = {}) {
  let now = 0, tick = () => {}, respond, budget = enabled ? budgetFixture() : disabledBudget();
  const calls = [], sockets = [], effects = {prepare: 0, cancel: 0, activate: 0, resume: 0, stop: 0, muted: [], private: [], preparedSources: [], activatedSources: [], cancelledSources: []};
  const audio = {prepareCapture: async selected => {effects.prepare++; effects.preparedSources.push(selected);}, cancelPreparedCapture: async selected => {effects.cancel++; effects.cancelledSources.push(selected);}, activateCapture: selected => {effects.activate++; effects.activatedSources.push(selected);}, startCapture: async () => {}, stopCapture: () => {}, stopAll: () => {effects.stop++;}, resumePlayback: async () => {effects.resume++;}, setPrivateMuted: (...args) => effects.muted.push(args), playPrivate: (...args) => effects.private.push(args), stopClip: () => {}, playClip: async () => {}};
  const client = new PilotClient("operator", {audio, now: () => now, interval: callback => {tick = callback; return () => {tick = () => {};};}, broadcastStop: () => {}, verifyClip: async () => true, openPublicWindow: () => {},
    fetch: async (path, options = {}) => {
      const body = options.body && JSON.parse(options.body); calls.push({path, body, options});
      const answer = respond?.(path, body, options); if (answer !== undefined) return answer;
      if (path === "/api/session") return json({role: "operator", csrf: "synthetic_csrf", generation: 1, stopped: false, providers_enabled: enabled, expires_in_seconds: 7200, capabilities: {source_captions: enabled, private_translation: false, live_assistant: false, live_interpreter: enabled, prepared_public_speech: false}, limitations: ["Synthetic test only"]});
      if (path === "/api/budget") return json(budget);
      if (path === "/api/routes") return json({state: "starting", generation: 1, route_id: body.route === "transcription" ? "captions_one" : "interpreter_one"});
      if (path === "/api/stop") return json({state: "stopped", generation: 2});
      if (path === "/api/source-gap") return json({state: "closed", generation: 1});
      throw new Error("Unexpected test request");
    }, socket: () => {const socket = {readyState: 1, bufferedAmount: 0, sent: [], closed: false, onmessage: null, send(value) {this.sent.push(value);}, close() {this.closed = true;}}; sockets.push(socket); return socket;},
  });
  const event = value => sockets.at(-1).onmessage?.({data: JSON.stringify({generation: 1, ...value})});
  return {client, calls, sockets, effects, audio, event, budget: value => {budget = value;}, respond: callback => {respond = callback;},
    advance(ms) {now += ms; tick();}, elapse(ms) {now += ms;},
    hello() {event({type: "hello", role: "operator", stopped: false});},
    route(route, state = "ready", code = null, overrides = {}) {event({type: "route", route_id: route === "transcription" ? "captions_one" : "interpreter_one", source, route, language: "en", live_mode: route === "live" ? "interpretation" : null, state, code, expires_in_seconds: state === "ready" ? 600 : null, ...overrides});},
    routes() {return calls.filter(call => call.path === "/api/routes");},
    async preview() {await client.initialize(null); client.setMeetSource(source); client.confirmCapture(true); await client.selectMeetAudio(); client.captureStatus(source, signal);},
    async first() {const start = client.startMeet("en", 670000); await flush(); this.hello(); await flush(); return {start};},
    async listening() {await this.preview(); const {start} = await this.first(); this.route("transcription"); await flush(); this.route("live"); await start;},
  };
}

test("local preview remains usable with providers disabled and sends no socket, provider request or PCM", async () => {
  const h = harness({enabled: false}); await h.client.initialize(null);
  assert.match(h.client.snapshot().message, /Key-free rehearsal/);
  h.client.setMeetSource("self");
  assert.doesNotMatch(h.client.snapshot().message, /Start will/);
  h.client.setMeetSource("remote_mix");
  await h.preview();
  assert.match(h.client.snapshot().message, /Local audio check active/);
  assert.equal(h.client.snapshot().meetPhase, "preview"); assert.equal(h.client.snapshot().meetSignalSeen, true);
  assert.equal(h.client.sendFrame("remote_mix", frame()), false); await h.client.startMeet("en", 670000);
  assert.equal(h.sockets.length, 0); assert.equal(h.routes().length, 0); assert.equal(h.effects.activate, 0); assert.equal(h.effects.resume, 0);
  h.client.dispose();
});

test("manual Start retains agreement, current quote, allowance and supported language checks", async () => {
  for (const blocked of ["agreement", "stale_quote", "low_max", "language"]) {
    const h = harness(); await h.preview();
    if (blocked === "agreement") h.client.confirmCapture(false);
    if (blocked === "silence") {h.client.cancelMeetSetup(); await flush(); await h.client.selectMeetTab(); h.client.captureStatus("remote_mix", {level: 0, frames: 50, muted: false});}
    if (blocked === "muted") h.client.captureStatus("remote_mix", {...signal, muted: true});
    if (blocked === "stale_signal") {h.elapse(20001); await h.client.refreshBudget();}
    if (blocked === "stale_quote") h.elapse(15001);
    await h.client.startMeet(blocked === "language" ? "xx" : "en", blocked === "low_max" ? 669999 : 670000);
    assert.equal(h.routes().length, 0, blocked); assert.equal(h.effects.activate, 0, blocked); assert.equal(h.sockets.length, 0, blocked); h.client.dispose();
  }
});

test("quiet, muted and old meter observations do not block deliberate Start", async () => {
  for (const observation of ["quiet", "muted", "old_signal"]) {
    const h = harness(); await h.preview();
    h.client.captureStatus("remote_mix", {level: 0, frames: 500, muted: observation === "muted"});
    if (observation === "old_signal") {h.elapse(21000); await h.client.refreshBudget();}
    const {start} = await h.first(); h.route("transcription"); await flush(); h.route("live"); await start;
    assert.equal(h.client.snapshot().meetPhase, "listening", observation); assert.equal(h.effects.activate, 1); h.client.dispose();
  }
});

test("primary Start invokes chooser and playback before network waits, refreshes cost and starts through silence", async () => {
  const h = harness(), chosen = deferred(); await h.client.initialize(null); h.client.confirmCapture(true);
  h.audio.prepareCapture = () => {h.effects.prepare++; return chosen.promise;};
  const requestCount = h.calls.length, started = h.client.startMeetSession("en", 670000);
  assert.equal(h.effects.prepare, 1); assert.equal(h.effects.resume, 1); assert.equal(h.calls.length, requestCount);
  await h.client.startMeetSession("en", 670000); assert.equal(h.effects.prepare, 1);
  chosen.resolve(); await flush(); h.hello(); await flush(); h.route("transcription"); await flush(); h.route("live"); await started;
  assert.equal(h.client.snapshot().meetPhase, "listening"); assert.equal(h.effects.resume, 1); assert.equal(h.effects.activate, 1);
  assert.equal(h.client.snapshot().meetSignalSeen, false); assert.equal(h.routes().length, 2); h.client.dispose();
});

test("live Start requires the displayed estimate and refuses a higher price after audio selection", async () => {
  for (const state of ["missing", "stale", "wrong amount", "different windows", "increased"]) {
    const h = harness();
    const budget = budgetFixture();
    if (state === "different windows") {
      budget.routes[0] = {...budget.routes[0], quote_seconds: 300, quote_microusd: 85000};
    } else if (state === "increased") {
      budget.routes = budget.routes.map(row => ({...row, quote_seconds: 300, quote_microusd: row.per_minute_microusd * 5}));
    }
    h.budget(budget);
    await h.client.initialize(null);
    h.client.confirmCapture(true);
    if (state === "stale") h.elapse(15001);
    if (state === "increased") h.budget(budgetFixture());
    const displayed = {"missing": undefined, "stale": 670000, "wrong amount": 999999, "different windows": 585000, "increased": 335000}[state];
    await h.client.startMeetSession("en", displayed);
    assert.equal(h.routes().length, 0, state);
    assert.equal(h.effects.prepare, state === "increased" ? 1 : 0, state);
    if (state === "increased") assert.match(h.client.snapshot().meetIssue, /estimate increased/);
    h.client.dispose();
  }
});

test("primary Start is cancelled by Stop, withdrawal or releasing a still-open picker", async () => {
  for (const action of ["stop", "withdraw", "release"]) {
    const h = harness(), chosen = deferred(); await h.client.initialize(null); h.client.confirmCapture(true);
    h.audio.prepareCapture = () => chosen.promise;
    const start = h.client.startMeetSession("en", 670000);
    if (action === "stop") await h.client.stop();
    if (action === "withdraw") h.client.confirmCapture(false);
    if (action === "release") h.client.cancelMeetSetup();
    chosen.resolve(); await start; await flush();
    assert.equal(h.routes().length, 0, action); assert.equal(h.effects.activate, 0); assert.equal(h.sockets.length, 0); h.client.dispose();
  }
});

test("cancelled picker and unavailable cost check can be retried without a process restart", async () => {
  const h = harness(); await h.client.initialize(null); h.client.confirmCapture(true);
  h.audio.prepareCapture = async () => {throw new Error("capture_permission_denied");};
  await h.client.startMeetSession("en", 670000); assert.equal(h.client.snapshot().meetPhase, "idle"); assert.equal(h.routes().length, 0);
  h.audio.prepareCapture = async () => {}; h.respond(path => path === "/api/budget" ? json(disabledBudget()) : undefined);
  await h.client.startMeetSession("en", 670000); assert.equal(h.client.snapshot().meetPhase, "preview"); assert.equal(h.client.snapshot().busy, false);
  assert.equal(h.client.snapshot().stopped, false); assert.equal(h.effects.activate, 0); assert.equal(h.routes().length, 0);
  h.respond(() => undefined); await h.client.refreshBudget(); const start = h.client.startMeetSession("en", 670000); await flush(); h.hello(); await flush(); h.route("transcription"); await flush(); h.route("live"); await start;
  assert.equal(h.client.snapshot().meetPhase, "listening"); h.client.dispose();
});

test("reusable selection failures keep the source open and preserve specific feedback without provider work", async () => {
  for (const code of ["tab_audio_missing", "select_browser_tab", "capture_permission_or_device_failed", "capture_selection_timeout"]) {
    const h = harness(); await h.client.initialize(null); h.client.confirmCapture(true);
    h.audio.prepareCapture = async () => {throw new Error(code);};
    await h.client.selectMeetTab(); assert.equal(h.client.snapshot().meetPhase, "idle"); assert.equal(h.client.snapshot().stopped, false);
    assert.equal(h.client.snapshot().capture.remote_mix, "idle"); assert.ok(h.client.snapshot().meetIssue);
    h.audio.prepareCapture = async () => {h.effects.prepare++;}; await h.client.selectMeetTab();
    assert.equal(h.client.snapshot().meetPhase, "preview"); assert.equal(h.effects.prepare, 1);
    assert.equal(h.sockets.length, 0); assert.equal(h.calls.some(call => ["/api/routes", "/api/source-gap"].includes(call.path)), false); h.client.dispose();
  }
});

test("release waits for resource acknowledgement; late old selection cannot enable Choose or erase a new selection", async () => {
  const h = harness(), selection = deferred(), release = deferred();
  await h.client.initialize(null); h.client.confirmCapture(true); h.audio.prepareCapture = () => selection.promise;
  h.audio.cancelPreparedCapture = () => release.promise;
  const choosing = h.client.selectMeetTab(); h.client.cancelMeetSetup();
  assert.equal(h.client.snapshot().busy, true); assert.equal(h.client.snapshot().meetPhase, "idle");
  selection.resolve(); await choosing; assert.equal(h.client.snapshot().busy, true);
  release.resolve(); await flush(); assert.equal(h.client.snapshot().busy, false);
  h.audio.prepareCapture = async () => {}; await h.client.selectMeetTab(); assert.equal(h.client.snapshot().meetPhase, "preview"); h.client.dispose();
});

test("pending native chooser is recoverable; uncertain release instead stops globally", async () => {
  for (const code of ["capture_selection_pending", "audio_cleanup_unconfirmed"]) {
    const h = harness(); await h.preview(); h.audio.cancelPreparedCapture = async () => {throw new Error(code);};
    h.client.cancelMeetSetup(); await flush();
    assert.equal(h.client.snapshot().stopped, code === "audio_cleanup_unconfirmed");
    assert.match(h.client.snapshot().meetIssue, code === "capture_selection_pending" ? /Cancel the browser tab chooser/ : /release could not be confirmed/);
    assert.equal(h.routes().length, 0); h.client.dispose();
  }
});

test("preview no-frames diagnostics survive asynchronous release; stopped callbacks cannot restore meter state", async () => {
  const h = harness(); await h.preview(); h.client.captureGap("remote_mix", "capture_no_frames"); await flush();
  assert.match(h.client.snapshot().meetIssue, /not delivering audio frames/); assert.equal(h.client.snapshot().meetPhase, "idle");
  h.client.captureStatus("remote_mix", signal); assert.equal(h.client.snapshot().captureStatus.remote_mix, null);
  await h.client.selectMeetTab(); h.client.captureStatus("remote_mix", {...signal, level: NaN}); await flush();
  assert.equal(h.client.snapshot().meetSignalSeen, false); await h.client.stop(); h.client.captureStatus("remote_mix", signal);
  assert.equal(h.client.snapshot().captureStatus.remote_mix, null);
});

test("Start admits the pair sequentially, overlaps provider startup, then admits only future PCM and interpreter sound", async () => {
  const h = harness(); await h.preview(); const start = h.client.startMeet("en", 670000);
  assert.equal(h.effects.resume, 1); assert.equal(h.routes().length, 0);
  await h.client.startMeet("en", 670000); await flush(); assert.equal(h.sockets.length, 1); assert.equal(h.routes().length, 0);
  h.hello(); await flush(); assert.deepEqual(h.routes().map(call => call.body.route), ["transcription", "live"]);
  assert.equal(h.client.sendFrame("remote_mix", frame()), false); assert.equal(h.effects.activate, 0);
  h.route("transcription"); await flush(); assert.deepEqual(h.routes().map(call => call.body.route), ["transcription", "live"]);
  assert.equal(h.routes()[1].body.live_mode, "interpretation"); assert.equal(h.client.sendFrame("remote_mix", frame()), false);
  h.route("live"); await start; assert.equal(h.client.snapshot().meetPhase, "listening"); assert.equal(h.effects.activate, 1);
  assert.deepEqual(h.effects.muted, [["live_interpretation", false]]);
  assert.equal(h.client.sendFrame("remote_mix", frame()), true); h.client.captureStatus("remote_mix", {...signal, frames: 99});
  assert.equal(h.client.snapshot().sentFrames.remote_mix, 1); assert.equal(h.client.snapshot().sourceFragments.length, 0); assert.equal(h.client.snapshot().fragments.length, 0);
  await h.client.startMeet("en", 670000); assert.equal(h.routes().length, 2); h.client.dispose();
});

test("Ready before HTTP is matched by immutable identity and does not duplicate routes", async () => {
  const h = harness(); await h.preview();
  h.respond((path, body) => {if (path === "/api/routes") {h.route(body.route); return json({state: "starting", generation: 1, route_id: body.route === "transcription" ? "captions_one" : "interpreter_one"});}});
  const {start} = await h.first(); await start;
  assert.equal(h.client.snapshot().meetPhase, "listening"); assert.equal(h.client.snapshot().routes.length, 2); assert.equal(h.effects.activate, 1); h.client.dispose();
});

test("independent 800ms and 1100ms provider startups reach capture at 1100ms in either Ready order", async () => {
  for (const latencies of [{transcription: 800, live: 1100}, {transcription: 1100, live: 800}]) {
    const h = harness(); await h.preview(); let elapsed = 0, activated = null;
    const admitted = {}, ready = {};
    h.respond((path, body) => {if (path === "/api/routes") admitted[body.route] = elapsed;});
    h.audio.activateCapture = () => {activated = elapsed; h.effects.activate++;};
    const {start} = await h.first();
    // Synthetic provider delays and zero-duration HTTP isolate coordinator
    // delay. This is not a measurement of OpenAI or network response times.
    assert.deepEqual(admitted, {transcription: 0, live: 0});
    while (elapsed < 1100) {
      elapsed += 100; h.elapse(100); h.event({type: "heartbeat"});
      for (const route of ["transcription", "live"]) {
        if (ready[route] === undefined && elapsed - admitted[route] >= latencies[route]) {ready[route] = elapsed; h.route(route);}
      }
      await flush();
      if (elapsed < 1100) {assert.equal(h.effects.activate, 0); assert.equal(h.client.sendFrame("remote_mix", frame()), false);}
    }
    await start;
    assert.equal(activated, 1100); assert.equal(h.effects.activate, 1);
    assert.equal(h.client.snapshot().meetPhase, "listening");
    assert.equal(h.routes().length, 2); h.client.dispose();
  }
});

test("caption failure during pending interpreter HTTP aborts the sibling and observes its late rejection", async () => {
  const h = harness(), pending = deferred(); await h.preview();
  h.respond((path, body) => path === "/api/routes" && body.route === "live" ? pending.promise : undefined);
  const {start} = await h.first();
  assert.equal(h.routes().length, 2); assert.equal(h.effects.activate, 0);
  h.route("transcription", "failed", "provider_protocol"); await start;
  assert.equal(h.client.snapshot().stopped, true);
  assert.equal(h.routes()[1].options.signal.aborted, true);
  assert.equal(h.calls.filter(call => call.path === "/api/stop").length, 1);
  pending.reject(new Error("late synthetic transport rejection")); await flush();
  assert.equal(h.effects.activate, 0); assert.deepEqual(h.effects.muted, []);
  assert.equal(h.routes().length, 2); h.client.dispose();
});

test("Stop while only the interpreter is Ready cannot activate capture when the caption Ready arrives late", async () => {
  const h = harness(); await h.preview(); const {start} = await h.first();
  h.route("live"); await flush(); assert.equal(h.effects.activate, 0);
  await h.client.stop(); await start; h.route("transcription"); await flush();
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.effects.activate, 0);
  assert.deepEqual(h.effects.muted, []); assert.equal(h.routes().length, 2);
  assert.equal(h.calls.filter(call => call.path === "/api/stop").length, 1); h.client.dispose();
});

test("lost or refused admission, either service failure and changed identity stop all without retry or PCM", async () => {
  for (const fail of ["first_refusal", "second_refusal", "lost_receipt", "captions_failure", "interpreter_failure", "identity"]) {
    const h = harness(); await h.preview();
    h.respond((path, body) => {
      if (path !== "/api/routes") return;
      if (fail === "lost_receipt") throw new Error("synthetic loss after possible admission");
      if (fail === "first_refusal" || (fail === "second_refusal" && body.route === "live")) return json({}, 429);
      if (fail === "identity") {h.route("transcription", "ready", null, {language: "ja"}); return json({state: "starting", generation: 1, route_id: "captions_one"});}
    });
    const {start} = await h.first();
    if (fail === "captions_failure") h.route("transcription", "failed", "provider_protocol");
    if (["second_refusal", "interpreter_failure"].includes(fail)) {h.route("transcription"); await flush();}
    if (fail === "interpreter_failure") h.route("live", "failed", "deadline");
    await start; await flush(); assert.equal(h.client.snapshot().stopped, true, fail); assert.equal(h.effects.activate, 0, fail);
    assert.equal(h.calls.filter(call => call.path === "/api/stop").length, 1, fail);
    const before = h.routes().length; await h.client.startMeet("en", 670000); assert.equal(h.routes().length, before);
    if (fail === "captions_failure") assert.match(h.client.snapshot().meetIssue, /unsupported response/);
    if (fail === "interpreter_failure") assert.match(h.client.snapshot().meetIssue, /deadline/);
  }
});

test("a more expensive second quote is refused within the originally displayed maximum", async () => {
  const h = harness(); await h.preview();
  const changed = budgetFixture({route_duration_seconds: 601}); changed.routes[2].quote_seconds = 601; changed.routes[2].quote_microusd = 500834;
  assert.equal(parseBudget(changed, 1).routes[2].can_reserve, true);
  h.respond((path, body) => {if (path === "/api/routes" && body.route === "transcription") h.budget(changed);});
  const {start} = await h.first(); await start;
  assert.equal(h.routes().length, 1); assert.equal(h.client.snapshot().stopped, true); assert.equal(h.effects.activate, 0);
  assert.match(h.client.snapshot().meetIssue, /no longer fits/);
});

test("Stop, withdrawal and source loss cancel each startup stage without later admission or sound", async () => {
  for (const stage of ["playback", "hello", "captions_http", "captions_ready", "interpreter_http", "interpreter_ready"]) {
    for (const action of ["stop", "withdraw", "track_end"]) {
      const h = harness(), pending = deferred(); await h.preview();
      if (stage === "playback") h.audio.resumePlayback = () => pending.promise;
      h.respond((path, body) => path === "/api/routes" && ((stage === "captions_http" && body.route === "transcription") || (stage === "interpreter_http" && body.route === "live")) ? pending.promise : undefined);
      const start = h.client.startMeet("en", 670000); await flush();
      if (!["playback", "hello"].includes(stage)) {h.hello(); await flush();}
      if (stage.startsWith("interpreter")) {h.route("transcription"); await flush();}
      const before = h.routes().length;
      if (action === "stop") await h.client.stop();
      if (action === "withdraw") h.client.confirmCapture(false);
      if (action === "track_end") h.client.captureGap("remote_mix", "capture_track_ended");
      pending.resolve(stage.endsWith("http") ? json({state: "starting", generation: 1, route_id: stage.startsWith("captions") ? "captions_one" : "interpreter_one"}) : undefined);
      await start; await flush(); assert.equal(h.client.snapshot().stopped, true, `${stage}/${action}`);
      assert.equal(h.routes().length, before, `${stage}/${action}`); assert.equal(h.effects.activate, 0); assert.equal(h.effects.muted.some(([, muted]) => !muted), false);
    }
  }
});

test("source gap during listening stops both services and never substitutes the microphone", async () => {
  const h = harness(); await h.listening(); h.client.captureGap("remote_mix", "capture_no_frames"); await flush();
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.effects.stop, 1); assert.equal(h.routes().length, 2);
  assert.match(h.client.snapshot().meetIssue, /not delivering audio frames/); assert.equal(h.client.sendFrame("self", frame()), false);
});

test("precise service limits preserve their cause through immediate paired Stop and server disconnect readback", async () => {
  for (const code of ["speech_boundary_limit", "source_ledger_limit", "usage_mismatch", "input_audio_limit", "source_response_deadline", "protocol_capacity_limit"]) {
    for (const eventKind of ["gap", "route"]) {
      const h = harness(), pending = deferred(); await h.listening();
      h.respond(path => path === "/api/stop" ? pending.promise : path === "/api/diagnostics" ? json({version: 1, generation: 2, stopped: true, stop: {code: "operator_disconnected", elapsed_seconds: 126}, last_issue: {code, source: "remote_mix", route: "transcription", elapsed_seconds: 126}}) : undefined);
      if (eventKind === "gap") h.event({type: "gap", source: "remote_mix", code});
      else h.route("transcription", "failed", code);
      // Local shutdown and its specific cause precede any Stop HTTP receipt.
      assert.equal(h.client.snapshot().stopped, true); assert.equal(h.effects.stop, 1);
      assert.equal(h.sockets[0].closed, true); assert.equal(h.client.snapshot().terminalMessage, providerMessage(code));
      assert.equal(h.client.snapshot().meetIssue, providerMessage(code));
      assert.equal(h.client.sendFrame("remote_mix", frame()), false);
      assert.equal(h.calls.filter(call => call.path === "/api/stop").length, 1);
      await h.client.startMeet("en", 670000); await h.client.openRoute("remote_mix", "transcription", "en");
      assert.equal(h.routes().length, 2); assert.equal(h.sockets.length, 1);
      pending.resolve(json({state: "stopped", generation: 2})); await flush();
      assert.equal(h.client.snapshot().terminalMessage, providerMessage(code));
      assert.equal(h.client.snapshot().diagnostics.last_issue.code, code);
      assert.equal(h.client.snapshot().diagnostics.stop.code, "operator_disconnected");
      assert.equal(h.client.snapshot().meetPhase, "failed");
      assert.equal(h.calls.filter(call => call.path === "/api/diagnostics").length, 1);
      h.client.dispose();
    }
  }
});

test("Finish during pending interpretation fences startup but preserves ordered source-only finalization", async () => {
  const h = harness(); await h.preview(); const {start} = await h.first(); h.route("transcription"); await flush();
  const finish = h.client.finishExport(); await start;
  assert.equal(h.client.snapshot().finishing, true); assert.equal(h.effects.activate, 0);
  assert.equal(h.client.sendFrame("remote_mix", frame()), false);
  const sent = h.sockets[0].sent.map(value => JSON.parse(value)); assert.equal(sent.at(-1).type, "finish");
  h.event({type: "finishing"}); h.event({type: "finished", source_completion: "complete"});
  assert.match(await finish, /FINALIZATION/); assert.equal(h.client.snapshot().stopped, true);
});

test("playback and overlapping provider readiness deadlines remain bounded with no late capture or sound", async () => {
  for (const stage of ["playback", "captions", "interpreter"]) {
    const h = harness(), pending = deferred(); await h.preview();
    if (stage === "playback") h.audio.resumePlayback = () => pending.promise;
    const start = h.client.startMeet("en", 670000); await flush();
    if (stage !== "playback") {h.hello(); await flush();}
    if (stage === "interpreter") {h.route("transcription"); await flush();}
    if (stage === "playback") h.advance(5001);
    else for (let n = 0; n < 6; n++) {h.elapse(2000); h.event({type: "heartbeat"}); h.advance(1);}
    await start; assert.equal(h.client.snapshot().stopped, true, stage);
    assert.equal(h.routes().length, stage === "playback" ? 0 : 2);
    assert.equal(h.effects.activate, 0); pending.resolve(); await flush(); assert.equal(h.effects.muted.some(([, muted]) => !muted), false);
  }
});

test("local route expiration stops the pair even when server heartbeats continue without a closure event", async () => {
  const h = harness(); await h.listening();
  // Keep the liveness lease current while the local pinned deadline expires.
  for (let n = 0; n < 300; n++) {h.elapse(2000); h.event({type: "heartbeat"});}
  h.advance(1); await flush(); assert.equal(h.client.snapshot().stopped, true); assert.equal(h.effects.stop, 1);
  assert.match(h.client.snapshot().meetIssue, /time limit/); assert.equal(h.calls.filter(call => call.path === "/api/stop").length, 1);
});

test("startup error preserves unconfirmed server Stop and cleanup disposition", async () => {
  const h = harness(); await h.preview();
  h.audio.stopAll = () => {throw new Error("synthetic cleanup failure");};
  h.respond(path => path === "/api/routes" ? json({}, 503) : path === "/api/stop" ? json({}, 503) : undefined);
  const {start} = await h.first(); await start;
  assert.match(h.client.snapshot().message, /Server Stop is unconfirmed/);
  assert.match(h.client.snapshot().message, /resource cleanup was unconfirmed/);
  assert.match(h.client.snapshot().meetIssue, /could not connect/);
});

test("pending picker after failed selection preserves cancellation guidance without pretending cleanup failed", async () => {
  const h = harness(); await h.client.initialize(null); h.client.confirmCapture(true);
  h.audio.prepareCapture = async () => {throw new Error("capture_selection_timeout");};
  h.audio.cancelPreparedCapture = async () => {throw new Error("capture_selection_pending");};
  await h.client.selectMeetTab(); assert.equal(h.client.snapshot().stopped, false);
  assert.equal(h.client.snapshot().meetPhase, "idle"); assert.match(h.client.snapshot().meetIssue, /Cancel the browser tab chooser/);
  h.client.dispose();
});

test("an in-flight budget read invalidated by Ready is refreshed before the second admission", async () => {
  const h = harness(), read = deferred(); await h.preview(); let reads = 0;
  h.respond(path => path === "/api/budget" && ++reads === 1 ? read.promise : undefined);
  const {start} = await h.first(); assert.equal(h.routes().length, 1);
  h.route("transcription"); await flush(); assert.equal(h.routes().length, 1);
  read.resolve(json(budgetFixture())); await flush(); assert.equal(h.routes().length, 2);
  h.route("live"); await start; assert.equal(h.client.snapshot().meetPhase, "listening"); h.client.dispose();
});

test("Stop reentered while capture or playback activates cannot restore listening state", async () => {
  for (const at of ["capture", "playback"]) {
    const h = harness(); await h.preview();
    if (at === "capture") h.audio.activateCapture = () => {void h.client.stop();};
    else h.audio.setPrivateMuted = (_kind, muted) => {if (!muted) void h.client.stop();};
    const {start} = await h.first(); h.route("transcription"); await flush(); h.route("live"); await start;
    assert.equal(h.client.snapshot().stopped, true); assert.notEqual(h.client.snapshot().meetPhase, "listening");
    assert.equal(h.client.snapshot().capture.remote_mix, "closed"); assert.equal(h.client.snapshot().muted.live_interpretation, true);
  }
});

test("explicit microphone Start sends only self audio after both routes are Ready and displays self ASR", async () => {
  const h = harness({source: "self"}); await h.preview();
  assert.equal(h.client.snapshot().meetSource, "self");
  assert.deepEqual(h.effects.preparedSources, ["self"]);
  assert.equal(h.client.snapshot().captureStatus.self.level, signal.level);
  assert.equal(h.client.snapshot().captureStatus.remote_mix, null);
  assert.equal(h.client.sendFrame("self", frame("self")), false);
  const {start} = await h.first(); h.route("live"); await flush();
  assert.equal(h.effects.activate, 0); h.route("transcription"); await start;
  assert.deepEqual(h.routes().map(call => [call.body.source, call.body.route]), [["self", "transcription"], ["self", "live"]]);
  assert.deepEqual(h.effects.activatedSources, ["self"]);
  assert.equal(h.client.snapshot().capture.remote_mix, "idle");
  assert.equal(h.client.sendFrame("remote_mix", frame()), false);
  assert.equal(h.client.sendFrame("self", frame("self")), true);
  h.client.captureStatus("self", {...signal, frames: 100});
  assert.deepEqual(h.client.snapshot().sentFrames, {remote_mix: 0, self: 1});
  h.event({type: "source_draft", route_id: "captions_one", source: "self", route: "transcription", item_id: "self_utterance", revision: 1, text: "青い箱", retired: false});
  assert.equal(h.client.snapshot().sourceDrafts[0].source, "self");
  assert.equal(h.client.snapshot().sourceDrafts[0].text, "青い箱");
  assert.equal(h.client.snapshot().sourceFragments.length, 0);
  h.client.dispose();
});

test("microphone primary Start keeps the browser gesture and starts through quiet input without tab fallback", async () => {
  const h = harness({source: "self"}), chosen = deferred(); await h.client.initialize(null);
  assert.equal(h.client.setMeetSource("self"), true); h.client.confirmCapture(true);
  h.audio.prepareCapture = selected => {h.effects.preparedSources.push(selected); return chosen.promise;};
  const before = h.calls.length, start = h.client.startMeetSession("en", 670000);
  assert.deepEqual(h.effects.preparedSources, ["self"]); assert.equal(h.effects.resume, 1);
  assert.equal(h.calls.length, before); assert.equal(h.client.setMeetSource("remote_mix"), false);
  h.client.captureStatus("self", {level: 0, frames: 10, muted: false});
  chosen.resolve(); await flush(); h.hello(); await flush(); h.route("transcription"); h.route("live"); await start;
  assert.equal(h.client.snapshot().meetPhase, "listening"); assert.equal(h.client.snapshot().meetSignalSeen, false);
  assert.deepEqual(h.effects.activatedSources, ["self"]); assert.equal(h.client.setMeetSource("remote_mix"), false);
  h.client.dispose();
});

test("denied microphone access preserves explicit source and gives microphone guidance with zero provider work", async () => {
  const h = harness({source: "self"}); await h.client.initialize(null); h.client.setMeetSource("self"); h.client.confirmCapture(true);
  h.audio.prepareCapture = async selected => {h.effects.preparedSources.push(selected); throw new Error("capture_permission_or_device_failed");};
  await h.client.startMeetSession("en", 670000);
  assert.equal(h.client.snapshot().meetPhase, "idle"); assert.equal(h.client.snapshot().meetSource, "self");
  assert.match(h.client.snapshot().meetIssue, /microphone permission/);
  assert.doesNotMatch(h.client.snapshot().meetIssue, /Meeting tab|Share tab audio/);
  assert.deepEqual(h.effects.preparedSources, ["self"]); assert.deepEqual(h.effects.cancelledSources, ["self"]);
  assert.equal(h.sockets.length, 0); assert.equal(h.routes().length, 0); assert.equal(h.effects.activate, 0);
  // A fresh explicit choice is permitted only after local release was confirmed.
  assert.equal(h.client.setMeetSource("remote_mix"), true); h.client.dispose();
});

test("microphone setup timeout offers input-device guidance without a tab-sharing instruction", () => {
  const message = captureMessage("capture_setup_timeout", "self");
  assert.match(message, /microphone/); assert.doesNotMatch(message, /tab|Share/);
});

test("microphone preview source stays pinned through cancellation and pending permission release", async () => {
  const h = harness({source: "self"}), release = deferred();
  assert.equal(h.client.snapshot().meetSourceLocked, false);
  await h.preview();
  assert.equal(h.client.snapshot().meetSourceLocked, true);
  assert.equal(h.client.setMeetSource("remote_mix"), false);
  await h.client.selectMeetTab(); assert.deepEqual(h.effects.preparedSources, ["self"]);
  h.audio.cancelPreparedCapture = selected => {h.effects.cancelledSources.push(selected); return release.promise;};
  h.client.cancelMeetSetup(); assert.equal(h.client.setMeetSource("remote_mix"), false);
  assert.equal(h.client.snapshot().meetSourceLocked, true);
  release.reject(new Error("capture_selection_pending")); await flush();
  assert.equal(h.client.snapshot().meetPhase, "idle"); assert.equal(h.client.snapshot().busy, false);
  assert.equal(h.client.setMeetSource("remote_mix"), false);
  assert.match(h.client.snapshot().meetIssue, /microphone permission prompt/);
  assert.equal(h.client.snapshot().meetSourceLocked, true);
  assert.deepEqual(h.effects.cancelledSources, ["self"]);
  const requests = h.calls.length;
  await h.client.startMeetSession("en", 670000); await h.client.selectMeetAudio(); await h.client.selectMeetTab();
  assert.equal(h.calls.length, requests); assert.deepEqual(h.effects.preparedSources, ["self"]);
  assert.equal(h.effects.resume, 0); assert.equal(h.routes().length, 0); assert.equal(h.sockets.length, 0);
  // The user now dismisses the native prompt. A deliberate cleanup retry checks
  // only the already-owned self source, and never reopens a permission prompt.
  h.audio.cancelPreparedCapture = async source => {h.effects.cancelledSources.push(source);};
  h.client.cancelMeetSetup(); await flush();
  assert.equal(h.client.snapshot().meetSourceLocked, false);
  assert.equal(h.client.snapshot().busy, false); assert.equal(h.client.snapshot().meetPhase, "idle");
  assert.deepEqual(h.effects.cancelledSources, ["self", "self"]);
  assert.deepEqual(h.effects.preparedSources, ["self"]); assert.equal(h.calls.length, requests);
  assert.equal(h.client.setMeetSource("remote_mix"), true);
  h.client.dispose();
});

test("microphone input loss stops its paired finite services without preparing another source", async () => {
  const h = harness({source: "self"}); await h.listening(); h.client.captureGap("self", "capture_no_frames"); await flush();
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.effects.stop, 1);
  assert.match(h.client.snapshot().meetIssue, /microphone was selected/);
  assert.equal(h.client.snapshot().meetSource, "self"); assert.equal(h.client.setMeetSource("remote_mix"), false);
  assert.deepEqual(h.effects.preparedSources, ["self"]); assert.equal(h.routes().length, 2);
});

test("Stop and Finish fence microphone setup and retain source-only ordered finalization", async () => {
  for (const action of ["stop", "finish"]) {
    const h = harness({source: "self"}); await h.preview(); const {start} = await h.first();
    h.route("transcription"); await flush();
    if (action === "stop") {
      await h.client.stop(); await start; h.route("live");
      assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().sourceFragments.length, 0);
    } else {
      const finish = h.client.finishExport(); await start;
      assert.equal(h.client.snapshot().finishing, true);
      assert.equal(h.client.setMeetSource("remote_mix"), false);
      const sent = h.sockets[0].sent.map(value => JSON.parse(value)); assert.equal(sent.at(-1).type, "finish");
      h.event({type: "finishing"});
      h.event({type: "transcript", route_id: "captions_one", source: "self", route: "transcription", provenance: "source_asr", text: "青い箱", final: true, item: {id: "self_final", revision: 1, sequence: 1}});
      h.event({type: "finished", source_completion: "complete"});
      assert.match(await finish, /\[self · source_asr · provider final\] 青い箱/);
    }
    assert.equal(h.effects.activate, 0); assert.equal(h.client.sendFrame("self", frame("self")), false);
    assert.deepEqual(h.effects.preparedSources, ["self"]); h.client.dispose();
  }
});
