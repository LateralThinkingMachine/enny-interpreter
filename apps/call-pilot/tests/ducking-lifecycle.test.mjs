import assert from 'node:assert/strict';
import test from 'node:test';
import {BrowserDuckingPlayback} from '../.test-build/audio/ducking.js';

function environment(t, options={}) {
  const contexts=[], renderers=[], old=Object.getOwnPropertyDescriptors(globalThis);
  class Parameter {
    value=1; values=[];
    setValueAtTime(value){this.value=value;this.values.push(value);}
    linearRampToValueAtTime(value){this.value=value;this.values.push(value);}
    cancelScheduledValues(){}
  }
  class Node {
    connections=[];disconnects=0;
    constructor(context){this.context=context;context.nodes.push(this);}
    connect(destination,output=0,input=0){this.connections.push({destination,output,input});}
    disconnect(){this.disconnects++;this.connections=[];if(this.breakDisconnect)throw new Error('native disconnect failure');}
  }
  class Context {
    state='suspended';currentTime=0;nodes=[];destination={};onstatechange=null;closed=0;
    audioWorklet={addModule:()=>options.modulePromise??Promise.resolve()};
    constructor(){contexts.push(this);}
    resume(){this.state='running';return Promise.resolve();}
    close(){this.closed++;this.state='closed';return options.closeFailure?Promise.reject(new Error('close failure')):Promise.resolve();}
    createMediaStreamSource(stream){const node=new Node(this);node.stream=stream;return node;}
    createGain(){const node=new Node(this);node.gain=new Parameter();return node;}
  }
  class Worklet extends Node {
    parameters=new Map([['translationEnabled',new Parameter()]]);onprocessorerror=null;
    port={onmessage:null,messages:[],closed:false,postMessage(data){this.messages.push(data);},close(){this.closed=true;}};
    constructor(context,name,settings){super(context);assert.equal(name,'private-audio-ducking');this.settings=settings;context.mixer=this;}
  }
  const document = {body: {append(renderer) {renderer.attached = true;}}, createElement(tag) {
    assert.equal(tag,'audio');
    const renderer = {muted:false,volume:1,controls:true,srcObject:null,pauses:0,removed:false,setAttribute(){},
      play() {assert.equal(this.muted,true);assert.equal(this.volume,0);assert.equal(this.attached,true);return options.playPromise ?? (options.playFailure ? Promise.reject(new Error('native play rejected')) : Promise.resolve());},
      pause() {this.pauses++;}, remove() {this.removed=true;this.attached=false;}};
    renderers.push(renderer); return renderer;
  }};
  for(const [key,value] of Object.entries({AudioContext:Context,AudioWorkletNode:Worklet,document}))Object.defineProperty(globalThis,key,{configurable:true,writable:true,value});
  t.after(()=>{for(const key of ['AudioContext','AudioWorkletNode','document'])if(old[key])Object.defineProperty(globalThis,key,old[key]);else delete globalThis[key];});
  function stream(id) {
    const track=new EventTarget();Object.assign(track,{id,readyState:'live',enabled:true,stops:0,stop(){this.stops++;this.readyState='ended';}});
    return {track,getAudioTracks:()=>[track]};
  }
  return {contexts,renderers,stream,Context};
}

test('one playback graph borrows unchanged tracks and honors prearmed context ownership',async t=>{
  const env=environment(t),original=env.stream('source'),translated=env.stream('speech'),context=new env.Context();await context.resume();
  const playback=await BrowserDuckingPlayback.create({original,translated,context});
  assert.equal(env.contexts.length,1);
  const destinations=context.nodes.flatMap(node=>node.connections).filter(connection=>connection.destination===context.destination);
  assert.equal(destinations.length,1,'mixer is the sole audible destination route');
  assert.equal(context.nodes.find(node=>node.stream===original).connections[0].input,0);
  assert.equal(context.nodes.find(node=>node.stream===translated).connections[0].destination.connections[0].input,1);
  await playback.dispose();
  assert.equal(context.closed,1);assert.equal(context.mixer.port.closed,true);assert.ok(context.nodes.every(node=>node.disconnects===1));
  assert.equal(original.track.stops,0);assert.equal(translated.track.stops,0);assert.equal(original.track.enabled,true);
  assert.equal(playback.getState().status,'disposed');assert.equal(await playback.dispose(),undefined);
  assert.equal(env.renderers.length,1);assert.equal(env.renderers[0].defaultMuted,true);assert.equal(env.renderers[0].hidden,true);
  assert.equal(env.renderers[0].srcObject,null);assert.equal(env.renderers[0].removed,true);assert.equal(env.renderers[0].pauses,1);
});

