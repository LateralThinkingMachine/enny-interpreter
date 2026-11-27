import {parseStopReceipt} from "../.test-build/stop-receipt.js";
import assert from "node:assert/strict";
import test from "node:test";
import {webcrypto} from "node:crypto";
import {parseEvent, parsePreview, parseSession, verifyClip} from "../.test-build/protocol.js";
import {utf8Length, textFits} from "../.test-build/text.js";

test("recovery is a private exact event with reviewed reason and bounded retry cadence", () => {
  const event = {type: "window", generation: 1, route_id: "source", source: "remote_mix", route: "transcription", window: 2, state: "recovering", code: "provider_connection_lost", retry_in_seconds: 1};
  for (const code of ["provider_connection_lost", "source_input_interrupted", "live_expired", "live_remote_hangup", "live_connection_lost"]) for (const delay of [1, 2, 4, 30]) assert.equal(parseEvent(JSON.stringify({...event, code, retry_in_seconds: delay}), "operator").code, code);
  for (const change of [{code: "usage_mismatch"}, {code: "provider_unavailable"}, {retry_in_seconds: 0}, {retry_in_seconds: 31}, {retry_in_seconds: 1.5}, {extra: true}, {state: "ready"}, {retry_in_seconds: undefined}, {code: undefined}]) assert.throws(() => parseEvent(JSON.stringify({...event, ...change}), "operator"));
  assert.throws(() => parseEvent(JSON.stringify(event), "public"));
});

test("text allowance measures UTF-8 bytes, including Japanese, Hindi and emoji", () => {
  assert.equal(utf8Length("aあह😀"), 11); assert.equal(textFits("あ".repeat(133), 400), true); assert.equal(textFits("あ".repeat(134), 400), false); assert.equal(textFits("   ", 2000), false); assert.equal(textFits("😀".repeat(500), 2000), true); assert.equal(textFits("😀".repeat(501), 2000), false);
});

test("strict role/session parser rejects private public capabilities and unknown fields", () => {
  const value = {role: "public", csrf: "token", generation: 1, stopped: false, providers_enabled: true, expires_in_seconds: 100, capabilities: {source_captions: false, private_translation: false, live_assistant: false, live_interpreter: false, prepared_public_speech: true}, limitations: []};
  assert.equal(parseSession(value, "public").role, "public"); assert.throws(() => parseSession({...value, key: "must_not_arrive"}, "public")); assert.throws(() => parseSession({...value, capabilities: {...value.capabilities, source_captions: true}}, "public")); assert.throws(() => parseSession({...value, generation: 1.5}, "public")); assert.throws(() => parseSession({...value, providers_enabled: false}, "public"));
});

test("public wire accepts only public generation-bound shapes and exact audio routes", () => {
  const value = {type: "delivery", generation: 1, delivery_id: "del_one", text: "日本語 हिन्दी English", language: "ja", digest: "1".repeat(64), audio_path: "/api/public/audio/del_one", duration_ms: 100};
  assert.equal(parseEvent(JSON.stringify(value), "public").type, "delivery");
  for (const path of ["https://evil.invalid/audio", "/api/previews/del_one/audio", "/api/public/audio/del_two", "/api/public/audio/del_one?token=secret"]) assert.throws(() => parseEvent(JSON.stringify({...value, audio_path: path}), "public"));
  assert.throws(() => parseEvent(JSON.stringify({...value, private_notes: "forbidden"}), "public")); assert.throws(() => parseEvent(JSON.stringify(value), "operator"));
});

test("audio enforces complete bounded PCM, supported provenance and no Realtime aliases", () => {
  const value = {type: "audio", generation: 1, route_id: "route_one", source: "remote_mix", route: "live", pcm16_base64: "AAA=", sample_rate: 16000};
  assert.equal(parseEvent(JSON.stringify(value), "operator").pcm.length, 2);
  for (const pcm16_base64 of ["AA==", "invalid!", Buffer.alloc(49_154).toString("base64")]) assert.throws(() => parseEvent(JSON.stringify({...value, pcm16_base64}), "operator"));
  assert.throws(() => parseEvent(JSON.stringify({...value, type: "response.audio.delta"}), "operator"));
  assert.throws(() => parseEvent(JSON.stringify({...value, route: "transcription"}), "operator"));
});

