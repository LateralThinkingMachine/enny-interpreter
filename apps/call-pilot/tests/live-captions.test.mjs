import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import {renderToStaticMarkup} from "react-dom/server";
import {LiveCaptions, LIVE_CAPTION_GROUP_GAP_MS} from "../.test-build/live-captions.js";
import {LiveCaptionPanel} from "../.test-build/LiveCaptionPanel.js";

const fragment = (changes = {}) => ({route_id: "interpreter", source: "remote_mix", language: "en", live_mode: "interpretation", window: 1, provenance: "live_output", text: "Hello", start_ms: 100, end_ms: 200, ...changes});
const allOriginals = (state) => state.items().flatMap(item => item.originals);

test("each delta is visible immediately in a growing stable row without a final or silence wait", () => {
  const state = new LiveCaptions();
  state.apply(fragment({text: " no"}));
  const key = state.items()[0].key;
  assert.equal(state.items()[0].text, " no");
  for (const [index, text] of [" no", "、", "違います", "।", " नहीं "].entries()) {
    state.apply(fragment({text, start_ms: 200 + index * 100, end_ms: 300 + index * 100}));
    assert.equal(state.items().length, 1);
    assert.equal(state.items()[0].key, key);
    assert.equal(state.items()[0].originals.length, index + 2);
  }
  assert.equal(state.items()[0].text, " no no、違います। नहीं ");
  assert.equal(state.items()[0].closed, false);
  assert.ok(!("final" in state.items()[0]));
  assert.ok(!("item" in state.items()[0]));
});

test("late overlapping fractional intervals preserve originals without replacement or invented whitespace", () => {
  const state = new LiveCaptions();
  state.apply(fragment({text: " not", start_ms: 100.5, end_ms: 200.75}));
  state.apply(fragment({text: " true", start_ms: 300.25, end_ms: 400.5}));
  const key = state.items()[0].key;
  state.apply(fragment({text: " always", start_ms: 210.5, end_ms: 350.75}));
  state.apply(fragment({text: " not", start_ms: 100.5, end_ms: 200.75}));
  assert.equal(state.items()[0].key, key);
  assert.equal(state.items()[0].text, " not true always not");
  assert.deepEqual(state.items()[0].originals.map(({text, start_ms, end_ms}) => ({text, start_ms, end_ms})), [
    {text: " not", start_ms: 100.5, end_ms: 200.75},
    {text: " true", start_ms: 300.25, end_ms: 400.5},
    {text: " always", start_ms: 210.5, end_ms: 350.75},
    {text: " not", start_ms: 100.5, end_ms: 200.75},
  ]);
});

test("approximate equal-start and overlapping intervals never shuffle words, negation or numbers", () => {
  for (const deltas of [
    [
      {text: "Revenue ", start_ms: 100, end_ms: 900},
      {text: "did not ", start_ms: 100, end_ms: 400},
      {text: "rise.", start_ms: 100, end_ms: 700},
    ],
    [
      {text: "売上は", start_ms: 100.5, end_ms: 700},
      {text: "増えて", start_ms: 150, end_ms: 800},
      {text: "いません。", start_ms: 110, end_ms: 750},
    ],
    [
      {text: "राजस्व ", start_ms: 100, end_ms: 300},
      {text: "नहीं ", start_ms: 80, end_ms: 200},
      {text: "बढ़ा।", start_ms: 120, end_ms: 280},
    ],
    [
      {text: "$", start_ms: 0, end_ms: 1000},
      {text: "1", start_ms: 0, end_ms: 700},
      {text: "2", start_ms: 0, end_ms: 600},
      {text: ".50", start_ms: 0, end_ms: 500},
    ],
  ]) {
    const state = new LiveCaptions();
    let expected = "";
    for (const delta of deltas) {
      state.apply(fragment(delta)); expected += delta.text;
      assert.equal(state.items().length, 1);
      assert.equal(state.items()[0].text, expected);
    }
    assert.deepEqual(state.items()[0].originals.map(({text, start_ms, end_ms}) => ({text, start_ms, end_ms})), deltas);
  }
});

