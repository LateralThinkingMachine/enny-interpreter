import {groupCaptionEntries, type CaptionGroup} from './caption-groups.js';
import type {CaptionEntry} from './caption-data.js';

export const PRIVATE_CAPTION_PAUSE_MS = 1500;
export type PrivateReadingGroup = CaptionGroup & Readonly<{body: string; tail: string; recent: boolean; pauseBefore: boolean}>;
// Pin UI boundary behavior across host locales. This does not classify speech
// as English; Unicode/dictionary segmentation still covers unspaced scripts.
const words = typeof Intl.Segmenter === 'function' ? new Intl.Segmenter('en', {granularity: 'word'}) : null;
const completeEnding = /[\s.!?。！？、，,;；:："'”’）)\]】」』]$/u;

/** Display boundary only. A timing gap cannot split a delayed lexical or
 * combining continuation, identify a speaker change, or establish a final.
 */
function lexicalBoundary(before: string, after: string): boolean {
  if (!before || !after || /^(?:\p{M}|\u200d|\ufe0e|\ufe0f)/u.test(after)) return false;
  if (/\d[.,]$/u.test(before) && /^\d/u.test(after)) return false;
  if (/[\p{L}\p{N}]['’-]$/u.test(before) && /^[\p{L}\p{N}]/u.test(after)) return false;
  if (/\s$/u.test(before) || /^\s/u.test(after)) return true;
  if (!words) return completeEnding.test(before);
  const joined = before + after, boundary = before.length;
  for (const segment of words.segment(joined)) {
    const end = segment.index + segment.segment.length;
    if (segment.index < boundary && end > boundary) return false;
    if (segment.index === boundary) return true;
  }
  return false;
}

function tailOffset(text: string): number {
  // A split decimal must remain one provisional token until the next part arrives.
  const decimal = /\d+[.,]$/u.exec(text);
  if (decimal) return decimal.index;
  const contraction = /[\p{L}\p{N}\p{M}]+['’-]$/u.exec(text);
  if (contraction) return contraction.index;
  if (completeEnding.test(text)) return text.length;
  if (!words) return text.search(/\S*$/u);
  let offset = 0;
  for (const segment of words.segment(text)) offset = segment.index;
  return offset;
}

/** Group observations for display in the private browser.
 * Input/output lanes remain independent even when their deltas interleave.
 */
export function privateReadingGroups(entries: readonly CaptionEntry[]): readonly (CaptionGroup & {pauseBefore: boolean})[] {
  const result: (CaptionGroup & {pauseBefore: boolean})[] = [];
  for (const original of groupCaptionEntries(entries)) {
    let chunk: CaptionEntry[] = [], text = '', pauseBefore = false;
    const push = (last: boolean) => {
      if (!chunk.length) return;
      result.push({...original, sequence: chunk[0].sequence, text, originals: Object.freeze(chunk),
        start_ms: Math.min(...chunk.map(row => row.start_ms)), end_ms: Math.max(...chunk.map(row => row.end_ms)),
        ending: last ? original.ending : 'boundary', continuation: pauseBefore ? false : original.continuation,
        earlierWordsUnavailable: pauseBefore ? false : original.earlierWordsUnavailable, pauseBefore});
    };
    for (const entry of original.originals) {
      const last = chunk.at(-1);
      if (last && entry.start_ms - last.end_ms >= PRIVATE_CAPTION_PAUSE_MS && lexicalBoundary(text, entry.text)) {
        push(false); chunk = []; text = ''; pauseBefore = true;
      }
      chunk.push(entry); text += entry.text;
    }
    push(true);
  }
  // Splitting an earlier lane must not move its later chunk ahead of a group
  // first seen on the other lane. Sequence is delivery identity, not audio time.
  return result.sort((left, right) => left.sequence - right.sequence);
}

export function privateReadingBody(group: CaptionGroup & {pauseBefore: boolean}, recent: boolean, previous?: PrivateReadingGroup): PrivateReadingGroup {
  let offset = recent ? tailOffset(group.text) : group.text.length;
  if (previous && group.window === previous.window && group.provenance === previous.provenance && group.text.startsWith(previous.body)) offset = Math.max(offset, previous.body.length);
  return Object.freeze({...group, body: group.text.slice(0, offset), tail: group.text.slice(offset), recent});
}
