import assert from 'node:assert/strict';
import test from 'node:test';
import {PCM16Resampler} from '../src/audio/capture-worklet.js';
import {encodeCaptureFrame, decodePCM, decodePreparedWav} from '../.test-build/audio/framing.js';
import {createAudioEngine} from '../.test-build/audio/index.js';

function resample(rate, frequency, chunk = 128) {
  const result = [];
  const engine = new PCM16Resampler(rate, frame => result.push(...frame));
  const input = new Float32Array(rate);
  for (let i = 0; i < rate; i++) input[i] = 0.7 * Math.sin(i * 2 * Math.PI * frequency / rate);
  for (let i = 0; i < input.length; i += chunk) engine.push(input.subarray(i, i + chunk));
  return result;
}
function rms(samples) { return Math.sqrt(samples.reduce((sum, item) => sum + (item / 32768) ** 2, 0) / samples.length); }

test('capture yields exactly 50 20ms frames at 16, 44.1, 48 and 96kHz without chunk-dependent drift', () => {
  for (const rate of [16000, 44100, 48000, 96000]) {
    const actual = resample(rate, 1000);
    assert.equal(actual.length, 16000);
    assert.deepEqual(actual, resample(rate, 1000, 173));
    assert.ok(rms(actual.slice(320)) > 0.45);
  }
});
test('FIR attenuates out-of-band 12kHz that would alias into 4kHz speech', () => {
  for (const rate of [44100, 48000, 96000]) assert.ok(rms(resample(rate, 12000).slice(320)) < 0.004);
});
test('PCM conversion is bounded, rejects NaN/rates and clips saturation', () => {
  assert.throws(() => new PCM16Resampler(8000, () => {}));
  const output = [];
  const engine = new PCM16Resampler(16000, frame => output.push(...frame));
  engine.push(new Float32Array(320).fill(2));
  engine.push(new Float32Array(320).fill(-2));
  assert.equal(Math.max(...output), 32767);
  assert.equal(Math.min(...output), -32768);
  assert.throws(() => engine.push([NaN]));
});
test('capture wire separates sources and encodes unsigned contiguous sequence and PCM little endian', () => {
  const pcm = new Int16Array(320); pcm[0] = -2345; pcm[1] = 1234;
  const view = new DataView(encodeCaptureFrame('self', 0xffff_fffe, pcm.buffer));
  assert.equal(view.byteLength, 646); assert.equal(view.getUint8(0), 1); assert.equal(view.getUint8(1), 1);
  assert.equal(view.getUint32(2, true), 0xffff_fffe); assert.equal(view.getInt16(6, true), -2345); assert.equal(view.getInt16(8, true), 1234);
  assert.equal(new DataView(encodeCaptureFrame('remote_mix', 0, pcm.buffer)).getUint8(1), 0);
  assert.throws(() => encodeCaptureFrame('self', 0x1_0000_0000, pcm.buffer));
  assert.throws(() => encodeCaptureFrame('self', 0, new ArrayBuffer(638)));
});
test('private PCM uses view offsets and refuses odd, oversized and unknown-rate blocks', () => {
  const bytes = new Uint8Array([255, 255, 0, 128, 255, 127, 0, 0]);
  assert.deepEqual([...decodePCM(bytes.subarray(2, 6), 24000)], [-1, 32767 / 32768]);
  for (const [frame, rate] of [[new Uint8Array(3), 16000], [new Uint8Array(48002), 16000], [new Uint8Array(640), 44100]]) assert.throws(() => decodePCM(frame, rate));
});

function wav(seconds = 1, rate = 24000) {
  const bytes = new ArrayBuffer(44 + seconds * rate * 2), view = new DataView(bytes);
  const tag = (at, value) => [...value].forEach((char, i) => view.setUint8(at + i, char.charCodeAt(0)));
  tag(0, 'RIFF'); view.setUint32(4, bytes.byteLength - 8, true); tag(8, 'WAVE'); tag(12, 'fmt ');
  view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true); view.setUint32(24, rate, true);
  view.setUint32(28, rate * 2, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true); tag(36, 'data'); view.setUint32(40, bytes.byteLength - 44, true);
  return bytes;
}
test('prepared WAV requires exact complete mono PCM length, rate and bounded duration', () => {
  assert.equal(decodePreparedWav(wav()).samples.length, 24000);
  assert.equal(decodePreparedWav(wav(60, 16000)).samples.length, 960000);
  const stereo = wav(); new DataView(stereo).setUint16(22, 2, true); assert.throws(() => decodePreparedWav(stereo));
  assert.throws(() => decodePreparedWav(wav().slice(0, -2)));
  assert.throws(() => decodePreparedWav(wav(61, 16000)));
  assert.throws(() => decodePreparedWav(wav(1, 44100)));
});

