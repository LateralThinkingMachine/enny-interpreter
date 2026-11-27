import {RustSelect, RustInput, RustTextarea} from "./rust-fields.js";
import {RustButton} from "./rust-button.js";
import {memo, useEffect, useRef, useState, useSyncExternalStore} from "react";
import {RustSurface} from "./rust-surface.js";
import {CaptionRows, fixedTarget} from "./rust-captions.js";
import {CostPanel} from "./CostPanel.js";
import {LiveCaptionPanel} from "./LiveCaptionPanel.js";
import {SourceDraftPanel} from "./SourceDraftPanel.js";
import {MeetSetup} from "./MeetSetup.js";
import {PrivateListeningPanel} from "./webrtc/PrivateListeningPanel.js";
import {providerMessage} from "./feedback.js";
import {StopReceiptPanel} from "./StopReceipt.js";
import {SessionDiagnostics} from "./SessionDiagnostics.js";
import {SessionTransition} from "./SessionTransition.js";
import type {SessionActions} from "./SessionTransition.js";
import {canPrepare, estimatedUsd, quoteDuration, ttsReservation, usd} from "./cost.js";
import {LIVE_TEXT_BYTES, PUBLIC_TEXT_BYTES, textFits, utf8Length} from "./text.js";
import type {FormEvent, RefObject} from "react";
import type {Fragment, PilotClient, View} from "./client.js";
import type {Language, LiveMode, Preview, Route, RouteKind, Source} from "./protocol.js";

import {LANGUAGE_LABELS as LANGUAGES, LEGACY_LANGUAGES, interpretationLabel} from "./languages.js";
const SOURCES: Record<Source, string> = {remote_mix: "Selected tab · remote mix", self: "My microphone · self"};
const ROUTES: Record<RouteKind, string> = {transcription: "Source captions · gpt-live-transcribe", translation: "Dedicated translation · gpt-realtime-translate", live: "GPT-Live-1 · interpreter or assistant"};
const LIVE_MODES: Record<LiveMode, string> = {interpretation: "GPT-Live-1 interpreter", assistant: "GPT-Live-1 assistant"};
const GEMINI_ROUTES: Record<RouteKind, string> = {transcription: "Source captions · gemini-3.5-transcribe-live", translation: "Unavailable", live: "Gemini Live Translate"};
function routeLabel(route: Pick<Route, "route" | "live_mode">, gemini = false): string {if (gemini) return GEMINI_ROUTES[route.route]; return route.route === "live" && route.live_mode ? LIVE_MODES[route.live_mode] : ROUTES[route.route];}
function LanguageSelect({value, change, id, fixed = false}: {fixed?: boolean; value: Language; change(value: Language): void; id: string}) {return <RustSelect id={id} value={value} onChange={event => change(event.target.value as Language)}>{Object.entries(LANGUAGES).filter(([key]) => !fixed || LEGACY_LANGUAGES.includes(key as Language)).map(([key, label]) => <option key={key} value={key}>{label}</option>)}</RustSelect>;}

