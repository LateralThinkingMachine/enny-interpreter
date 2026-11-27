import {RustButton} from "./rust-button.js";
import {useState} from "react";

export type SessionActions = Readonly<{
  available: boolean;
  creating: boolean;
  error: string | null;
  create(): void;
  resume?(): void;
}>;

/** A fresh session is an explicit action, never a reset of the stopped client. */
export function SessionTransition({actions, paused = false, exporting = false, exportError = null, exportRetained}: {
  actions: SessionActions;
  paused?: boolean;
  exporting?: boolean;
  exportError?: string | null;
  exportRetained?(): Promise<boolean>;
}) {
  const [discarding, setDiscarding] = useState(false);
  if (!actions.available) return null;
  const busy = actions.creating || exporting;
  return <section className="session-transition" aria-labelledby="new-session-heading" aria-busy={busy}>
    <h2 id="new-session-heading">{paused ? "Continue with a new session" : "Ready for your next call?"}</h2>
    <p>{paused ? "Export your retained source captions and notes before starting again. Generated interpretation is not included." : "Your language choice stays the same. Choose the audio again when you start."}</p>
    <div className="button-row">
      {paused ? <><RustButton id="export-new-session" className="primary-button" disabled={busy || !exportRetained} onClick={() => {void (async () => {if (await exportRetained?.()) actions.create();})();}}>{exporting ? "Preparing export…" : actions.creating ? "Preparing new session…" : "Export & new session"}</RustButton><RustButton id="discard-new-session" className="quiet" disabled={busy} aria-expanded={discarding} aria-controls="discard-session-choice" onClick={() => setDiscarding(true)}>New session without export…</RustButton></> : <RustButton id="new-session" className="primary-button" disabled={busy} onClick={actions.create}>{actions.creating ? "Preparing new session…" : exporting ? "Preparing export…" : "New session"}</RustButton>}
    </div>
    {paused && <p className="help">Your browser starts a download. Check Downloads before closing the page.</p>}
    {paused && <div id="discard-session-choice" className="discard-session-choice" hidden={!discarding}><p>Starting a new session will discard the captions, interpretation and notes retained on this page.</p><div className="button-row"><RustButton id="confirm-discard-new-session" className="negative" disabled={busy} onClick={actions.create}>Discard & new session</RustButton><RustButton className="quiet" disabled={busy} onClick={() => setDiscarding(false)}>Keep this session</RustButton></div></div>}
    {actions.error && <p className="notice unavailable" role="alert">{actions.error}</p>}
    {exportError && <p className="notice unavailable" role="alert">{exportError}</p>}
  </section>;
}
