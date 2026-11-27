import {StrictMode, Suspense, lazy, useSyncExternalStore} from "react";
import {createRoot} from "react-dom/client";
import {installRustSurfaces} from "./rust-surface.js";
import {PilotClient, takeEntry} from "./client.js";
import {browserCostNotifications} from "./cost-notices.js";
import {createAudioEngine} from "./audio/index.js";
import {browserPrivateListening} from "./webrtc/private-listening.js";
import {verifyClip} from "./protocol.js";
import {publicWindowLink, trustedParentStop} from "./window-link.js";
import {sessionStopChannel} from "./session-stop-channel.js";
import type {SocketPort} from "./ports.js";
import {EVENT_PROTOCOL, tabSession} from "./session-transport.js";
import {SessionHost} from "./session-host.js";
import "./styles.css";
import {browserPreferences} from "./preferences.js";
const App = lazy(() => import("./App.js").then(module => ({default: module.App})));

function SessionApp({host}: {host: SessionHost}) {
  const state = useSyncExternalStore(host.subscribe, host.snapshot, host.snapshot);
  return <App key={state.key} focusNewSession={state.key > 1} client={state.client} preferredLanguage={state.language} onLanguageChange={language => host.setLanguage(language)} preferencesSaved={state.preferencesSaved} sessionActions={{available: state.available, creating: state.creating, error: state.error, create: () => {void host.create(state.client);}, resume: () => {void host.resume(state.client);}}} />;
}

function socket(url: string, protocols: string[]): SocketPort {
  const native = new WebSocket(url, protocols);
  const port: SocketPort = {get readyState() {return native.readyState;}, get bufferedAmount() {return native.bufferedAmount;}, onopen: null, onmessage: null, onerror: null, onclose: null, send: data => native.send(data), close: () => native.close()};
  native.onopen = () => {if (native.protocol === EVENT_PROTOCOL) port.onopen?.(); else native.close();}; native.onerror = () => port.onerror?.(); native.onclose = () => port.onclose?.(); native.onmessage = event => {if (native.protocol === EVENT_PROTOCOL) port.onmessage?.({data: event.data});};
  return port;
}

// Consume and scrub the entry fragment before mounting, effects or networking.
let entry: ReturnType<typeof takeEntry> | null = null;
try {
  entry = takeEntry(window.location, window.history);
} catch { /* Fixed message below; never log the link. */ }
const root = createRoot(document.getElementById("root")!);
if (!entry) root.render(<main className="invalid-entry"><h1>Open the exact local companion link.</h1><p>The invalid access fragment has been removed. Ask the local launcher for a fresh link; never put an API key here.</p></main>);
else {
  const role = entry.role;
  const parent = role === "public" ? window.opener as unknown : null;
  if (role === "public") window.opener = null;
  const tab = tabSession(role, window.location.origin, entry.ticket !== null, {fetch: window.fetch.bind(window), socket, storage: () => window.sessionStorage});
  const host = new SessionHost(() => {
    const transport = tab.openClient();
    const publicWindow = publicWindowLink(url => window.open(url, "_blank"));
    const audio = createAudioEngine({onFrame: (source, frame) => client.sendFrame(source, frame), onGap: (source, code) => client.captureGap(source, code), onCaptureStatus: (source, status) => client.captureStatus(source, status), onPlaybackFault: kind => client.playbackFault(kind)});
    const client = new PilotClient(role, {costNotifications: role === "operator" ? browserCostNotifications(typeof Notification === "undefined" ? undefined : Notification) : undefined, fetch: transport.fetch, socket: transport.socket, beginLogout: transport.beginLogout, audio, now: () => performance.now(), interval: (callback, duration) => {const id = window.setInterval(callback, duration); return () => window.clearInterval(id);}, broadcastStop: () => {publicWindow.stop(); stopChannel?.stop();}, openPublicWindow: url => publicWindow.open(url), verifyClip});
    const stopChannel = role === "operator" && typeof BroadcastChannel !== "undefined" ? sessionStopChannel({digest: input => crypto.subtle.digest("SHA-256", input), open: name => new BroadcastChannel(name), onStop: () => {publicWindow.stop(); void client.stop(false);}}) : null;
    if (role === "operator") client.privateListening = browserPrivateListening(client.privateListeningOwner(), transport.fetch);
    const cancelStopBinding = client.subscribe(() => {
      const session = client.snapshot().session;
      if (session) {cancelStopBinding(); void stopChannel?.bind(session.csrf);}
    });
    const parentStop = (event: MessageEvent) => {if (trustedParentStop(event, parent)) void client.stop(false);};
    if (role === "public") window.addEventListener("message", parentStop);
    return {client, dispose: () => {
      try {client.dispose();} finally {
        transport.dispose();
        try {cancelStopBinding();} finally {
          try {stopChannel?.close();} finally {window.removeEventListener("message", parentStop);}
        }
      }
    }};
  }, () => Array.from(crypto.getRandomValues(new Uint8Array(32)), byte => byte.toString(16).padStart(2, "0")).join(""), role === "operator" ? browserPreferences(() => window.localStorage) : undefined);
  let leftPage = false;
  window.addEventListener("pagehide", () => {leftPage = true; tab.leave(); host.dispose();}, {once: true});
  window.addEventListener("pageshow", event => {if (event.persisted) window.location.reload();});
  const start = entry;
  const loading = <main className="loading" role="status">Opening the interpreter controls…</main>;
  root.render(loading);
  void (async () => {
    try {
      const [ui, {default: wasmUrl}] = await Promise.all([
        import("../generated/interpreter_ui.js"), import("../generated/interpreter_ui_bg.wasm?url")
      ]);
      await ui.default({module_or_path: wasmUrl});
      if (leftPage) return;
      installRustSurfaces(ui.mount_operator_ui);
      void host.snapshot().client.initialize(start.ticket);
      root.render(<StrictMode><Suspense fallback={loading}><SessionApp host={host} /></Suspense></StrictMode>);
    } catch {
      tab.leave();
      host.dispose();
      if (!leftPage) root.render(<main className="invalid-entry" role="alert"><h1>The interpreter controls could not open.</h1><p>Use a browser with WebAssembly enabled and reopen the complete local download from the launcher.</p></main>);
    } finally {
      start.ticket = null;
    }
  })();
}
