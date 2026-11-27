import assert from 'node:assert/strict';
import test from 'node:test';
import {privateListeningPolicy} from '../.test-build/webrtc/private-listening-setup.js';

const selection = {mode: 'auto_to_target', target: 'ja', first: 'ja', second: 'en'};

test('private setup produces independent immutable personal or two-way policies', () => {
  const mutable = {...selection};
  const personal = privateListeningPolicy(mutable);
  mutable.target = 'en';
  assert.deepEqual(personal, {schema_version: 'interpretation.v1', mode: 'auto_to_target', target: 'ja'});
  assert.equal(Object.isFrozen(personal), true);
  for (const [first, second] of [['ja', 'en'], ['en', 'ja'], ['es', 'vi'], ['hi', 'ja']]) {
    const pair = privateListeningPolicy({...selection, mode: 'two_way', first, second});
    assert.deepEqual(pair, {schema_version: 'interpretation.v1', mode: 'two_way', first, second});
    assert.equal(Object.isFrozen(pair), true);
  }
});

test('private setup refuses identical and unsupported languages or modes before controller admission', () => {
  for (const patch of [
    {mode: 'two_way', first: 'ja', second: 'ja'},
    {mode: 'two_way', first: 'en', second: 'en'},
    {mode: 'two_way', first: 'unknown'},
    {mode: 'two_way', second: null},
    {target: 'unknown'}, {mode: 'room_broadcast'},
  ]) assert.throws(() => privateListeningPolicy({...selection, ...patch}));
});
