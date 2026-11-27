import assert from "node:assert/strict";
import test from "node:test";
import {SourceArchive} from "../.test-build/source-archive.js";
import {SourceArchive as Baseline} from "./fixtures/source-archive-baseline.ts";

const row = (sequence, changes = {}) => ({key: sequence, route_id: "remote", source: "remote_mix", route: "transcription", live_mode: null, language: "ja", provenance: "source_asr", text: `原文 ${sequence}`, final: false, item: {id: `item_${sequence}`, revision: 1, sequence}, ...changes});
const outcome = action => {try {return action();} catch (error) {return error.message;}};
function equivalent(actual, reference) {
  assert.deepEqual(actual.items(), reference.items());
  assert.equal(actual.byteLength(), reference.byteLength());
  assert.equal(actual.discardedCount(), reference.discardedCount());
  assert.equal(actual.records.size, reference.records.size);
  assert.equal(actual.sequences.size, reference.sequences.size);
  assert.equal(actual.sizes.size, actual.records.size);
}

test("source snapshots and nested item identities are immutable, shared until visible change and isolated from caller mutation", () => {
  const archive = new SourceArchive(), input = row(2); archive.apply(input);
  const before = archive.items(), original = before[0];
  assert.equal(archive.items(), before);
  input.text = "Caller mutation"; input.item.sequence = 99;
  assert.equal(original.text, "原文 2"); assert.equal(original.item.sequence, 2);
  assert.throws(() => before.push(row(3))); assert.throws(() => {original.text = "Cannot mutate";});
  assert.throws(() => {original.item.sequence = 99;});
  assert.equal(archive.apply(row(2)), "unchanged"); assert.equal(archive.items(), before);
  archive.close({source: "self"}); assert.equal(archive.items(), before);
  archive.apply(row(1, {final: true})); const inserted = archive.items();
  assert.deepEqual(inserted.map(item => item.item.sequence), [1, 2]); assert.equal(inserted[1], original);
  archive.apply(row(2, {key: 100, text: "訂正した原文", item: {id: "item_2", revision: 2, sequence: 2}}));
  const revised = archive.items(); assert.equal(revised[0], inserted[0]); assert.equal(revised[1].key, 2);
  assert.equal(before[0], original); assert.equal(inserted[1].text, "原文 2");
  archive.close({route_id: "remote"}); const closed = archive.items();
  assert.equal(closed[1].incomplete, true); assert.equal(revised[1].incomplete, undefined);
  archive.close({route_id: "remote"}); assert.equal(archive.items(), closed);
  archive.clear(); assert.deepEqual(archive.items(), []); assert.equal(closed.length, 2);
  assert.equal(archive.records.size, 0); assert.equal(archive.sizes.size, 0); assert.equal(archive.visible.length, 0);
});

test("empty final identities never reorder routes and do not invalidate a visually identical snapshot", () => {
  const archive = new SourceArchive(); const empty = archive.items();
  archive.apply(row(1, {text: "", final: true})); assert.equal(archive.items(), empty);
  archive.apply(row(1, {route_id: "self", source: "self", final: true}));
  archive.apply(row(2, {final: true}));
  assert.deepEqual(archive.items().map(item => item.route_id), ["remote", "self"]);
  archive.apply(row(3)); const withProvisional = archive.items();
  archive.apply(row(3, {text: "", final: true, item: {id: "item_3", revision: 2, sequence: 3}}));
  assert.equal(archive.items().length, 2); assert.equal(withProvisional.length, 3);
  assert.throws(() => archive.apply(row(3, {item: {id: "different", revision: 1, sequence: 3}})));
});

test("shuffled multi-route revisions, duplicates, stale/conflicting updates and closures match the frozen prior implementation", () => {
  const actual = new SourceArchive(), reference = new Baseline();
  let state = 0x19ec0281;
  const random = max => {state = (Math.imul(state, 1664525) + 1013904223) >>> 0; return state % max;};
  const apply = value => {assert.equal(outcome(() => actual.apply(value)), outcome(() => reference.apply(value))); equivalent(actual, reference);};
  for (let index = 0; index < 1600; index++) {
    const route = random(4), sequence = random(80) + 1;
    apply(row(sequence, {key: index + 1, route_id: `route_${route}`, source: route % 2 ? "self" : "remote_mix", language: route % 2 ? "hi" : "ja",
      text: random(5) ? `字幕 ${random(100)} राजस्व` : "", final: random(6) === 0,
      item: {id: `item_${sequence}`, revision: random(5) + 1, sequence}}));
    if (index === 800) {actual.close({route_id: "route_2"}); reference.close({route_id: "route_2"}); equivalent(actual, reference);}
  }
});

test("rolling eviction removes every visible/size index while retaining current-window identity and ordered gaps", () => {
  const actual = new SourceArchive(), reference = new Baseline(); actual.continuous(); reference.continuous();
  let retained;
  for (let window = 1; window <= 4; window++) {
    assert.equal(actual.advanceWindow("remote", window), reference.advanceWindow("remote", window));
    for (let sequence = 1; sequence <= 3000; sequence++) {
      const value = row(sequence, {window, final: sequence % 997 !== 0, text: sequence % 199 === 0 ? "" : "字幕"});
      assert.equal(actual.apply(value), reference.apply(value));
    }
    equivalent(actual, reference);
    if (window === 2) retained = actual.items();
  }
  assert.equal(actual.records.size, 10000); assert.equal(actual.sizes.size, 10000); assert.equal(actual.discardedCount(), 2000);
  assert.equal(actual.visible.length, actual.items().length); assert.equal(retained.length, 5970);
  assert.equal(actual.apply(row(1, {window: 1})), "unchanged"); equivalent(actual, reference);
  actual.clear(); reference.clear(); equivalent(actual, reference);
});

test("capacity refusal preserves snapshots unless permitted old-window eviction changed the retained archive", () => {
  const actual = new SourceArchive(), reference = new Baseline();
  actual.continuous(); reference.continuous(); actual.advanceWindow("remote", 1); reference.advanceWindow("remote", 1);
  for (let sequence = 1; sequence <= 1024; sequence++) {
    const value = row(sequence, {window: 1, final: true, text: "x".repeat(4096)}); actual.apply(value); reference.apply(value);
  }
  const full = actual.items(); assert.equal(actual.apply(row(1025, {window: 1})), "capacity");
  assert.equal(actual.items(), full); equivalent(actual, reference);
  actual.advanceWindow("remote", 2); reference.advanceWindow("remote", 2);
  const tooLarge = row(1, {window: 2, text: "x".repeat(4 * 1024 * 1024 + 1)});
  assert.equal(actual.apply(tooLarge), "capacity"); assert.equal(reference.apply(tooLarge), "capacity");
  equivalent(actual, reference); assert.deepEqual(actual.items(), []); assert.equal(actual.discardedCount(), 1024);
  assert.equal(full.length, 1024);
});