class Track extends EventTarget {
  readyState = 'live'; stopped = 0; muted = false; enabled = true;
  constructor(surface = 'browser') { super(); this.surface = surface; }
  stop() { this.readyState = 'ended'; this.stopped++; }
  getSettings() { return {displaySurface: this.surface}; }
}
function stream(surface = 'browser', audio = true) {
  const a = audio ? [new Track()] : [], v = [new Track(surface)];
  return {getTracks: () => [...a, ...v], getAudioTracks: () => a, getVideoTracks: () => v};
}
class FakeContext {
  static instances = [];
  state = 'running'; currentTime = 0; sampleRate = 48000; destination = {}; nodes = [];
  audioWorklet = {addModule: async () => {}};
  constructor() { FakeContext.instances.push(this); }
  async resume() { this.state = 'running'; }
  async close() { this.state = 'closed'; }
  createMediaStreamSource() { return {connect() {}, disconnect() {}}; }
  createBuffer(_channels, count, rate) { return {duration: count / rate, rate, samples: new Float32Array(count), copyToChannel(data) {this.samples.set(data);}}; }
  createBufferSource() {
    const node = {onended: null, stopped: false, buffer: undefined, startTime: undefined, connect() {}, disconnect() {}, start(time) { this.startTime = time; }, stop() { this.stopped = true; }};
    this.nodes.push(node); return node;
  }
}
class FakeWorklet {
  static instances = [];
  port = {onmessage: undefined, posted: [], postMessage(data) { this.posted.push(data); }, close() {}};
  constructor() { FakeWorklet.instances.push(this); }
  connect() {} disconnect() {}
}
function browser(t, display = async () => stream()) {
  FakeContext.instances = []; FakeWorklet.instances = [];
  const old = Object.getOwnPropertyDescriptors(globalThis);
  const set = (name, value) => Object.defineProperty(globalThis, name, {configurable: true, writable: true, value});
  set('window', new EventTarget()); set('navigator', {mediaDevices: {getDisplayMedia: display, getUserMedia: display}});
  set('AudioContext', FakeContext); set('AudioWorkletNode', FakeWorklet); set('MediaStream', class {});
  t.after(() => { for (const name of ['window', 'navigator', 'AudioContext', 'AudioWorkletNode', 'MediaStream']) { if (old[name]) Object.defineProperty(globalThis, name, old[name]); else delete globalThis[name]; } });
}
test('Stop fences unresolved device permission and releases all returned tracks', async t => {
  let grant; browser(t, () => new Promise(resolve => { grant = resolve; }));
  const engine = createAudioEngine({onFrame: () => true, onGap() {}});
  const pending = engine.startCapture('remote_mix'); engine.stopAll();
  const selected = stream(); grant(selected); await assert.rejects(pending, /capture_closed/);
  assert.ok(selected.getTracks().every(track => track.stopped > 0));
  await assert.rejects(engine.startCapture('self'), /capture_closed/);
});
test('capture refuses entire-screen audio', async t => {
  let calls = 0; browser(t, async () => { calls++; return stream('monitor'); });
  const engine = createAudioEngine({onFrame: () => true, onGap() {}});
  await assert.rejects(engine.startCapture('remote_mix'), /select_browser_tab/); assert.equal(calls, 1); engine.stopAll();
});
test('socket backpressure permanently closes capture and releases tracks', async t => {
  const selected = stream(); browser(t, async () => selected);
  const gaps = []; const engine = createAudioEngine({onFrame: () => false, onGap: (...args) => gaps.push(args)});
  await engine.startCapture('remote_mix');
  FakeContext.instances[0].currentTime = 0.06;
  FakeWorklet.instances[0].port.onmessage({data: {type: 'pcm', at: 0.06, pcm: new ArrayBuffer(640)}});
  assert.deepEqual(gaps, [['remote_mix', 'capture_backpressure']]);
  assert.ok(selected.getTracks().every(track => track.stopped > 0));
  await assert.rejects(engine.startCapture('remote_mix'), /capture_closed/); engine.stopAll();
});
test('track loss reports a single gap and cannot reopen a source', async t => {
  const selected = stream(); browser(t, async () => selected);
  const gaps = []; const engine = createAudioEngine({onFrame: () => true, onGap: (...args) => gaps.push(args)});
  await engine.startCapture('remote_mix'); selected.getAudioTracks()[0].dispatchEvent(new Event('ended'));
  selected.getVideoTracks()[0].dispatchEvent(new Event('ended'));
  assert.deepEqual(gaps, [['remote_mix', 'capture_track_ended']]);
  await assert.rejects(engine.startCapture('remote_mix'), /capture_closed/); engine.stopAll();
});
test('stale PCM is refused before any socket emission', async t => {
  browser(t); let sent = 0; const gaps = [];
  const engine = createAudioEngine({onFrame: () => { sent++; return true; }, onGap: (...args) => gaps.push(args)});
  await engine.startCapture('self'); FakeContext.instances[0].currentTime = 0.5;
  FakeWorklet.instances[0].port.onmessage({data: {type: 'pcm', at: 0, pcm: new ArrayBuffer(640)}});
  assert.equal(sent, 0); assert.deepEqual(gaps, [['self', 'capture_stale']]); engine.stopAll();
});
test('meeting capture uses the selected browser tab without inspecting a conference title or requesting a microphone', async t => {
  const selected = stream();
  for (const track of selected.getTracks()) {
    Object.defineProperty(track, 'label', {get: () => assert.fail('Tab titles are private')});
  }
  browser(t, async options => {
    assert.deepEqual(options.video, {displaySurface: 'browser'});
    assert.equal(options.audio, true);
    assert.equal(options.systemAudio, 'exclude');
    return selected;
  });
  navigator.mediaDevices.getUserMedia = () => assert.fail('Microphone requires a separate selection');
  const engine = createAudioEngine({onFrame: () => assert.fail('Local preview must not forward audio'), onGap() {}});
  await engine.prepareCapture('remote_mix');
  engine.stopAll();
  assert.ok(selected.getTracks().every(track => track.stopped));
});

