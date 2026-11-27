import assert from 'node:assert/strict';
import test from 'node:test';
import {PrivateCaptions, PRIVATE_CAPTION_REFRESH_MS, PRIVATE_CAPTION_MAX_DELAY_MS, PRIVATE_CAPTION_PARAGRAPHS} from '../.test-build/webrtc/private-captions.js';
import {privateReadingGroups, privateReadingBody} from '../.test-build/webrtc/private-caption-reading.js';
const entry = (sequence, text, patch = {}) => Object.freeze({sequence, text, window: 1, provenance: 'gpt_live_output', model: 'gpt-live-1', start_ms: 0, end_ms: 100, participants: Object.freeze([]), ...patch});
function fixture(initial = [], stallFrames = false) {
  let now = 0, view = {entries: initial, phase: 'listening', discarded: 0, gaps: 0};
  const timers = new Set(), sourceListeners = new Set(), called = [];
  const source = {snapshot: () => view, subscribe(fn) {sourceListeners.add(fn); return () => sourceListeners.delete(fn);}};
  const schedule = (fn, delay) => {const timer = {fn, at: now + delay}; timers.add(timer); return () => timers.delete(timer);};
  const store = new PrivateCaptions(source, {now: () => now, after: schedule, frame: fn => schedule(fn, stallFrames ? Infinity : 16)});
  const unsubscribe = store.subscribe(() => called.push({at: now, value: store.snapshot()}));
  return {store, source, called, timers, sourceListeners, unsubscribe, get raw() {return view.entries;},
    set(patch) {view = {...view, ...patch}; for (const fn of sourceListeners) fn();},
    advance(ms) {const target = now + ms; for (;;) {const next = [...timers].filter(t => t.at <= target).sort((a, b) => a.at - b.at)[0]; if (!next) break; now = next.at; timers.delete(next); next.fn();} now = target;},
  };
}

test('first words display immediately and burst coalescing has one fixed deadline with no typewriter queue', () => {
  const h = fixture(); h.set({entries: [entry(1, 'The ')]});
  assert.equal(h.store.snapshot().paragraphs[0].text, 'The '); assert.equal(h.called.length, 1);
  for (let n = 2; n <= 200; n++) h.set({entries: [...h.raw, entry(n, n === 200 ? '日本語。' : 'blue ')]});
  assert.equal(h.called.length, 1); assert.equal(h.timers.size, 2);
  h.advance(PRIVATE_CAPTION_REFRESH_MS - 1); assert.equal(h.called.length, 1);
  h.advance(1); assert.equal(h.called.length, 1, 'coalesced update waits for one animation frame');
  h.advance(16); assert.equal(h.called.length, 2); assert.equal(h.timers.size, 0);
  assert.equal(h.store.snapshot().paragraphs[0].text, h.raw.map(row => row.text).join(''));
  assert.deepEqual(h.store.snapshot().paragraphs[0].originals, h.raw);
  h.advance(60_000); assert.equal(h.called.length, 2, 'no idle polling or character replay'); h.unsubscribe();
});

test('continuous arrivals cannot postpone the fixed refresh deadline', () => {
  const h = fixture(); h.set({entries: [entry(1, 'あ')]});
  for (let n = 2; n <= 41; n++) {h.advance(1); h.set({entries: [...h.raw, entry(n, 'い')]});}
  assert.equal(h.called.length, 1); h.advance(16);
  assert.equal(h.called.length, 2); assert.equal(h.called[1].at, PRIVATE_CAPTION_REFRESH_MS + 16);
  assert.equal(h.called[1].value.paragraphs[0].text, 'あ' + 'い'.repeat(40));
  assert.equal(h.timers.size, 0); h.unsubscribe();
});

test('old paragraph objects remain stable while literal lane text grows without sorting approximate intervals', () => {
  const originals = Object.freeze([entry(1, 'The price is 20.50. '), entry(2, '訂正です。', {provenance: 'gpt_live_input'}), entry(3, 'Earlier window.', {window: 2})]);
  const before = JSON.stringify(originals), h = fixture(originals), first = h.store.snapshot().paragraphs[0];
  h.set({entries: Object.freeze([...originals, entry(4, ' Not 30.', {window: 2, start_ms: 5, end_ms: 80})])});
  assert.equal(h.store.snapshot().paragraphs[0], first);
  assert.equal(h.store.snapshot().paragraphs.at(-1).text, 'Earlier window. Not 30.');
  assert.equal(JSON.stringify(originals), before); assert.equal(h.raw[0], originals[0]); h.unsubscribe();
});

