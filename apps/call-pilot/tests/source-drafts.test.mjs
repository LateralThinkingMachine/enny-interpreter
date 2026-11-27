import assert from "node:assert/strict";
import test from "node:test";
import {SourceDrafts} from "../.test-build/source-drafts.js";
import {parseEvent} from "../.test-build/protocol.js";

const draft = (overrides = {}) => ({type: "source_draft", generation: 1, route_id: "captions", source: "remote_mix", route: "transcription", item_id: "item_a", revision: 1, text: "明日の売上", retired: false, ...overrides});
test("draft wire has no source ordinal, finality, public disclosure or arbitrary fields", () => {
  assert.equal(parseEvent(JSON.stringify(draft()), "operator").text, "明日の売上");
  for (const overrides of [{route: "live"}, {provenance: "source_asr"}, {final: true}, {sequence: 1}, {item_id: "bad id"}, {revision: 0}, {revision: 4097}, {retired: true}, {text: ""}, {text: "x\0"}, {text: "字".repeat(4097)}, {window: 0}]) assert.throws(() => parseEvent(JSON.stringify(draft(overrides)), "operator"));
  assert.throws(() => parseEvent(JSON.stringify(draft()), "public"));
  assert.equal(parseEvent(JSON.stringify(draft({retired: true, text: ""})), "operator").retired, true);
});
test("draft snapshots retire before binding and cannot reappear after a final", () => {
  const state = new SourceDrafts(); state.apply(draft());
  state.apply(draft({revision: 2, text: "明日の売上ではない"}));
  assert.equal(state.items()[0].text, "明日の売上ではない");
  assert.throws(() => state.bind("captions", "remote_mix", undefined, "item_a"));
  assert.throws(() => state.apply(draft({revision: 2, retired: true, text: "", item_id: "unknown"})));
  state.apply(draft({revision: 2, retired: true, text: ""}));
  state.bind("captions", "remote_mix", undefined, "item_a");
  assert.equal(state.items().length, 0);
  assert.throws(() => state.apply(draft({revision: 3})));
  state.bind("captions", "remote_mix", undefined, "no_early_draft");
  assert.throws(() => state.apply(draft({item_id: "no_early_draft"})));
});
test("revision, source, window and text continuity are checked independently", () => {
  for (const change of [{revision: 1}, {revision: 2}, {revision: 3}, {revision: 2, text: "different"}, {revision: 2, source: "self"}, {revision: 2, window: 2}, {revision: 2, text: "", retired: true}]) {
    const state = new SourceDrafts(); state.apply(draft()); assert.throws(() => state.apply(draft(change)));
  }
  const state = new SourceDrafts(); state.apply(draft()); state.clear({source: "self"}); assert.equal(state.items().length, 1);
  state.clear({route_id: "captions"}); state.apply(draft({window: 2})); assert.equal(state.items()[0].window, 2);
});
test("two sources have independent bounded hypotheses and 4096 per-window identities", () => {
  const state = new SourceDrafts();
  for (let i = 0; i < 8; i++) state.apply(draft({item_id: `a_${i}`}));
  assert.throws(() => state.apply(draft({item_id: "ninth"})));
  state.apply(draft({source: "self", route_id: "microphone"}));
  assert.throws(() => state.apply(draft({route_id: "third"})));
  state.clear();
  for (let i = 0; i < 4096; i++) state.bind("captions", "remote_mix", 1, `bound_${i}`);
  assert.throws(() => state.apply(draft({window: 1})));
  state.clear(); assert.equal(state.items().length, 0);
});