test("preview duration and exact immutable operator audio path are bounded", () => {
  const value = {id: "preview_one", generation: 1, text: "words", language: "en", digest: "0".repeat(64), audio_path: "/api/previews/preview_one/audio", duration_ms: 100, auditioned: false, state: "prepared"};
  assert.equal(parsePreview(value, 1).text, "words"); assert.throws(() => parsePreview(value, 2)); assert.throws(() => parsePreview({...value, duration_ms: Infinity}, 1)); assert.throws(() => parsePreview({...value, audio_path: "/api/public/audio/preview_one"}, 1));
});

test("WebCrypto verification binds actual WAV bytes, exact text and language", async () => {
  globalThis.crypto ??= webcrypto;
  const bytes = new Uint8Array(46); bytes.set(new TextEncoder().encode("RIFF"), 0); bytes.set(new TextEncoder().encode("WAVE"), 8);
  const hex = data => Buffer.from(data).toString("hex");
  const audio_sha256 = hex(await crypto.subtle.digest("SHA-256", bytes));
  const language = "hi", text = "हिन्दी exact\n日本語";
  const digest = hex(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify({audio_sha256, language, text}))));
  assert.equal(await verifyClip(bytes.buffer, {digest, language, text}), true);
  assert.equal(await verifyClip(bytes.buffer, {digest, language: "en", text}), false);
  assert.equal(await verifyClip(bytes.buffer, {digest, language, text: text+" "}), false);
  bytes[45] = 1; assert.equal(await verifyClip(bytes.buffer, {digest, language, text}), false);
  assert.equal(await verifyClip(new ArrayBuffer(43), {digest, language, text}), false);
});

test("finish controls are exact private shapes with a closed completion vocabulary", () => {
  const start = {type: "finishing", generation: 1};
  const end = {type: "finished", generation: 1, source_completion: "complete"};
  for (const event of [start, end]) {
    assert.deepEqual(parseEvent(JSON.stringify(event), "operator"), event);
    assert.throws(() => parseEvent(JSON.stringify(event), "public"));
    assert.throws(() => parseEvent(JSON.stringify({...event, extra: true}), "operator"));
  }
  assert.throws(() => parseEvent(JSON.stringify({...end, source_completion: "all_audio_verified"}), "operator"));
});

test("Live route mode is required immutable metadata, never implied by a missing field or target language", () => {
  const value = {type: "route", generation: 1, route_id: "interpret", source: "remote_mix", route: "live", live_mode: "interpretation", language: "hi", state: "ready", code: null, expires_in_seconds: 60};
  for (const live_mode of ["interpretation", "assistant"]) assert.equal(parseEvent(JSON.stringify({...value, live_mode}), "operator").live_mode, live_mode);
  for (const live_mode of [null, undefined, "translation", true]) assert.throws(() => parseEvent(JSON.stringify({...value, live_mode}), "operator"));
  for (const route of ["translation", "transcription"]) {
    assert.throws(() => parseEvent(JSON.stringify({...value, route}), "operator"));
    assert.equal(parseEvent(JSON.stringify({...value, route, live_mode: null}), "operator").live_mode, null);
    assert.throws(() => parseEvent(JSON.stringify({...value, route, live_mode: undefined}), "operator"));
  }
  assert.throws(() => parseEvent(JSON.stringify(value), "public"));
});