test('an unavailable tab picker leaves microphone capture available only by explicit selection', async t => {
  browser(t);
  navigator.mediaDevices.getDisplayMedia = undefined;
  let microphoneRequests = 0;
  navigator.mediaDevices.getUserMedia = async options => {
    microphoneRequests++;
    assert.equal(options.video, false);
    const audio = [new Track()];
    return {getTracks: () => audio, getAudioTracks: () => audio, getVideoTracks: () => []};
  };
  const engine = createAudioEngine({onFrame: () => true, onGap() {}});
  await assert.rejects(engine.prepareCapture('remote_mix'), /capture_unavailable/);
  assert.equal(microphoneRequests, 0);
  await engine.prepareCapture('self');
  assert.equal(microphoneRequests, 1);
  engine.stopAll();
});

test('missing shared-tab audio is visibly refused without device fallback', async t => {
  const selected = stream('browser', false); browser(t, async () => selected);
  let microphoneRequests = 0;
  navigator.mediaDevices.getUserMedia = () => {microphoneRequests++; return Promise.resolve(stream());};
  const engine = createAudioEngine({onFrame: () => true, onGap() {}});
  await assert.rejects(engine.startCapture('remote_mix'), /tab_audio_missing/);
  assert.equal(microphoneRequests, 0);
  assert.ok(selected.getTracks().every(track => track.stopped)); engine.stopAll();
});
test('private audio defaults muted, has a strict queue bound, and clears synchronously on Stop', async t => {
  browser(t); const faults = []; const engine = createAudioEngine({onFrame: () => true, onGap() {}, onPlaybackFault: (...args) => faults.push(args)});
  engine.playPrivate('live', new Uint8Array(48000), 24000); assert.equal(FakeContext.instances.length, 0);
  await engine.resumePlayback(); engine.setPrivateMuted('live', false);
  engine.playPrivate('live', new Uint8Array(48000), 24000);
  engine.playPrivate('live', new Uint8Array(48000), 24000);
  assert.deepEqual(faults, [['live', 'playback_backpressure']]);
  assert.ok(FakeContext.instances[0].nodes.every(node => node.stopped)); engine.stopAll();
});
test('public clip schedules under 200ms and stops when renewed delivery authority is lost', async t => {
  browser(t); let current = true, started = 0, failed = 0;
  const engine = createAudioEngine({onFrame: () => true, onGap() {}}); await engine.resumePlayback();
  const playing = engine.playClip(wav(60), {isCurrent: () => current, started: () => { started++; }, ended: () => assert.fail('not completed'), failed: () => { failed++; }});
  const context = FakeContext.instances[0];
  assert.ok(Math.max(...context.nodes.map(node => node.startTime + node.buffer.duration)) <= 0.2);
  assert.equal(started, 1); current = false;
  await assert.rejects(playing, /clip_stopped/); assert.equal(failed, 1);
  assert.ok(context.nodes.every(node => node.stopped)); engine.stopAll();
});
test('Stop rejects an audition immediately and never invokes completed callback', async t => {
  browser(t); const engine = createAudioEngine({onFrame: () => true, onGap() {}}); await engine.resumePlayback();
  const playing = engine.playClip(wav(2), {started() {}, ended: () => assert.fail('not completed'), failed() {}});
  engine.stopAll(); assert.ok(FakeContext.instances[0].nodes.every(node => node.stopped));
  await assert.rejects(playing, /clip_stopped/);
});
test('reentrant Stop in local playback receipt schedules no further clip chunks', async t => {
  browser(t); const engine = createAudioEngine({onFrame: () => true, onGap() {}}); await engine.resumePlayback();
  const playing = engine.playClip(wav(10), {started: () => engine.stopAll(), ended: () => assert.fail('not completed'), failed() {}});
  await assert.rejects(playing, /clip_stopped/);
  assert.equal(FakeContext.instances[0].nodes.length, 1); assert.ok(FakeContext.instances[0].nodes[0].stopped);
});

