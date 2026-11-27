import type {Interpretation, Language, LiveMode, Source, Transcript} from "./protocol.js";

export type Fragment = Transcript & Readonly<{key: number; language: Language; live_mode: LiveMode | null; interpretation?: Interpretation; incomplete?: boolean}>;
const MAX_ITEMS = 10_000, MAX_BYTES = 4 * 1024 * 1024;
const encoder = new TextEncoder();
const utf8 = (text: string) => encoder.encode(text).length;
const immutable = (row: Fragment): Fragment => Object.freeze({...row, item: row.item && Object.freeze({...row.item})});
/** Cumulative source ASR only. Continuous retention evicts sealed older windows. */
export class SourceArchive {
  private records = new Map<string, Fragment>();
  private routeOrder = new Map<string, number>();
  private sequences = new Map<string, string>();
  private windows = new Map<string, number>();
  private sizes = new Map<string, number>();
  // Kept in source order as items arrive, independent of Map insertion/receipt
  // order. Only nonempty rows participate in presentation; empty final item
  // identities still belong to records/sequences and count toward capacity.
  private visible: Fragment[] = [];
  private snapshot: readonly Fragment[] | null = Object.freeze([]);
  private bytes = 0;
  private discarded = 0;
  private rolling = false;
  continuous(): void {this.rolling = true;}
  clear(): void {this.records.clear(); this.routeOrder.clear(); this.sequences.clear(); this.windows.clear(); this.sizes.clear(); this.visible = []; this.snapshot = Object.freeze([]); this.bytes = 0; this.discarded = 0;}
  byteLength(): number {return this.bytes;}
  discardedCount(): number {return this.discarded;}
  /** A renewal is an ordered fence; older updates can never recreate evicted items. */
  advanceWindow(route: string, window: number): boolean {
    const previous = this.windows.get(route) ?? 0;
    if (!this.rolling || !Number.isSafeInteger(window) || window !== previous + 1 || (!previous && this.windows.size >= 16)) throw new Error("Invalid source window");
    let incomplete = false;
    for (const [key, row] of this.records) if (row.route_id === route && !row.final && !row.incomplete) {
      const closed = immutable({...row, incomplete: true});
      this.records.set(key, closed); this.updateVisible(row, closed); incomplete = true;
    }
    this.windows.set(route, window);
    return incomplete;
  }
  items(): readonly Fragment[] {
    // Publication copies references once per visible change. Repeated reads
    // reuse the immutable snapshot; future revisions never mutate old views.
    return this.snapshot ??= Object.freeze(this.visible.slice());
  }
  private position(row: Fragment): number {
    const route = this.routeOrder.get(row.route_id)!;
    let low = 0, high = this.visible.length;
    while (low < high) {
      const mid = (low + high) >>> 1, candidate = this.visible[mid];
      const order = this.routeOrder.get(candidate.route_id)! - route
        || (candidate.window ?? 0) - (row.window ?? 0)
        || candidate.item!.sequence - row.item!.sequence;
      if (order < 0) low = mid + 1; else high = mid;
    }
    return low;
  }
  private updateVisible(previous: Fragment | undefined, next?: Fragment): void {
    if (!previous?.text && !next?.text) return;
    const index = this.position(previous ?? next!);
    if (previous?.text) {
      if (next?.text) this.visible[index] = next;
      else this.visible.splice(index, 1);
    } else this.visible.splice(index, 0, next!);
    this.snapshot = null;
  }
  apply(next: Fragment): "changed" | "unchanged" | "capacity" {
    if (next.route !== "transcription" || next.live_mode !== null || next.provenance !== "source_asr" || !next.item) throw new Error("Missing source item identity");
    if (this.rolling) {
      const window = this.windows.get(next.route_id);
      if (next.window === undefined || window === undefined || next.window > window) throw new Error("Unknown source window");
      if (next.window < window) return "unchanged";
    } else if (next.window !== undefined) throw new Error("Unexpected source window");
    const scope = next.route_id+"/"+(next.window ?? 0), key = scope+"/"+next.item.id, sequence = scope+"/"+next.item.sequence;
    const previous = this.records.get(key), assigned = this.sequences.get(sequence);
    if (assigned && assigned !== key) throw new Error("Changed source sequence identity");
    if (previous) {
      if (previous.source !== next.source || previous.item!.sequence !== next.item.sequence || previous.language !== next.language) throw new Error("Changed source item identity");
      if (next.item.revision < previous.item!.revision) return "unchanged";
      if (next.item.revision === previous.item!.revision) {
        if (next.text !== previous.text || next.final !== previous.final) throw new Error("Conflicting source revision");
        return "unchanged";
      }
      if (previous.final || previous.incomplete) throw new Error("Closed source item changed");
    }
    const nextBytes = utf8(next.text), delta = nextBytes - (this.sizes.get(key) ?? 0);
    const fits = () => (previous !== undefined || this.records.size < MAX_ITEMS) && this.bytes + delta <= MAX_BYTES;
    if (this.rolling && !fits()) {
      for (const [oldKey, row] of this.records) {
        if (fits()) break;
        if (!(row.final || row.incomplete) || row.window === undefined || row.window >= (this.windows.get(row.route_id) ?? 0)) continue;
        this.records.delete(oldKey);
        this.sequences.delete(row.route_id+"/"+row.window+"/"+row.item!.sequence);
        this.bytes -= this.sizes.get(oldKey)!; this.sizes.delete(oldKey); this.updateVisible(row);
        this.discarded = Math.min(Number.MAX_SAFE_INTEGER, this.discarded + 1);
      }
    }
    // Current-window identities and provisional items are never silently evicted.
    if (!fits()) return "capacity";
    if (!this.routeOrder.has(next.route_id)) {if (this.routeOrder.size >= 16) return "capacity"; this.routeOrder.set(next.route_id, this.routeOrder.size);}
    const stored = immutable({...next, key: previous?.key ?? next.key});
    this.records.set(key, stored); this.sequences.set(sequence, key); this.sizes.set(key, nextBytes); this.bytes += delta;
    this.updateVisible(previous, stored);
    return "changed";
  }
  close(scope: {route_id?: string; source?: Source}): void {
    for (const [key, row] of this.records) if (!row.final && !row.incomplete && (!scope.route_id || row.route_id === scope.route_id) && (!scope.source || row.source === scope.source)) {
      const closed = immutable({...row, incomplete: true});
      this.records.set(key, closed); this.updateVisible(row, closed);
    }
  }
}
