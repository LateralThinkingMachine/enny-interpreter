import {CaptionReader, excerpt} from "../CaptionReader.js";
import {interpretationLabel} from "../languages.js";
import {interpretationAnchor} from "../protocol.js";
import type {PrivateWindowConfiguration} from "./private-observations.js";
import {memo, useMemo, useSyncExternalStore} from 'react';
import type {PrivateReadingGroup} from './private-caption-reading.js';
import {PrivateCaptions, type PrivateCaptionSource} from './private-captions.js';

const Paragraph = memo(function Paragraph({group}: {group: PrivateReadingGroup}) {
  return <article className="private-live-paragraph" data-caption-sequence={group.sequence}>
    <div className="private-caption-label"><strong>{group.provenance === 'gpt_live_input' ? 'Original speech · unverified' : 'Interpretation'}</strong>{group.recent && <small>Recent words</small>}{group.pauseBefore && <small>After a pause</small>}</div>
    {group.earlierWordsUnavailable && <span className="help"> Earlier words unavailable · retained excerpt</span>}
    <p dir="auto"><span className="private-caption-body">{group.body}</span><span className="private-caption-tail">{group.tail}</span></p>
    <details><summary>Original fragments</summary>{group.originals.map(entry => <pre key={entry.sequence}>{entry.text}</pre>)}</details>
  </article>;
});

/** Stable rows and short display batches, with no typewriter queue, auto-scroll,
 * or per-token screen-reader announcement. Export still reads the controller.
 */
export const PrivateLiveCaptions = memo(function PrivateLiveCaptions({controller, configuration}: {controller: PrivateCaptionSource; configuration?: PrivateWindowConfiguration}) {
  const projection = useMemo(() => new PrivateCaptions(controller), [controller]);
  const view = useSyncExternalStore(projection.subscribe, projection.snapshot, projection.snapshot);
  // An earlier window may still have retained paragraphs while a replacement
  // starts. The main reader only accepts observations from its pinned window.
  const latest = (lane: "gpt_live_input" | "gpt_live_output") => {
    if (configuration) for (let index = view.paragraphs.length - 1; index >= 0; index--) {
      const row = view.paragraphs[index];
      if (row.window === configuration.window && row.provenance === lane) return excerpt(row.text);
    }
    return "";
  };
  const sourceText = latest("gpt_live_input"), targetText = latest("gpt_live_output");
  const history = <div className="private-caption-lanes">{(['gpt_live_input', 'gpt_live_output'] as const).map(provenance => {
      const original = provenance === 'gpt_live_input', paragraphs = view.paragraphs.filter(group => group.provenance === provenance);
      const titleId = original ? 'private-original-words' : 'private-interpreted-words';
      return <section key={provenance} className={`private-caption-lane ${original ? 'original' : 'interpretation'}`} aria-labelledby={titleId}>
        <h4 id={titleId}>{original ? 'Original' : 'Interpretation'}</h4><p className="help">{original ? 'GPT-Live input text · recognition is unverified.' : 'GPT-Live generated text · separate from heard audio.'}</p>
        {paragraphs.length ? paragraphs.map(group => <Paragraph key={group.sequence} group={group}/>) : <p className="help">{original ? 'No original words received in the retained view.' : 'No interpretation words received in the retained view.'}</p>}
      </section>;
    })}</div>;
  return <section className="private-live-reading" aria-labelledby="private-live-words-title" aria-live="off">
    <div className="private-live-heading"><h3 id="private-live-words-title">Original and interpreted words</h3><span className="tag">{view.receiving ? 'Live' : view.paragraphs.length ? 'Retained observations' : 'No observations yet'}</span></div>
    <p className="help">Original and interpretation arrive independently; rows are not aligned sentence pairs or verified speaker turns. The underlined tail is still arriving. Pauses only group the display; they do not mark completed speech. These paragraphs preserve delivered text; unfinished speech stays unfinished. They are not final source-ASR captions. {view.discarded} older raw fragments discarded from bounded memory.</p>
    {(view.gaps > 0 || view.hiddenParagraphs > 0) && <p className="help">{view.gaps > 0 && `${view.gaps} connection-transition gaps. `}{view.hiddenParagraphs > 0 && `${view.hiddenParagraphs} earlier paragraphs hidden from this reading view; export includes all retained original observations.`}</p>}
    {configuration && <CaptionReader prefix="private" scope={JSON.stringify(configuration)}
      sourceTitle="Original speech · unverified" sourceLabel={configuration.source === "microphone" ? "My microphone" : "Meeting tab"}
      sourceText={sourceText || "Waiting for original words."} sourceNote="GPT-Live input text. Recognition is unverified."
      targetLabel={interpretationLabel(configuration.interpretation, interpretationAnchor(configuration.interpretation))}
      targetText={targetText || "Waiting for interpreted words."} targetNote="Generated interpretation. Delivered text and audible speech are separate observations."
      hasWords={!!(sourceText || targetText)} />}
    {configuration ? <details><summary>Retained words and original fragments</summary>{history}</details> : history}
  </section>;
});
