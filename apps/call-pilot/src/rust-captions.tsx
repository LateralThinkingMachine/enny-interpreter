import {RustSurface} from "./rust-surface.js";

import type {Interpretation, Language, LiveMode, RouteKind, Source} from "./protocol.js";

type InterpretationRow = Readonly<{route?: RouteKind; source: Source; language: Language; live_mode: LiveMode | null; provenance: string; interpretation?: Interpretation}>;

/** A fixed-target row names the requested output, not a detected language.
 * Timed Live groups have an implicit live route. Untimed rows bind it explicitly. */
export function fixedTarget(row: InterpretationRow) {
  if ((row.route ?? "live") !== "live" || row.live_mode !== "interpretation" || row.provenance !== "live_output" || row.interpretation) return undefined;
  return {source: row.source, language: row.language};
}

interface CaptionRow {key: number; source: string; status: string; text: string; note?: string; fixed?: Readonly<{source: Source; language: Language}>}
export function CaptionRows({rows, empty, label}: {rows: CaptionRow[]; empty: string; label: string}) {
  return <RustSurface kind="fragments" state={{order: rows.map(row => String(row.key)),
    rows: Object.fromEntries(rows.map(row => [row.key, row])), empty, label}}
    fallback={rows.length ? <ol className="fragment-list" aria-label={label}>{rows.map(row =>
      <li key={row.key} data-interpretation-target={row.fixed?.language} data-interpretation-source={row.fixed?.source} data-interpretation-mode={row.fixed ? "fixed" : undefined}><div className="fragment-label"><span>{row.source}</span><span>{row.status}</span></div>
        <p>{row.text}</p>{row.note && <small>{row.note}</small>}</li>)}</ol>
      : <div className="empty-state"><span aria-hidden="true">◌</span><p>{empty}</p></div>}/>;
}
