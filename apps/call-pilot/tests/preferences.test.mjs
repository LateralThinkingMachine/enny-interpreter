import test from 'node:test';
import assert from 'node:assert/strict';
import {browserPreferences, parsePreferences, defaultPreferences, PREFERENCES_KEY} from '../.test-build/preferences.js';

test('preferences persist only intentional non-sensitive choices with an exact versioned schema', () => {
  const data = new Map(), writes = [];
  const storage = browserPreferences(() => ({getItem: key => data.get(key) ?? null, setItem(key, value) {data.set(key, value); writes.push({key, value});}}));
  assert.deepEqual(storage.read(), defaultPreferences()); assert.equal(writes.length, 0);
  const choices = {version: 1, language: 'ja'};
  assert.equal(storage.write(choices), true);
  assert.deepEqual(storage.read(), choices);
  assert.deepEqual(writes, [{key: PREFERENCES_KEY, value: JSON.stringify(choices)}]);
  for (const extra of ['transcript', 'token', 'consent', 'deviceId', 'pinned', 'budget']) assert.throws(() => parsePreferences({...choices, [extra]: 'private'}));
  for (const change of [{version: 2}, {language: 'fr'}]) assert.throws(() => parsePreferences({...choices, ...change}));
});
test('blocked, oversized, malformed or unavailable browser storage never prevents default operation', () => {
  for (const raw of ['{', 'null', JSON.stringify({...defaultPreferences(), token: 'not-accepted'}), 'x'.repeat(257)]) {
    const store = browserPreferences(() => ({getItem: () => raw, setItem() {throw new Error('quota');}}));
    assert.deepEqual(store.read(), defaultPreferences()); assert.equal(store.write(defaultPreferences()), false);
  }
  const blocked = browserPreferences(() => {throw new Error('blocked');});
  assert.deepEqual(blocked.read(), defaultPreferences()); assert.equal(blocked.write(defaultPreferences()), false);
});
