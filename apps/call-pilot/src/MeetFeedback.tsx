import {useMemo} from "react";
import type {View} from "./client.js";
import {CaptionReader, excerpt} from "./CaptionReader.js";
import {sameInterpretation} from "./protocol.js";
import type {Language} from "./protocol.js";

import {interpretationLabel} from "./languages.js";

function last<T>(items: readonly T[], matches: (item: T) => boolean): T | undefined {
  for (let index = items.length - 1; index >= 0; index--) if (matches(items[index])) return items[index];
}

export function MeetFeedback({view, language}: {view: View; language: Language}) {
  const source = view.meetSource;
  const microphone = source === "self";
  const status = view.captureStatus[source];
  const preview = view.meetPhase === "preview";
  const observing = !view.aiPaused && !view.manualPause && !view.finishing && !view.stopped && view.meetPhase !== "idle" && view.meetPhase !== "failed";
  const level = observing && status && !status.muted && Number.isFinite(status.level) ? Math.max(0, Math.min(1, status.level)) : 0;
  const label = !observing || !status ? microphone ? "Microphone not active" : "No tab selected"
    : status.muted ? microphone ? "Microphone input muted" : "Waiting for tab audio"
      : !status.frames ? "Waiting for audio frames" : level > 0 ? microphone ? "Microphone audio detected" : "Tab audio detected" : "Quiet right now";
  const {originalText, translatedText, captionState, earlierOutput} = useMemo(() => {
    const draft = last(view.sourceDrafts, item => item.source === source && !item.retired && !!item.text.trim());
    const caption = last(view.sourceFragments, item => item.source === source && item.route === "transcription" && item.provenance === "source_asr" && !!item.text.trim());
    const group = last(view.liveCaptions, item => item.source === source && item.live_mode === "interpretation" && item.provenance === "live_output" && item.language === language && sameInterpretation(item.interpretation, view.interpretation ?? undefined) && !!item.text.trim());
    const generated = last(view.fragments, item => item.source === source && item.route === "live" && item.live_mode === "interpretation" && item.provenance === "live_output" && item.language === language && sameInterpretation(item.interpretation, view.interpretation ?? undefined) && !!item.text.trim());
    return {originalText: excerpt(draft?.text ?? caption?.text ?? ""), translatedText: excerpt(group?.text ?? generated?.text ?? ""),
      captionState: draft ? "Live draft · may change · not yet confirmed" : caption?.incomplete ? "Source caption · incomplete at interruption" : caption?.final ? "Source caption · provider final" : "Source caption · provisional",
      earlierOutput: !!group?.closed};
  }, [view.sourceDrafts, view.sourceFragments, view.liveCaptions, view.fragments, view.interpretation, source, language]);
  const sent = view.sentFrames[source] > 0;
  const ended = view.aiPaused || !!view.manualPause || view.finishing || view.stopped;
  return <section id="meet-feedback" className="meet-feedback" aria-label="Audio and speech feedback">
    <div className="meet-audio-status">
      <div className="meter-label"><strong>{label}</strong><span>{ended ? "Input stopped" : preview ? "Local check · nothing sent" : microphone ? "My microphone input level" : "Selected tab audio level"}</span></div>
      <meter id="meet-audio-level" min={0} max={1} value={level} aria-label={microphone ? "My microphone input level" : "Selected meeting tab audio level"} />
      <p className="meet-hint">The meter shows sound, not recognized speech. Returned words appear below.</p>
    </div>
    <ul className="pipeline-status" aria-label="Speech processing observations" aria-live="polite" aria-atomic="true">
      <li className={sent ? "received" : ""}>Audio {sent ? "sent to local service this session" : "not sent yet"}</li>
      <li className={originalText ? "received" : ""}>Captions {originalText ? "received this session" : "waiting"}</li>
      <li className={translatedText ? "received" : ""}>Interpretation {translatedText ? "received this session" : "waiting"}</li>
    </ul>
    <p className="meet-hint">{ended ? "Processing is inactive. Any text below is retained from this session." : view.meetPhase === "recovering" ? "A speech connection is recovering. Received indicators and text may be from before the interruption." : "Received indicators describe this session so far; they do not confirm current delivery, accuracy or audible playback."}</p>
    {!view.browserListening && <CaptionReader scope={JSON.stringify([view.session?.generation, source, language, view.interpretation, !!ended])}
      prefix="meet" sourceTitle="Original speech" sourceLabel={microphone ? "My microphone" : "Meeting tab"}
      sourceText={originalText || "Your original words will appear here."}
      sourceNote={originalText ? `${captionState}. Check these words against what was spoken. Longer text is shortened here; the caption panel below keeps the retained history.` : ended ? "No source words were received for this input before processing stopped." : preview ? "This is a local audio check. Start interpretation to receive captions." : sent ? "Audio has been sent to the local service. Waiting for source words; pause briefly after a sentence." : "Source transcription starts after you choose audio and press Start."}
      targetLabel={interpretationLabel(view.interpretation ?? undefined, language)}
      targetText={translatedText || "Your translation will appear here."}
      targetNote={translatedText ? `${earlierOutput ? "Earlier connection · " : ""}Latest generated interpretation · may change. These independent excerpts may cover different phrases. Longer text is shortened here.` : ended ? "No interpretation text was received for this input before processing stopped." : "Translated text will appear here as it arrives. Headphone sound is controlled separately."}
      hasWords={!!(originalText || translatedText)} />}
  </section>;
}