test('Stop attempts every owned release despite port, node, track, context and callback failures', async t => {
  const streams = [stream(), stream()]; let selected = 0, frames = 0;
  browser(t, async () => streams[selected++]);
  const attempts = [], inputs = [];
  const original = FakeContext.prototype.createMediaStreamSource;
  FakeContext.prototype.createMediaStreamSource = function () {
    const input = {connect() {}, disconnect() { attempts.push(`input${inputs.indexOf(input)}`); throw Error('synthetic disconnect'); }};
    inputs.push(input); return input;
  };
  t.after(() => { FakeContext.prototype.createMediaStreamSource = original; });
  const engine = createAudioEngine({onFrame: () => { frames++; return true; }, onGap() {}});
  await engine.startCapture('remote_mix'); await engine.startCapture('self'); await engine.resumePlayback();
  for (const kind of ['translation', 'live', 'live_interpretation']) { engine.setPrivateMuted(kind, false); engine.playPrivate(kind, new Uint8Array(640), 16000); }
  const playing = engine.playClip(wav(2), {started() {}, ended: () => assert.fail('not completed'), failed() { attempts.push('clip-failed'); throw Error('synthetic callback'); }});
  const rejected = assert.rejects(playing, /clip_stopped_or_failed/);
  const handlers = FakeWorklet.instances.map(node => node.port.onmessage);
  const playback = FakeContext.instances[2], nodeCount = playback.nodes.length;
  FakeWorklet.instances.forEach((node, i) => {
    node.port.postMessage = () => {
      attempts.push(`port-stop${i}`);
      for (const handler of handlers) handler({data: {type: 'pcm', at: 0, pcm: new ArrayBuffer(640)}});
      for (const kind of ['translation', 'live', 'live_interpretation']) engine.playPrivate(kind, new Uint8Array(640), 16000);
      throw Error('synthetic port');
    };
    node.port.close = () => { attempts.push(`port-close${i}`); throw Error('synthetic port'); };
    node.disconnect = () => { attempts.push(`worklet${i}`); throw Error('synthetic disconnect'); };
  });
  streams.forEach((value, i) => value.getTracks().forEach((track, j) => { track.stop = () => { attempts.push(`track${i}:${j}`); if (j === 0) throw Error('synthetic track'); track.readyState = 'ended'; }; }));
  FakeContext.instances.forEach((context, i) => { context.close = () => { attempts.push(`context${i}`); if (i === 0) throw Error('synthetic close'); context.state = 'closed'; return Promise.resolve(); }; });
  playback.nodes.forEach((node, i) => { node.stop = () => { attempts.push(`play-stop${i}`); throw Error('synthetic stop'); }; node.disconnect = () => { attempts.push(`play-disconnect${i}`); throw Error('synthetic disconnect'); }; });
  const remove = globalThis.window.removeEventListener.bind(globalThis.window); globalThis.window.removeEventListener = (...args) => { attempts.push('remove-listener'); remove(...args); };
  assert.throws(() => engine.stopAll(), /^Error: audio_cleanup_unconfirmed$/); await rejected;
  for (let i = 0; i < 2; i++) for (const prefix of ['port-stop', 'port-close', 'worklet', 'input']) assert.equal(attempts.filter(value => value === `${prefix}${i}`).length, 1);
  for (let i = 0; i < 2; i++) for (let j = 0; j < 2; j++) assert.equal(attempts.filter(value => value === `track${i}:${j}`).length, 1);
  for (let i = 0; i < 3; i++) assert.ok(attempts.includes(`context${i}`));
  for (let i = 0; i < nodeCount; i++) { assert.ok(attempts.includes(`play-stop${i}`)); assert.ok(attempts.includes(`play-disconnect${i}`)); }
  assert.equal(attempts.filter(value => value === 'clip-failed').length, 1); assert.ok(attempts.includes('remove-listener'));
  assert.equal(frames, 0); assert.equal(playback.nodes.length, nodeCount);
  assert.ok(FakeWorklet.instances.every(node => node.port.onmessage === null && node.onprocessorerror === null));
  assert.ok(FakeContext.instances.every(context => context.onstatechange === null));
  const previous = attempts.length; assert.throws(() => engine.stopAll(), /audio_cleanup_unconfirmed/); assert.equal(attempts.length, previous);
});

test('single-source gap completes cleanup and notification when its worklet release throws', async t => {
  const selected = stream(); browser(t, async () => selected);
  const gaps = []; const engine = createAudioEngine({onFrame: () => false, onGap: (...args) => gaps.push(args)});
  await engine.startCapture('remote_mix');
  const node = FakeWorklet.instances[0], handler = node.port.onmessage;
  node.port.postMessage = () => { throw Error('synthetic port'); }; node.disconnect = () => { throw Error('synthetic disconnect'); };
  FakeContext.instances[0].currentTime = 0.06;
  assert.doesNotThrow(() => handler({data: {type: 'pcm', at: 0.06, pcm: new ArrayBuffer(640)}}));
  assert.deepEqual(gaps, [['remote_mix', 'capture_backpressure']]); assert.ok(selected.getTracks().every(track => track.stopped));
  assert.equal(FakeContext.instances[0].state, 'closed'); handler({data: {type: 'pcm', at: 0, pcm: new ArrayBuffer(640)}}); assert.equal(gaps.length, 1); assert.throws(() => engine.stopAll(), /audio_cleanup_unconfirmed/);
});

test('late permission cleanup attempts later tracks even when the first track throws', async t => {
  let grant; browser(t, () => new Promise(resolve => { grant = resolve; }));
  const engine = createAudioEngine({onFrame: () => true, onGap() {}}), pending = engine.startCapture('remote_mix');
  engine.stopAll(); const selected = stream(); selected.getAudioTracks()[0].stop = () => { throw Error('synthetic track'); }; grant(selected);
  await assert.rejects(pending, /audio_cleanup_unconfirmed/); assert.ok(selected.getVideoTracks()[0].stopped); assert.equal(FakeContext.instances.length, 0);
});

test('suspension mutes every private lane before callbacks and retires the clip despite a throwing observer', async t => {
  browser(t); const faults = [];
  const engine = createAudioEngine({onFrame: () => true, onGap() {}, onPlaybackFault: kind => {
    faults.push(kind); for (const lane of ['translation', 'live', 'live_interpretation']) engine.playPrivate(lane, new Uint8Array(640), 16000); throw Error('synthetic observer');
  }});
  await engine.resumePlayback(); for (const lane of ['translation', 'live', 'live_interpretation']) { engine.setPrivateMuted(lane, false); engine.playPrivate(lane, new Uint8Array(640), 16000); }
  const playing = engine.playClip(wav(2), {started() {}, ended: () => assert.fail('not completed'), failed() {}}), rejected = assert.rejects(playing, /clip_stopped/);
  const context = FakeContext.instances[0], nodeCount = context.nodes.length; context.state = 'suspended';
  assert.doesNotThrow(() => context.onstatechange()); await rejected;
  assert.deepEqual(faults, ['translation', 'live', 'live_interpretation', 'clip']); assert.equal(context.nodes.length, nodeCount); assert.ok(context.nodes.every(node => node.stopped)); assert.throws(() => engine.stopAll(), /audio_cleanup_unconfirmed/);
});