export function App({client, sessionActions, preferredLanguage, onLanguageChange, focusNewSession = false, preferencesSaved}: {client: PilotClient; sessionActions?: SessionActions; preferredLanguage?: Language; onLanguageChange?(language: Language): void; focusNewSession?: boolean; preferencesSaved?: boolean}) {
  const view = useSyncExternalStore(client.subscribe, client.snapshot, client.snapshot);
  const [localLanguage, setLocalLanguage] = useState<Language>("en");
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);
  const exportPending = useRef(false);
  const main = useRef<HTMLElement>(null);
  const stoppedHeading = useRef<HTMLHeadingElement>(null);
  const wasStopped = useRef(view.stopped);
  useEffect(() => {if (focusNewSession) main.current?.querySelector<HTMLElement>("#meet-heading")?.focus();}, [focusNewSession]);
  useEffect(() => {
    const justStopped = !wasStopped.current && view.stopped;
    wasStopped.current = view.stopped;
    if (!justStopped) return;
    stoppedHeading.current?.focus({preventScroll: true});
    stoppedHeading.current?.closest<HTMLElement>(".stopped-panel")?.scrollIntoView({behavior: "instant", block: "start"});
  }, [view.stopped]);
  const language = preferredLanguage ?? localLanguage;
  const changeLanguage = onLanguageChange ?? setLocalLanguage;
  const actions = view.role === "operator" && view.session ? sessionActions : undefined;
  const stopConfirmed = view.stopAcknowledged;
  const exportNotes = async (finish = false): Promise<boolean> => {
    if (exportPending.current) return false;
    exportPending.current = true; setExporting(true); setExportError(null);
    try {
      const text = finish && !view.aiPaused && !view.manualPause ? await client.finishExport() : client.exportText();
      if (text === null) return false;
      const blob = new Blob([text], {type: "text/plain;charset=utf-8"});
      if (blob.size > 8 * 1024 * 1024) throw new Error("Export size limit");
      const url = URL.createObjectURL(blob);
      try {const link = document.createElement("a"); link.href = url; link.download = "private-call-pilot-notes.txt"; link.click();}
      finally {setTimeout(() => URL.revokeObjectURL(url), 0);}
      return true;
    } catch {setExportError("The export could not be prepared. A new session has not been started."); return false;}
    finally {exportPending.current = false; setExporting(false);}
  };
  return <div className={view.role === "public" ? "pilot public-page" : "pilot operator-page"}>
    <RustSurface kind="session"
      state={{operator: view.role === "operator", stopped: view.stopped, stopConfirmed, authenticated: !!view.session}}
      dispatch={name => {if (name === "stop") void client.stop(); else if (name === "logout") void client.logout();}}
      fallback={<header className="topbar"><a className="brand" href="#main"><span className="brand-mark" aria-hidden="true">E</span><span>Enny<small>{view.role === "operator" ? "Private operator" : "Approved public screen"}</small></span></a><div className="header-actions"><span className="pilot-badge">Supervised pilot · unverified</span>{!view.stopped ? <RustButton className="stop-button" onClick={() => {void client.stop();}}>{view.role === "public" ? "■ Stop public screen" : "■ Stop pilot"}</RustButton> : !stopConfirmed && <RustButton className="quiet" onClick={() => {void client.stop();}}>Retry server Stop</RustButton>}{view.session && <RustButton className="quiet" onClick={() => {void client.logout();}}>Sign out</RustButton>}</div></header>} />
    <main id="main" ref={main}>
      <p className="pilot-context">{view.role === "operator" ? "Supervised local pilot · Interpretation quality is unverified. Keep this page private." : "Approved public screen · Only explicitly approved content appears here."}</p>
      <div className={view.stopped ? "status stopped" : "status"} role="status" aria-live="polite">{view.message}</div>
      {exportError && view.role === "operator" && !actions?.available && <p className="notice unavailable" role="alert">{exportError}</p>}
      {view.stopped ? <StoppedPanel view={view} client={client} actions={actions} exporting={exporting} exportError={exportError} headingRef={stoppedHeading} /> : !view.session ? <section className="panel"><p className="eyebrow">Local access only</p><h1>Your companion, alongside the call.</h1><p>{view.busy ? "Verifying this one-use local access link…" : "Use the current access link from the local launcher. No API key belongs in this browser."}</p></section> : view.role === "operator" ? <Operator key={`operator:${view.session.generation}`} view={view} client={client} actions={actions} preferencesSaved={preferencesSaved} language={language} changeLanguage={changeLanguage} exportNotes={exportNotes} exporting={exporting} exportError={exportError} /> : <PublicScreen key={`public:${view.session.generation}`} view={view} client={client} />}
    </main><footer>{view.role === "operator" ? "Meeting content stays in session memory until an explicit export · Language preference stays on this browser" : "Approved public content only · No automatic publication"}</footer>
  </div>;
}

