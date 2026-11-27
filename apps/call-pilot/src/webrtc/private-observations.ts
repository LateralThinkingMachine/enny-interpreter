import type {Interpretation} from '../protocol.js';
import type {CaptionEntry} from './caption-data.js';
import {INTERPRETER_VOICES} from './caption-data.js';

export type PrivateVoice = typeof INTERPRETER_VOICES[number];
export function parsePrivateVoice(value: unknown): PrivateVoice {
  if (typeof value !== 'string' || !(INTERPRETER_VOICES as readonly string[]).includes(value)) throw new Error('Unsupported private voice');
  return value as PrivateVoice;
}
export type PrivateWindowConfiguration = Readonly<{
  window: number; model: 'gpt-live-1'; requested_voice: PrivateVoice;
  interpretation: Interpretation; source: 'tab' | 'microphone';
}>;

/** Configuration retention follows exact fragment windows, plus the current
 * attempt even before its first observation. It never accumulates empty starts. */
export function retainPrivateConfigurations(configurations: readonly PrivateWindowConfiguration[], entries: readonly CaptionEntry[], currentWindow: number): readonly PrivateWindowConfiguration[] {
  const windows = new Set(entries.map(entry => entry.window)); windows.add(currentWindow);
  return Object.freeze(configurations.filter(configuration => windows.has(configuration.window)));
}

/** Pure bounded observation export. Fragments retain their exact original
 * objects/text/order; requested settings are not provider voice receipts. */
export function exportPrivateObservations(view: Readonly<{entries: readonly CaptionEntry[]; configurations: readonly PrivateWindowConfiguration[]; discarded: number; gaps: number}>) {
  return Object.freeze({schema_version: 'private-live-observations.v2', model: 'gpt-live-1',
    meaning: 'Provider input/output text observations; not a complete or authoritative source transcript.',
    configuration_meaning: 'Requested settings pinned for each connection window; voice quality and effective provider voice are not verified.',
    discarded: view.discarded, coverage_gaps: view.gaps,
    configurations: view.configurations, fragments: view.entries});
}