test('interpreter, assistant and dedicated translation queues are isolated and never replay muted input', async t => {
  browser(t); const engine = createAudioEngine({onFrame: () => true, onGap() {}});
  await engine.resumePlayback(); const context = FakeContext.instances[0];
  for (const lane of ['translation', 'live', 'live_interpretation']) engine.playPrivate(lane, new Uint8Array(640), 16000);
  assert.equal(context.nodes.length, 0);
  engine.setPrivateMuted('live_interpretation', false); assert.equal(context.nodes.length, 0);
  for (const lane of ['translation', 'live', 'live_interpretation']) engine.playPrivate(lane, new Uint8Array(640), 16000);
  assert.equal(context.nodes.length, 1);
  engine.setPrivateMuted('live', false); engine.playPrivate('live', new Uint8Array(640), 16000);
  engine.setPrivateMuted('live_interpretation', true);
  assert.ok(context.nodes[0].stopped); assert.equal(context.nodes[1].stopped, false);
  engine.playPrivate('live_interpretation', new Uint8Array(640), 16000); assert.equal(context.nodes.length, 2);
  engine.setPrivateMuted('live_interpretation', false); assert.equal(context.nodes.length, 2);
  engine.stopAll(); assert.ok(context.nodes.every(node => node.stopped));
});

function emitCapture(time, value = 0, at = time) {
  const context = FakeContext.instances[0], node = FakeWorklet.instances[0];
  context.currentTime = time;
  const pcm = new Int16Array(320).fill(value);
  node.port.onmessage({data: {type: 'pcm', at, pcm: pcm.buffer}});
  return pcm;
}
async function microtasks() { for (let index = 0; index < 8; index++) await Promise.resolve(); }

test('retirement stops immediately and waits for both capture and playback context closure', async t => {
  const selected = stream(); browser(t, async () => selected);
  const engine = createAudioEngine({onFrame: () => assert.fail('retired capture never sends'), onGap() {}});
  await engine.startCapture('remote_mix'); await engine.resumePlayback();
  engine.setPrivateMuted('live_interpretation', false);
  engine.playPrivate('live_interpretation', new Uint8Array(640), 16000);
  const [capture, playback] = FakeContext.instances;
  let captureClosed, playbackClosed;
  capture.close = () => new Promise(resolve => {captureClosed = () => {capture.state = 'closed'; resolve();};});
  playback.close = () => new Promise(resolve => {playbackClosed = () => {playback.state = 'closed'; resolve();};});
  let result;
  const retiring = engine.retire().then(value => {result = value; return value;});
  assert.ok(selected.getTracks().every(track => track.stopped > 0));
  assert.ok(playback.nodes.every(node => node.stopped));
  assert.equal(FakeWorklet.instances[0].port.onmessage, null);
  await microtasks(); assert.equal(result, undefined);
  captureClosed(); await microtasks(); assert.equal(result, undefined);
  playbackClosed(); assert.equal(await retiring, true);
  assert.equal(await engine.retire(), true);
  await assert.rejects(engine.startCapture('self'), /capture_closed/);
  assert.equal(FakeContext.instances.length, 2);
});

for (const kind of ['capture', 'playback']) {
  for (const failure of ['rejected', 'timeout']) {
    test(`${kind} ${failure} close refuses retirement and never exposes browser error text`, async t => {
      t.mock.timers.enable({apis: ['setTimeout']});
      const selected = stream(); browser(t, async () => selected);
      const gaps = [];
      const engine = createAudioEngine({onFrame: () => assert.fail('no frames'), onGap: (...args) => gaps.push(args)});
      if (kind === 'capture') await engine.startCapture('remote_mix'); else await engine.resumePlayback();
      const context = FakeContext.instances[0];
      context.close = () => failure === 'rejected' ? Promise.reject(new Error('private-device-details')) : new Promise(() => {});
      let result;
      const retiring = engine.retire().then(value => {result = value; return value;});
      if (kind === 'capture') assert.ok(selected.getTracks().every(track => track.stopped > 0));
      await microtasks();
      if (failure === 'timeout') {
        assert.equal(result, undefined); t.mock.timers.tick(999); await microtasks(); assert.equal(result, undefined);
        t.mock.timers.tick(1);
      }
      assert.equal(await retiring, false);
      assert.equal(await engine.retire(), false);
      assert.ok(!JSON.stringify(gaps).includes('private-device-details'));
      await assert.rejects(engine.startCapture('self'), /capture_closed/);
      assert.equal(FakeContext.instances.length, 1);
    });
  }
}

test('retirement refuses an outstanding native picker and releases its later grant', async t => {
  let grant; browser(t, () => new Promise(resolve => {grant = resolve;}));
  const engine = createAudioEngine({onFrame: () => assert.fail('late picker never sends'), onGap() {}});
  const pending = engine.prepareCapture('remote_mix'), rejected = assert.rejects(pending, /capture_closed/);
  assert.equal(await engine.retire(), false); await rejected;
  const selected = stream(); grant(selected); await microtasks();
  assert.ok(selected.getTracks().every(track => track.stopped > 0));
  assert.equal(FakeContext.instances.length, 0);
  assert.equal(await engine.retire(), true);
  await assert.rejects(engine.prepareCapture('remote_mix'), /capture_closed/);
});

