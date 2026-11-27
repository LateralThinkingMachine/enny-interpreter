import type {Source} from "./protocol.js";

/** Fixed messages only: device/provider text is never interpolated into the UI. */
export function captureMessage(code: string, source: Source = "remote_mix"): string {
  if (source === "self") {
    const microphoneMessages: Record<string, string> = {
      tab_audio_missing: "Your browser did not provide a live microphone track. Check the selected microphone and its permission, then try again.",
      capture_permission_or_device_failed: "Your browser could not open your microphone. Check its microphone permission and the selected device, then try again.",
      capture_permission_denied: "Microphone access was cancelled or denied. Allow this companion to use your microphone when you are ready.",
      capture_selection_timeout: "Microphone permission timed out. Dismiss any pending browser permission prompt, then try again.",
      capture_setup_timeout: "Your browser did not finish opening the microphone. Check the input device and microphone permission.",
      capture_selection_pending: "Dismiss the browser microphone permission prompt that is still open before trying again or changing audio sources.",
      capture_no_frames: "The microphone was selected, but your browser is not delivering audio frames. Check the selected input device and microphone permission.",
      capture_track_ended: "Your browser stopped the selected microphone. Audio processing has stopped.",
      capture_unavailable: "Microphone capture is unavailable in this browser. Use desktop Chrome and check its microphone permissions.",
      capture_preview_expired: "The local microphone check expired. Select your microphone again before starting.",
      capture_preview_timeout: "The local microphone check expired. Select your microphone again before starting.",
      capture_not_prepared: "The selected microphone is no longer ready. Processing stopped; select it again in a new session.",
      capture_cleanup_failed: "Microphone release could not be confirmed. Use your browser’s microphone controls and close this companion page.",
      audio_cleanup_unconfirmed: "Microphone release could not be confirmed. Use your browser’s microphone controls and close this companion page.",
    };
    if (Object.hasOwn(microphoneMessages, code)) return microphoneMessages[code];
  }
  const messages: Record<string, string> = {
    tab_audio_missing: "Your browser did not share any tab audio. Choose the tab containing your call and turn on Share tab audio.",
    select_browser_tab: "Select the browser tab containing your call and turn on Share tab audio.",
    capture_permission_or_device_failed: "Your browser could not open the selected audio. Check its sharing permission, then choose the meeting tab again.",
    capture_permission_denied: "Audio selection was cancelled or denied. Choose the meeting tab again when you are ready.",
    capture_selection_timeout: "The tab chooser timed out. Cancel any open chooser, then choose the meeting tab again.",
    capture_setup_timeout: "Your browser did not finish opening the audio device. Check the tab and browser permissions.",
    capture_preview_expired: "The local audio check expired. Choose the meeting tab again before starting.",
    capture_no_frames: "The tab was selected, but your browser is not delivering audio frames. Check that the remote participant is audible in your meeting tab and tab audio is shared.",
    capture_track_ended: "Your browser stopped sharing the selected tab. Audio processing has stopped.",
    capture_suspended: "Your browser paused the audio device. Return to the companion and check the browser's audio permissions.",
    capture_processor_failed: "The browser audio processor failed. Audio processing has stopped.",
    capture_stale: "Audio delivery fell behind. Processing stopped instead of replaying delayed speech.",
    capture_backpressure: "Audio delivery could not keep up. Processing stopped to avoid a growing delay.",
    capture_cleanup_failed: "Device release could not be confirmed. Use your browser's Stop sharing control and close this companion page.",
    capture_unavailable: "Tab audio is unavailable in this browser. Use desktop Chrome and check its audio permissions.",
    capture_preview_timeout: "The local audio check expired. Choose the meeting tab again before starting.",
    capture_selection_pending: "Cancel the browser tab chooser that is still open, then choose the meeting tab again.",
    audio_cleanup_unconfirmed: "Device release could not be confirmed. Use your browser’s Stop sharing control and close this companion page.",
    capture_not_prepared: "The selected audio is no longer ready. Processing stopped; choose the tab again in a new test.",
    capture_closed: "This audio source is closed. No capture was restarted.",
  };
  return Object.hasOwn(messages, code) ? messages[code] : source === "self" ? "The selected microphone stopped or could not be checked. Check your browser’s microphone permission and input device." : "The selected audio stopped or could not be checked. Check the sound in the meeting tab and Share tab audio.";
}