function StoppedPanel({view, client, actions, exporting, exportError, headingRef}: {view: View; client: PilotClient; actions?: SessionActions; exporting: boolean; exportError: string | null; headingRef: RefObject<HTMLHeadingElement | null>}) {
  return <section className="panel stopped-panel" aria-labelledby="stopped-heading"><p className="eyebrow">{view.role === "public" ? "Nothing is being captured" : "Local session"}</p><h1 id="stopped-heading" tabIndex={-1} ref={headingRef}>Session ended.</h1>
    <p>{view.role === "public" ? "Capture, queued sound and unsaved private text have been cleared." : "Unsaved private text has been cleared."} Your meeting app keeps its own microphone and presentation controls.</p>
    <StopReceiptPanel view={view} client={client} />
    {actions && <SessionTransition actions={actions} exporting={exporting} exportError={exportError} />}
    {view.role === "operator" && <details className="meet-details session-diagnostics-details"><summary>Session details</summary><SessionDiagnostics view={view} /></details>}
  </section>;
}

function Operator({view, client, actions, language: meetLanguage, changeLanguage, exportNotes, exporting, exportError, preferencesSaved}: {view: View; preferencesSaved?: boolean; client: PilotClient; actions?: SessionActions; language: Language; changeLanguage(language: Language): void; exportNotes(finish?: boolean): Promise<boolean>; exporting: boolean; exportError: string | null}) {
  const [source, setSource] = useState<Source>("remote_mix"), [route, setRoute] = useState<RouteKind>("transcription"), [language, setLanguage] = useState<Language>("ja");
  const [liveMode, setLiveMode] = useState<LiveMode>("interpretation");
  const [liveText, setLiveText] = useState(""), [liveRoute, setLiveRoute] = useState("");
  const [publicText, setPublicText] = useState(""), [publicLanguage, setPublicLanguage] = useState<Language>("ja");
  const capabilities = view.session!.capabilities;
  const gemini = view.session?.provider_vendor === "gemini";
  const meetActive = view.browserListening || view.meetPhase !== "idle" && view.meetPhase !== "failed";
  const allowed = {transcription: capabilities.source_captions, translation: capabilities.private_translation, live: liveMode === "interpretation" ? capabilities.live_interpreter : capabilities.live_assistant}[route];
  const quote = view.budget?.routes.find(item => item.route === route);
  const publicReservation = ttsReservation(view.budget, utf8Length(publicText));
  const readyLive = view.routes.filter(item => item.route === "live" && item.live_mode === "assistant" && item.state === "ready");
  const selectedLive = readyLive.some(item => item.route_id === liveRoute) ? liveRoute : readyLive[0]?.route_id ?? "";
  const submit = (event: FormEvent, action: () => void) => {event.preventDefault(); action();};
  const meetSetup = <MeetSetup view={view} client={client} sessionActions={actions} preferredLanguage={meetLanguage} onLanguageChange={changeLanguage} finish={() => {void exportNotes(true);}} />;
  return <>
    {client.privateListening && view.session?.continuous && view.session.providers_enabled && <PrivateListeningPanel controller={client.privateListening} budget={view.budgetFresh && !view.budgetRefreshing ? view.budget : null} blocked={view.stopped || view.finishing || view.aiPaused || !!view.manualPause || !view.browserListening && (view.meetSourceLocked || view.routes.some(route => route.state === "ready" || route.state === "starting"))} />}
    {view.browserListening && <p className="notice">The existing source-caption workflow is paused while private listening owns the audio. <a href="#private-listening-title">Private listening</a> shows the current connection and cost status.</p>}
    <fieldset style={{border: 0, minWidth: 0, padding: 0, margin: 0}} disabled={view.browserListening} hidden={view.browserListening}><legend className="sr-only">Source captions and existing interpretation workflow</legend>{meetSetup}</fieldset>
    {preferencesSaved === false && <p className="help" role="status">Your choices work for this session. Browser storage is unavailable, so they may reset when you reopen the page.</p>}
    {view.aiPaused && actions && <SessionTransition actions={actions} paused exporting={exporting} exportError={exportError} exportRetained={() => exportNotes(false)} />}
    {(view.earlierSegments.length > 0 || view.earlierSegmentsDropped > 0) && <section className="panel" aria-labelledby="earlier-segments-heading"><h2 id="earlier-segments-heading">Earlier captions · before Pause</h2><p className="help">Source captions retained from earlier processing segments. Paused intervals were not captured; these are not a continuous or complete transcript. {view.earlierSegmentsDropped} older segments discarded under the 8 segment / 2 MiB retention limit. Generated interpretation is not carried into a resumed segment.</p>{view.earlierSegments.map(segment => <details key={segment.generation}><summary>Earlier segment · {segment.items} source fragments</summary><pre className="earlier-source-text">{segment.text}</pre></details>)}</section>}
    <div className="workspace-grid"><section className="panel"><div className="section-heading"><div><p className="eyebrow">Source record</p><h2>Private captions</h2></div><span className="tag">Source ASR only</span></div><p className="help">Words from the selected source, in its spoken language. {gemini && <>Source model: gemini-3.5-transcribe-live. </>} Check names, numbers and speaker attribution; provisional captions may change.</p><SourceDraftPanel drafts={view.sourceDrafts}/><Fragments fragments={view.sourceFragments} empty="Source captions from your selected audio appear here after you start." /><p className="help">Showing the latest 100 of {view.sourceFragments.length} retained source fragments. {view.session?.continuous ? `Rolling retention holds at most 10,000 source items / 4 MiB. ${view.sourceDiscarded} sealed earlier-window items discarded; export includes only retained content.` : "Up to 10,000 fragments / 4 MiB stay in memory for export; reaching capacity closes processing with a visible gap."}</p></section>
      <section className="panel"><div className="section-heading"><div><p className="eyebrow">In your language</p><h2>Private interpretation</h2></div><span className="tag">Unverified generated output</span></div><LiveCaptionPanel groups={view.liveCaptions} discarded={view.liveCaptionDropped}/>{(view.fragments.length > 0 || view.liveCaptions.length === 0) && <Fragments gemini={gemini} fragments={view.fragments} empty="Your requested-language interpretation appears here after you start." />}{view.fragments.length > 0 && <p className="help">Latest 100 legacy or dedicated-translation fragments displayed. {view.generatedDropped} older fragments discarded. These are not the source archive.</p>}{!gemini && <details className="assistant-details"><summary>Private assistant context</summary><fieldset className="advanced-controls" disabled={meetActive || view.finishing}><legend className="sr-only">Assistant context controls</legend><form onSubmit={event => submit(event, () => {const text = liveText; setLiveText(""); void client.liveText(selectedLive, text);})}><label htmlFor="live-route">Ready assistant route<RustSelect id="live-route" value={selectedLive} onChange={event => setLiveRoute(event.target.value)}>{!readyLive.length && <option value="">No ready assistant route</option>}{readyLive.map(item => <option key={item.route_id} value={item.route_id}>{SOURCES[item.source]}</option>)}</RustSelect></label><label htmlFor="live-text">Private assistant context<RustTextarea id="live-text" value={liveText} onChange={event => setLiveText(event.target.value)} maxLength={LIVE_TEXT_BYTES} aria-invalid={utf8Length(liveText) > LIVE_TEXT_BYTES} aria-describedby="live-text-limit" rows={3} placeholder="A question or context for your private assistant…" /></label><p className="help">Typed questions and context go only to assistant routes. Interpreter routes receive selected-source speech only.</p><p id="live-text-limit" className="help">{utf8Length(liveText)} / {LIVE_TEXT_BYTES} UTF-8 bytes. Japanese and Hindi letters may use multiple bytes.</p><RustButton type="submit" disabled={view.busy || !view.connected || !selectedLive || !textFits(liveText, LIVE_TEXT_BYTES)}>Send privately</RustButton></form></fieldset></details>}</section></div>
    <RustSurface kind="notes"
      state={{notes: view.notes, finishDisabled: view.browserListening || view.finishing || (!view.connected && !view.aiPaused),
        finishLabel: view.finishing ? "Finishing source captions…" : view.aiPaused ? "Export retained captions & notes" : "Finish captions & export notes"}}
      dispatch={(name, value) => {
        if (name === "notes") client.setNotes(value);
        else if (name === "finish") void exportNotes(true);
        else if (name === "export") void exportNotes();
      }} fallback={<section className="panel"><div className="section-heading"><div><p className="eyebrow">Your own draft</p><h2>Private notes</h2></div><div className="button-row"><RustButton className="quiet" onClick={() => {void exportNotes();}}>Export private captions & notes (.txt)</RustButton><RustButton id="notes-finish-export" disabled={view.browserListening || view.finishing || (!view.connected && !view.aiPaused)} onClick={() => {void exportNotes(true);}}>{view.finishing ? "Finishing source captions…" : view.aiPaused ? "Export retained captions & notes" : "Finish captions & export notes"}</RustButton></div></div><label htmlFor="private-notes">Manual draft · not shared or automatically inferred<RustTextarea id="private-notes" value={view.notes} onChange={event => client.setNotes(event.target.value)} maxLength={20_000} rows={7} placeholder="Write your own notes. Verify names, numbers, decisions and commitments." /></label><p className="help">Only this page’s memory holds these notes. Export deliberately creates a local file. Finish stops capture and sound immediately, waits up to 35 seconds for final source captions, then exports and stops the pilot. An incomplete finish is labeled in the file. Stop and signout cancel a pending finish and clear unsaved work.</p></section>} />
    <details className="panel advanced-panel"><summary>Advanced audio &amp; manual routes</summary><fieldset className="advanced-controls" disabled={meetActive || view.finishing}>
      <legend className="sr-only">Manual audio controls</legend>
      <div className="connection"><span className={view.connected ? "connection-dot on" : "connection-dot"} />{view.connected ? "Private stream connected" : "Private stream closed"}<RustButton disabled={view.busy || view.connected} onClick={() => {void client.connect();}}>Connect private stream</RustButton></div><div className="section-heading"><div><p className="eyebrow">01 · Listen intentionally</p><h2>Sources & private routes</h2></div><span className="tag">Nothing captures automatically</span></div>
      <form className="route-form" onSubmit={event => submit(event, () => {void client.openRoute(source, route, language, route === "live" ? liveMode : null);})}>
        <label htmlFor="route-source">Source<RustSelect id="route-source" value={source} onChange={event => setSource(event.target.value as Source)}><option value="remote_mix">Selected tab · remote mix</option><option value="self">My microphone · self</option></RustSelect></label>
        <label htmlFor="route-kind">Private service<RustSelect id="route-kind" value={route} onChange={event => setRoute(event.target.value as RouteKind)}>{Object.entries(gemini ? GEMINI_ROUTES : ROUTES).filter(([key]) => !gemini || key !== "translation").map(([key, label]) => <option key={key} value={key}>{label}</option>)}</RustSelect></label>
        {route === "live" && !gemini && <label htmlFor="live-mode">GPT-Live-1 mode<RustSelect id="live-mode" value={liveMode} onChange={event => setLiveMode(event.target.value as LiveMode)}>{Object.entries(LIVE_MODES).map(([key, label]) => <option key={key} value={key}>{label}</option>)}</RustSelect></label>}
        {route === "transcription" ? <p className="help">Multilingual source ASR · no fixed language filter. Japanese, English and Hindi quality still needs verification.</p> : <label htmlFor="route-language">Requested output language<LanguageSelect fixed={gemini} id="route-language" value={language} change={setLanguage} /></label>}
        <p id="route-cost" className="route-quote">{view.budgetFresh && quote?.quote_microusd !== null && quote?.quote_microusd !== undefined ? <>Estimated local reservation for {quote.model} is <strong>US{estimatedUsd(quote.quote_microusd)}</strong> for up to {quoteDuration(quote.quote_seconds!)}. {gemini ? "Google" : "OpenAI"} can bill the account that owns your API key when the connection opens, even during silence. Enny charges no fee. {quote.can_reserve ? "The server checks current limits again when you open it." : "This route cannot be reserved within the current limits."}</> : "A current server reservation quote is required before opening a route."}</p>
        <RustButton type="submit" aria-describedby="route-cost" disabled={view.busy || !view.connected || !allowed || !view.budgetFresh || !quote?.can_reserve || view.capture[source] === "closed" || view.routes.filter(item => item.state === "ready" || item.state === "starting").length >= 6}>{route === "live" && liveMode === "interpretation" ? `Start live interpretation · billed by your own ${gemini ? "Google" : "OpenAI"} account` : "Open selected route"}</RustButton>
      </form>{route === "live" && <p className="help">{liveMode === "interpretation" ? "Interpreter mode translates selected-source speech into the requested output language. It is not a question-answering assistant. Source captions require a separate Source captions route." : "Assistant mode can answer and use private typed context. Use Interpreter mode for faithful speech interpretation."} Each source supports one Live mode at a time. Close its Live route before deliberately opening a different mode; its reservation is not refunded.</p>}{!allowed && <p className="notice unavailable">The selected service or mode is not enabled in this activation. Opening another mode does not grant its permission.</p>}<p className="help">A ready route is not a language-quality guarantee. Open only the services and sources you intend to send to the provider; every open route may incur cost.</p>
      <p className="help">Use the participant agreement checkbox in meeting setup above before selecting any source.</p><div className="source-grid">{(["remote_mix", "self"] as const).map(item => <article className="source-card" key={item}><h3>{SOURCES[item]}</h3><p>{item === "remote_mix" ? "Choose the tab containing your meeting and explicitly share its audio. Missing tab audio is refused; this never switches to your microphone." : "Optional: your own voice is a separate source. A remote tab may not include it. This leaves the meeting app’s microphone setting unchanged."}</p><p className="source-state">Capture: <strong>{view.capture[item]}</strong></p><div className="button-row"><RustButton disabled={view.busy || !view.connected || view.capture[item] !== "idle" || !view.captureConfirmed || !view.routes.some(route => route.source === item && route.state === "ready")} onClick={() => {void client.capture(item);}}>Select {item === "remote_mix" ? "tab audio" : "microphone"}</RustButton><RustButton className="negative" disabled={view.capture[item] === "closed"} onClick={() => client.gap(item)}>Stop this source</RustButton></div></article>)}</div>
      {view.routes.length > 0 && <ul className="route-list" aria-label="Current private routes">{view.routes.map(item => <li key={item.route_id}><span><strong>{routeLabel(item, gemini)}</strong> · {SOURCES[item.source]} · {item.route === "transcription" ? "multilingual source" : interpretationLabel(item.interpretation, item.language)}<small>{item.state}{item.expires_in_seconds !== null ? ` · ${item.expires_in_seconds}s left; renewal is manual` : ""}{item.code ? ` · ${providerMessage(item.code)}` : ""}</small></span><RustButton className="quiet" disabled={view.finishing || item.state === "closed" || item.state === "failed"} onClick={() => {void client.closeRoute(item.route_id);}}>Close route</RustButton></li>)}</ul>}
      <div className="private-audio"><div><h3>Private sound · headphones only</h3><p>Muted sound is discarded; provider processing and its cost continue while the route stays open. Close the route to end processing. Enabling sound never replays the muted interval. Each sound control covers routes of the named service and mode, across both sources; interpreter and assistant sound are independent.</p></div>{(["translation", "live_interpretation", "live"] as const).filter(kind => !gemini || kind === "live_interpretation").map(kind => <RustButton key={kind} aria-pressed={!view.muted[kind]} disabled={view.finishing || !view.connected} onClick={() => {void client.mute(kind, !view.muted[kind]);}}>{view.muted[kind] ? "Enable" : "Mute"} {kind === "translation" ? "dedicated translation sound" : kind === "live_interpretation" ? (gemini ? "Gemini interpreter sound" : "GPT-Live-1 interpreter sound") : "GPT-Live-1 assistant sound"}</RustButton>)}</div>
    </fieldset>{meetActive && <p className="help">Manual controls are paused during meeting setup and processing. Cancel the local audio check to switch workflows, or finish the active test.</p>}</details>
    <details className="panel advanced-panel"><summary>Cost &amp; limits</summary><CostPanel view={view} client={client} /></details>
    {!gemini && <details className="panel public-composer advanced-panel"><summary>Public presentation</summary><fieldset className="advanced-controls" disabled={meetActive || view.finishing}><legend className="sr-only">Public presentation controls</legend><div className="section-heading"><div><p className="eyebrow">05 · Review before the room hears</p><h2>Approved public presentation</h2></div><span className="tag">A separate public tab</span></div><p>Prepare exact words and a complete clip. Audition privately, acknowledge that review, and approve once. Live voice and private notes never become public output automatically.</p><div className="button-row"><RustButton disabled={view.busy || !view.connected} onClick={() => {void client.issuePublicTicket();}}>Prepare public-screen link</RustButton><RustButton disabled={!view.publicTicketReady || view.busy} onClick={() => {client.openPublicScreen();}}>Open one-use public screen</RustButton></div><p className="help">In that tab, explicitly enable the screen. Share only that public tab with audio using your meeting app’s sharing controls. A local playback receipt does not prove the other participants heard it.</p>
      {!capabilities.prepared_public_speech && <p className="notice unavailable">Prepared public speech is unavailable in this activation. No public clip can be prepared; opening the separate screen does not enable speech.</p>}<form onSubmit={event => submit(event, () => {void client.prepare(publicText, publicLanguage);})}><label htmlFor="public-text">Exact proposed public words<RustTextarea id="public-text" value={publicText} onChange={event => setPublicText(event.target.value)} maxLength={PUBLIC_TEXT_BYTES} aria-invalid={utf8Length(publicText) > PUBLIC_TEXT_BYTES} aria-describedby="public-text-limit" rows={5} placeholder="Only the text you intend the room to hear…" /></label><p id="public-text-limit" className="help">{utf8Length(publicText)} / {PUBLIC_TEXT_BYTES} UTF-8 bytes. Shorten text above this limit before preparing speech.</p><div className="button-row"><label htmlFor="public-language">Speech language<LanguageSelect id="public-language" value={publicLanguage} change={setPublicLanguage} /></label><RustButton type="submit" aria-describedby="speech-cost" disabled={view.busy || !view.connected || !capabilities.prepared_public_speech || !view.budgetFresh || !canPrepare(view.budget, utf8Length(publicText)) || !textFits(publicText, PUBLIC_TEXT_BYTES)}>Prepare immutable preview</RustButton></div><p id="speech-cost" className="help">{publicReservation === null ? "Enter bounded text to estimate the speech reservation." : <>Preparing this exact text reserves <strong>{usd(publicReservation)}</strong>. This conservative UTF-8 byte reservation is not the provider’s character invoice; the server rechecks available funds and request limits.</>}</p></form>
      {view.preview && <PreviewCard key={view.preview.id} preview={view.preview} view={view} client={client} />}
      <p className="receipt" role="status">{view.receipt}</p>
    </fieldset></details>}
    <details className="panel limitations"><summary>Current capability limitations</summary><ul>{view.session!.limitations.map((limitation, index) => <li key={index}>{limitation}</li>)}</ul><p>Do not make the stakeholder call the first capture, language or remote-playback trial.</p></details>
  </>;
}

