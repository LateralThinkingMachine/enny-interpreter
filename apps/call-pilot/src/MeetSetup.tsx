import {RustSelect, RustInput} from "./rust-fields.js";
import {RustButton} from "./rust-button.js";
import {RustSurface} from "./rust-surface.js";
import type {SessionActions} from "./SessionTransition.js";
import {LEGACY_LANGUAGES} from "./languages.js";
import {SpeechSettings} from "./SpeechSettings.js";
import {useState} from "react";
import type {PilotClient, View} from "./client.js";
import {SessionDiagnostics} from "./SessionDiagnostics.js";
import {CostNotices} from "./CostNotices.js";
import {MeetFeedback} from "./MeetFeedback.js";
import {estimatedUsd, meetReservation, quoteDuration, usd} from "./cost.js";
import type {Language, Source} from "./protocol.js";

import {LANGUAGE_LABELS as LANGUAGES, interpretationLabel} from "./languages.js";

/** Start owns the browser gesture; signal strength is feedback, never permission. */
export function MeetSetup({view, client, finish, preferredLanguage, onLanguageChange, sessionActions}: {sessionActions?: SessionActions; view: View; client: PilotClient; finish(): void; preferredLanguage?: Language; onLanguageChange?(language: Language): void}) {
  const [localLanguage, setLocalLanguage] = useState<Language>("en");
  const language = preferredLanguage ?? localLanguage;
  const setLanguage = (next: Language) => {
    (onLanguageChange ?? setLocalLanguage)(next);
    if (view.interpretation?.mode === "auto_to_target") client.setInterpretation({...view.interpretation, target: next});
  };
  const policy = view.interpretation;
  const interpretationTitle = interpretationLabel(policy ?? undefined, language);
  const pausedByUser = !!view.manualPause;
  const pauseReady = view.manualPause?.resume_allowed && view.manualAudioRetired && !view.manualPauseChecking;
  const chooseMode = (mode: string) => {
    if (mode === "fixed") {client.setInterpretation(null); if (!LEGACY_LANGUAGES.includes(language)) setLanguage("en");}
    else if (mode === "two_way") client.setInterpretation({schema_version: "interpretation.v1", mode, first: "ja", second: language === "ja" ? "en" : language});
    else if (mode === "auto_to_target") client.setInterpretation({schema_version: "interpretation.v1", mode, target: language});
  };
  const phase = view.meetPhase;
  const microphone = view.meetSource === "self";
  const sourceLabel = microphone ? "My microphone" : "Meeting tab · other people";
  const preview = phase === "preview";
  const starting = phase === "connecting" || phase === "starting_captions" || phase === "starting_interpreter";
  const recovering = phase === "recovering";
  const listening = phase === "listening";
  const active = starting || listening || recovering;
  const manualActive = phase === "idle" && (view.routes.some(route => route.state === "ready" || route.state === "starting") || view.capture.remote_mix === "capturing" || view.capture.self === "capturing");
  const pendingRelease = phase === "idle" && view.meetSourceLocked;
  const available = (phase === "idle" || preview) && !pendingRelease && !manualActive && !view.busy && !view.finishing && !view.aiPaused && !pausedByUser && !view.stopped && view.capture[view.meetSource] !== "closed";
  const status = view.captureStatus[view.meetSource];
  const reservation = view.budgetFresh && !view.budgetRefreshing ? meetReservation(view.budget) : null;
  const enabled = view.session?.providers_enabled && view.session.capabilities.source_captions && view.session.capabilities.live_interpreter;
  const rehearsal = view.session?.providers_enabled === false;
  const canStart = available && !!enabled && reservation !== null && !view.speechHintsError && (!!policy || LEGACY_LANGUAGES.includes(language));
  const seconds = view.budget?.remaining_seconds ?? view.session?.expires_in_seconds;
  const duration = seconds == null ? null : `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
  const stateLabel = view.stopped ? "Session ended" : pausedByUser ? view.manualPause?.state === "paused" ? "Interpretation paused" : view.manualPause?.state === "closure_unconfirmed" ? "Pause · closure unconfirmed" : "Closing paid connections…" : view.finishing ? "Finishing source captions…" : view.aiPaused ? "AI paused · session retained" : recovering ? "Reconnecting…" : starting ? "Connecting…" : listening ? status?.muted ? "Listening · waiting for audio" : "Listening" : phase === "failed" || view.meetIssue ? "Setup needs attention" : preview ? status ? microphone ? "Microphone selected" : "Meeting tab selected" : microphone ? "Allow microphone access in your browser" : "Choose your meeting tab" : view.session?.providers_enabled ? "Ready when you are" : "Local audio check available";
  const gemini = view.session?.provider_vendor === "gemini";
  const provider = gemini ? "Google Gemini" : "OpenAI";
  const payer = gemini ? "Google" : "OpenAI";
  const startLabel = view.aiPaused ? "AI paused" : view.session ? `Start live interpretation · billed by your own ${payer} account` : "Waiting for session";
  const quoteSeconds = view.budget?.routes.find(route => route.route === "live")?.quote_seconds;
  const quoteTime = quoteSeconds == null ? "" : quoteDuration(quoteSeconds);
  const costTitle = rehearsal ? "Free rehearsal · US$0.00" : reservation === null ? "Live estimate unavailable" : `${gemini ? "Estimated local reservation" : "Estimated"} US${estimatedUsd(reservation)} for ${view.session?.continuous ? "each" : "up to"} ${quoteTime} of captions and interpretation`;
  const costHelp = rehearsal ? "Audio stays on this device. You pay Enny and the speech provider nothing."
    : !view.session ? "Wait for Enny to load your session and its estimate."
    : `Opening Enny is free. Enny charges no fee for live interpretation. After you start and allow audio, ${payer} can bill the account that owns your API key while the speech connections are open, even during silence. `
      + (view.session.continuous ? "Connections renew automatically until you stop. There is no total spending cap. " : view.budget?.amounts ? `Your local estimate limit is US${usd(view.budget.amounts.approved_microusd)}. ` : "")
      + (gemini ? "The reservation uses a preview rate. It does not quote Google’s price. " : "")
      + (reservation === null ? "Wait for a current estimate that covers both services before starting." : `${payer} determines the actual charge, which can differ from this estimate.`);
  const start = () => {
    if (!canStart || reservation === null) return;
    client.confirmCapture(true);
    void client.startMeetSession(language, reservation);
  };
  const consentLabel = rehearsal ? "Everyone agrees to this local audio check." : "Everyone agrees to AI processing. I’m wearing headphones and understand interpretation can make mistakes.";
  const checkLabel = preview ? microphone ? "Release microphone" : "Release selected tab" : rehearsal ? "Start free rehearsal" : microphone ? "Check microphone locally" : "Check tab audio locally";
  const checkAudio = () => {
    if (!available || !view.captureConfirmed) return;
    if (preview) client.cancelMeetSetup(); else void client.selectMeetAudio();
  };
  const audioCheck = <RustSurface kind="audio-check" state={{consent: view.captureConfirmed, available, finishing: view.finishing, rehearsal, consentLabel, label: checkLabel}}
    dispatch={(action, value) => {
      if (action === "consent") client.confirmCapture(value === "true");
      else if (action === "check") checkAudio();
    }} fallback={<><label className="check meet-consent"><RustInput id="capture-agreement" type="checkbox" checked={view.captureConfirmed} disabled={view.finishing} onChange={event => client.confirmCapture(event.target.checked)} /><span>{consentLabel}</span></label>
      <div className="button-row"><RustButton id="choose-meet-tab" aria-describedby={rehearsal ? "meet-start-estimate" : undefined} className={rehearsal ? "primary-button" : undefined} disabled={!available || !view.captureConfirmed} onClick={checkAudio}>{checkLabel}</RustButton></div></>} />;

  return <section className="panel meet-setup" aria-labelledby="meet-heading">
    <div className="meet-heading"><div><p className="eyebrow">Your private companion</p><h1 id="meet-heading" tabIndex={-1}>Interpret your meeting.</h1><p>See the original words and hear their interpretation in your language.</p></div><span className="tag">Headphones only</span></div>
    {!active && !pausedByUser && <div className="meet-start-area">
      {view.session?.repeatable_sessions && <div className="interpretation-settings"><label htmlFor="interpretation-mode">Interpretation mode<RustSelect id="interpretation-mode" value={policy?.mode ?? "fixed"} disabled={!available} onChange={event => chooseMode(event.target.value)}><option value="fixed">Fixed target · existing mode</option><option value="two_way">Two-way language pair</option><option value="auto_to_target">Auto → chosen language</option></RustSelect></label>{policy?.mode === "two_way" && <div className="interpretation-pair"><label htmlFor="pair-first">First language<RustSelect id="pair-first" value={policy.first} disabled={!available} onChange={event => client.setInterpretation({...policy, first: event.target.value as Language})}>{Object.entries(LANGUAGES).map(([value, label]) => <option key={value} value={value} disabled={value === policy.second}>{label}</option>)}</RustSelect></label><span aria-hidden="true">↔</span><label htmlFor="pair-second">Second language<RustSelect id="pair-second" value={policy.second} disabled={!available} onChange={event => client.setInterpretation({...policy, second: event.target.value as Language})}>{Object.entries(LANGUAGES).map(([value, label]) => <option key={value} value={value} disabled={value === policy.first}>{label}</option>)}</RustSelect></label></div>}</div>}
      {!policy && !LEGACY_LANGUAGES.includes(language) && <p className="meet-hint">Choose Two-way language pair or Auto → chosen language to use your saved Spanish or Vietnamese target.</p>}
      {policy && <p className="meet-hint">{policy.mode === "two_way" ? "One translation into the other selected language. Direction is requested automatically; no per-speaker microphone switch." : "Requests automatic source-language recognition and translation into the chosen language. Speech already in the target language should stay silent."} Language recognition and fidelity need listening validation, including Spanish and Vietnamese. Detected source language is not independently reported by this connection.</p>}
      {!view.aiPaused && <div id="meet-start-estimate" className="meet-cost"><strong>{costTitle}</strong><span>{costHelp}</span></div>}
      <RustSurface kind="setup" state={{source: view.meetSource, sourceDisabled: !available || preview || view.meetSourceLocked,
        language: policy?.mode === "auto_to_target" ? policy.target : language, showLanguage: policy?.mode !== "two_way", languageLabel: rehearsal ? "Language for live sessions" : "Interpret into", languageDisabled: !available,
        canStart, rehearsal, startLabel, paused: view.aiPaused, languages: [
          ...(!policy && !LEGACY_LANGUAGES.includes(language) ? [{value: language, label: LANGUAGES[language] + " · choose Auto mode", disabled: true}] : []),
          ...Object.entries(LANGUAGES).filter(([value]) => !!policy || LEGACY_LANGUAGES.includes(value as Language)).map(([value, label]) => ({value, label}))
        ]}}
        dispatch={(action, value) => {
          if (action === "source" && (value === "remote_mix" || value === "self")) client.setMeetSource(value);
          else if (action === "language" && Object.hasOwn(LANGUAGES, value)) setLanguage(value as Language);
          else if (action === "start") start();
        }} fallback={<div className="meet-start-row"><label htmlFor="meet-source">Listen to<RustSelect id="meet-source" value={view.meetSource} disabled={!available || preview || view.meetSourceLocked} aria-describedby="meet-source-help" onChange={event => client.setMeetSource(event.target.value as Source)}><option value="remote_mix">Meeting tab · other people</option><option value="self">My microphone · my voice</option></RustSelect></label>{policy?.mode !== "two_way" && <label htmlFor="meet-language">{rehearsal ? "Language for live sessions" : "Interpret into"}<RustSelect id="meet-language" value={policy?.mode === "auto_to_target" ? policy.target : language} disabled={!available} onChange={event => setLanguage(event.target.value as Language)}>{!policy && !LEGACY_LANGUAGES.includes(language) && <option value={language} disabled>{LANGUAGES[language]} · choose Auto mode</option>}{Object.entries(LANGUAGES).filter(([value]) => !!policy || LEGACY_LANGUAGES.includes(value as Language)).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</RustSelect></label>}
        {!rehearsal && <RustButton id="start-meet" className="primary-button" aria-describedby={view.aiPaused ? undefined : "meet-start-estimate meet-start-agreement meet-start-help"} disabled={!canStart} onClick={start}>{startLabel}</RustButton>}</div>} />
      <p id="meet-source-help" className="meet-source-help">{microphone ? <>Your browser will request access to your microphone. Your meeting app keeps its own mute setting.</> : <>The meeting tab captures <strong>other people’s voices</strong>. To interpret your own voice, choose <strong>My microphone</strong>.</>}{preview && <> Release the selected audio below before changing sources.</>}</p>
      {rehearsal && !view.aiPaused && <><p className="meet-hint">Check the input meter on this device. Live captions and interpretation require a new session approved with your own provider API key.</p>{audioCheck}</>}
      {!rehearsal && !view.aiPaused && <><p id="meet-start-agreement" className="meet-start-agreement">By starting, I confirm everyone has agreed to sending this audio to {provider} for source captions and live interpretation. Interpretation will play through my headphones.</p><p id="meet-start-help" className="meet-hint">{preview ? "Your audio is selected. Start connects captions and live interpretation, even while it is quiet." : microphone ? "Press Start, allow microphone access, then speak. Captions and live interpretation connect together." : <>Open your call in a <strong>browser tab</strong>. Select that tab and turn on <strong>Share tab audio</strong> when the browser asks.</>}</p></>}
      {manualActive && <p className="meet-hint">Manual audio routes are already in use. Finish the current test before starting guided meeting setup.</p>}
    </div>}
    {!active && !pausedByUser && <SpeechSettings view={view} client={client} disabled={!available} />}
    <div className={`meet-progress${active ? " active" : ""}`} role="status" aria-live="polite" aria-atomic="true">
      <span className={`meet-status-mark${starting || recovering ? " connecting" : listening ? " listening" : ""}`} aria-hidden="true"><span /><span /><span /></span>
      <div><strong>{stateLabel}</strong>{active && <p className="meet-active-source">{sourceLabel} · {interpretationTitle}</p>}{listening && <p>{microphone ? "Speak into this Mac’s microphone. Pauses are welcome." : "Waiting for the other person’s speech. Your microphone is not included."}</p>}{recovering && <p>Keep this tab open. Reconnecting the speech service; audio during this gap may be missed.</p>}{starting && <p>Connecting your selected audio to captions and private interpretation.</p>}{view.aiPaused && <p>Your captions and notes are still here. Review the reason below and export before leaving.</p>}</div>
      {active && <RustSurface kind="transport" state={{pause: !!view.session?.repeatable_sessions}}
        dispatch={name => {if (name === "stop") void client.stop(); else if (name === "pause") void client.pauseInterpretation();}}
        fallback={<>{active && view.session?.repeatable_sessions && <RustButton id="pause-interpretation" onClick={() => {void client.pauseInterpretation();}}>Pause interpretation</RustButton>}
      {active && <RustButton id="stop-meet" className="stop-button" onClick={() => {void client.stop();}}>Stop</RustButton>}</>} />}
    </div>
    {!active && !pausedByUser && view.busy && view.session?.repeatable_sessions && <RustButton id="pause-interpretation" onClick={() => {void client.pauseInterpretation();}}>Cancel setup &amp; pause</RustButton>}
    {pausedByUser && <section className="interpretation-pause" aria-labelledby="interpretation-pause-heading"><h2 id="interpretation-pause-heading">Your meeting can continue while interpretation is paused.</h2><p>No new audio is captured or replayed. {view.manualPause?.provider_closed_confirmed ? "The service confirmed the paid speech connections closed. Earlier usage may still appear on a later bill." : "Paid connection closure is not confirmed yet. This is not a claim of zero remaining charges."}</p>{!view.manualAudioRetired && <p>Close any open browser permission or sharing prompt, then check pause status to confirm local audio release.</p>}<div className="button-row"><RustButton id="resume-interpretation" className="primary-button" aria-describedby="resume-interpretation-agreement" disabled={!pauseReady || !sessionActions?.resume || sessionActions.creating} onClick={() => sessionActions?.resume?.()}>{sessionActions?.creating ? "Preparing resume…" : "Resume interpretation"}</RustButton><RustButton id="check-interpretation-pause" disabled={view.manualPauseChecking || sessionActions?.creating} onClick={() => {void client.checkInterpretationPause();}}>{view.manualPauseChecking ? "Checking closure…" : "Check pause status"}</RustButton></div><p id="resume-interpretation-agreement" className="meet-hint">Resume retains source captions and notes. In this browser version, press Start once more to select fresh audio; your browser requires a new permission gesture. Interpretation from before Pause is not replayed or carried into the new segment.</p>{sessionActions?.error && <p className="notice unavailable" role="alert">{sessionActions.error}</p>}</section>}
    {view.session?.providers_enabled && !enabled && <p className="notice unavailable">This launch needs both source captions and live interpreter mode. Check the local AI settings before restarting.</p>}
    <SessionDiagnostics view={view} />
    {view.meetIssue && !view.aiPaused && <p className="notice unavailable meet-issue" role="alert">{view.meetIssue}</p>}
    {pendingRelease && !view.aiPaused && !view.finishing && <div className="meet-release"><p>Dismiss any open browser permission or sharing prompt, then release the previous selection before starting again.</p><RustButton id="retry-meet-release" disabled={view.busy} onClick={() => client.cancelMeetSetup()}>Retry audio release</RustButton></div>}
    {!manualActive && <MeetFeedback view={view} language={policy?.mode === "two_way" ? policy.first : policy?.mode === "auto_to_target" ? policy.target : language} />}
    {(listening || recovering) && <RustSurface kind="sound" state={{language: interpretationTitle, muted: view.muted.live_interpretation, disabled: view.finishing}}
      dispatch={name => {if (name === "mute") void client.mute("live_interpretation", !view.muted.live_interpretation);}} fallback={<div className="meet-sound-control"><div><strong>Interpretation · {interpretationTitle}</strong><p id="meet-sound-help">Private headphone sound. Muting discards sound; processing and reservations continue. Enabling sound never replays the muted interval.</p></div><RustButton id="meet-sound-toggle" aria-pressed={!view.muted.live_interpretation} aria-describedby="meet-sound-help" disabled={view.finishing} onClick={() => {void client.mute("live_interpretation", !view.muted.live_interpretation);}}>{view.muted.live_interpretation ? "Enable interpretation sound" : "Mute interpretation sound"}</RustButton></div>} />}
    <CostNotices view={view} />
    <details className="meet-details"><summary>Audio &amp; connection details</summary>
      <p className="meet-hint">{microphone ? "Only your selected microphone is captured. Incoming voices from the meeting tab are not included. Check your browser’s microphone permission and selected input device if the meter stays flat." : <>Select the tab containing your meeting and include <strong>Share tab audio</strong>. This does not capture your microphone. Your meeting app keeps its own microphone and presentation controls.</>}</p>
      {!rehearsal && audioCheck}
      {preview && <p className="meet-hint">{rehearsal ? "Check the input level above." : "Quiet audio never blocks Start."} {microphone ? "If you expect sound but the meter stays flat, check your browser’s microphone permission and selected input device." : "If you expect sound but hear none, check Share tab audio, your meeting’s speaker output and whether you chose the right tab."} This optional check keeps audio on this device.</p>}
      {view.renewalCount > 0 && <p className="meet-hint">Speech connection refreshes: {view.renewalCount}. Transition gaps are included in the export; earlier audio is never replayed.</p>}
      {!rehearsal && <div id="meet-start-cost" className="meet-cost">
        {pausedByUser ? <><strong>Interpretation paused</strong><span>New audio is not captured. Paid closure status is shown above. Resume keeps source captions and notes with a labeled gap.</span></> : view.finishing ? <><strong>Finishing source captions</strong><span>Capture and private interpretation have stopped. Waiting for final source captions before export; Stop cancels the pending export.</span></> : view.aiPaused ? <><strong>AI paused · session retained</strong><span>No new audio is sent. Stop clears unsaved work and ends this session.</span></> : view.session?.continuous && view.session.providers_enabled ? <><strong>Runs until you stop</strong><span>Connections refresh in bounded windows; transition audio can be missed. {reservation !== null ? `${usd(reservation)} estimated for the next captions and interpreter windows. ` : ""}Spending warnings never stop processing. This mode has no total spending cap.</span></> : active ? <><strong>This test is running</strong><span>{duration} remains in this activation. {view.budget?.amounts ? `${usd(view.budget.amounts.reserved_microusd)} reserved by this process. ` : ""}Local reservations are not measured account spend. Closing early does not refund a reservation.</span></> : reservation !== null ? <><strong>Reserve up to {usd(reservation)} for both services</strong><span>{duration} remains in this activation. Two stream admissions; a local reservation, not measured account spend. Early closes and failed attempts are not refunded.</span></> : <><strong>Live estimate unavailable</strong><span>A live start requires a current estimate for both services.</span></>}
      </div>}
    </details>
    <div className="meet-controls"><span>{active ? "Finish saves source captions and notes. Stop clears unsaved work immediately." : "Keep this tab private. Confirm important names and details with the speaker."}</span><RustButton id="finish-export" disabled={view.finishing || (!view.connected && !view.aiPaused && !pausedByUser)} onClick={finish}>{view.finishing ? "Finishing source captions…" : view.aiPaused || pausedByUser ? "Export retained captions & notes" : "Finish & export"}</RustButton></div>
  </section>;
}
