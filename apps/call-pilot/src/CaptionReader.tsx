import {RustSurface} from "./rust-surface.js";

type ReaderState = Readonly<{
  prefix: "meet" | "private"; sourceTitle: string; sourceLabel: string; sourceText: string; sourceNote: string;
  targetLabel: string; targetText: string; targetNote: string; hasWords: boolean;
}>;

/** Excerpts are bounded by Unicode scalar count in both speech lanes. */
export function excerpt(text: string): string {
  const characters = Array.from(text);
  return characters.length > 600 ? `…${characters.slice(-600).join("")}` : text;
}

/** Only presentation fields cross into WASM. A binding change destroys the held snapshot. */
export function CaptionReader({scope, ...state}: ReaderState & {scope: string}) {
  const {prefix, sourceTitle, sourceLabel, sourceText, sourceNote, targetLabel, targetText, targetNote} = state;
  return <RustSurface key={scope} kind="reader" state={state} fallback={
    <section className="caption-reader" aria-label="Caption reader" data-size="standard">
      <div className="reader-controls"><button type="button" disabled={!state.hasWords}>Hold text</button>
        <label>Caption size<select defaultValue="standard"><option value="standard">Standard</option><option value="large">Large</option><option value="extra">Extra large</option></select></label>
        <span role="status" aria-live="polite">Showing the latest available text.</span></div>
      <div className="meet-text-preview" aria-live="off">
        <article id={prefix + "-caption-preview"} aria-labelledby={prefix + "-original-heading"}>
          <div className="meet-preview-heading"><h2 id={prefix + "-original-heading"}>{sourceTitle}</h2><span>{sourceLabel}</span></div>
          <p className="meet-preview-text" dir="auto">{sourceText}</p><p className="meet-hint">{sourceNote}</p>
        </article>
        <article id={prefix + "-interpretation-preview"} aria-labelledby={prefix + "-translated-heading"}>
          <div className="meet-preview-heading"><h2 id={prefix + "-translated-heading"}>Interpretation</h2><span>{targetLabel}</span></div>
          <p className="meet-preview-text" dir="auto">{targetText}</p><p className="meet-hint">{targetNote}</p>
        </article>
      </div>
    </section>}/>;
}