test("late text keeps delivery order across later same-lane groups while input stays independent", () => {
  const state = new LiveCaptions();
  state.apply(fragment({text: "No ", start_ms: 100, end_ms: 300}));
  state.apply(fragment({text: "नहीं ", provenance: "live_input", start_ms: 100, end_ms: 300}));
  state.apply(fragment({text: "Later", start_ms: 10_000, end_ms: 10_500}));
  const keys = state.items().map(group => group.key);
  state.apply(fragment({text: "no.", start_ms: 90, end_ms: 200}));
  assert.deepEqual(state.items().slice(0, 3).map(group => group.key), keys);
  assert.deepEqual(state.items().map(group => group.text), ["No ", "नहीं ", "Later", "no."]);
  assert.equal(state.items().filter(group => group.provenance === "live_output").map(group => group.text).join(""), "No Laterno.");
  state.apply(fragment({text: "नहीं।", provenance: "live_input", start_ms: 90, end_ms: 200}));
  assert.equal(state.items().length, 4);
  assert.equal(state.items()[1].text, "नहीं नहीं।");
  state.close({route_id: "interpreter", window: 1});
  state.apply(fragment({text: "After gap", start_ms: 90, end_ms: 200}));
  assert.equal(state.items().length, 5);
  assert.equal(state.items()[0].text, "No ");
  state.clear();
  state.apply(fragment({text: "New session", start_ms: 90, end_ms: 200}));
  assert.equal(state.items().length, 1);
  assert.equal(state.items()[0].text, "New session");
  assert.equal(state.discardedCount(), 0);
});

test("input and output can overlap while routes, sources, modes, targets and windows never merge", () => {
  const state = new LiveCaptions();
  for (const changes of [{}, {provenance: "live_input"}, {route_id: "other"}, {source: "self"}, {live_mode: "assistant"}, {language: "hi"}, {window: 2}]) state.apply(fragment(changes));
  assert.equal(state.items().length, 7);
  state.apply(fragment({text: " world", start_ms: 200, end_ms: 300}));
  assert.equal(state.items().length, 7);
  assert.equal(state.items()[0].text, "Hello world");
  assert.equal(state.items()[1].text, "Hello");
});

test("long pauses create display groups only and late text cannot backfill before delivered words", () => {
  const state = new LiveCaptions();
  state.apply(fragment({text: "First"}));
  state.apply(fragment({text: "Later", start_ms: 10_000, end_ms: 10_100}));
  const before = state.items().map(item => item.key);
  state.apply(fragment({text: " idea", start_ms: 201, end_ms: 400}));
  assert.deepEqual(state.items().slice(0, 2).map(item => item.key), before);
  assert.deepEqual(state.items().map(item => item.text), ["First", "Later", " idea"]);
  assert.ok(state.items().every(item => !item.closed));
  assert.equal(LIVE_CAPTION_GROUP_GAP_MS, 300);
});

test("reading-group limits split presentation without splitting or discarding an original delta", () => {
  const state = new LiveCaptions();
  for (let i = 0; i < 101; i++) state.apply(fragment({text: "x", start_ms: 0, end_ms: 0}));
  assert.deepEqual(state.items().map(item => item.originals.length), [100, 1]);
  assert.equal(state.originalCount(), 101);
  state.clear();
  state.apply(fragment({text: "wide fragment", start_ms: 0, end_ms: 20_000}));
  state.apply(fragment({text: "next", start_ms: 20_000, end_ms: 20_100}));
  assert.equal(state.items().length, 2);
  assert.equal(state.items()[0].text, "wide fragment");
  assert.equal(state.items()[0].originals[0].end_ms, 20_000);
  state.clear();
  for (let i = 0; i < 6; i++) state.apply(fragment({text: "字".repeat(1000), start_ms: 0, end_ms: 1}));
  assert.equal(state.items().length, 2);
  assert.ok(state.items().every(item => Buffer.byteLength(item.text) <= 16_384));
  assert.equal(state.originalCount(), 6);
  assert.equal(state.discardedCount(), 0);
});

test("connection gaps fence grouping without erasing text or changing other sources", () => {
  const state = new LiveCaptions();
  state.apply(fragment()); state.apply(fragment({source: "self", route_id: "microphone"}));
  const original = state.items()[0];
  state.close({source: "remote_mix", window: 1});
  state.apply(fragment({text: "Again", start_ms: 201, end_ms: 220}));
  state.apply(fragment({source: "self", route_id: "microphone", text: " there", start_ms: 201, end_ms: 220}));
  assert.equal(state.items().length, 3);
  assert.equal(state.items()[0].key, original.key);
  assert.equal(state.items()[0].closed, true);
  assert.equal(state.items()[0].text, "Hello");
  assert.equal(state.items()[1].text, "Hello there");
  state.close(); assert.ok(state.items().every(item => item.closed));
});

test("original count, UTF-8 bytes and every retained grouping index remain bounded", () => {
  const state = new LiveCaptions();
  for (let i = 0; i < 1200; i++) state.apply(fragment({text: `${i}`, start_ms: i * 500, end_ms: i * 500 + 100}));
  assert.equal(state.originalCount(), 500);
  assert.equal(state.discardedCount(), 700);
  assert.equal(allOriginals(state).length, 500);
  assert.equal(new Set(allOriginals(state).map(item => item.key)).size, 500);
  assert.ok(allOriginals(state).every(item => Number(item.text) >= 700));
  assert.equal(state.byteLength(), allOriginals(state).reduce((sum, item) => sum + Buffer.byteLength(item.text), 0));
  state.clear();
  for (let i = 0; i < 100; i++) state.apply(fragment({text: "🙂".repeat(4096), start_ms: i * 100, end_ms: i * 100 + 100}));
  assert.equal(state.originalCount(), 64);
  assert.equal(state.discardedCount(), 36);
  assert.equal(state.byteLength(), 1_048_576);
  assert.equal(allOriginals(state).length, 64);
  state.clear();
  assert.deepEqual([state.items().length, state.byteLength(), state.originalCount(), state.discardedCount()], [0, 0, 0, 0]);
});

