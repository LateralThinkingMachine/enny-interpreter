import {memo} from "react";
import type {LiveCaptionGroup} from "./live-captions.js";

const source = (value: LiveCaptionGroup["source"]) => value === "remote_mix" ? "Selected tab" : "My microphone";
import {interpretationLabel} from "./languages.js";

import {CaptionRows, fixedTarget} from "./rust-captions.js";

const Rows = memo(function Rows({groups, input = false}: {groups: readonly LiveCaptionGroup[]; input?: boolean}) {
  const rows = groups.slice(-100).map(group => ({
    key: group.key, source: source(group.source), text: group.text, fixed: fixedTarget(group),
    status: (input ? "Live model input · not source ASR" : `${group.live_mode === "interpretation" ? "Interpretation" : "Assistant"} · requested ${interpretationLabel(group.interpretation, group.language)}`) + " · generated text",
    note: group.closed ? "Earlier connection segment" : undefined,
  }));
  return <CaptionRows rows={rows} empty="" label={input ? "Live model input text, not source captions" : "Streaming interpretation text"}/>;
});

/** Grouping changes presentation only; there is no debounce or reveal animation. */
export const LiveCaptionPanel = memo(function LiveCaptionPanel({groups, discarded = 0}: {groups: readonly LiveCaptionGroup[]; discarded?: number}) {
  const output = groups.filter(group => group.provenance === "live_output");
  const input = groups.filter(group => group.provenance === "live_input");
  if (!groups.length) return null;
  return <div className="live-caption-panel">
    {output.length ? <Rows groups={output}/> : <p className="help">Listening. Interpretation text will appear as it arrives.</p>}
    <p className="help">Text grows as it arrives. These reading groups are not completed turns or a record of what you heard. {discarded > 0 ? `${discarded} older text fragments discarded. ` : ""}Generated text is not included in the source-caption export.</p>
    {input.length > 0 && <details className="assistant-details"><summary>What GPT-Live-1 heard · separate from source captions</summary><Rows groups={input} input/></details>}
  </div>;
});