test('suspend fences generated speech synchronously and stale callbacks cannot revive it',async t=>{
  const env=environment(t),states=[],playback=await BrowserDuckingPlayback.create({original:env.stream('source'),translated:env.stream('speech'),onState:state=>states.push(state)});
  const context=env.contexts[0],callback=context.mixer.port.onmessage;
  assert.equal(playback.getState().translationEnabled,false,'construction cannot bypass session readiness');playback.resumeTranslation();
  callback({data:{type:'ducking',active:true}});assert.equal(playback.getState().ducking,true);
  playback.suspendTranslation();assert.equal(context.mixer.parameters.get('translationEnabled').value,0);
  callback({data:{type:'ducking',active:true}});assert.equal(playback.getState().ducking,false);
  playback.updateSettings({originalMuted:true});playback.resumeTranslation();
  callback({data:{type:'ducking',active:true}});assert.equal(playback.getState().ducking,false);
  playback.updateSettings({translatedVolume:0,originalMuted:false});
  callback({data:{type:'ducking',active:true}});assert.equal(playback.getState().ducking,false);
  await playback.dispose();const count=states.length;callback({data:{type:'ducking',active:true}});assert.equal(states.length,count);
});

test('original-only fence survives volume edits and replacement without mutating either borrowed track',async t=>{
  const env=environment(t),original=env.stream('original'),first=env.stream('first'),next=env.stream('next');
  const playback=await BrowserDuckingPlayback.create({original,translated:first});
  const context=env.contexts[0],gate=context.nodes.find(node=>node.gain),originalNode=context.nodes.find(node=>node.stream===original);
  playback.resumeTranslation();playback.suspendTranslation();
  assert.equal(gate.gain.value,0);assert.equal(context.mixer.parameters.get('translationEnabled').value,0);
  playback.updateSettings({originalVolume:.65,translatedVolume:.8,translatedMuted:false});
  assert.equal(gate.gain.value,0,'unmuting/adjusting volume cannot undo the owner playback fence');
  await playback.replaceTranslation(next);
  assert.equal(playback.getState().translationEnabled,false);assert.equal(playback.getState().ducking,false);
  assert.equal(gate.gain.value,0);assert.equal(originalNode.connections.length,1);
  for(const stream of [original,first,next]){assert.equal(stream.track.enabled,true);assert.equal(stream.track.stops,0);}
  assert.equal(env.renderers[0].srcObject,null);assert.equal(env.renderers[1].volume,0);assert.equal(env.renderers[1].muted,true);
  playback.resumeTranslation();assert.equal(gate.gain.value,1);assert.equal(playback.getState().settings.originalVolume,.65);
  await playback.dispose();
});

test('unsupported settings and same/ambiguous streams are rejected before playback resources open',async t=>{
  const env=environment(t),stream=env.stream('same');
  await assert.rejects(BrowserDuckingPlayback.create({original:stream,translated:stream}),/separate_audio_tracks_required/);
  await assert.rejects(BrowserDuckingPlayback.create({original:null,translated:{getAudioTracks:()=>[]}}),/separate_audio_tracks_required/);
  await assert.rejects(BrowserDuckingPlayback.create({original:null,translated:null}),/separate_audio_tracks_required/);
  assert.equal(env.contexts.length,0);
  const playback=await BrowserDuckingPlayback.create({original:null,translated:stream});
  assert.equal(playback.getState().selectiveDuckingAvailable,false);
  const prior=playback.getState().settings;assert.throws(()=>playback.updateSettings({version:'unknown'}),/invalid_ducking_settings/);
  assert.deepEqual(playback.getState().settings,prior);await playback.dispose();
});

test('original monitoring stays connected across speech negotiation and provider track replacement',async t=>{
  const env=environment(t),original=env.stream('source'),first=env.stream('first'),second=env.stream('second'),faults=[];
  const playback=await BrowserDuckingPlayback.create({original,translated:null,onFault:fault=>faults.push(fault)});
  const context=env.contexts[0],originalNode=context.nodes.find(node=>node.stream===original);
  assert.equal(playback.getState().status,'translation_suspended');assert.equal(originalNode.connections.length,1);
  playback.resumeTranslation();assert.equal(playback.getState().translationEnabled,false,'no speech track cannot be resumed');
  await playback.replaceTranslation(first);assert.equal(playback.getState().translationEnabled,false,'new track stays gated until owner readiness');
  playback.resumeTranslation();assert.equal(playback.getState().translationEnabled,true);
  playback.detachTranslation();first.track.dispatchEvent(new Event('ended'));
  assert.deepEqual(faults,[]);assert.equal(originalNode.disconnects,0);assert.equal(originalNode.connections.length,1);
  await playback.replaceTranslation(second);playback.resumeTranslation();
  assert.equal(env.contexts.length,1);assert.equal(originalNode.disconnects,0);assert.equal(context.mixer.disconnects,0);
  assert.equal(context.nodes.find(node=>node.stream===first).disconnects,1);
  assert.equal(first.track.stops,0);assert.equal(original.track.enabled,true);await playback.dispose();
});

