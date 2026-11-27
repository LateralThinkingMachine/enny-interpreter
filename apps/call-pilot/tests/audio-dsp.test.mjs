import assert from 'node:assert/strict';
import test from 'node:test';
import {PCM16Resampler} from '../src/audio/capture-worklet.js';

function convert(rate, input, chunks = [128]) {
  const output = [];
  const engine = new PCM16Resampler(rate, frame => output.push(...frame));
  let offset = 0, index = 0;
  while (offset < input.length) {const end = Math.min(input.length, offset + chunks[index++ % chunks.length]); engine.push(input.subarray(offset,end)); offset = end;}
  return output;
}
function tone(rate, hz, seconds = 1, amplitude = 0.7) {return Float32Array.from({length: Math.floor(rate * seconds)}, (_, i) => amplitude * Math.sin(i * 2 * Math.PI * hz / rate));}
function rms(values) {let energy=0;for(const sample of values)energy+=(sample/32768)**2;return Math.sqrt(energy/values.length);}
const RATES=[16000,22050,32000,44100,48000,88200,96000];

test('speech passband stays flat from300Hz through6kHz at every common device rate', () => {
  for(const rate of RATES)for(let hz=300;hz<=6000;hz+=300){
    const values=convert(rate,tone(rate,hz)).slice(320), gain=20*Math.log10(rms(values)/(0.7/Math.sqrt(2)));
    assert.ok(Math.abs(gain)<0.05, `${rate}Hz input, ${hz}Hz tone gain ${gain}dB`);
  }
});
test('phase-aware anti-alias filter rejects near-Nyquist and higher energy by at least65dB', () => {
  for(const rate of RATES.filter(rate=>rate!==16000))for(const hz of [8000,8050,8100,8250,8500,9000,10000,12000,18000]){
    if(hz>=rate/2)continue;
    const ratio=rms(convert(rate,tone(rate,hz)).slice(320))/(0.7/Math.sqrt(2));
    assert.ok(ratio<10**(-65/20),`${rate}Hz input, ${hz}Hz rejected by ${-20*Math.log10(ratio)}dB`);
  }
});
test('filter transition is monotonic from6kHz to8kHz and keeps the7kHz cutoff explicit',()=>{
  for(const rate of [22050,44100,48000,96000]){
    const gains=[6000,6250,6500,6750,7000,7250,7500,7750,8000].map(hz=>rms(convert(rate,tone(rate,hz)).slice(320))/(.7/Math.sqrt(2)));
    for(let i=1;i<gains.length;i++)assert.ok(gains[i]<=gains[i-1]+0.0002,`${rate}Hz transition ${i}`);
    assert.ok(Math.abs(20*Math.log10(gains[4])+6.02)<0.02);
  }
});
test('arbitrary input partitions and long rational-rate phase accumulation preserve every output sample',()=>{
  for(const rate of [...RATES,16001,47999,95999]){
    const input=tone(rate,1733,2.04);
    const output=convert(rate,input);
    assert.equal(output.length,Math.floor(Math.floor(input.length*16000/rate)/320)*320);
    assert.deepEqual(output,convert(rate,input,[1,7,173,511,23,4096]));
  }
});
test('fractional-phase FIR follows analytic sine timing without linear-interpolation passband loss',()=>{
  for(const rate of [22050,44100,47999,95999])for(const hz of [300,4000,6000]){
    const half=Math.ceil(rate*64/48000), output=convert(rate,tone(rate,hz));let squared=0;
    for(let k=320;k<output.length;k++){
      const time=(k+1)/16000-(half+1)/rate;
      const expected=.7*Math.sin(2*Math.PI*hz*time);
      squared+=(output[k]/32768-expected)**2;
    }
    assert.ok(Math.sqrt(squared/(output.length-320))<0.0004,`${rate}Hz/${hz}Hz phase or amplitude error`);
  }
});
test('impulse delay remains approximately1.33ms and DC/saturation remain bounded',()=>{
  for(const rate of RATES){
    const input=new Float32Array(rate);input[1000]=.5;const output=convert(rate,input);
    let peak=0;for(let i=1;i<output.length;i++)if(Math.abs(output[i])>Math.abs(output[peak]))peak=i;
    const half=rate===16000?0:Math.ceil(rate*64/48000), expected=(1000+half+1)*16000/rate-1;
    assert.ok(Math.abs(peak-expected)<=1,`${rate}Hz impulse ${peak} vs ${expected}`);
    for(const level of [-2,-.25,0,.25,2]){
      const dc=convert(rate,new Float32Array(rate/10|0).fill(level));
      const target=level<0?Math.round(Math.max(-1,level)*32768):Math.round(Math.min(1,level)*32767);
      assert.ok(dc.slice(320).every(sample=>Math.abs(sample-target)<=1),`${rate}Hz DC ${level}`);
      assert.ok(dc.every(sample=>sample>=-32768&&sample<=32767));
    }
  }
});
test('phase bank memory and convolution work remain bounded for uncommon device rates',()=>{
  for(const rate of [16000,16001,22050,44100,47999,48000,88200,95999,96000]){
    const engine=new PCM16Resampler(rate,()=>{});
    assert.ok(engine.phases>=1&&engine.phases<=512);
    assert.ok(engine.taps<=258&&engine.filter.byteLength<=1_058_832&&engine.ring.byteLength<=2064);
    assert.ok(engine.taps*16000<=4_128_000,'at most this many output convolution taps per audio second');
    assert.ok(engine.half/rate<=.0014,'bounded FIR group delay');
  }
});
test('actual worklet credit bound, silent monitoring and Stop remain intact',async t=>{
  const previous=Object.getOwnPropertyDescriptors(globalThis), names=['AudioWorkletProcessor','sampleRate','currentTime','registerProcessor'];
  let Processor;
  for(const [name,value] of Object.entries({sampleRate:48000,currentTime:0,registerProcessor:(name,type)=>{assert.equal(name,'pilot-pcm-capture');Processor=type;},AudioWorkletProcessor:class{port={messages:[],onmessage:null,postMessage(data){this.messages.push(data);}};} }))Object.defineProperty(globalThis,name,{configurable:true,writable:true,value});
  t.after(()=>{for(const name of names)if(previous[name])Object.defineProperty(globalThis,name,previous[name]);else delete globalThis[name];});
  await import('../src/audio/capture-worklet.js?signal-credit-test');
  const processor=new Processor(), input=new Float32Array(128).fill(.3), monitor=new Float32Array(128).fill(1);
  let keep=true;
  for(let quantum=0;quantum<68&&keep;quantum++){globalThis.currentTime=quantum*128/48000;keep=processor.process([[input]],[[monitor]]);assert.ok(monitor.every(sample=>sample===0),'never monitor captured speech');}
  assert.equal(processor.port.messages.filter(message=>message.type==='pcm').length,8);
  assert.deepEqual(processor.port.messages.filter(message=>message.type==='gap'),[{type:'gap',code:'capture_backpressure'}]);assert.equal(keep,false);
  const stopped=new Processor();stopped.port.onmessage({data:'stop'});assert.equal(stopped.process([[input]],[[monitor]]),false);assert.deepEqual(stopped.port.messages,[]);
  const flowing=new Processor();for(let quantum=0;quantum<100;quantum++){flowing.process([[input]],[[monitor]]);while(flowing.pending)flowing.port.onmessage({data:'ack'});}
  assert.ok(flowing.port.messages.filter(message=>message.type==='pcm').length>8);assert.equal(flowing.port.messages.filter(message=>message.type==='gap').length,0);
});
