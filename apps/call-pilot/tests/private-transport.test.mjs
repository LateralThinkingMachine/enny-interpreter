import test from 'node:test';
import assert from 'node:assert/strict';
import {PrivateTransportSampler, UNAVAILABLE_TRANSPORT_EVIDENCE} from '../.test-build/webrtc/private-transport.js';

const inbound = {id: 'audio', type: 'inbound-rtp', kind: 'audio', timestamp: 1000, jitter: .01, jitterBufferDelay: 50, jitterBufferEmittedCount: 1000, transportId: 'transport'};
const rows = (changes = {}, extras = []) => new Map([
  ['audio', {...inbound, ...changes}],
  ['transport', {id: 'transport', type: 'transport', selectedCandidatePairId: 'selected'}],
  ['selected', {id: 'selected', type: 'candidate-pair', state: 'succeeded', currentRoundTripTime: .15, address: '192.0.2.1'}],
  ...extras,
]);
function near(actual, expected) {assert.ok(Math.abs(actual - expected) < .000001, `${actual} != ${expected}`);}

test('selected-pair RTT and inbound jitter are separate from consecutive sample-weighted buffer delay', () => {
  const sampler = new PrivateTransportSampler();
  assert.deepEqual(sampler.sample(rows()), {status: 'observed', roundTripMs: 150, inboundJitterMs: 10, jitterBufferDelayMs: null, sampleIntervalMs: null});
  const next = sampler.sample(rows({timestamp: 2000, jitterBufferDelay: 53, jitterBufferEmittedCount: 1100}));
  near(next.jitterBufferDelayMs, 30); assert.equal(next.sampleIntervalMs, 1000);
  assert.equal(Object.isFrozen(next), true); assert.doesNotMatch(JSON.stringify(next), /audio|selected|192\.0\.2|address|transportId/);
  const more = sampler.sample(rows({timestamp: 4000, jitterBufferDelay: 61, jitterBufferEmittedCount: 1300}));
  near(more.jitterBufferDelayMs, 40); assert.equal(more.sampleIntervalMs, 2000);
});

test('missing, changed, reset, cached, stagnant and excessively delayed counter observations cannot invent buffer latency', () => {
  for (const changed of [
    {id: 'replacement', timestamp: 2000, jitterBufferDelay: 51, jitterBufferEmittedCount: 1100},
    {timestamp: 2000, jitterBufferDelay: 1, jitterBufferEmittedCount: 10},
    {timestamp: 999, jitterBufferDelay: 51, jitterBufferEmittedCount: 1100},
    {timestamp: 1000, jitterBufferDelay: 51, jitterBufferEmittedCount: 1100},
    {timestamp: 2000, jitterBufferDelay: 50, jitterBufferEmittedCount: 1000},
    {timestamp: 7000, jitterBufferDelay: 51, jitterBufferEmittedCount: 1100},
    {timestamp: 2000, jitterBufferDelay: undefined, jitterBufferEmittedCount: 1100},
  ]) {
    const sampler = new PrivateTransportSampler(); sampler.sample(rows());
    const next = sampler.sample(rows(changed)); assert.equal(next.jitterBufferDelayMs, null); assert.equal(next.sampleIntervalMs, null);
  }
  const sampler = new PrivateTransportSampler(); sampler.sample(rows()); sampler.reset();
  assert.equal(sampler.sample(rows({timestamp: 2000, jitterBufferDelay: 53, jitterBufferEmittedCount: 1100})).jitterBufferDelayMs, null);
  sampler.sample(new Map());
  assert.equal(sampler.sample(rows({timestamp: 3000, jitterBufferDelay: 55, jitterBufferEmittedCount: 1200})).jitterBufferDelayMs, null);
});

test('unsupported and invalid fields are unknown, while actual zero jitter and RTT stay valid', () => {
  for (const value of [-1, NaN, Infinity, '1', null, 61]) {
    const report = rows({jitter: value, jitterBufferDelay: value}); report.get('selected').currentRoundTripTime = value;
    assert.deepEqual(new PrivateTransportSampler().sample(report), UNAVAILABLE_TRANSPORT_EVIDENCE);
  }
  const zero = rows({jitter: 0}); zero.get('selected').currentRoundTripTime = 0;
  const result = new PrivateTransportSampler().sample(zero); assert.equal(result.roundTripMs, 0); assert.equal(result.inboundJitterMs, 0);
  const absent = rows({jitter: undefined, jitterBufferDelay: undefined}); absent.delete('selected');
  assert.deepEqual(new PrivateTransportSampler().sample(absent), UNAVAILABLE_TRANSPORT_EVIDENCE);
});

test('candidate nomination alone, video rows and ambiguous audio streams cannot be borrowed as current audio transport', () => {
  const sampler = new PrivateTransportSampler();
  const report = rows({}, [['other', {id: 'other', type: 'candidate-pair', nominated: true, state: 'succeeded', currentRoundTripTime: 8}]]);
  report.get('transport').selectedCandidatePairId = 'missing'; assert.equal(sampler.sample(report).roundTripMs, null);
  report.get('transport').selectedCandidatePairId = 'selected'; report.get('selected').state = 'failed'; assert.equal(sampler.sample(report).roundTripMs, null);
  report.set('video', {...inbound, id: 'video', kind: 'video', jitter: 9}); assert.equal(sampler.sample(report).inboundJitterMs, 10);
  report.set('other-audio', {...inbound, id: 'other-audio'}); assert.deepEqual(sampler.sample(report), UNAVAILABLE_TRANSPORT_EVIDENCE);
  report.delete('other-audio'); report.get('audio').kind = 'video'; assert.deepEqual(sampler.sample(report), UNAVAILABLE_TRANSPORT_EVIDENCE);
});

test('bounded report and counter validation refuses unsafe or contradictory delay arithmetic', () => {
  const sampler = new PrivateTransportSampler();
  const report = rows(); for (let i = 0; i < 254; i++) report.set(`extra${i}`, {id: `extra${i}`, type: 'codec'});
  assert.deepEqual(sampler.sample(report), UNAVAILABLE_TRANSPORT_EVIDENCE);
  for (const emitted of [-1, 2.5, Number.MAX_SAFE_INTEGER + 1, Infinity, '1200']) {
    sampler.sample(rows()); assert.equal(sampler.sample(rows({timestamp: 2000, jitterBufferDelay: 60, jitterBufferEmittedCount: emitted})).jitterBufferDelayMs, null);
  }
  sampler.sample(rows()); assert.equal(sampler.sample(rows({timestamp: 2000, jitterBufferDelay: 200, jitterBufferEmittedCount: 1001})).jitterBufferDelayMs, null);
});
