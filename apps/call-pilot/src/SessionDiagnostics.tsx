import type {View} from "./client.js";
import {elapsedLabel, stopMessage} from "./diagnostics.js";
import {providerMessage} from "./feedback.js";

const SOURCES = {remote_mix: "Selected tab · remote mix", self: "My microphone · self"};
const SERVICES = {transcription: "Source captions · gpt-live-transcribe", translation: "Dedicated translation · gpt-realtime-translate", live: "GPT-Live-1 · interpreter or assistant"};

// The parser guarantees integer milliseconds. Decimal placement preserves every
// digit even at the supported upper bound, without floating-point rounding.
function durationLabel(milliseconds: number): string {
  const digits = String(milliseconds).padStart(4, "0");
  return `${digits.slice(0, -3)}.${digits.slice(-3)} seconds`;
}

/** Fixed observations only. This component cannot grant or restart processing. */
export function SessionDiagnostics({view}: {view: View}) {
  if (view.role !== "operator" || !view.session || (!view.stopped && !view.aiPaused)) return null;
  const paused = !view.stopped, stop = view.diagnostics?.stop, issue = view.diagnostics?.last_issue;
  const services = view.session.provider_vendor === "gemini" ? {transcription: "Source captions · gemini-3.5-transcribe-live", live: "Gemini Live Translate", translation: "Unavailable"} : SERVICES;
  const heading = paused ? "pause-reason-heading" : "stop-reason-heading";
  const observed = view.terminalMessage ?? (paused ? view.meetIssue : null);
  return <section aria-labelledby={heading} className="stop-diagnostics">
    <h2 id={heading}>{paused ? "Why AI paused" : "Why the test stopped"}</h2>
    {observed && <p className="terminal-observation"><strong>This page observed:</strong> {observed}</p>}
    <div role="status" aria-live="polite">
      {view.diagnosticsState === "loading" ? <p>{paused ? "Checking the local server’s service record…" : "Checking the local server’s stop record…"}</p> : stop ? <p><strong>Server stop:</strong> {stopMessage(stop.code)} <span className="help">{elapsedLabel(stop.elapsed_seconds)}.</span></p> : paused ? <p>{view.diagnosticsState === "ready" ? "The server has not recorded a global Stop. AI remains paused; this observation does not permit a restart." : "Server service details are unavailable. The first interruption observed on this page is preserved; no cause is inferred."}</p> : <p>The server’s stop reason is unavailable. It may have exited, expired, or not yet recorded Stop. No reason is inferred.</p>}
      {issue && <p><strong>Last service issue:</strong> {providerMessage(issue.code)} <span className="help">{SOURCES[issue.source]}{issue.route ? ` · ${services[issue.route]}` : ""}; {elapsedLabel(issue.elapsed_seconds)}. {paused ? "This is the server’s last recorded issue; it may differ from the first interruption observed on this page." : "This is the last recorded issue, which may differ from the final stop trigger."}</span></p>}
      {issue?.asr_usage && <div>
        <p><strong>Duration check for this source-caption item</strong></p>
        <dl>
          <dt>Submitted duration</dt><dd>{durationLabel(issue.asr_usage.submitted_ms)}</dd>
          <dt>Provider-reported duration</dt><dd>{issue.asr_usage.reported_ms === null ? "Beyond the supported numeric range" : `${durationLabel(issue.asr_usage.reported_ms)} (rounded up to a whole millisecond)`}</dd>
          <dt>Local allowed upper bound</dt><dd>{durationLabel(issue.asr_usage.allowed_ms)}</dd>
        </dl>
        <p className="help">The extra 1.000 second is this application’s compatibility policy. It is not documented OpenAI rounding behavior or a record of billed usage.</p>
      </div>}
    </div>
    {paused && <p>Export retained captions and notes before restarting the service or reloading this page. Stop, signout and closing the page clear unsaved work. Generated interpretation is not included in the source-caption export.</p>}
    <p className="help">This record contains fixed status and timing metadata only, including bounded duration details when available. It remains available while this local process and your login are valid. A server Stop record does not confirm provider closure or final account charges.</p>
  </section>;
}