const Fragments = memo(function Fragments({fragments, empty, limit = 100, gemini = false}: {gemini?: boolean; fragments: readonly Fragment[]; empty: string; limit?: number}) {
  const visible = fragments.slice(-limit);
  const rows = visible.map(fragment => ({
    key: fragment.key, source: SOURCES[fragment.source], text: fragment.text, fixed: fixedTarget(fragment),
    status: (fragment.route === "live"
      ? `${routeLabel(fragment, gemini)} · ${fragment.provenance === "live_input" ? "input transcript · not source ASR" : "generated output"}`
      : fragment.provenance === "translation" ? "Dedicated translation · generated output" : fragment.provenance.replaceAll("_", " "))
      + (fragment.provenance === "translation" || fragment.provenance === "live_output" ? ` · requested ${interpretationLabel(fragment.interpretation, fragment.language)}` : "")
      + ` · ${fragment.final ? "provider final" : fragment.incomplete ? "incomplete at closure" : "provisional · may change"}`,
  }));
  return <CaptionRows rows={rows} empty={empty} label="Private received fragments"/>;
});

function PreviewCard({preview, view, client}: {preview: Preview; view: View; client: PilotClient}) {
  const [acknowledged, setAcknowledged] = useState(false);
  return <article className="preview-card" aria-label="Immutable public preview"><p className="eyebrow">Exact prepared preview · {preview.state}</p><h3>{LANGUAGES[preview.language]} · {(preview.duration_ms / 1000).toFixed(1)} seconds</h3><p className="prepared-words">{preview.text}</p><p className="digest">Content digest <code>{preview.digest}</code></p><div className="button-row"><RustButton disabled={view.busy || preview.state !== "prepared"} onClick={() => {setAcknowledged(false); void client.audition();}}>{view.auditioning ? "Audition playing…" : "Audition exact clip privately"}</RustButton><span>{view.auditionComplete ? "Local audition reached its end" : "Complete a private audition before acknowledgement"}</span></div><label className="check"><RustInput type="checkbox" checked={acknowledged} disabled={!view.auditionComplete || view.busy || preview.state !== "prepared"} onChange={event => setAcknowledged(event.target.checked)} />I reviewed these exact words and listened to this clip. This is my acknowledgement, not proof of anyone else hearing it.</label><div className="button-row"><RustButton disabled={view.busy || !acknowledged || !view.auditionComplete || preview.auditioned || preview.state !== "prepared"} onClick={() => {void client.acknowledge();}}>Acknowledge this audition</RustButton><RustButton className="approve-button" disabled={view.busy || !acknowledged || !view.auditionComplete || !preview.auditioned || preview.state !== "prepared" || view.approvalAttempted} onClick={() => {void client.approve();}}>Approve & offer once to public screen</RustButton></div><p className="help">A failed or unknown attempt is never retried automatically. Prepare a new preview for a deliberate correction.</p></article>;
}

