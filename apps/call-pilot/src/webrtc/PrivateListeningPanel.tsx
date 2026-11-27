import {RustSelect, RustInput} from "../rust-fields.js";
import {RustButton} from "../rust-button.js";
import {useEffect, useState, useSyncExternalStore} from 'react';
import type {PrivateInput, PrivateListening} from './private-listening.js';
import type {Interpretation, Language} from '../protocol.js';
import {LANGUAGE_LABELS} from '../languages.js';
import {PrivateLiveCaptions} from './PrivateLiveCaptions.js';
import {INTERPRETER_VOICES} from './caption-data.js';
import {exportPrivateObservations, parsePrivateVoice, type PrivateVoice} from './private-observations.js';
import {privateListeningPolicy} from './private-listening-setup.js';

import {estimatedUsd, privateWindowEstimate, type Budget} from '../cost.js';

const measuredMilliseconds = (value: number | null | undefined) => value == null ? 'Unavailable' : `${value.toFixed(1)} ms`;

export function PrivateListeningPanel({controller, budget, blocked}: {controller: PrivateListening; budget: Budget | null; blocked: boolean}) {
  const view = useSyncExternalStore(controller.subscribe, controller.snapshot, controller.snapshot);
  const [source, setSource] = useState<PrivateInput>('tab');
  const [language, setLanguage] = useState<Language>('ja');
  const [mode, setMode] = useState<Interpretation['mode']>('auto_to_target');
  const [first, setFirst] = useState<Language>('ja');
  const [second, setSecond] = useState<Language>('en');
  const [voice, setVoice] = useState<PrivateVoice>('marin');
  const [agreement, setAgreement] = useState({controller, phase: view.phase, checked: false});
  // A replaced operator or a terminal attempt requires fresh agreement. Renewal
  // remains part of the same explicit Start and does not reset it.
  if (agreement.controller !== controller || agreement.phase !== view.phase) {
    setAgreement({controller, phase: view.phase, checked: agreement.controller === controller && !['closed', 'failed', 'unconfirmed'].includes(view.phase) && agreement.checked});
  }
  const agreed = agreement.controller === controller && agreement.phase === view.phase && agreement.checked;
  const setAgreed = (checked: boolean) => setAgreement({controller, phase: view.phase, checked});
  const active = controller.isActive();
  const estimate = privateWindowEstimate(budget, view.status?.allocation_duration_seconds);
  const originalOnly = view.listeningMode === 'original_only';
  const playbackSource = active ? view.source : source;
  const canChangeLocalPlayback = !!view.playback && ['connecting', 'listening', 'renewing'].includes(view.phase)
    && !['failed', 'disposed'].includes(view.playback.status);
  const canChooseListeningMode = playbackSource === 'tab' && !!view.playback?.selectiveDuckingAvailable && canChangeLocalPlayback;
  let interpretation: Interpretation | null = null;
  try {interpretation = privateListeningPolicy({mode, target: language, first, second});} catch { /* Invalid selections cannot reach Start. */ }
  const unresolvedProvider = view.status?.closure_confirmed === false;
  const activityLabel = view.phase === 'listening' ? originalOnly ? 'Interpretation off for me · paid connection open' : 'Interpretation on · paid connection open'
    : view.phase === 'connecting' || view.phase === 'renewing' ? 'Connecting · may incur cost'
    : view.phase === 'closing' || view.phase === 'unconfirmed' ? 'Audio stopped · checking cost closure'
    : unresolvedProvider ? 'Another connection may still incur cost'
    : view.status === null ? 'Connection and cost status unverified'
    : 'Private interpretation off';
  useEffect(() => {if (!blocked) void controller.refresh();}, [controller, blocked]);
  const chooseAgreement = (checked: boolean) => {setAgreed(checked); if (!checked && active) void controller.stop('Participant agreement was withdrawn.');};
  const exportObservation = () => {
    const content = exportPrivateObservations(view);
    const url = URL.createObjectURL(new Blob([JSON.stringify(content, null, 2)], {type: 'application/json'}));
    try {const link = document.createElement('a'); link.href = url; link.download = 'private-interpretation-observations.json'; link.click();} finally {setTimeout(() => URL.revokeObjectURL(url), 0);}
  };
  return <section className="panel private-listening" aria-labelledby="private-listening-title">
    <div className="section-heading"><div><p className="eyebrow">Your private interpreter</p><h2 id="private-listening-title">Listen in your language.</h2></div><span className="tag">This device only · Headphones</span></div>
    <p>The selected audio goes to the interpretation provider. Captions and playback are private to this browser. Changing a target or language pair does not change what other participants hear.</p>
    <div className="private-listening-state" role="status"><strong>{activityLabel}</strong><p>{view.message}</p></div>
    {view.failure && <p className="notice" role="alert">{view.failure.message}</p>}
    <fieldset className="advanced-controls" disabled={active || blocked}>
      <legend className="sr-only">Private listening setup</legend>
      <div className="route-form"><label>Listen to<RustSelect value={source} onChange={event => setSource(event.target.value as PrivateInput)}><option value="tab">Meeting tab · other people</option><option value="microphone">My microphone · speech check</option></RustSelect></label>
        <label>Interpretation mode<RustSelect id="private-listening-mode" value={mode} onChange={event => setMode(event.target.value as Interpretation['mode'])}><option value="auto_to_target">Personal target language · this device</option><option value="two_way">Two-way language pair · this device</option></RustSelect></label>
        {mode === 'auto_to_target' ? <label>Interpret into<RustSelect id="private-listening-target" value={language} onChange={event => setLanguage(event.target.value as Language)}>{Object.entries(LANGUAGE_LABELS).map(([key, label]) => <option key={key} value={key}>{label}</option>)}</RustSelect></label> : <><label>First language<RustSelect id="private-listening-first" value={first} onChange={event => setFirst(event.target.value as Language)}>{Object.entries(LANGUAGE_LABELS).map(([key, label]) => <option key={key} value={key}>{label}</option>)}</RustSelect></label><label>Second language<RustSelect id="private-listening-second" value={second} onChange={event => setSecond(event.target.value as Language)}>{Object.entries(LANGUAGE_LABELS).map(([key, label]) => <option key={key} value={key}>{label}</option>)}</RustSelect></label></>}
        <label>Interpretation voice<RustSelect id="private-listening-voice" value={voice} onChange={event => setVoice(parsePrivateVoice(event.target.value))}>{INTERPRETER_VOICES.map(value => <option key={value} value={value}>{value[0].toUpperCase() + value.slice(1)}</option>)}</RustSelect></label></div>
      <p className="help">Choose a built-in GPT-Live voice before Start. Stop before changing it; the choice stays fixed across connection refreshes. These are not language-specific native-speaker presets.</p>
      <p className="help">{mode === 'two_way' ? 'Either selected language is interpreted into the other, on this device.' : 'Captured speech is interpreted into your target language; speech already in that language should not be repeated.'} Stop before changing languages. Settings stay fixed during connection refreshes.</p>
      {!interpretation && <p className="notice" role="alert">Choose two different supported languages before starting.</p>}
    </fieldset>
    <label className="check"><RustInput type="checkbox" checked={agreed} onChange={event => chooseAgreement(event.target.checked)}/><span>Everyone agrees to AI audio processing. I’m wearing headphones. The selected tab has no shared AI interpreter speaking; I will share only the intended meeting audio.</span></label>
    {!active && <div id="private-listening-estimate" className="meet-cost"><strong>{estimate === null ? 'Private listening estimate unavailable' : `Estimated US${estimatedUsd(estimate)} per 5-minute connection`}</strong>
      <span>Enny charges no fee. OpenAI can bill the account that owns your API key as soon as the connection opens, even during setup or silence. The estimate includes setup and closing time. Connections renew automatically until you stop. There is no total spending cap.</span></div>}
    <div className="button-row">
      {!active ? <RustButton id="start-private-listening" aria-describedby="private-listening-estimate" disabled={blocked || estimate === null || !agreed || !view.available || !interpretation} onClick={() => {if (interpretation && estimate !== null) void controller.start(source, interpretation, agreed, estimate, voice);}}>Start private listening · billed by your own OpenAI account</RustButton> : <RustButton id="stop-private-listening" className="stop-button" onClick={() => {setAgreed(false); void controller.stop();}}>{view.phase === 'unconfirmed' ? 'Check closure again' : 'Stop private listening'}</RustButton>}
      {!active && <RustButton className="quiet" disabled={blocked} onClick={() => {void controller.refresh();}}>Check availability</RustButton>}
      {view.entries.length > 0 && <RustButton className="quiet" onClick={exportObservation}>Export original observations</RustButton>}
    </div>
    <p className="help">{source === 'tab' ? 'Choose the browser tab containing your call and enable Share tab audio. The browser must suppress its original local playback so this page can provide one controlled headphone mix. Window or whole-screen capture is refused.' : 'This checks your microphone speech. We do not play your microphone back to you, so original-audio ducking is unavailable in this mode.'}</p>
    {(active || view.playback) && <fieldset className="advanced-controls"><legend>Private sound</legend>
      {canChooseListeningMode && <div className="private-listening-choice">
        <div className="button-row" role="group" aria-label="What I hear in this browser"><RustButton id="private-hear-interpretation" aria-pressed={!originalOnly} onClick={() => controller.setListeningMode('interpretation')}>Interpretation on</RustButton><RustButton id="private-hear-original" aria-pressed={originalOnly} onClick={() => controller.setListeningMode('original_only')}>Original audio only</RustButton></div>
        <p className="help">{originalOnly ? <><strong>Interpretation off for me.</strong> Original audio returns to your selected volume; original mute still applies. </> : 'Choose what you hear in this browser only. '}Live captions continue in either mode. GPT-Live-1 remains running and billable until you select Stop private listening. Any speech already present in the tab audio remains audible.</p>
      </div>}
      {canChangeLocalPlayback && playbackSource === 'microphone' && originalOnly && <div className="private-listening-choice"><p className="help">Interpretation is off for this browser. Your microphone is not played back. Live captions and paid GPT-Live-1 processing continue until Stop.</p><RustButton id="private-enable-microphone-interpretation" onClick={() => controller.setListeningMode('interpretation')}>Turn interpretation on</RustButton></div>}
      <label className="check"><RustInput type="checkbox" checked={view.settings.enabled} disabled={playbackSource !== 'tab' || originalOnly} onChange={event => controller.updateSettings({enabled: event.target.checked})}/><span>Lower original voices while interpretation speaks {!originalOnly && view.playback?.ducking ? '· lowering now' : ''}</span></label>
      <div className="route-form"><label>Original volume<RustInput id="private-original-volume" type="range" min="0" max="1" step="0.05" value={view.settings.originalVolume} disabled={playbackSource !== 'tab'} onChange={event => controller.updateSettings({originalVolume: Number(event.target.value)})}/></label>
        <label>Interpretation volume<RustInput id="private-interpretation-volume" type="range" min="0" max="1" step="0.05" value={view.settings.translatedVolume} disabled={originalOnly} onChange={event => controller.updateSettings({translatedVolume: Number(event.target.value)})}/></label></div>
      <div className="button-row"><RustButton disabled={playbackSource !== 'tab'} aria-pressed={view.settings.originalMuted} onClick={() => controller.updateSettings({originalMuted: !view.settings.originalMuted})}>{view.settings.originalMuted ? 'Unmute original' : 'Mute original'}</RustButton>
        <RustButton disabled={originalOnly} aria-pressed={view.settings.translatedMuted} onClick={() => controller.updateSettings({translatedMuted: !view.settings.translatedMuted})}>{view.settings.translatedMuted ? 'Unmute interpretation' : 'Mute interpretation'}</RustButton></div>
      <p className="help">Volume and ducking affect headphone playback only while this listener is active. Stop returns original sound to the meeting tab. Muting does not stop processing or cost. Use Stop to close the paid connection.</p>
    </fieldset>}
    {view.costNotice && <p className="notice" role="status">{view.costNotice}</p>}
    <details className="meet-details"><summary>Connection and cost details</summary><p>GPT-Live-1 audio uses WebRTC. The Rust service owns setup, closure, usage and a bounded 300-second connection window. Refreshes have a visible coverage gap; audio is not saved for replay.</p>
      <p>Audio and captions are not sent to other meeting participants. This local page does not provide remote access; each participant needs a separately authorized Companion listener on their own device. A two-way pair does not capture your microphone together with the meeting tab. The private listener never sends speech to the meeting app’s microphone.</p>
      {view.status?.requested_voice && <p>Current or last requested voice: {view.status.requested_voice}. This is the submitted setting, not a verified voice-quality result.</p>}
      <p>{view.status?.allocated_microusd != null ? `US$${(view.status.allocated_microusd / 1_000_000).toFixed(2)} estimated allocations since service start. ` : 'Local allocation total unavailable. '}Estimates include reserved windows and failed attempts; they are not invoice charges.</p>
      <p>{view.status?.usage_seconds != null ? `Provider observed ${view.status.usage_seconds.toFixed(1)} seconds in the current/last window. ` : 'Provider usage has not been observed. '}{view.status?.usage_confirmed ? 'Final usage confirmed by the provider.' : 'Final usage is not confirmed.'} {view.gaps} connection-transition gaps.</p>
      {view.audioEvidence && <div className="private-audio-evidence"><p>Current or last connection observations · these do not confirm that you heard sound.</p><dl>
        <div><dt>Received audio packets</dt><dd>{view.audioEvidence.receivedPackets}{view.audioEvidence.status === 'pending' ? ' · awaiting a browser report' : view.audioEvidence.status === 'unavailable' ? ' · latest browser report unavailable' : ' · browser reported'}</dd></div>
        <div><dt>Incoming audio energy</dt><dd>{view.audioEvidence.receivedSignal ? 'Observed' : 'Not observed'}</dd></div>
        <div><dt>Browser audio renderer</dt><dd>{view.audioEvidence.rendererReady === null ? 'Not observed' : view.audioEvidence.rendererReady ? 'Prepared' : 'Not prepared'}</dd></div>
        <div><dt>Translated signal reached the mixer</dt><dd>{view.audioEvidence.translatedSignalSeen === null ? 'Not observed' : view.audioEvidence.translatedSignalSeen ? 'Observed' : 'No positive signal observed'}</dd></div>
        <div><dt>Translated signal after playback gain</dt><dd>{view.audioEvidence.renderedSignalSeen === null ? 'Not observed' : view.audioEvidence.renderedSignalSeen ? 'Observed' : 'No positive signal observed'}</dd></div>
      </dl><p>These are separate browser reception and audio-rendering observations. Captions, provider usage and a prepared renderer do not establish audible playback. Muting can leave reception active while playback is silent. None of these observations confirms another participant heard interpretation.</p></div>}
      {view.audioEvidence?.transport && <div className="private-audio-evidence"><p>Current or last browser transport estimates{view.audioEvidence.transport.status === 'pending' ? ' · awaiting a report' : view.audioEvidence.transport.status === 'unavailable' ? ' · latest estimates unavailable' : ''}</p><dl>
        <div><dt>Network round-trip time</dt><dd>{measuredMilliseconds(view.audioEvidence.transport.roundTripMs)}</dd></div>
        <div><dt>Incoming audio jitter</dt><dd>{measuredMilliseconds(view.audioEvidence.transport.inboundJitterMs)}</dd></div>
        <div><dt>Mean jitter-buffer delay</dt><dd>{measuredMilliseconds(view.audioEvidence.transport.jitterBufferDelayMs)}</dd></div>
        <div><dt>Jitter-buffer observation interval</dt><dd>{measuredMilliseconds(view.audioEvidence.transport.sampleIntervalMs)}</dd></div>
      </dl><p>These measure parts of browser transport, not model processing or end-to-end speech delay. Round-trip time is not one-way delay; these values cannot be added into a total interpretation delay. Missing browser fields remain unavailable.</p></div>}
      {view.status?.issue && <p>Connection diagnostic: <code>{view.status.issue}</code>. This fixed status contains no meeting words or credentials.</p>}
      {view.failure && <p>Stopped at: <code>{view.failure.stage}</code>{view.failure.httpStatus == null ? '' : ` · local HTTP ${view.failure.httpStatus}`}. The failure is retained separately from provider closure.</p>}
    </details>
    <PrivateLiveCaptions controller={controller} configuration={view.phase === "listening" ? view.configurations?.find(config => config.window === view.windows && config.source === view.source) : undefined}/>
  </section>;
}