test("UTF-8 grouping capacity remains exact after partial eviction, closure and a new session", () => {
  const state = new LiveCaptions();
  for (let i = 0; i < 128; i++) state.apply(fragment({text: "🙂".repeat(2048), start_ms: 0, end_ms: 1}));
  const firstKey = state.items()[0].key;
  assert.equal(state.byteLength(), 1_048_576);
  assert.equal(state.items().length, 64);
  state.apply(fragment({text: "字".repeat(1365) + "a", start_ms: 0, end_ms: 1}));
  assert.equal(state.discardedCount(), 1);
  assert.equal(Buffer.byteLength(state.items()[0].text), 8192);
  state.apply(fragment({text: "x".repeat(4096), start_ms: 0, end_ms: 1}));
  assert.equal(state.items()[0].key, firstKey);
  assert.equal(Buffer.byteLength(state.items()[0].text), 8192);
  assert.equal(Buffer.byteLength(state.items().at(-1).text), 8192);
  assert.equal(state.byteLength(), 1_048_576);
  assert.ok(state.items().every(group => Buffer.byteLength(group.text) <= 16_384));
  state.close(); state.clear();
  for (let i = 0; i < 4; i++) state.apply(fragment({text: "x".repeat(4096), start_ms: 0, end_ms: 1}));
  assert.equal(state.items().length, 1);
  assert.equal(Buffer.byteLength(state.items()[0].text), 16_384);
  assert.equal(state.byteLength(), 16_384);
});

test("eviction retains a surviving group's stable key and orders late timestamps by arrival for discard", () => {
  const state = new LiveCaptions();
  state.apply(fragment({text: "old", start_ms: 200, end_ms: 300}));
  state.apply(fragment({text: "young but earlier", start_ms: 100, end_ms: 200}));
  const key = state.items()[0].key;
  for (let i = 0; i < 499; i++) state.apply(fragment({text: "x", start_ms: 10_000 + i * 500, end_ms: 10_100 + i * 500}));
  assert.equal(state.items()[0].key, key);
  assert.equal(state.items()[0].text, "young but earlier");
  assert.equal(state.discardedCount(), 1);
});

test("invalid fields and intervals reject before storage while empty valid text is a no-op", () => {
  const state = new LiveCaptions();
  for (const change of [{route_id: "a b"}, {source: "stranger"}, {language: "fr"}, {live_mode: "none"}, {provenance: "source_asr"}, {window: 0}, {window: 1.5}, {start_ms: -1}, {end_ms: Infinity}, {start_ms: NaN}, {start_ms: 201}, {end_ms: 3_610_001}, {text: "x\0"}, {text: "字".repeat(4097)}]) assert.throws(() => state.apply(fragment(change)));
  assert.throws(() => state.apply(fragment({text: "", start_ms: -1})));
  assert.equal(state.apply(fragment({text: "", start_ms: 2.5, end_ms: 2.5})), false);
  assert.equal(state.items().length, 0);
});

test("caller mutation cannot rewrite kept text or grouping", () => {
  const state = new LiveCaptions(), input = fragment(); state.apply(input);
  input.text = "changed";
  assert.equal(state.items()[0].text, "Hello");
  assert.throws(() => {state.items()[0].originals[0].text = "changed";});
  assert.throws(() => {state.items().pop();});
});

test("rendering keeps generated output primary, model input separate and text escaped", () => {
  const state = new LiveCaptions();
  state.apply(fragment({text: "<script>unsafe</script>"}));
  state.apply(fragment({text: "source-model-text", provenance: "live_input"}));
  const html = renderToStaticMarkup(React.createElement(LiveCaptionPanel, {groups: state.items(), discarded: 7}));
  assert.ok(html.includes("&lt;script&gt;unsafe&lt;/script&gt;"));
  assert.ok(!html.includes("<script>"));
  assert.ok(html.indexOf("unsafe") < html.indexOf("<details"));
  assert.ok(html.indexOf("source-model-text") > html.indexOf("<details"));
  assert.ok(html.includes("not source ASR"));
  assert.ok(html.includes("not completed turns"));
  assert.ok(html.includes("7 older text fragments discarded"));
  assert.ok(!html.includes("provider final"));
  assert.ok(!html.includes("aria-live"));
});