test('phase/gap changes flush current observations; clear fences pending text immediately', () => {
  const h = fixture(); h.set({entries: [entry(1, 'No')]}); h.set({entries: [...h.raw, entry(2, ' further words')]});
  assert.equal(h.timers.size, 2); h.set({phase: 'closing'});
  assert.equal(h.store.snapshot().paragraphs[0].text, 'No further words'); assert.equal(h.store.snapshot().receiving, false); assert.equal(h.timers.size, 0);
  h.set({phase: 'listening'}); h.set({entries: [...h.raw, entry(3, ' pending')]});
  h.set({entries: [], phase: 'closed'}); assert.deepEqual(h.store.snapshot().paragraphs, []); assert.equal(h.timers.size, 0);
  h.advance(100); assert.deepEqual(h.store.snapshot().paragraphs, []);
  h.set({entries: [entry(4, 'next', {window: 2})], phase: 'renewing', gaps: 1});
  assert.equal(h.store.snapshot().gaps, 1); assert.equal(h.store.snapshot().paragraphs[0].earlierWordsUnavailable, true); h.unsubscribe();
});

test('projection caps rows and labels retention without mutating the full audit snapshot', () => {
  const raw = Object.freeze(Array.from({length: 120}, (_, i) => entry(i + 100, `word ${i}`, {window: i + 1})));
  const h = fixture(raw); assert.equal(h.store.snapshot().paragraphs.length, PRIVATE_CAPTION_PARAGRAPHS);
  assert.equal(h.store.snapshot().hiddenParagraphs, 40); assert.equal(h.raw, raw); assert.equal(h.raw.length, 120);
  h.set({entries: raw.slice(1), discarded: 1}); assert.equal(h.store.snapshot().discarded, 1); assert.equal(h.store.snapshot().hiddenParagraphs, 39); h.unsubscribe();
});

test('unmount cancels every pending display task and remount catches intervening source changes', () => {
  const h = fixture(); h.set({entries: [entry(1, 'start')]}); h.set({entries: [...h.raw, entry(2, 'pending')]});
  const staleCallback = [...h.timers][0].fn; h.unsubscribe(); assert.equal(h.timers.size, 0); assert.equal(h.sourceListeners.size, 0);
  h.set({entries: [], phase: 'closed'}); staleCallback(); assert.equal(h.called.length, 1);
  const detach = h.store.subscribe(() => {}); assert.deepEqual(h.store.snapshot().paragraphs, []); assert.equal(h.sourceListeners.size, 1); detach();
});

test('playback and cost-only controller updates neither regroup text nor schedule work', () => {
  const h = fixture([entry(1, 'unchanged')]), previous = h.store.snapshot();
  h.set({playback: {ducking: true}, status: {usage_seconds: 20}});
  assert.equal(h.store.snapshot(), previous); assert.equal(h.called.length, 0); assert.equal(h.timers.size, 0); h.unsubscribe();
});

test('remount flushes a previously pending snapshot even when no new source event arrives', () => {
  const h = fixture(); h.set({entries: [entry(1, 'first')]}); h.set({entries: [...h.raw, entry(2, ' latest')]});
  h.unsubscribe(); const detach = h.store.subscribe(() => {});
  assert.equal(h.store.snapshot().paragraphs[0].text, 'first latest'); assert.equal(h.timers.size, 0); detach();
});

test('a stalled animation frame uses the fixed80ms fallback and stale frame cannot republish', () => {
  const h = fixture([], true); h.set({entries: [entry(1, 'first ')]}); h.set({entries: [...h.raw, entry(2, 'latest')]});
  h.advance(40); const frame = [...h.timers].find(timer => timer.at === Infinity).fn;
  h.advance(PRIVATE_CAPTION_MAX_DELAY_MS - 41); assert.equal(h.called.length, 1);
  h.advance(1); assert.equal(h.called[1].at, PRIVATE_CAPTION_MAX_DELAY_MS); assert.equal(h.store.snapshot().paragraphs[0].tail, 'latest');
  assert.equal(h.timers.size, 0); frame(); assert.equal(h.called.length, 2);
  h.set({entries: [...h.raw, entry(3, 'next')]}); h.advance(40);
  const late = [...h.timers].find(timer => timer.at === Infinity).fn;
  h.set({entries: [], phase: 'closed'}); const count = h.called.length; late(); h.advance(100);
  assert.equal(h.called.length, count); assert.deepEqual(h.store.snapshot().paragraphs, []); h.unsubscribe();
});