test('local preparation meters actual PCM promptly without forwarding or retaining preview sound', async t => {
  let selections = 0; browser(t, async () => { selections++; return stream(); });
  const sent = [], statuses = [];
  const engine = createAudioEngine({onFrame: (_source, frame) => { sent.push(frame); return true; }, onGap() {}, onCaptureStatus: (_source, value) => statuses.push(value)});
  await engine.prepareCapture('remote_mix');
  assert.equal(selections, 1); assert.equal(sent.length, 0);
  emitCapture(0.02); const silent = statuses.at(-1);
  assert.deepEqual(silent, {level: 0, frames: 1, muted: false});
  const pcm = emitCapture(0.04, 8192);
  assert.deepEqual(statuses.at(-1), {level: 1, frames: 2, muted: false});
  assert.ok(pcm.every(sample => sample === 0)); assert.equal(sent.length, 0);
  const count = statuses.length; emitCapture(0.06, 100); emitCapture(0.08, 100);
  assert.equal(statuses.length, count); assert.equal(sent.length, 0);
  assert.equal(FakeWorklet.instances[0].port.posted.filter(value => value === 'ack').length, 4);
  engine.stopAll();
});

test('activation uses the selected stream once and emits sequence zero only after the future sample boundary', async t => {
  let selections = 0; browser(t, async () => { selections++; return stream(); });
  const sent = []; const engine = createAudioEngine({onFrame: (_source, frame) => { sent.push(frame); return true; }, onGap() {}});
  await engine.prepareCapture('remote_mix');
  emitCapture(0.02, 500); emitCapture(0.04, 500); emitCapture(0.1, 500);
  engine.activateCapture('remote_mix');
  emitCapture(0.12, 900, 0.1); emitCapture(0.13, 900);
  assert.equal(sent.length, 0);
  const pcm = emitCapture(0.16, 1234); emitCapture(0.18, -1234);
  assert.equal(selections, 1); assert.equal(sent.length, 2);
  assert.deepEqual(sent.map(frame => new DataView(frame).getUint32(2, true)), [0, 1]);
  assert.equal(new DataView(sent[0]).getInt16(6, true), 1234); assert.ok(pcm.every(sample => sample === 0));
  assert.throws(() => engine.activateCapture('remote_mix'), /capture_not_prepared/);
  await engine.cancelPreparedCapture('remote_mix');
  emitCapture(0.2, 50); assert.equal(sent.length, 3); // Preview cancel cannot stop an activated source.
  engine.stopAll(); await assert.rejects(engine.prepareCapture('remote_mix'), /capture_closed/);
});

test('clean rejected selection can be retried locally without an irreversible source gap', async t => {
  let selections = 0; const invalid = stream('browser', false), valid = stream();
  browser(t, async () => ++selections === 1 ? invalid : valid);
  const gaps = []; const engine = createAudioEngine({onFrame: () => assert.fail('preview never sends'), onGap: (...args) => gaps.push(args)});
  await assert.rejects(engine.prepareCapture('remote_mix'), /tab_audio_missing/);
  assert.ok(invalid.getTracks().every(track => track.stopped)); assert.deepEqual(gaps, []);
  await engine.prepareCapture('remote_mix'); emitCapture(0.02, 400);
  assert.equal(selections, 2); assert.deepEqual(gaps, []); engine.stopAll();
});

test('pending picker cancellation prevents reselection until the late result is released', async t => {
  let grant; browser(t, () => new Promise(resolve => { grant = resolve; }));
  const engine = createAudioEngine({onFrame: () => assert.fail('never emits'), onGap() {}});
  const pending = engine.prepareCapture('remote_mix'), rejected = assert.rejects(pending, /capture_closed/);
  await assert.rejects(engine.cancelPreparedCapture('remote_mix'), /capture_selection_pending/); await rejected;
  await assert.rejects(engine.prepareCapture('remote_mix'), /capture_closed/);
  const late = stream(); grant(late); await microtasks();
  assert.ok(late.getTracks().every(track => track.stopped)); assert.equal(FakeContext.instances.length, 0);
  navigator.mediaDevices.getDisplayMedia = async () => stream();
  await engine.prepareCapture('remote_mix'); engine.stopAll();
});

test('30-second chooser deadline fences late grants and never starts an audio context', async t => {
  t.mock.timers.enable({apis: ['setTimeout']});
  let grant; browser(t, () => new Promise(resolve => { grant = resolve; }));
  const engine = createAudioEngine({onFrame: () => assert.fail('never emits'), onGap() {}});
  const pending = engine.prepareCapture('remote_mix'), rejected = assert.rejects(pending, /capture_selection_timeout/);
  t.mock.timers.tick(30_000); await rejected;
  const late = stream(); grant(late); await microtasks();
  assert.ok(late.getTracks().every(track => track.stopped)); assert.equal(FakeContext.instances.length, 0); engine.stopAll();
});

test('10-second worklet setup deadline releases tracks and prevents late setup resurrection', async t => {
  t.mock.timers.enable({apis: ['setTimeout']}); const selected = stream(); browser(t, async () => selected);
  let moduleReady;
  const original = FakeContext.prototype.resume;
  FakeContext.prototype.resume = function () { return new Promise(resolve => { moduleReady = () => { this.state = 'running'; resolve(); }; }); };
  t.after(() => { FakeContext.prototype.resume = original; });
  const engine = createAudioEngine({onFrame: () => assert.fail('never emits'), onGap() {}});
  const pending = engine.prepareCapture('remote_mix'), rejected = assert.rejects(pending, /capture_setup_timeout/);
  await microtasks(); t.mock.timers.tick(10_000); await rejected;
  moduleReady(); await microtasks();
  assert.ok(selected.getTracks().every(track => track.stopped)); assert.equal(FakeWorklet.instances.length, 0);
  assert.throws(() => engine.activateCapture('remote_mix'), /capture_not_prepared/); engine.stopAll();
});

