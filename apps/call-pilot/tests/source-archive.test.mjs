import assert from "node:assert/strict";
import test from "node:test";
import {SourceArchive} from "../.test-build/source-archive.js";
import {parseEvent} from "../.test-build/protocol.js";

const row = overrides => ({key: 1, route_id: "route_a", source: "remote_mix", route: "transcription", live_mode: null, language: "en", provenance: "source_asr", text: "Asha and", final: false, item: {id: "item_a", revision: 1, sequence: 1}, ...overrides});
test("cumulative interim becomes one corrected final item with exact byte accounting", () => {
  const archive = new SourceArchive();
  archive.apply(row()); archive.apply(row({key: 2, text: "Asha and Ken", item: {id: "item_a", revision: 2, sequence: 1}}));
  archive.apply(row({key: 3, text: "Asha and Ken meet.", final: true, item: {id: "item_a", revision: 3, sequence: 1}}));
  assert.equal(archive.items().length, 1); assert.equal(archive.items()[0].key, 1); assert.equal(archive.items()[0].final, true);
  assert.equal(archive.items()[0].text, "Asha and Ken meet."); assert.equal(archive.byteLength(), 18);
  archive.clear(); assert.equal(archive.byteLength(), 0); assert.deepEqual(archive.items(), []);
});
test("out-of-order finals are sorted by item sequence within their own route", () => {
  const archive = new SourceArchive();
  archive.apply(row({text: "second", final: true, item: {id: "item_b", revision: 1, sequence: 2}}));
  archive.apply(row({key: 2, text: "first", final: true}));
  archive.apply(row({key: 3, route_id: "route_b", source: "self", text: "independent", final: true}));
  assert.deepEqual(archive.items().map(item => item.text), ["first", "second", "independent"]);
});
test("duplicates and stale revisions cannot regress content; conflicting identity or final changes reject", () => {
  const archive = new SourceArchive(), final = row({text: "final", final: true, item: {id: "item_a", revision: 2, sequence: 1}});
  archive.apply(final); assert.equal(archive.apply(final), "unchanged"); assert.equal(archive.apply(row()), "unchanged");
  for (const next of [row({text: "changed", item: {id: "item_a", revision: 2, sequence: 1}}), row({item: {id: "item_a", revision: 3, sequence: 1}}), row({item: {id: "item_a", revision: 2, sequence: 2}}), row({item: {id: "different", revision: 1, sequence: 1}})]) assert.throws(() => archive.apply(next));
  assert.equal(archive.items()[0].text, "final");
});
test("empty final removes provisional text, retains sealed identity and releases its bytes", () => {
  const archive = new SourceArchive(); archive.apply(row());
  const empty = row({text: "", final: true, item: {id: "item_a", revision: 2, sequence: 1}});
  archive.apply(empty); assert.equal(archive.byteLength(), 0); assert.deepEqual(archive.items(), []);
  assert.equal(archive.apply(row()), "unchanged"); assert.throws(() => archive.apply(row({item: {id: "item_a", revision: 3, sequence: 1}})));
});
test("source closure marks only unresolved matching items incomplete without promoting them to final", () => {
  const archive = new SourceArchive(); archive.apply(row());
  archive.apply(row({key: 2, route_id: "route_b", source: "self", final: true}));
  archive.close({route_id: "route_a"}); assert.equal(archive.items()[0].incomplete, true); assert.equal(archive.items()[0].final, false); assert.equal(archive.items()[1].incomplete, undefined);
  assert.throws(() => archive.apply(row({item: {id: "item_a", revision: 2, sequence: 1}})));
});
test("source item protocol binds Unicode bounds, exact identity and empty-final-only semantics", () => {
  const source = row(); delete source.key; delete source.language; delete source.live_mode;
  const event = {type: "transcript", generation: 1, ...source};
  assert.equal(parseEvent(JSON.stringify({...event, text: "😀".repeat(4096)}), "operator").text.length, 8192);
  assert.equal(parseEvent(JSON.stringify({...event, text: "", final: true}), "operator").text, "");
  for (const changed of [{item: null}, {item: {...event.item, revision: 0}}, {item: {...event.item, sequence: 4097}}, {item: {...event.item, other: true}}, {text: "", final: false}, {text: "😀".repeat(4097)}, {route: "translation", provenance: "translation"}]) assert.throws(() => parseEvent(JSON.stringify({...event, ...changed}), "operator"));
  assert.equal(parseEvent(JSON.stringify({...event, route: "translation", provenance: "translation", item: null}), "operator").item, null);
});