test('complete lexical prefix is stable and provisional word tail stays visible without fabricated completion', () => {
  const rows = [entry(1, 'The inter')];
  let previous = privateReadingBody(privateReadingGroups(rows)[0], true);
  assert.equal(previous.body, 'The '); assert.equal(previous.tail, 'inter');
  for (const [sequence, text] of [[2, 'pretation'], [3, ' is '], [4, 'crisp'], [5, '.']]) {
    rows.push(entry(sequence, text)); const next = privateReadingBody(privateReadingGroups(rows)[0], true, previous);
    assert.ok(next.body.startsWith(previous.body)); assert.equal(next.body + next.tail, rows.map(row => row.text).join('')); previous = next;
  }
  assert.equal(previous.text, 'The interpretation is crisp.'); assert.equal(previous.tail, ''); assert.equal(previous.recent, true, 'punctuation is not a provider final');
  const pending = privateReadingBody(privateReadingGroups([entry(1, '日本語で話しま')])[0], true);
  assert.equal(pending.body + pending.tail, '日本語で話しま'); assert.ok(pending.tail.length > 0);
  const closed = privateReadingBody(pending, false, pending); assert.equal(closed.tail, ''); assert.equal(closed.text, pending.text);
});

test('only same-lane approximate pause at a lexical boundary separates readable paragraphs', () => {
  const rows = [entry(1, 'I intended to finish this', {provenance: 'gpt_live_input', end_ms: 500}), entry(2, '先に進みます。', {start_ms: 90000, end_ms: 91000}), entry(3, ' Oh, wait', {provenance: 'gpt_live_input', start_ms: 2100, end_ms: 2500})];
  const groups = privateReadingGroups(rows);
  assert.equal(groups.length, 3); assert.deepEqual(groups.map(group => group.sequence), [1, 2, 3]);
  assert.equal(groups[2].pauseBefore, true); assert.equal(groups[0].text, 'I intended to finish this');
  assert.equal(groups[2].text, ' Oh, wait');
  assert.equal(groups[0].originals[0], rows[0]); assert.equal(groups[2].originals[0], rows[2]);
  const noPause = privateReadingGroups([rows[0], {...rows[2], sequence: 2, start_ms: 1000}]);
  assert.equal(noPause.length, 1, 'the phrase does not itself establish an interruption');
  const oppositeOnly = privateReadingGroups([rows[0], rows[1], {...rows[2], start_ms: 600}]);
  assert.equal(oppositeOnly.length, 2, 'opposite-lane timestamps cannot split source speech');
});

test('long provider gaps do not split delayed English words, decimals, combining marks or emoji joins', () => {
  for (const [before, after] of [['inter', 'pretation'], ['20.', '50'], ["don'", 't'], ['can’', 't'], ['real-', 'time'], ['か', '\u3099'], ['e', '\u0301'], ['👩', '\u200d💻'], ['\ud83d', '\ude00']]) {
    const groups = privateReadingGroups([entry(1, before, {end_ms: 100}), entry(2, after, {start_ms: 1800, end_ms: 1900})]);
    assert.equal(groups.length, 1, `${before}/${after}`); assert.equal(groups[0].text, before + after);
    const display = privateReadingBody(groups[0], true); assert.equal(display.body + display.tail, before + after);
  }
});

test('apostrophe and decimal tails remain provisional; pause-separated old rows survive opposite-lane growth', () => {
  for (const text of ["I don'", 'It costs 20.']) {
    const group = privateReadingBody(privateReadingGroups([entry(1, text)])[0], true);
    assert.equal(group.body, text.startsWith('I ') ? 'I ' : 'It costs '); assert.equal(group.body + group.tail, text);
  }
  const rows = [entry(1, 'Abandoned clause', {provenance: 'gpt_live_input'}), entry(2, ' A different thought', {provenance: 'gpt_live_input', start_ms: 1800, end_ms: 1900}), entry(3, '訳', {start_ms: 0, end_ms: 50})];
  const h = fixture(rows), first = h.store.snapshot().paragraphs[0];
  h.set({entries: [...rows, entry(4, '文', {start_ms: 30, end_ms: 100})]});
  assert.equal(h.store.snapshot().paragraphs[0], first); assert.equal(first.body, 'Abandoned clause');
  assert.equal(first.tail, ''); assert.equal(first.recent, false); h.unsubscribe();
});
