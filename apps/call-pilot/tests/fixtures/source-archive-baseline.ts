// Frozen pre-index implementation, 2026-09-21. Only the type import path is
// adjusted for this fixture location. Used for differential checks/benchmarks.
import type {Language, LiveMode, Source, Transcript} from "../../src/protocol.js";

export type Fragment = Transcript & Readonly<{key: number; language: Language; live_mode: LiveMode | null; incomplete?: boolean}>;
const MAX_ITEMS = 10_000, MAX_BYTES = 4 * 1024 * 1024;
const utf8 = (text: string) => new TextEncoder().encode(text).length;
/** Cumulative source ASR only. Continuous retention evicts sealed older windows. */
export class SourceArchive {
  private records = new Map<string, Fragment>();
  private routeOrder = new Map<string, number>();
  private sequences = new Map<string, string>();
  private windows = new Map<string, number>();
  private bytes = 0;
  private discarded = 0;
  private rolling = false;
  continuous(): void {this.rolling = true;}
  clear(): void {this.records.clear(); this.routeOrder.clear(); this.sequences.clear(); this.windows.clear(); this.bytes = 0; this.discarded = 0;}
  byteLength(): number {return this.bytes;}
  discardedCount(): number {return this.discarded;}
  /** A renewal is an ordered fence; older updates can never recreate evicted items. */
  advanceWindow(route: string, window: number): boolean {
    const previous = this.windows.get(route) ?? 0;
    if (!this.rolling || !Number.isSafeInteger(window) || window !== previous + 1 || (!previous && this.windows.size >= 16)) throw new Error("Invalid source window");
    let incomplete = false;
    for (const [key, row] of this.records) if (row.route_id === route && !row.final && !row.incomplete) {this.records.set(key, {...row, incomplete: true}); incomplete = true;}
    this.windows.set(route, window);
    return incomplete;
  }
  items(): readonly Fragment[] {
    return [...this.records.values()].filter(row => row.text !== "").sort((a, b) => this.routeOrder.get(a.route_id)! - this.routeOrder.get(b.route_id)! || (a.window ?? 0) - (b.window ?? 0) || a.item!.sequence - b.item!.sequence);
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
    const delta = utf8(next.text) - utf8(previous?.text ?? "");
    const fits = () => (previous !== undefined || this.records.size < MAX_ITEMS) && this.bytes + delta <= MAX_BYTES;
    if (this.rolling && !fits()) {
      for (const [oldKey, row] of this.records) {
        if (fits()) break;
        if (!(row.final || row.incomplete) || row.window === undefined || row.window >= (this.windows.get(row.route_id) ?? 0)) continue;
        this.records.delete(oldKey);
        this.sequences.delete(row.route_id+"/"+row.window+"/"+row.item!.sequence);
        this.bytes -= utf8(row.text); this.discarded = Math.min(Number.MAX_SAFE_INTEGER, this.discarded + 1);
      }
    }
    // Current-window identities and provisional items are never silently evicted.
    if (!fits()) return "capacity";
    if (!this.routeOrder.has(next.route_id)) {if (this.routeOrder.size >= 16) return "capacity"; this.routeOrder.set(next.route_id, this.routeOrder.size);}
    this.records.set(key, {...next, key: previous?.key ?? next.key}); this.sequences.set(sequence, key); this.bytes += delta;
    return "changed";
  }
  close(scope: {route_id?: string; source?: Source}): void {
    for (const [key, row] of this.records) if (!row.final && (!scope.route_id || row.route_id === scope.route_id) && (!scope.source || row.source === scope.source)) this.records.set(key, {...row, incomplete: true});
  }
}
