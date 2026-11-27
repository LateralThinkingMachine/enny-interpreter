import type {Source, SourceDraft} from "./protocol.js";

type Scope = {route_id?: string; source?: Source};
type Entry = {source: Source; window?: number; items: Map<string, SourceDraft>; retired: Set<string>; awaitingBind: Set<string>};

/** Display-only hypotheses. No source sequence, persistence or export API. */
export class SourceDrafts {
  private routes = new Map<string, Entry>();
  private entry(route_id: string, source: Source, window?: number): Entry {
    let entry = this.routes.get(route_id);
    if (!entry) {
      if (this.routes.size >= 2) throw new Error("Source draft route capacity");
      entry = {source, window, items: new Map(), retired: new Set(), awaitingBind: new Set()}; this.routes.set(route_id, entry);
    }
    if (entry.source !== source || entry.window !== window) throw new Error("Changed draft scope");
    return entry;
  }
  apply(draft: SourceDraft): void {
    const entry = this.entry(draft.route_id, draft.source, draft.window);
    const previous = entry.items.get(draft.item_id);
    if (draft.retired) {
      if (!previous || previous.retired || previous.revision !== draft.revision) throw new Error("Unbound draft retirement");
      entry.items.delete(draft.item_id); entry.retired.add(draft.item_id); entry.awaitingBind.add(draft.item_id); return;
    }
    if (entry.retired.has(draft.item_id)) throw new Error("Retired draft");
    if (draft.revision !== (previous?.revision ?? 0) + 1 || (previous && (!draft.text.startsWith(previous.text) || draft.text.length <= previous.text.length))) throw new Error("Invalid draft revision");
    if (!previous && (entry.items.size + entry.retired.size >= 4096 || entry.items.size >= 8)) throw new Error("Source draft item capacity");
    entry.items.set(draft.item_id, draft);
  }
  bind(route_id: string, source: Source, window: number | undefined, item_id: string): void {
    const entry = this.entry(route_id, source, window);
    if (entry.items.get(item_id)) throw new Error("Source item preceded draft retirement");
    if (!entry.retired.has(item_id) && entry.items.size + entry.retired.size >= 4096) throw new Error("Source draft identity capacity");
    entry.retired.add(item_id);
    entry.awaitingBind.delete(item_id);
  }
  items(): readonly SourceDraft[] {return [...this.routes.values()].flatMap(entry => [...entry.items.values()]);}
  incompleteSources(scope: Scope = {}): Source[] {
    return [...new Set([...this.routes].filter(([id, entry]) => (!scope.route_id || scope.route_id === id) && (!scope.source || scope.source === entry.source) && (entry.items.size > 0 || entry.awaitingBind.size > 0)).map(([, entry]) => entry.source))];
  }
  clear(scope: Scope = {}): void {
    for (const [id, entry] of this.routes) if ((!scope.route_id || scope.route_id === id) && (!scope.source || scope.source === entry.source)) this.routes.delete(id);
  }
}
