import {memo} from "react";
import type {SourceDraft} from "./protocol.js";

/** Separate from the confirmed-source list: arrival order is not chronology. */
export const SourceDraftPanel = memo(function SourceDraftPanel({drafts}: {drafts: readonly SourceDraft[]}) {
  if (!drafts.length) return null;
  return <section className="source-drafts" aria-label="Live caption drafts">
    <h3>Live draft <small>Awaiting source confirmation</small></h3>
    <p className="hint">Early words may change. Drafts are not included in exports or data lookups.</p>
    <ul>{drafts.map(draft => <li key={`${draft.route_id}:${draft.window ?? 0}:${draft.item_id}`}>
      <small>{draft.source === "remote_mix" ? "Selected tab" : "My microphone"} · provisional</small>
      <p>{draft.text}</p>
    </li>)}</ul>
  </section>;
});
