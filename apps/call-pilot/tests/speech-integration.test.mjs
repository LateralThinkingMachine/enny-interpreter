import assert from "node:assert/strict";
import test from "node:test";
import {createElement} from "react";
import {renderToStaticMarkup} from "react-dom/server";
import {PilotClient} from "../.test-build/client.js";
import {App} from "../.test-build/App.js";
import {parseEvent} from "../.test-build/protocol.js";
import {speechHints} from "../.test-build/speech-hints.js";
import {budgetFixture} from "./budget-fixture.mjs";

const flush = async () => {for (let i = 0; i < 8; i++) await new Promise(resolve => setImmediate(resolve));};
const json = (value, status = 200) => new Response(JSON.stringify(value), {status, headers: {"Content-Type": "application/json"}});
const live = (text, start_ms = 0, overrides = {}) => ({type: "live_transcript", generation: 1, route_id: "live_one", source: "remote_mix", route: "live", provenance: "live_output", text, start_ms, end_ms: start_ms + 100, ...overrides});

function harness() {
  let socket, denyBudget = false;
  const calls = [], effects = {capture: 0, sound: 0};
  const client = new PilotClient("operator", {
    now: () => 0, interval: () => () => {}, broadcastStop: () => {}, verifyClip: async () => false, openPublicWindow: () => {},
    audio: {prepareCapture: async () => {}, cancelPreparedCapture: async () => {}, activateCapture: () => effects.capture++, startCapture: async () => {}, stopCapture: () => {}, stopAll: () => {}, resumePlayback: async () => {}, setPrivateMuted: () => {}, playPrivate: () => effects.sound++, stopClip: () => {}, playClip: async () => {}},
    fetch: async (path, options = {}) => {
      const body = options.body && JSON.parse(options.body); calls.push({path, body, options});
      if (path === "/api/session") return json({role: "operator", csrf: "synthetic", generation: 1, stopped: false, providers_enabled: true, expires_in_seconds: 7200, capabilities: {source_captions: true, private_translation: true, live_assistant: true, live_interpreter: true, prepared_public_speech: false}, limitations: []});
      if (path === "/api/budget") return denyBudget ? json({}, 401) : json(budgetFixture());
      if (path === "/api/routes") return json({state: "starting", generation: 1, route_id: body.route === "live" ? "live_one" : "asr_one"});
      if (path === "/api/routes/close" || path === "/api/source-gap") return json({state: "closed", generation: 1});
      if (path === "/api/stop" || path === "/api/logout") return json({state: "stopped", generation: 2});
      throw new Error("Unexpected synthetic request");
    },
    socket: () => {socket = {readyState: 1, bufferedAmount: 0, sent: [], onmessage: null, send(value) {this.sent.push(value);}, close() {}}; return socket;},
  });
  const event = row => socket.onmessage?.({data: JSON.stringify(row)});
  const route = kind => event({type: "route", generation: 1, route_id: kind === "live" ? "live_one" : "asr_one", source: "remote_mix", route: kind, live_mode: kind === "live" ? "interpretation" : null, language: "en", state: "ready", code: null, expires_in_seconds: 600});
  return {client, calls, effects, event, route, denyBudget: () => {denyBudget = true;},
    async connect() {await client.initialize(null); await client.connect(); event({type: "hello", generation: 1, role: "operator", stopped: false}); await flush();},
  };
}

test("caption hints bind only to source admission; default payload and interpreter input remain compatible", async () => {
  for (const customized of [false, true]) {
    const h = harness(); await h.connect();
    if (customized) h.client.setSpeechHints("ja", "佐藤\nAC-42\nराजस्व");
    await h.client.openRoute("remote_mix", "transcription", "en");
    const source = h.calls.find(row => row.path === "/api/routes").body;
    if (customized) assert.deepEqual(source.transcription, {languages: ["ja"], keywords: ["佐藤", "AC-42", "राजस्व"]});
    else assert.equal(Object.hasOwn(source, "transcription"), false);
    const before = h.client.snapshot().speechTerms;
    h.client.setSpeechHints("hi", "Changed after admission"); assert.equal(h.client.snapshot().speechTerms, before);
    await h.client.refreshBudget(); await h.client.openRoute("remote_mix", "live", "en", "interpretation");
    const interpreter = h.calls.filter(row => row.path === "/api/routes")[1].body;
    assert.equal(Object.hasOwn(interpreter, "transcription"), false); assert.equal(interpreter.live_mode, "interpretation");
    await h.client.stop(); assert.equal(h.client.snapshot().speechTerms, ""); assert.equal(h.client.snapshot().spokenLanguage, "auto");
    h.client.setSpeechHints("ja", "Cannot reappear"); assert.equal(h.client.snapshot().speechTerms, ""); h.client.dispose();
  }
});

