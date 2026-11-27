"use strict";

const languages = { en: ["English", "en-US"], es: ["Spanish", "es-ES"], fr: ["French", "fr-FR"], de: ["German", "de-DE"], ja: ["Japanese", "ja-JP"] };

function supportsExplicitTrack(doc) {
  // Without the track overload, WebIDL ignores its argument and start() can open a microphone.
  // Probe in a detached document that was explicitly denied microphone access beforehand.
  const frame = doc.createElement("iframe");
  frame.hidden = true;
  frame.allow = "microphone 'none'; camera 'none'";
  try {
    doc.body.append(frame);
    const child = frame.contentWindow;
    if (child.document.featurePolicy?.allowsFeature("microphone") !== false) return false;
    const LocalRecognizer = child.SpeechRecognition || child.webkitSpeechRecognition;
    const probe = new LocalRecognizer();
    probe.processLocally = true;
    frame.remove();
    try { probe.start({}); }
    catch (error) { return error.name === "TypeError" && error.message.includes("MediaStreamTrack"); }
    probe.abort();
  } catch {
    // Some browser policies prevent the disposable frame. Audio mode stays unavailable.
  } finally { frame.remove(); }
  return false;
}

function makeInterpreter(env, view) {
  const nav = env.navigator;
  const Recognizer = env.SpeechRecognition || env.webkitSpeechRecognition;
  let sequence = 0, phase = "idle", selection, speechState, speechQuality, checked = false;
  let translator, stream, recognizer, aborter, speechTimer, pending = [], translating = false;
  function state(next, message) {
    phase = next;
    const needsSpeech = selection?.mode !== "text" && speechState !== "available";
    view.state(next, message, needsSpeech ? "Prepare speech" : "Prepare translation");
  }
  function stopTracks(s) { for (const track of s?.getTracks() || []) track.stop(); }
  function releaseCapture() {
    if (speechTimer !== undefined) env.clearTimeout(speechTimer);
    speechTimer = undefined;
    if (recognizer) {
      recognizer.onspeechstart = recognizer.onresult = recognizer.onerror = recognizer.onend = null;
      try { recognizer.abort(); } catch {}
      recognizer = null;
    }
    stopTracks(stream);
    stream = null;
  }
  function release() {
    ++sequence;
    releaseCapture();
    aborter?.abort();
    aborter = null;
    translator?.destroy();
    translator = null;
    pending = [];
    translating = false;
  }
  function end(message = "Stopped. Prepare the languages to start another session.") {
    release();
    state(checked ? "setup" : "unavailable", message);
  }
  function speechOpts(quality = speechQuality) { return { langs: [languages[selection.from][1]], processLocally: true, quality }; }
  function pair() { return { sourceLanguage: selection.from, targetLanguage: selection.to }; }

  async function check(chosen) {
    release();
    selection = { ...chosen };
    checked = false;
    speechState = undefined;
    speechQuality = "dictation";
    const token = sequence;
    state("checking", "Checking language support…");
    if (chosen.from === chosen.to) return state("unavailable", "Choose two different languages.");
    if (!env.isSecureContext || !env.Translator?.availability || !env.Translator?.create) {
      return state("unavailable", "This browser cannot translate on this page. Try desktop Chrome, or try the offline sample.");
    }
    if (chosen.mode !== "text" && (!Recognizer?.available || !Recognizer?.install
        || !("processLocally" in Recognizer.prototype) || !nav.mediaDevices?.[chosen.mode === "tab" ? "getDisplayMedia" : "getUserMedia"]
        || !supportsExplicitTrack(env.document))) {
      return state("unavailable", "Local audio recognition is unavailable here. Choose Typed text or try the sample.");
    }
    try {
      const [translation, preferredSpeech] = await Promise.all([
        env.Translator.availability(pair()),
        chosen.mode === "text" ? "available" : Recognizer.available(speechOpts())
      ]);
      if (token !== sequence) return;
      const usable = value => ["available", "downloadable", "downloading"].includes(value);
      if (!usable(translation)) return state("unavailable", "The browser cannot supply this translation pair. Choose different languages or try the sample.");
      let speech = preferredSpeech;
      if (chosen.mode !== "text" && speech === "unavailable") {
        speech = await Recognizer.available(speechOpts("command"));
        if (token !== sequence) return;
        speechQuality = "command";
      }
      if (!usable(speech)) return state("unavailable", "The browser cannot supply local recognition for this language. Choose another source language or Typed text.");
      checked = true;
      speechState = speech;
      state("setup", speechQuality === "command"
        ? "Only basic local speech recognition is available for this language. Caption accuracy may vary. Prepare the languages to continue."
        : "Prepare the selected languages to continue.");
    } catch {
      if (token === sequence) state("unavailable", "Browser access to local models is unavailable here. Try localhost, a different language pair, or the sample.");
    }
  }

  async function prepare() {
    if (phase !== "setup") return;
    const token = sequence;
    aborter = new AbortController();
    state("preparing", "Preparing local models. Your browser may download language files…");
    try {
      // Each model API can consume transient user activation. Request one per click.
      if (selection.mode !== "text" && speechState !== "available") {
        const options = speechOpts();
        await Recognizer.install(options);
        if (sequence !== token) return;
        if (await Recognizer.available(options) !== "available") throw new Error("speech model unavailable");
        if (sequence === token) {
          speechState = "available";
          state("setup", "Local speech is ready. Prepare translation to continue.");
        }
        return;
      }
      const model = await env.Translator.create({ ...pair(), signal: aborter.signal });
      if (sequence !== token) { model.destroy(); return; }
      translator = model;
      if (sequence === token) state("ready", selection.mode === "text" ? "Ready to translate typed text." : "Ready. Start captions to choose the audio source.");
    } catch {
      if (sequence === token) end("Local model setup did not finish. Check browser downloads, storage and language support, then try again.");
    }
  }

  function enqueue(value, token) {
    if (token !== sequence || phase !== "live") return;
    const text = value.trim();
    if (!text) return;
    if (text.length > 4000 || pending.length >= 16) return end("Captions stopped because local translation could not keep up. Prepare languages to resume.");
    pending.push(text);
    if (!translating) void drain(token);
  }
  async function drain(token) {
    translating = true;
    try {
      while (token === sequence && pending.length) {
        const original = pending.shift();
        const translated = await translator.translate(original, { signal: aborter.signal });
        if (token === sequence) view.line(original, translated);
      }
    } catch {
      if (token === sequence) end("Local translation stopped. Prepare the languages to try again.");
    } finally {
      if (token === sequence) {
        translating = false;
        if (phase === "finishing") end("Local speech recognition ended. Prepare languages to resume.");
      }
    }
  }

  async function start(text = "", agreement = false) {
    if (phase !== "ready") return;
    if (selection.mode === "text") {
      if (!text.trim() || text.length > 4000) return state("ready", "Enter between 1 and 4,000 characters.");
      const token = sequence;
      state("typing", "Translating on this device…");
      try {
        const translated = await translator.translate(text.trim(), { signal: aborter.signal });
        if (sequence === token) { view.line(text.trim(), translated); state("ready", "Translation ready."); }
      } catch {
        if (sequence === token) end("Translation stopped. Prepare the languages to try again.");
      }
      return;
    }
    if (agreement !== true) return state("ready", "Confirm participant agreement before sharing audio.");
    const token = sequence;
    state("selecting", selection.mode === "tab" ? "Choose a single browser tab and share its audio." : "Choose a microphone in the browser prompt.");
    try {
      // Request in the Start click handler while user activation is available.
      const picked = selection.mode === "tab"
        ? await nav.mediaDevices.getDisplayMedia({ video: { displaySurface: "browser" }, audio: true,
            systemAudio: "exclude", selfBrowserSurface: "exclude", surfaceSwitching: "exclude", monitorTypeSurfaces: "exclude" })
        : await nav.mediaDevices.getUserMedia({ audio: true, video: false });
      if (token !== sequence) { stopTracks(picked); return; }
      stream = picked;
      const audio = stream.getAudioTracks();
      const video = stream.getVideoTracks();
      if (selection.mode === "tab" && (video.length !== 1 || video[0].kind !== "video" || video[0].readyState !== "live" || video[0].getSettings().displaySurface !== "browser")) {
        return end("Select a browser tab in the picker to share meeting audio.");
      }
      if ((selection.mode === "mic" && video.length !== 0) || stream.getTracks().length !== audio.length + video.length) {
        return end("The browser returned an unexpected audio source. Select the source again.");
      }
      if (audio.length !== 1 || audio[0].kind !== "audio" || audio[0].readyState !== "live") {
        return end("The selected source shared no live audio. For a tab, enable Share tab audio in the picker.");
      }
      for (const track of stream.getTracks()) track.addEventListener("ended", () => {
        if (token === sequence) end("Audio sharing ended.");
      }, { once: true });
      // The picker requires a video track to obtain tab audio. No video is displayed or recorded.
      for (const track of video) track.enabled = false;
      recognizer = new Recognizer();
      recognizer.processLocally = true;
      if (recognizer.processLocally !== true) return end("Local recognition is unavailable on this device.");
      recognizer.lang = languages[selection.from][1];
      if ("quality" in recognizer) recognizer.quality = speechQuality;
      recognizer.continuous = true;
      recognizer.interimResults = false;
      let hasFinal = false;
      const listening = selection.mode === "tab" ? "Listening to the selected tab." : "Listening to the selected microphone.";
      const finishMessage = () => hasFinal ? "Local speech recognition ended. Prepare languages to resume." :
        "The browser ended recognition without returning captions. Check the selected audio, switch to Typed text, or try the full app.";
      recognizer.onspeechstart = () => {
        if (token !== sequence || hasFinal || speechTimer !== undefined) return;
        speechTimer = env.setTimeout(() => {
          if (token === sequence && !hasFinal && phase === "live") {
            state("live", "The browser detected speech but has not returned captions. Check the language, or stop and choose Typed text.");
          }
        }, 20_000);
      };
      recognizer.onresult = event => {
        for (let i = event.resultIndex; i < event.results.length; i++) {
          const entry = event.results[i];
          if (token === sequence && entry.isFinal && entry[0].transcript.trim()) {
            if (!hasFinal) state("live", listening);
            hasFinal = true;
            enqueue(entry[0].transcript, token);
          }
        }
      };
      recognizer.onerror = () => { if (token === sequence) end("Local speech recognition stopped. Check the audio source and language pack."); };
      recognizer.onend = () => {
        if (token !== sequence) return;
        releaseCapture();
        if (translating) state("finishing", "Audio ended. Finishing the last translation…");
        else end(finishMessage());
      };
      recognizer.start(audio[0]);
      if (token === sequence) state("live", listening);
    } catch {
      if (token === sequence) end("Audio capture did not start. Check browser permissions and the selected source.");
    }
  }
  return { check, prepare, start, end };
}

let portableController;
function connectPortableBrowser(state, line, hide, restore) {
  portableController = makeInterpreter(window, { state, line });
  window.addEventListener("pagehide", () => { portableController.end(); hide(); });
  window.addEventListener("pageshow", event => { if (event.persisted) restore(); });
}
function checkPortableLanguages(mode, from, to) { void portableController.check({ mode, from, to }); }
function preparePortableLanguages() { void portableController.prepare(); }
function startPortableTranslation(text, consent) { void portableController.start(text, consent); }
function stopPortableTranslation() { portableController.end(); }
