import {sameCaptionGroup} from './caption-groups.js';
import type {PrivateListeningView} from './private-listening.js';
import {privateReadingBody, privateReadingGroups, type PrivateReadingGroup} from './private-caption-reading.js';

export const PRIVATE_CAPTION_REFRESH_MS = 40;
export const PRIVATE_CAPTION_MAX_DELAY_MS = 80;
export const PRIVATE_CAPTION_PARAGRAPHS = 80;
type CaptionInput = Pick<PrivateListeningView, 'entries' | 'phase' | 'discarded' | 'gaps'>;
export interface PrivateCaptionSource {snapshot(): CaptionInput; subscribe(listener: () => void): () => void;}
export interface PrivateCaptionClock {now(): number; after(callback: () => void, delay: number): () => void; frame(callback: () => void): () => void;}
export type PrivateCaptionView = Readonly<{paragraphs: readonly PrivateReadingGroup[]; discarded: number; hiddenParagraphs: number; gaps: number; receiving: boolean}>;
const clock: PrivateCaptionClock = {now: () => performance.now(), after: (callback, delay) => {const timer = setTimeout(callback, delay); return () => clearTimeout(timer);}, frame: callback => {const frame = requestAnimationFrame(callback); return () => cancelAnimationFrame(frame);}};

/** A display-only latest-snapshot scheduler, never an observation or export store.
 * Fixed display bursts align to a frame, with an 80ms timer fallback. First
 * words and lifecycle boundaries publish immediately. No character queue/finals.
 */
export class PrivateCaptions {
  private view: PrivateCaptionView;
  private observed: CaptionInput;
  private listeners = new Set<() => void>();
  private detach: (() => void) | null = null;
  private cancelWait: (() => void) | null = null;
  private cancelFrame: (() => void) | null = null;
  private cancelDeadline: (() => void) | null = null;
  private lastPublished = -Infinity;
  private unpublished = false;
  private epoch = 0;
  private ticket = 0;
  constructor(private source: PrivateCaptionSource, private timing: PrivateCaptionClock = clock) {
    this.observed = source.snapshot(); this.view = this.project(this.observed);
  }
  snapshot = (): PrivateCaptionView => this.view;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    if (this.listeners.size === 1) {
      this.detach = this.source.subscribe(this.changed);
      this.changed(); // closes the render/subscribe race without starting media
      if (this.unpublished) this.publish();
    }
    return () => {
      this.listeners.delete(listener);
      if (!this.listeners.size) {this.epoch++; this.cancelPending(); this.detach?.(); this.detach = null;}
    };
  };
  private project(input: CaptionInput): PrivateCaptionView {
    const previous = new Map(this.view?.paragraphs.map(group => [group.sequence, group]) ?? []);
    const all = privateReadingGroups(input.entries);
    const latest = new Map(all.map(group => [group.provenance, group.sequence]));
    const window = input.entries.at(-1)?.window;
    const paragraphs = all.slice(-PRIVATE_CAPTION_PARAGRAPHS).map(group => {
      const retained = previous.get(group.sequence);
      const recent = input.phase === 'listening' && group.window === window && latest.get(group.provenance) === group.sequence && ['open', 'punctuation'].includes(group.ending);
      const next = privateReadingBody(group, recent, retained);
      return retained && sameCaptionGroup(retained, next) && retained.body === next.body && retained.tail === next.tail && retained.recent === next.recent && retained.pauseBefore === next.pauseBefore ? retained : next;
    });
    return Object.freeze({paragraphs: Object.freeze(paragraphs), discarded: input.discarded, hiddenParagraphs: Math.max(0, all.length - paragraphs.length), gaps: input.gaps, receiving: input.phase === 'listening'});
  }
  private publish = (): void => {
    this.cancelPending();
    this.unpublished = false; this.lastPublished = this.timing.now(); this.view = this.project(this.observed);
    for (const listener of this.listeners) listener();
  };
  private cancelPending(): void {
    this.ticket++;
    this.cancelWait?.(); this.cancelWait = null; this.cancelFrame?.(); this.cancelFrame = null; this.cancelDeadline?.(); this.cancelDeadline = null;
  }
  private changed = (): void => {
    const next = this.source.snapshot(), previous = this.observed;
    if (next.entries === previous.entries && next.phase === previous.phase && next.discarded === previous.discarded && next.gaps === previous.gaps) return;
    this.observed = next;
    this.unpublished = true;
    const boundary = next.phase !== previous.phase || next.gaps !== previous.gaps || next.discarded !== previous.discarded
      || !previous.entries.length || !next.entries.length || next.entries[0]?.sequence !== previous.entries[0]?.sequence
      || next.entries.at(-1)?.window !== previous.entries.at(-1)?.window;
    const remaining = PRIVATE_CAPTION_REFRESH_MS - (this.timing.now() - this.lastPublished);
    if (boundary || !Number.isFinite(this.lastPublished)) {this.publish(); return;}
    if (this.cancelDeadline) return;
    const epoch = this.epoch, ticket = ++this.ticket;
    const valid = () => epoch === this.epoch && ticket === this.ticket && this.listeners.size > 0;
    const publish = () => {if (valid()) this.publish();};
    this.cancelDeadline = this.timing.after(publish, PRIVATE_CAPTION_MAX_DELAY_MS);
    const frame = () => {this.cancelWait = null; if (valid()) this.cancelFrame = this.timing.frame(publish);};
    if (remaining > 0) this.cancelWait = this.timing.after(frame, remaining); else frame();
  };
}