function PublicScreen({view, client}: {view: View; client: PilotClient}) {
  // The viewer projection contains only explicitly approved delivery data.
  const delivery = view.delivery;
  return <RustSurface kind="viewer" state={{connected: view.connected, connectDisabled: view.busy || view.connected,
    hasDelivery: !!delivery, language: delivery?.language, languageLabel: delivery ? LANGUAGES[delivery.language] : "",
    words: delivery?.text, receipt: delivery?.state.replaceAll("_", " ")}}
    dispatch={name => {if (name === "connect") void client.connect();}} fallback={<section className="panel presentation"><div className="section-heading"><div><p className="eyebrow">Approved public screen</p><h1>{view.delivery ? "For the conversation." : "Ready when you approve."}</h1></div><span className="tag">{view.connected ? "Viewer connected" : "Viewer closed"}</span></div><p>Share only this tab with audio in your meeting app. This screen receives no private captions, notes or Live audio. Confirm actual remote hearing on a second device before the call.</p><RustButton className="arm-button" disabled={view.busy || view.connected} onClick={() => {void client.connect();}}>Enable this public screen & sound</RustButton>{view.delivery ? <article className="public-delivery" lang={view.delivery.language}><p className="eyebrow">Approved wording · {LANGUAGES[view.delivery.language]}</p><div className="presentation-words">{view.delivery.text}</div><p className="receipt">{view.delivery.state.replaceAll("_", " ")} · local browser report only</p><p className="help">No replay control. A new statement requires a new operator preview and approval.</p></article> : <div className="public-empty"><span aria-hidden="true">◌</span><h2>No approved statement yet.</h2><p>Reloading never replays a prior clip.</p></div>}</section>} />;
}