test("caption term controls validate scalar, byte, count and control bounds without storing meeting content elsewhere", () => {
  assert.deepEqual(speechHints("auto", " 佐藤 \r\n\n AC-42 "), {languages: ["ja", "en", "hi"], keywords: ["佐藤", "AC-42"]});
  assert.equal(speechHints("ja", "字".repeat(80)).keywords[0].length, 80);
  for (const invalid of ["a\na", "字".repeat(81), "bad\tterm", "bad\u202eterm", "<instructions>", Array.from({length: 21}, (_, i) => `term${i}`).join("\n"), Array.from({length: 6}, (_, i) => `${i}${"字".repeat(79)}`).join("\n")]) assert.throws(() => speechHints("auto", invalid));
});

test("new Live text forms reading groups while legacy generated fragments remain separate from source and retrieval", async () => {
  const h = harness(); await h.connect(); h.route("live");
  h.event(live("What was ", 0)); h.event(live("the revenue last month?", 100));
  h.event(live("先月の売上", 0, {provenance: "live_input"}));
  h.event({type: "transcript", generation: 1, route_id: "live_one", source: "remote_mix", route: "live", provenance: "live_output", text: "legacy output retained", final: false, item: null});
  await flush();
  const view = h.client.snapshot(); assert.equal(view.liveCaptions.length, 2);
  assert.equal(view.liveCaptions[0].text, "What was the revenue last month?"); assert.equal(view.liveCaptions[0].originals.length, 2);
  assert.equal(view.fragments[0].text, "legacy output retained"); assert.equal(view.sourceFragments.length, 0);
  assert.equal(h.effects.sound, 0);
  assert.doesNotMatch(h.client.exportText(), /What was|先月|legacy output/);
  await h.client.closeRoute("live_one"); assert.ok(h.client.snapshot().liveCaptions.every(group => group.closed));
  h.event(live("late closed output", 200)); assert.equal(h.client.snapshot().liveCaptions.length, 2); h.client.dispose();
});

test("Finish fences generated Live text immediately while the canonical source-only barrier still completes", async () => {
  const h = harness(); await h.connect(); h.route("live"); h.route("transcription");
  h.event(live("Unverified generated interpretation"));
  const finish = h.client.finishExport(); assert.ok(h.client.snapshot().liveCaptions[0].closed);
  h.event(live("Cannot extend after Finish", 100));
  assert.equal(h.client.snapshot().liveCaptions[0].text, "Unverified generated interpretation");
  h.event({type: "transcript", generation: 1, route_id: "asr_one", source: "remote_mix", route: "transcription", provenance: "source_asr", text: "Source final only", final: true, item: {id: "source_one", revision: 1, sequence: 1}});
  h.event({type: "finishing", generation: 1}); h.event({type: "finished", generation: 1, source_completion: "complete"});
  const exported = await finish; assert.match(exported, /Source final only/); assert.doesNotMatch(exported, /Unverified generated interpretation|Cannot extend/);
  assert.equal(h.client.snapshot().liveCaptions.length, 0); h.client.dispose();
});

test("interpreted negation and numbers retain delivery order through the actual client and rendered rows", async () => {
  const h = harness(); await h.connect(); h.route("live");
  const phrases = [
    ["Revenue ", 100, 900], ["did not ", 100, 400], ["rise. ", 100, 700],
    ["Fifteen—", 3000, 3100], ["sorry, fifty.", 200, 500],
  ];
  for (const [text, start, end] of phrases) h.event(live(text, start, {end_ms: end}));
  const rows = h.client.snapshot().liveCaptions;
  assert.equal(rows.map(row => row.text).join(""), "Revenue did not rise. Fifteen—sorry, fifty.");
  const originals = rows.flatMap(row => row.originals);
  assert.deepEqual(originals.map(row => [row.text, row.start_ms, row.end_ms]), phrases);
  const html = renderToStaticMarkup(createElement(App, {client: h.client}));
  assert.match(html, /Revenue did not rise\./);
  assert.ok(html.indexOf("Fifteen—") < html.indexOf("sorry, fifty."));
  assert.equal(h.client.snapshot().sourceFragments.length, 0);
  assert.doesNotMatch(h.client.exportText(), /Revenue|Fifteen|fifty/);
  await h.client.stop(); assert.equal(h.client.snapshot().liveCaptions.length, 0); h.client.dispose();
});