test('render fault retires mixer before single original fallback and does not undo user mute',async t=>{
  const env=environment(t),faults=[],playback=await BrowserDuckingPlayback.create({original:env.stream('source'),translated:env.stream('speech'),onFault:fault=>faults.push(fault)});
  const context=env.contexts[0];playback.updateSettings({originalMuted:true});context.mixer.onprocessorerror();
  assert.equal(playback.getState().status,'failed');assert.equal(playback.getState().ducking,false);
  assert.equal(context.mixer.connections.length,0);const audible=context.nodes.filter(node=>node.connections.some(connection=>connection.destination===context.destination));
  assert.equal(audible.length,1);assert.equal(audible[0].gain.value,0);
  playback.resumeTranslation();assert.equal(context.mixer.parameters.get('translationEnabled').value,0);
  assert.deepEqual(faults,['playback_processor_failed']);await playback.dispose();
});

test('track end reports a fixed fault and graph failure still attempts every release',async t=>{
  const env=environment(t),translated=env.stream('speech'),faults=[];
  const playback=await BrowserDuckingPlayback.create({original:env.stream('source'),translated,onFault:fault=>faults.push(fault)});
  translated.track.dispatchEvent(new Event('ended'));assert.deepEqual(faults,['translated_stream_ended']);
  const context=env.contexts[0];context.nodes[0].breakDisconnect=true;
  await assert.rejects(playback.dispose(),/private_audio_cleanup_incomplete/);
  assert.ok(context.nodes.every(node=>node.disconnects===1));assert.equal(context.closed,1);assert.equal(context.mixer.port.closed,true);
});

test('abort while worklet loads closes context and late completion cannot create nodes',async t=>{
  let release;const modulePromise=new Promise(resolve=>{release=resolve;});
  const env=environment(t,{modulePromise}),abort=new AbortController();
  const pending=BrowserDuckingPlayback.create({original:null,translated:env.stream('speech'),signal:abort.signal});
  abort.abort();await assert.rejects(pending,/private_audio_cancelled/);release();
  assert.equal(env.contexts[0].closed,1);assert.equal(env.contexts[0].nodes.length,0);
});

test('renderer play rejection retires the silent helper and every audio resource',async t=>{
  const env=environment(t,{playFailure:true});
  await assert.rejects(BrowserDuckingPlayback.create({original:null,translated:env.stream('speech')}),/native play rejected/);
  assert.equal(env.renderers[0].srcObject,null);assert.equal(env.renderers[0].removed,true);
  assert.equal(env.contexts[0].closed,1);assert.ok(env.contexts[0].nodes.every(node=>node.disconnects>=1));
});

test('Stop during pending renderer play detaches borrowed media and late completion cannot resume it',async t=>{
  let resolve;const playPromise=new Promise(done=>{resolve=done;});
  const env=environment(t,{playPromise}),translated=env.stream('speech');
  const playback=await BrowserDuckingPlayback.create({original:env.stream('original'),translated:null});
  const attachment=playback.replaceTranslation(translated);
  assert.equal(playback.getState().rendererReady,false);playback.resumeTranslation();
  assert.equal(playback.getState().translationEnabled,false);
  await playback.dispose();await assert.rejects(attachment,/private_audio_renderer_retired/);
  resolve();await Promise.resolve();
  assert.equal(env.renderers[0].srcObject,null);assert.equal(env.renderers[0].removed,true);
  assert.equal(translated.track.stops,0);assert.equal(playback.getState().translationEnabled,false);assert.equal(playback.getState().status,'disposed');
});

test('render evidence contains only per-track booleans and ignores delayed predecessor observations',async t=>{
  const env=environment(t),playback=await BrowserDuckingPlayback.create({original:null,translated:env.stream('first')});
  const context=env.contexts[0],callback=context.mixer.port.onmessage;
  const first=context.mixer.port.messages.findLast(message=>message.type==='reset_signals').generation;
  callback({data:{type:'signal',generation:first,translated:true,rendered:true}});
  assert.equal(playback.getState().translatedSignalSeen,true);assert.equal(playback.getState().renderedSignalSeen,true);
  await playback.replaceTranslation(env.stream('second'));
  callback({data:{type:'signal',generation:first,translated:true,rendered:true}});
  assert.equal(playback.getState().translatedSignalSeen,false);assert.equal(playback.getState().renderedSignalSeen,false);
  assert.equal(playback.getState().rendererReady,true);await playback.dispose();
});

test('renderer retirement attempts every release when processor messages and listeners fail',async t=>{
  const env=environment(t),translated=env.stream('speech');
  const playback=await BrowserDuckingPlayback.create({original:null,translated});
  const context=env.contexts[0],renderer=env.renderers[0],source=context.nodes.find(node=>node.stream===translated);
  context.mixer.port.postMessage=()=>{throw new Error('native message failure');};
  translated.track.removeEventListener=()=>{throw new Error('native listener removal failure');};
  assert.throws(()=>playback.detachTranslation(),/private_audio_renderer_cleanup_incomplete/);
  assert.equal(renderer.pauses,1);assert.equal(renderer.srcObject,null);assert.equal(renderer.removed,true);assert.equal(source.disconnects,1);
  assert.equal(playback.getState().translationEnabled,false);assert.equal(playback.getState().rendererReady,false);
  await assert.rejects(playback.dispose(),/private_audio_cleanup_incomplete/);assert.equal(context.closed,1);
});
