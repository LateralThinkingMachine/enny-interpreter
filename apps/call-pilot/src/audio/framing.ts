export type AudioSource = 'remote_mix' | 'self';
export type PrivateKind = 'translation' | 'live' | 'live_interpretation';

export function encodeCaptureFrame(source: AudioSource, sequence: number, pcm: ArrayBuffer): ArrayBuffer {
  if ((source !== 'remote_mix' && source !== 'self') || !Number.isInteger(sequence) || sequence < 0 || sequence > 0xffff_ffff || pcm.byteLength !== 640) throw new Error('invalid_capture_frame');
  const frame = new ArrayBuffer(646);
  const view = new DataView(frame);
  view.setUint8(0, 1);
  view.setUint8(1, source === 'remote_mix' ? 0 : 1);
  view.setUint32(2, sequence, true);
  // Worklet's native Int16 buffer is converted explicitly to the wire's little endian.
  const samples = new Int16Array(pcm);
  for (let index = 0; index < samples.length; index++) view.setInt16(6 + index * 2, samples[index], true);
  return frame;
}

export function decodePCM(pcm: Uint8Array, rate: number): Float32Array<ArrayBuffer> {
  if ((rate !== 16000 && rate !== 24000) || pcm.byteLength === 0 || pcm.byteLength > 48000 || pcm.byteLength % 2 !== 0) throw new Error('invalid_playback_frame');
  const result = new Float32Array(pcm.byteLength / 2);
  const view = new DataView(pcm.buffer, pcm.byteOffset, pcm.byteLength);
  for (let index = 0; index < result.length; index++) result[index] = view.getInt16(index * 2, true) / 32768;
  return result;
}

/** Parse the complete bounded PCM WAV we are willing to audition/publish. */
export function decodePreparedWav(bytes: ArrayBuffer): {samples: Float32Array<ArrayBuffer>; rate: number} {
  if (bytes.byteLength < 44 || bytes.byteLength > 4 * 1024 * 1024) throw new Error('invalid_clip');
  const view = new DataView(bytes);
  const tag = (offset: number) => String.fromCharCode(...new Uint8Array(bytes, offset, 4));
  if (tag(0) !== 'RIFF' || tag(8) !== 'WAVE' || view.getUint32(4, true) + 8 !== bytes.byteLength) throw new Error('invalid_clip');
  let rate: number | undefined;
  let data: {offset: number; length: number} | undefined;
  let offset = 12;
  let sawFormat = false;
  while (offset + 8 <= bytes.byteLength) {
    const kind = tag(offset);
    const size = view.getUint32(offset + 4, true);
    const start = offset + 8;
    if (size > bytes.byteLength - start) throw new Error('invalid_clip');
    if (kind === 'fmt ') {
      if (sawFormat || size < 16 || view.getUint16(start, true) !== 1 || view.getUint16(start + 2, true) !== 1 || view.getUint16(start + 14, true) !== 16) throw new Error('invalid_clip');
      sawFormat = true;
      rate = view.getUint32(start + 4, true);
      if (![16000, 24000].includes(rate) || view.getUint32(start + 8, true) !== rate * 2 || view.getUint16(start + 12, true) !== 2) throw new Error('invalid_clip');
    } else if (kind === 'data') {
      if (data || size === 0 || size % 2 !== 0) throw new Error('invalid_clip');
      data = {offset: start, length: size};
    }
    offset = start + size + (size % 2);
  }
  if (offset !== bytes.byteLength || rate === undefined || data === undefined || data.length / (2 * rate) > 60) throw new Error('invalid_clip');
  const samples = new Float32Array(data.length / 2);
  for (let index = 0; index < samples.length; index++) samples[index] = view.getInt16(data.offset + index * 2, true) / 32768;
  return {samples, rate};
}