test("Live interpreter capability is independent of assistant and forbidden on public sessions", () => {
  const value = {role: "operator", csrf: "token", generation: 1, stopped: false, providers_enabled: true, expires_in_seconds: 100, capabilities: {source_captions: false, private_translation: false, live_assistant: false, live_interpreter: true, prepared_public_speech: false}, limitations: []};
  assert.equal(parseSession(value, "operator").capabilities.live_interpreter, true);
  assert.equal(parseSession(value, "operator").capabilities.live_assistant, false);
  assert.throws(() => parseSession({...value, role: "public"}, "public"));
  assert.throws(() => parseSession({...value, providers_enabled: false}, "operator"));
  assert.throws(() => parseSession({...value, capabilities: {...value.capabilities, live_interpreter: undefined}}, "operator"));
});

test("Live intervals preserve fractional milliseconds without accepting source items or public delivery", () => {
  const value = {type: "live_transcript", generation: 1, route_id: "live", source: "remote_mix", route: "live", provenance: "live_output", text: " 12,000—not 20,000.", start_ms: 125.25, end_ms: 901.75, window: 1};
  const parsed = parseEvent(JSON.stringify(value), "operator");
  assert.equal(parsed.start_ms,125.25); assert.equal(parsed.end_ms,901.75); assert.equal(parsed.text,value.text);
  assert.throws(() => parseEvent(JSON.stringify(value),"public"));
  for (const change of [{start_ms:-1},{end_ms:100},{end_ms:3_610_001},{start_ms:null},{start_ms:"125"},{start_ms:NaN},{end_ms:Infinity},{text:""},{text:"a\0b"},{text:"字".repeat(4097)},{provenance:"source_asr"},{route:"transcription"},{item:null},{final:false},{speaker:"A"},{window:0}]) assert.throws(() => parseEvent(JSON.stringify({...value,...change}),"operator"));
});

test("Gemini vendor metadata is operator-only, finite and bound to the supported capability set", () => {
  const capabilities = {source_captions: true, live_interpreter: true, private_translation: false, live_assistant: false, prepared_public_speech: false};
  const row = {role: "operator", csrf: "fixture", generation: 1, stopped: false, providers_enabled: true, continuous: false, expires_in_seconds: 120, capabilities, limitations: [], provider_vendor: "gemini"};
  assert.equal(parseSession(row, "operator").provider_vendor, "gemini");
  assert.equal(parseSession({...row, expires_in_seconds: 600}, "operator").expires_in_seconds, 600);
  assert.throws(() => parseSession({...row, expires_in_seconds: 601}, "operator"));
  const legacy = {...row, expires_in_seconds: 601};
  delete legacy.provider_vendor;
  assert.equal(parseSession(legacy, "operator").expires_in_seconds, 601);
  for (const change of [
    {provider_vendor: "guess"}, {provider_vendor: null}, {continuous: true, expires_in_seconds: null},
    ...["private_translation", "prepared_public_speech", "live_assistant"].map(key => ({capabilities: {...capabilities, [key]: true}})),
  ]) assert.throws(() => parseSession({...row, ...change}, "operator"));
  assert.throws(() => parseSession({...row, role: "public", capabilities: {...capabilities, source_captions: false, live_interpreter: false}}, "public"));
});

test("Stop receipt activity must agree with the finite reservation evidence and allowance bound", () => {
  const session = {role: "operator", stopped: false, generation: 4};
  const verify = (activity, reserved) => parseStopReceipt({version: 1, session_generation: 4, generation: 5,
    provider_activity: activity, reserved_microusd: reserved, invoice: "unavailable"}, session);
  for (const [activity, reserved] of [["none_admitted", null], ["none_admitted", 1], ["close_observed", null], ["close_observed", 0],
    ["close_observed", 1_000_000_001], ["unconfirmed", 1_000_000_001], ["unconfirmed", -1], ["unconfirmed", 1.5]]) {
    assert.throws(() => verify(activity, reserved));
  }
  for (const [activity, reserved] of [["none_admitted", 0], ["close_observed", 1], ["close_observed", 1_000_000_000],
    ["unconfirmed", null], ["unconfirmed", 0], ["unconfirmed", 1_000_000_000]]) {
    assert.equal(verify(activity, reserved).reserved_microusd, reserved);
  }
});
