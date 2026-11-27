import {parseInterpretation, sameInterpretation} from "./protocol.js";
import type {Interpretation, Language, LiveMode, Source} from "./protocol.js";

export type LiveCaptionFragment = Readonly<{
  route_id: string; source: Source; language: Language; live_mode: LiveMode; interpretation?: Interpretation;
  window?: number; provenance: "live_input" | "live_output";
  text: string; start_ms: number; end_ms: number;
}>;
export type LiveCaptionOriginal = LiveCaptionFragment & Readonly<{key: number}>;
export type LiveCaptionGroup = Readonly<{
  key: number; route_id: string; source: Source; language: Language; live_mode: LiveMode; interpretation?: Interpretation;
  window?: number; provenance: "live_input" | "live_output";
  text: string; start_ms: number; end_ms: number;
  originals: readonly LiveCaptionOriginal[]; closed: boolean;
}>;
type Scope = {route_id?: string; source?: Source; window?: number};
const MAX_ORIGINALS = 500, MAX_BYTES = 1_048_576, MAX_GROUP_ORIGINALS = 100;
const MAX_GROUP_BYTES = 16_384, MAX_GROUP_SPAN_MS = 15_000;
/** A display choice only: never a speech boundary, silence detector or tool trigger. */
export const LIVE_CAPTION_GROUP_GAP_MS = 300;
const encoder = new TextEncoder();
const size = (text: string) => encoder.encode(text).byteLength;
function matches(group: LiveCaptionGroup, scope: Scope): boolean {
  return (scope.route_id === undefined || group.route_id === scope.route_id)
    && (scope.source === undefined || group.source === scope.source)
    && (scope.window === undefined || group.window === scope.window);
}
function sameChannel(group: LiveCaptionGroup, fragment: LiveCaptionFragment): boolean {
  return group.route_id === fragment.route_id && group.source === fragment.source
    && group.window === fragment.window && group.provenance === fragment.provenance
    && group.language === fragment.language && group.live_mode === fragment.live_mode && sameInterpretation(group.interpretation, fragment.interpretation);
}
function compose(group: LiveCaptionGroup, originals: readonly LiveCaptionOriginal[]): LiveCaptionGroup {
  // Deltas are append-only text, not independent words or revisions. Provider
  // intervals are approximate: sorting them can reverse a negation or numeral.
  // Timing selects a reading group; arrival order remains the text order within it.
  const retained = Object.freeze([...originals]);
  return Object.freeze({...group, originals: retained, text: retained.map(item => item.text).join(""),
    start_ms: Math.min(...retained.map(item => item.start_ms)),
    end_ms: Math.max(...retained.map(item => item.end_ms))});
}

/** Bounded private presentation. Original deltas survive regrouping unchanged.
 * Timestamps are half-open session intervals, not wall time or playback receipts.
 * There is deliberately no canonical item, finality, evidence or export API.
 */
export class LiveCaptions {
  private groups: readonly LiveCaptionGroup[] = Object.freeze([]);
  private nextKey = 0;
  private bytes = 0;
  private count = 0;
  private discarded = 0;
  // Exactly one bounded entry per retained group. Avoid repeatedly UTF-8
  // encoding every candidate's accumulated text on each incoming fragment.
  private groupBytes = new Map<number, number>();