test('no PCM after setup gets a distinct first-frame diagnostic and releases a local check', async t => {
  t.mock.timers.enable({apis: ['setTimeout']}); const selected = stream(); browser(t, async () => selected);
  const gaps = []; const engine = createAudioEngine({onFrame: () => assert.fail('never emits'), onGap: (...args) => gaps.push(args)});
  await engine.prepareCapture('remote_mix'); t.mock.timers.tick(3000); await microtasks();
  assert.deepEqual(gaps, [['remote_mix', 'capture_no_frames']]);
  assert.ok(selected.getTracks().every(track => track.stopped)); assert.equal(FakeContext.instances[0].state, 'closed'); engine.stopAll();
});

test('120-second local preview expires even while valid silent PCM continues', async t => {
  t.mock.timers.enable({apis: ['setTimeout']}); browser(t);
  const gaps = []; const engine = createAudioEngine({onFrame: () => assert.fail('never emits'), onGap: (...args) => gaps.push(args)});
  await engine.prepareCapture('remote_mix');
  for (let second = 1; second < 120; second++) { t.mock.timers.tick(1000); emitCapture(second); }
  t.mock.timers.tick(1000); await microtasks();
  assert.deepEqual(gaps, [['remote_mix', 'capture_preview_timeout']]); engine.stopAll();
});

test('live track mute and unmute are immediate local status, while ended video is rejected', async t => {
  const selected = stream(); browser(t, async () => selected); const statuses = [];
  const engine = createAudioEngine({onFrame: () => true, onGap() {}, onCaptureStatus: (_source, value) => statuses.push(value)});
  await engine.prepareCapture('remote_mix'); emitCapture(0.02, 600); const audio = selected.getAudioTracks()[0];
  audio.muted = true; audio.dispatchEvent(new Event('mute')); assert.equal(statuses.at(-1).muted, true);
  audio.muted = false; audio.dispatchEvent(new Event('unmute')); assert.equal(statuses.at(-1).muted, false); assert.equal(statuses.at(-1).level, 0);
  await engine.cancelPreparedCapture('remote_mix');
  const ended = stream(); ended.getVideoTracks()[0].readyState = 'ended'; navigator.mediaDevices.getDisplayMedia = async () => ended;
  await assert.rejects(engine.prepareCapture('remote_mix'), /select_browser_tab/);
  assert.ok(ended.getTracks().every(track => track.stopped)); engine.stopAll();
});

test('pending context close blocks reselection and rejected cleanup stays closed', async t => {
  browser(t); const gaps = [];
  const engine = createAudioEngine({onFrame: () => true, onGap: (...args) => gaps.push(args)});
  await engine.prepareCapture('remote_mix');
  let rejectClose; FakeContext.instances[0].close = () => new Promise((_resolve, reject) => { rejectClose = reject; });
  const cancellation = engine.cancelPreparedCapture('remote_mix'), rejected = assert.rejects(cancellation, /audio_cleanup_unconfirmed/);
  await assert.rejects(engine.prepareCapture('remote_mix'), /capture_closed/);
  rejectClose(new Error('private browser details')); await rejected;
  assert.deepEqual(gaps, [['remote_mix', 'audio_cleanup_unconfirmed']]);
  await assert.rejects(engine.prepareCapture('remote_mix'), /capture_closed/);
  assert.throws(() => engine.stopAll(), /audio_cleanup_unconfirmed/);
});

test('Stop during pending setup closes resources and no late PCM or status revives the preview', async t => {
  const selected = stream(); browser(t, async () => selected); const statuses = [];
  let resume; const original = FakeContext.prototype.resume;
  FakeContext.prototype.resume = () => new Promise(resolve => { resume = resolve; });
  t.after(() => { FakeContext.prototype.resume = original; });
  const engine = createAudioEngine({onFrame: () => assert.fail('never emits'), onGap() {}, onCaptureStatus: (_source, value) => statuses.push(value)});
  const pending = engine.prepareCapture('remote_mix'), rejected = assert.rejects(pending, /capture_closed/);
  await microtasks(); engine.stopAll(); await rejected; const count = statuses.length;
  resume(); await microtasks();
  assert.ok(selected.getTracks().every(track => track.stopped)); assert.equal(FakeWorklet.instances.length, 0); assert.equal(statuses.length, count);
});

test('reentrant Stop from initial capture status cannot open a permission prompt', async t => {
  let selections = 0; browser(t, async () => { selections++; return stream(); });
  let engine; engine = createAudioEngine({onFrame: () => true, onGap() {}, onCaptureStatus: () => engine.stopAll()});
  await assert.rejects(engine.prepareCapture('remote_mix'), /capture_closed/);
  assert.equal(selections, 0); assert.equal(FakeContext.instances.length, 0);
});