test("source snapshot optimization preserves late final corrections and empty finalized items through export", async () => {
  const h = harness(); await h.connect(); h.route("transcription");
  const source = (text, id, sequence, revision, final) => h.event({type: "transcript", generation: 1,
    route_id: "asr_one", source: "remote_mix", route: "transcription", provenance: "source_asr",
    text, final, item: {id, sequence, revision}});
  source("Fifteen", "first", 1, 1, false);
  source("Next sentence.", "second", 2, 1, true);
  const before = h.client.snapshot().sourceFragments;
  source("Fifty, not fifteen.", "first", 1, 2, true);
  const corrected = h.client.snapshot().sourceFragments;
  assert.equal(before[0].text, "Fifteen"); assert.equal(corrected[0].key, before[0].key);
  assert.equal(corrected[1], before[1]);
  assert.deepEqual(corrected.map(row => row.text), ["Fifty, not fifteen.", "Next sentence."]);
  source("", "empty", 3, 1, true);
  assert.equal(h.client.snapshot().sourceFragments, corrected);
  assert.ok(h.client.exportText().indexOf("Fifty, not fifteen.") < h.client.exportText().indexOf("Next sentence."));
  await h.client.stop(); assert.equal(h.client.snapshot().sourceFragments.length, 0); h.client.dispose();
});

test("authorization loss clears session terms and generated groups and ignores later Live output", async () => {
  const h = harness(); await h.connect(); h.client.setSpeechHints("ja", "Private session term"); h.route("live"); h.event(live("Private interpretation"));
  await flush();
  h.denyBudget(); await h.client.refreshBudget(); await flush();
  assert.equal(h.client.snapshot().stopped, true); assert.equal(h.client.snapshot().speechTerms, ""); assert.equal(h.client.snapshot().liveCaptions.length, 0);
  h.event(live("late private text", 100)); assert.equal(h.client.snapshot().liveCaptions.length, 0); h.client.dispose();
});

test("Live wire rejects source authority, public delivery, invalid time bounds and unreviewed fields", () => {
  assert.equal(parseEvent(JSON.stringify(live("Hello", 0)), "operator").type, "live_transcript");
  assert.throws(() => parseEvent(JSON.stringify(live("Hello")), "public"));
  for (const overrides of [{route: "transcription"}, {provenance: "source_asr"}, {final: true}, {item: {id: "made_up"}}, {text: ""}, {text: "x\0y"}, {text: "字".repeat(4097)}, {start_ms: -1}, {start_ms: 101, end_ms: 100}, {end_ms: 3_610_001}, {start_ms: null}, {window: 0}]) assert.throws(() => parseEvent(JSON.stringify(live("hello", 0, overrides)), "operator"));
});

test("rendering escapes grouped text, labels generated provenance and hides private speech settings from public", async () => {
  const h = harness(); await h.connect(); h.client.setSpeechHints("ja", "Private session term"); h.route("live");
  h.event(live("<img src=x onerror=alert(1)>", 0)); h.event(live("入力", 0, {provenance: "live_input"}));
  const operator = renderToStaticMarkup(createElement(App, {client: h.client}));
  assert.match(operator, /&lt;img src=x onerror=alert\(1\)&gt;/); assert.doesNotMatch(operator, /<img src=x/);
  assert.match(operator, /not source ASR/); assert.match(operator, /not included in the source-caption export/);
  const view = {...h.client.snapshot(), role: "public"};
  const publicClient = {snapshot: () => view, subscribe: () => () => {}};
  const publicHTML = renderToStaticMarkup(createElement(App, {client: publicClient}));
  assert.doesNotMatch(publicHTML, /Private session term|Caption accuracy|onerror|入力/); h.client.dispose();
});