  apply(fragment: LiveCaptionFragment): boolean {
    if (!/^[A-Za-z0-9_-]{1,128}$/.test(fragment.route_id)
      || !["remote_mix", "self"].includes(fragment.source)
      || !["ja", "en", "hi", "es", "vi"].includes(fragment.language)
      || !["assistant", "interpretation"].includes(fragment.live_mode)
      || !["live_input", "live_output"].includes(fragment.provenance)
      || (fragment.window !== undefined && (!Number.isSafeInteger(fragment.window) || fragment.window < 1))
      || !Number.isFinite(fragment.start_ms) || !Number.isFinite(fragment.end_ms)
      || fragment.start_ms < 0 || fragment.end_ms < fragment.start_ms || fragment.end_ms > 3_610_000
      || typeof fragment.text !== "string" || fragment.text.includes("\0")
      || Array.from(fragment.text).length > 4096) throw new Error("Invalid Live caption fragment");
    const bytes = size(fragment.text);
    if (bytes > 16_384) throw new Error("Invalid Live caption fragment");
    if (!fragment.text.length) return false;
    if (this.nextKey >= Number.MAX_SAFE_INTEGER) throw new Error("Live caption identity capacity");
    // Copy only the reviewed fields; no caller-owned object can mutate a row.
    const original: LiveCaptionOriginal = Object.freeze({key: ++this.nextKey,
      route_id: fragment.route_id, source: fragment.source, language: fragment.language,
      live_mode: fragment.live_mode, ...(fragment.interpretation ? {interpretation: parseInterpretation(fragment.interpretation)} : {}), window: fragment.window, provenance: fragment.provenance,
      text: fragment.text, start_ms: fragment.start_ms, end_ms: fragment.end_ms});
    const groups = [...this.groups];
    // Only grow the latest row in this exact lane. Returning to an earlier row
    // after a newer same-lane row would move delivered words backward visually.
    // Approximate/late timestamps affect grouping, never the delta text order.
    let selected = -1;
    for (let index = groups.length - 1; index >= 0; index--) {
      const group = groups[index];
      if (!sameChannel(group, original)) continue;
      const distance = Math.max(0, group.start_ms - original.end_ms, original.start_ms - group.end_ms);
      if (!group.closed && group.originals.length < MAX_GROUP_ORIGINALS
        && Math.max(group.end_ms, original.end_ms) - Math.min(group.start_ms, original.start_ms) <= MAX_GROUP_SPAN_MS
        && distance <= LIVE_CAPTION_GROUP_GAP_MS
        && this.groupBytes.get(group.key)! + bytes <= MAX_GROUP_BYTES) selected = index;
      break;
    }
    if (selected >= 0) {
      const group = groups[selected];
      groups[selected] = compose(group, [...group.originals, original]);
      this.groupBytes.set(group.key, this.groupBytes.get(group.key)! + bytes);
    } else {
      groups.push(compose({...original, originals: [], closed: false}, [original]));
      this.groupBytes.set(original.key, bytes);
    }
    this.bytes += bytes; this.count++;
    // Evict original fragments by arrival, never by a provider timestamp that
    // can arrive late. A surviving row keeps its original stable display key.
    while (this.count > MAX_ORIGINALS || this.bytes > MAX_BYTES) {
      let groupIndex = -1, oldest = Infinity;
      for (const [index, group] of groups.entries()) for (const item of group.originals) {
        if (item.key < oldest) {oldest = item.key; groupIndex = index;}
      }
      const group = groups[groupIndex];
      const removed = group.originals.find(item => item.key === oldest)!;
      const remaining = group.originals.filter(item => item.key !== oldest);
      const removedBytes = size(removed.text);
      this.bytes -= removedBytes; this.count--; this.discarded++;
      if (remaining.length) {
        groups[groupIndex] = compose(group, remaining);
        this.groupBytes.set(group.key, this.groupBytes.get(group.key)! - removedBytes);
      } else {
        groups.splice(groupIndex, 1);
        this.groupBytes.delete(group.key);
      }
    }
    this.groups = Object.freeze(groups);
    return true;
  }

  items(): readonly LiveCaptionGroup[] {return this.groups;}
  originalCount(): number {return this.count;}
  byteLength(): number {return this.bytes;}
  discardedCount(): number {return this.discarded;}
  /** A route/window gap fences future grouping, retaining already shown text. */
  close(scope: Scope = {}): void {
    this.groups = Object.freeze(this.groups.map(group => matches(group, scope) && !group.closed ? Object.freeze({...group, closed: true}) : group));
  }
  clear(): void {this.groups = Object.freeze([]); this.groupBytes.clear(); this.nextKey = 0; this.bytes = 0; this.count = 0; this.discarded = 0;}
}
