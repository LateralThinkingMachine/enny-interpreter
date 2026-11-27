import {RustSurface} from "./rust-surface.js";
import type {PilotClient, View} from "./client.js";
import type {SpokenLanguage} from "./speech-hints.js";

export function SpeechSettings({view, client, disabled}: {view: View; client: PilotClient; disabled: boolean}) {
  const noKeywords = view.session?.provider_vendor === "gemini";
  const state = {disabled, noKeywords, pair: !!view.interpretation, language: view.spokenLanguage, terms: view.speechTerms, error: view.speechHintsError ?? ""};
  const dispatch = (action: string, value: string) => {
    if (disabled) return;
    if (action === "terms" && !noKeywords) client.setSpeechHints(view.spokenLanguage, value);
    if (action === "language" && !view.interpretation && ["auto", "ja", "en", "hi"].includes(value)) {
      client.setSpeechHints(value as SpokenLanguage, view.speechTerms);
    }
  };
  return <RustSurface kind="speech-hints" state={state} dispatch={dispatch} fallback={<><details className="speech-settings">
    <summary>Caption accuracy <span>Optional</span></summary>
    <fieldset disabled={disabled}>
      <legend className="sr-only">Session caption hints</legend>
      {!view.interpretation && <label htmlFor="spoken-language">Expected spoken language
        <select id="spoken-language" value={view.spokenLanguage} onChange={event => client.setSpeechHints(event.target.value as SpokenLanguage, view.speechTerms)}>
          <option value="auto">Japanese, English and Hindi</option>
          <option value="ja">Japanese · 日本語</option><option value="en">English</option><option value="hi">Hindi · हिन्दी</option>
        </select>
      </label>}
      {view.interpretation && <p className="meet-hint">Caption hints follow the selected language pair, or Japanese, English, Hindi, Spanish and Vietnamese in Auto mode. This list is a recognition hint, not independently detected speech or language-quality acceptance.</p>}
      {!view.interpretation && <p className="meet-hint">Keep all three for mixed-language speech. This is independent of the interpretation output language.</p>}
      {!noKeywords && <><label htmlFor="speech-terms">Names and specialist terms
        <textarea id="speech-terms" rows={3} value={view.speechTerms} maxLength={4096} spellCheck={false} autoComplete="off" aria-describedby="speech-terms-help speech-terms-error" aria-invalid={!!view.speechHintsError} placeholder="One name, acronym or product term per line" onChange={event => client.setSpeechHints(view.spokenLanguage, event.target.value)} />
      </label>
      <p id="speech-terms-help" className="meet-hint">Up to 20 terms. Sent to the caption service only when you start; kept in memory for this session. Hints may help recognition but do not guarantee exact words.</p></>}
    </fieldset>
  </details><p id="speech-terms-error" className="meet-hint" role="status">{view.speechHintsError && `${view.speechHintsError} Open Caption accuracy to correct or clear the optional terms.`}</p></>}/>;
}