export function providerMessage(code: string | null): string {
  const messages: Record<string, string> = {
    provider_unavailable: "The speech service could not connect. No successful interpretation is claimed.",
    provider_protocol: "The speech service returned an unsupported response. Processing stopped.",
    budget_exhausted: "A local spending or resource limit was reached, but this record does not identify which one or prove billed spend.",
    provider_budget: "A local spending or resource limit was reached, but this record does not identify which one or prove billed spend.",
    stream_limit: "The activation's approved number of speech connections was reached. Processing stopped; no connection was reopened.",
    input_audio_limit: "The activation's incoming audio volume limit was reached. Processing stopped. This limit does not report billed spend.",
    output_audio_limit: "The activation's generated audio volume limit was reached. Processing stopped. This limit does not report billed spend.",
    input_pacing: "Incoming audio arrived faster than real time. Processing stopped to avoid sending a backlog; check capture timing before a new test.",
    speech_boundary_limit: "Source captions did not detect a natural speech boundary within 120 seconds. Processing stopped; review background sound and speech pauses before a new test.",
    source_ledger_limit: "The local source-caption tracking limit was reached. Processing stopped to preserve caption order; review this diagnostic before another test.",
    transcript_item_limit: "A source caption exceeded its text or revision limit. Processing stopped to preserve bounded caption tracking.",
    usage_mismatch: "The speech service reported audio usage inconsistent with the audio submitted. Processing stopped because usage could not be verified; this is not a record of billed spend.",
    source_response_deadline: "The source-caption service did not confirm an audio receipt or final caption within the required time. Processing stopped with a caption gap.",
    protocol_capacity_limit: "The speech service exceeded a local event or output queue limit. Processing stopped to avoid unbounded memory or delay.",
    deadline: "The speech service reached a time limit or missed a required response or audio deadline.",
    provider_deadline: "The speech service reached a time limit or missed a required response or audio deadline.",
    text_limit: "The activation's text-processing limit was reached. This is a text volume limit, not evidence that the dollar allowance was spent.",
    provider_closed: "The speech service connection closed before normal completion could be confirmed.",
    provider_connection_lost: "The speech connection was interrupted and processing could not continue.",
    source_input_interrupted: "Incoming audio delivery was interrupted and processing could not continue.",
    live_closed_requested: "The provider reported a close request for the GPT-Live-1 session before local completion was confirmed. Processing has stopped; no automatic retry.",
    live_expired: "The provider reported that the GPT-Live-1 session expired before local completion was confirmed. Processing has stopped; no automatic retry.",
    live_content_closed: "The provider ended the GPT-Live-1 session because of its content handling. Processing has stopped; no automatic retry.",
    live_remote_hangup: "The provider reported a remote hangup of the GPT-Live-1 session before local completion was confirmed. Processing has stopped; no automatic retry.",
    live_connection_lost: "The provider reported a lost connection for the GPT-Live-1 session before local completion was confirmed. Processing has stopped; no automatic retry.",
    source_incomplete: "The previous source captions did not finish reliably. AI is paused; the final phrase may be missing.",
    source_gap: "The selected audio source stopped delivering a continuous stream.",
    sequence_gap: "Audio frames arrived with a sequence gap. Processing stopped.",
    backpressure: "Audio or event delivery could not keep up. Processing stopped to avoid a growing delay.",
  };
  return code !== null && Object.hasOwn(messages, code) ? messages[code] : "The speech route ended or failed. Check its status before any further test.";
}
