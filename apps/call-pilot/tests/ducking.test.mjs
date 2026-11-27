import assert from 'node:assert/strict';
import test from 'node:test';
import {DuckingMixer, DEFAULT_DUCKING_SETTINGS, DUCKING_PROFILE, validateDuckingSettings} from '../src/audio/ducking-worklet.js';

const RATE = 48000;
const defaults = patch => ({...DEFAULT_DUCKING_SETTINGS, ...patch});
function quantum(mixer, original = 0.5, translated = 0, enabled = true, length = 128) {
  const source = new Float32Array(length).fill(original), speech = new Float32Array(length).fill(translated);
  const output = new Float32Array(length);
  mixer.render([source], [speech], [output], enabled);
  return output;
}
function run(mixer, ms, original = 0.5, translated = 0, enabled = true) {
  let remaining = Math.ceil(ms * RATE / 1000), result;
  while (remaining) {const size = Math.min(128, remaining); result = quantum(mixer, original, translated, enabled, size); remaining -= size;}
  return result;
}
const near = (value, expected, tolerance = 1e-6) => assert.ok(Math.abs(value - expected) < tolerance, `${value} != ${expected}`);

test('frozen inspectable profile and strict bounded settings reject incompatible policy', () => {
  assert.deepEqual(DUCKING_PROFILE, {version:'private-ducking.v1', attackMs:25, releaseMs:180, holdMs:180, volumeRampMs:20, thresholdOn:0.008, thresholdOff:0.004, duckGain:0.2, headroom:0.5});
  assert.ok(Object.isFrozen(DUCKING_PROFILE));
  assert.ok(Object.isFrozen(validateDuckingSettings(DEFAULT_DUCKING_SETTINGS)));
  for (const patch of [{version:'v2'}, {originalVolume:-1}, {translatedVolume:1.01}, {translatedVolume:NaN}, {enabled:1}, {originalMuted:null}, {extra:true}]) assert.throws(() => validateDuckingSettings(defaults(patch)), /invalid_ducking_settings/);
  for (const rate of [0, 7999, 192001, Infinity]) assert.throws(() => new DuckingMixer(rate), /invalid_ducking_sample_rate/);
});

test('playout activity drives sample-continuous attenuation within the frozen attack; source is untouched', () => {
  const mixer = new DuckingMixer(RATE); run(mixer, 200);
  const source = new Float32Array(128).fill(.4), copy = source.slice(), speech = new Float32Array(128).fill(.2), originalSpeech = speech.slice(), output = new Float32Array(128);
  assert.equal(mixer.render([source], [speech], [output]), true);
  assert.equal(mixer.ducking, true);
  assert.deepEqual(source, copy); assert.deepEqual(speech, originalSpeech);
  const settled = run(mixer, DUCKING_PROFILE.attackMs, .4, .2);
  near(settled.at(-1), (.4 * .2 + .2) * .5);
  assert.equal(mixer.render([source], [speech], [output]), false, 'no activity IPC on every quantum');
});

test('silence hysteresis avoids pumping, then original selected gain returns after bounded hold/release', () => {
  const mixer = new DuckingMixer(RATE, defaults({originalVolume:.7})); run(mixer, 200, .5, .1);
  run(mixer, 100, .5, .006); assert.equal(mixer.ducking, true, 'between thresholds sustains activity');
  run(mixer, 170, .5, 0); assert.equal(mixer.ducking, true);
  run(mixer, 12, .5, 0); assert.equal(mixer.ducking, false);
  const restored = run(mixer, 180, .5, 0); near(restored.at(-1), .5 * .7 * .5);
  const silent = new DuckingMixer(RATE); run(silent, 500, .5, .006); assert.equal(silent.ducking, false, 'below onset does not spuriously duck');
});

test('provider silence stops translated samples immediately while ducking hold restores only original gain', () => {
  const mixer = new DuckingMixer(RATE); run(mixer, 200, .5, .25);
  assert.equal(mixer.active, true); assert.equal(mixer.ducking, true);
  const firstSilentQuantum = quantum(mixer, .5, 0);
  assert.equal(mixer.active, true, 'activity hysteresis has not yet elapsed');
  assert.equal(mixer.ducking, true);
  for (const sample of firstSilentQuantum) near(sample, .5 * DUCKING_PROFILE.duckGain * DUCKING_PROFILE.headroom);
  const emptyInput = new Float32Array(128);
  mixer.render([new Float32Array(128).fill(.5)], [], [emptyInput], true);
  for (const sample of emptyInput) near(sample, .5 * DUCKING_PROFILE.duckGain * DUCKING_PROFILE.headroom);
  assert.ok(quantum(mixer, 0, 0).every(sample => sample === 0), 'no generated tail or replay survives into a silent quantum');
  near(run(mixer, DUCKING_PROFILE.holdMs + DUCKING_PROFILE.releaseMs, .5, 0).at(-1), .25);
});

test('mute, zero translation volume, off and suspend restore original without suppressing source', () => {
  for (const patch of [{translatedMuted:true}, {translatedVolume:0}, {enabled:false}]) {
    const mixer = new DuckingMixer(RATE); run(mixer, 200, .5, .1);
    mixer.updateSettings(defaults(patch)); const output = run(mixer, 185, .5, .1);
    assert.equal(mixer.ducking, false); near(output.at(-1), patch.enabled === false ? .3 : .25);
  }
  const mixer = new DuckingMixer(RATE); run(mixer, 200, .5, .1);
  const stopped = quantum(mixer, .5, 1, false);
  assert.equal(mixer.ducking, false); assert.ok(stopped.every(value => value < .08), 'generated speech gated on first stopped frame');
  near(run(mixer, 185, .5, 1, false).at(-1), .25);
  mixer.updateSettings(defaults({originalMuted:true}));
  assert.ok(quantum(mixer, .5, 1, false).every(value => value === 0), 'restore never unmutes original');
  run(mixer, 1000, .5, 1, false); assert.equal(mixer.ducking, false);
});