test('reentrant Stop from a meter callback clears the PCM and emits no frame', async t => {
  browser(t); let engine, shouldStop = false, frames = 0;
  engine = createAudioEngine({onFrame: () => { frames++; return true; }, onGap() {}, onCaptureStatus: () => { if (shouldStop) engine.stopAll(); }});
  await engine.prepareCapture('remote_mix'); engine.activateCapture('remote_mix'); shouldStop = true;
  const pcm = emitCapture(0.06, 500);
  assert.equal(frames, 0); assert.ok(pcm.every(sample => sample === 0)); assert.equal(FakeContext.instances[0].state, 'closed');
});

test('failed cleanup of a cancelled chooser late grant is reported and cannot permit a new selection', async t => {
  let grant; browser(t, () => new Promise(resolve => { grant = resolve; })); const gaps = [];
  const engine = createAudioEngine({onFrame: () => true, onGap: (...args) => gaps.push(args)});
  const pending = engine.prepareCapture('remote_mix'), rejected = assert.rejects(pending, /capture_closed/);
  await assert.rejects(engine.cancelPreparedCapture('remote_mix'), /capture_selection_pending/); await rejected;
  const selected = stream(); selected.getAudioTracks()[0].stop = () => { throw Error('private browser detail'); };
  grant(selected); await microtasks();
  assert.ok(selected.getVideoTracks()[0].stopped); assert.deepEqual(gaps, [['remote_mix', 'audio_cleanup_unconfirmed']]);
  await assert.rejects(engine.prepareCapture('remote_mix'), /capture_closed/); assert.throws(() => engine.stopAll(), /audio_cleanup_unconfirmed/);
});

test('release acknowledgement waits for context shutdown before the next tab can be selected', async t => {
  const first = stream(); let selections = 0;
  browser(t, async () => ++selections === 1 ? first : stream());
  const engine = createAudioEngine({onFrame: () => true, onGap() {}});
  await engine.prepareCapture('remote_mix');
  const context = FakeContext.instances[0]; let close;
  context.close = () => new Promise(resolve => { close = () => { context.state = 'closed'; resolve(); }; });
  let acknowledged = false;
  const released = engine.cancelPreparedCapture('remote_mix').then(() => { acknowledged = true; });
  assert.ok(first.getTracks().every(track => track.stopped)); // Release is synchronous before awaiting context.
  await microtasks(); assert.equal(acknowledged, false);
  await assert.rejects(engine.prepareCapture('remote_mix'), /capture_closed/); assert.equal(selections, 1);
  close(); await released; assert.equal(acknowledged, true);
  await engine.prepareCapture('remote_mix'); assert.equal(selections, 2); engine.stopAll();
});

test('private playback uses one startup cushion and preserves sample-contiguous scheduling through bounded arrival jitter', async t => {
  browser(t);
  const faults = [], engine = createAudioEngine({onFrame: () => true, onGap() {}, onPlaybackFault: (...args) => faults.push(args)});
  await engine.resumePlayback(); engine.setPrivateMuted('live_interpretation', false);
  const context = FakeContext.instances[0], arrivals = [0, 0.028, 0.045, 0.077, 0.083, 0.106];
  const input = Int16Array.from({length: arrivals.length * 320}, (_, index) => Math.round(16000 * Math.sin(index * 2 * Math.PI * 997 / 16000)));
  for (const [index, at] of arrivals.entries()) {context.currentTime = at; const pcm = input.slice(index * 320, (index + 1) * 320); engine.playPrivate('live_interpretation', new Uint8Array(pcm.buffer), 16000);}
  assert.equal(context.nodes[0].startTime, 0.04);
  assert.ok(context.nodes.every(node => node.buffer.rate === 16000 && node.buffer.duration === 0.02));
  assert.deepEqual(context.nodes.flatMap(node => [...node.buffer.samples]), [...input].map(sample => sample / 32768), 'PCM waveform is preserved at the original rate');
  for (let index = 1; index < context.nodes.length; index++) assert.ok(Math.abs(context.nodes[index].startTime - (context.nodes[index - 1].startTime + 0.02)) < 1e-9, 'safe queued predecessor must not gain an artificial gap');
  assert.deepEqual(faults, []); engine.stopAll();
});
test('private playback restores its cushion after real underrun without rate changes, old replay or a lane fault', async t => {
  browser(t);
  const faults = [], engine = createAudioEngine({onFrame: () => true, onGap() {}, onPlaybackFault: (...args) => faults.push(args)});
  await engine.resumePlayback(); engine.setPrivateMuted('live_interpretation', false);
  const context = FakeContext.instances[0];
  engine.playPrivate('live_interpretation', new Uint8Array(960), 24000);
  context.currentTime = 2; context.nodes[0].onended();
  engine.playPrivate('live_interpretation', new Uint8Array(960), 24000);
  assert.equal(context.nodes.length, 2); assert.equal(context.nodes[1].startTime, 2.04);
  assert.equal(context.nodes[1].buffer.duration, 0.02); assert.deepEqual(faults, []);
  engine.stopAll(); assert.ok(context.nodes.every(node => node.stopped || node === context.nodes[0]));
});
test('private playback does not schedule too close to the audio rendering boundary', async t => {
  browser(t); const engine = createAudioEngine({onFrame: () => true, onGap() {}});
  await engine.resumePlayback(); engine.setPrivateMuted('translation', false); const context = FakeContext.instances[0];
  engine.playPrivate('translation', new Uint8Array(640), 16000);
  context.currentTime = 0.058; engine.playPrivate('translation', new Uint8Array(640), 16000);
  assert.equal(context.nodes[1].startTime, 0.098); engine.stopAll();
});