test('original-only consumes live translated frames silently and explicit On never replays muted speech', () => {
  const mixer = new DuckingMixer(RATE, defaults({originalVolume: .7}));
  run(mixer, 200, .5, .2);
  assert.equal(mixer.ducking, true);
  for (let block = 0; block < 250; block++) {
    const source = new Float32Array(128).fill(.5), copy = source.slice();
    const translated = new Float32Array(128).fill(block % 2 ? -.9 : .9), output = new Float32Array(128);
    mixer.render([source], [translated], [output], false);
    assert.equal(mixer.ducking, false); assert.deepEqual(source, copy);
    assert.ok(output.every(sample => sample >= 0 && sample <= .175 + 1e-6), 'neither sign of ignored speech reaches output');
  }
  const resumedSilence = quantum(mixer, .5, 0, true);
  for (const sample of resumedSilence) near(sample, .175, 1e-6);
  assert.equal(mixer.ducking, false, 'turning On does not resurrect prior activity');
  near(run(mixer, 30, .5, .4, true).at(-1), (.5 * .7 * .2 + .4) * .5);
  mixer.updateSettings(defaults({originalVolume: .7, originalMuted: true}));
  assert.ok(quantum(mixer, .5, .9, false).every(value => value === 0), 'original-only retains an explicit original mute');
});

test('sample ramps do not click and stereo/mono mixing cannot clip or duplicate either branch', () => {
  const mixer = new DuckingMixer(RATE, defaults({enabled:false}));
  let previous = 0, maxStep = 0;
  for (let q = 0; q < 100; q++) for (const value of quantum(mixer, 1, 1)) {maxStep = Math.max(maxStep, Math.abs(value - previous)); previous = value;}
  near(previous, 1); assert.ok(maxStep <= 1 / (RATE * DUCKING_PROFILE.volumeRampMs / 1000) + 1e-7, `maximum gain increment ${maxStep}`);
  const left = new Float32Array(128).fill(.6), right = new Float32Array(128).fill(-.4), speech = new Float32Array(128).fill(.2);
  const output = [new Float32Array(128), new Float32Array(128)];
  mixer.render([left,right], [speech], output);
  near(output[0][127], .4); near(output[1][127], -.1);
  mixer.render([new Float32Array(128).fill(NaN)], [new Float32Array(128).fill(Infinity)], output);
  assert.ok(output.every(channel => channel.every(sample => sample === 0)));
});

test('worklet Stop and invalid settings fence rendering; no PCM is sent to main thread', async t => {
  const names = ['AudioWorkletProcessor','sampleRate','registerProcessor'], old = Object.getOwnPropertyDescriptors(globalThis); let Processor;
  for (const [name,value] of Object.entries({sampleRate: RATE, registerProcessor: (name,type) => {assert.equal(name,'private-audio-ducking'); Processor=type;}, AudioWorkletProcessor:class {port={messages:[],onmessage:null,postMessage(data){this.messages.push(data);}};} })) Object.defineProperty(globalThis,name,{configurable:true,writable:true,value});
  t.after(() => {for(const name of names) if(old[name]) Object.defineProperty(globalThis,name,old[name]); else delete globalThis[name];});
  await import('../src/audio/ducking-worklet.js?worklet-test');
  const processor = new Processor({processorOptions:{settings:DEFAULT_DUCKING_SETTINGS}});
  const input = new Float32Array(128).fill(.3), output = new Float32Array(128);
  for(let i=0;i<100;i++) assert.equal(processor.process([[input],[input]], [[output]], {translationEnabled:[1]}),true);
  assert.equal(processor.port.messages.length,2); assert.deepEqual(Object.keys(processor.port.messages[0]),['type','active','frame']);
  assert.deepEqual(processor.port.messages[1],{type:'signal',generation:0,translated:true,rendered:true});
  processor.port.onmessage({data:{type:'reset_signals',generation:1}});
  processor.process([[input],[input]],[[output]],{translationEnabled:[0]});
  assert.equal(processor.mixer.translatedSignalSeen,false);assert.equal(processor.mixer.renderedSignalSeen,false);
  processor.process([[input],[input]],[[output]],{translationEnabled:[1]});
  assert.deepEqual(processor.port.messages.at(-1),{type:'signal',generation:1,translated:true,rendered:true});
  processor.port.onmessage({data:{type:'stop'}});
  assert.equal(processor.process([[input],[input]],[[output]],{translationEnabled:[1]}),false); assert.ok(output.every(value=>value===0));
  const invalid = new Processor({processorOptions:{settings:DEFAULT_DUCKING_SETTINGS}});
  invalid.port.onmessage({data:{type:'settings',settings:{...DEFAULT_DUCKING_SETTINGS,extra:true}}});
  assert.deepEqual(invalid.port.messages,[{type:'invalid_settings'}]);
  assert.equal(invalid.process([[input],[input]],[[output]],{translationEnabled:[1]}),false);
});
