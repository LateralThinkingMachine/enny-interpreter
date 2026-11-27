import {interpretationAnchor} from './protocol.js';
import {ORIGIN, PUBLIC_ORIGIN, identifier, object, parseEvent, parsePreview, parseSession, samePreview, parseInterpretation, parseInterpretationLifecycle, sameInterpretation} from "./protocol.js";
import type {Delivery, Event, FinishCompletion, Interpretation, InterpretationLifecycle, Language, LiveMode, Preview, ReceiptState, Role, Route, RouteKind, Session, Source} from "./protocol.js";
import type {PrivateKind} from "./audio/framing.js";
import type {CaptureStatus, CostNotificationState, Dependencies, SocketPort} from "./ports.js";
import {INTERPRETATION_LANGUAGES, LEGACY_LANGUAGES} from "./languages.js";
import {LIVE_TEXT_BYTES, PUBLIC_TEXT_BYTES, textFits} from "./text.js";
import {canPrepare, meetReservation, parseBudget, privateWindowEstimate} from "./cost.js";
import {captureMessage, providerMessage} from "./feedback.js";
import {parseStopAck, parseStopReceipt, type StopReceipt} from "./stop-receipt.js";
import {parseDiagnostics, stopMessage} from "./diagnostics.js";
import type {Diagnostics} from "./diagnostics.js";
import type {Budget} from "./cost.js";
import {speechHints, hasSpeechHints} from "./speech-hints.js";
import type {SpokenLanguage} from "./speech-hints.js";
import {LiveCaptions} from "./live-captions.js";
import type {LiveCaptionGroup} from "./live-captions.js";
import {SourceDrafts} from "./source-drafts.js";
import type {SourceDraft} from "./protocol.js";
import {SourceArchive} from "./source-archive.js";
import type {Fragment} from "./source-archive.js";
export type {Fragment} from "./source-archive.js";
import {CostMilestones, costNoticeText} from "./cost-notices.js";
import type {CostNotice} from "./cost-notices.js";

import type {PrivateListening, PrivateListeningOwner} from "./webrtc/private-listening.js";

export type EarlierSegment = Readonly<{generation: number; text: string; items: number}>;
export type ResumeWorkspace = Readonly<{segments: readonly EarlierSegment[]; dropped: number; notes: string; source: Source; interpretation: Interpretation | null; spokenLanguage: SpokenLanguage; speechTerms: string}>;

export type View = Readonly<{
  browserListening: boolean;
  interpretation: Interpretation | null; manualPause: InterpretationLifecycle | null; manualPauseChecking: boolean; manualAudioRetired: boolean; earlierSegments: readonly EarlierSegment[]; earlierSegmentsDropped: number;
  liveCaptions: readonly LiveCaptionGroup[]; liveCaptionDropped: number;
  spokenLanguage: SpokenLanguage; speechTerms: string; speechHintsError: string | null;
  costNotice: CostNotice | null; costNotificationState: CostNotificationState | "requesting";
  role: Role; session: Session | null; busy: boolean; connected: boolean; stopped: boolean; finishing: boolean; message: string;
  routes: readonly Route[]; fragments: readonly Fragment[]; sourceFragments: readonly Fragment[]; sourceDrafts: readonly SourceDraft[]; generatedDropped: number; coverageGaps: readonly Source[]; notes: string;
  capture: Readonly<Record<Source, "idle" | "starting" | "capturing" | "finishing" | "closed">>;
  muted: Readonly<Record<PrivateKind, boolean>>;
  preview: Preview | null; auditionComplete: boolean; auditioning: boolean; approvalAttempted: boolean;
  delivery: (Delivery & {state: ReceiptState}) | null; receipt: string; publicTicketReady: boolean;
  captureConfirmed: boolean; budget: Budget | null; budgetFresh: boolean; budgetRefreshing: boolean;
  meetSource: Source;
  /** The previous native selection is owned or its local release is unconfirmed. */
  meetSourceLocked: boolean;
  meetPhase: "idle" | "preview" | "connecting" | "starting_captions" | "starting_interpreter" | "listening" | "recovering" | "failed" | "paused";
  captureStatus: Readonly<Record<Source, CaptureStatus | null>>; meetSignalSeen: boolean; meetIssue: string | null;
  sentFrames: Readonly<Record<Source, number>>;
  aiPaused: boolean; renewalCount: number; sourceDiscarded: number;
  stopAcknowledged: boolean;
  localAudioRetirement: "checking" | "confirmed" | "unconfirmed";
  rtcAudioRetirement: "unused" | "confirmed" | "unconfirmed";
  stopReceipt: StopReceipt | null; stopReceiptState: "idle" | "checking" | "ready" | "unavailable" | "unauthorized";
  terminalMessage: string | null; diagnostics: Diagnostics | null;
  diagnosticsState: "idle" | "loading" | "ready" | "unavailable";
}>;
class RequestError extends Error {constructor(readonly status: number) {super("Pilot request failed");}}
class SetupError extends Error {}
function privateLane(route: Pick<Route, "route" | "live_mode">): PrivateKind {
  if (route.route === "translation") return "translation";
  if (route.route === "live") return route.live_mode === "interpretation" ? "live_interpretation" : "live";
  throw new Error("Source captions have no generated playback lane");
}
function guidedLiveAvailable(session: Session | null): boolean {
  return !!(session?.providers_enabled && session.capabilities.source_captions && session.capabilities.live_interpreter);
}
const MAX_FRAGMENTS = 500, MAX_TEXT_BYTES = 1_000_000;

export class PilotClient {
  privateListening: PrivateListening | null = null;
  private view: View;
  private listeners = new Set<() => void>();
  private socket: SocketPort | null = null;
  private pending = new Set<AbortController>();
  private epoch = 0;
  private hello = false;
  private lastHeartbeat = 0;
  private expires = 0;
  private cancelClock: (() => void) | null = null;
  private fragmentKey = 0;
  private budgetCheckedAt = -Infinity;
  private budgetRevision = 0;
  private costMilestones = new CostMilestones();
  private notificationRequested = false;
  private sourceArchive = new SourceArchive();
  private sourceDrafts = new SourceDrafts();
  private liveCaptions = new LiveCaptions();
  private publicTicket: {ticket: string; expires: number} | null = null;
  private seenDeliveries = new Set<string>();
  private previewPins = new Map<string, Preview>();
  private attemptedApprovals = new Set<string>();
  private deliveryBusy = false;
  private cleanupFailed = false;
  private audioStopFailed = false;
  private checkingAudio = false;
  private stopRead = 0;
  private stopAttempt = 0;
  private muteEpoch = {translation: 0, live: 0, live_interpretation: 0};
  // Keep immutable identities beyond the short visible closed-route history.
  // An activation admits at most 16 provider reservations.
  private routePins = new Map<string, Route>();
  private routeDeadlines = new Map<string, number>();
  private closingRoutes = new Set<string>();
  private meetAttempted = false;
  private meetQuickStart = false;
  private meetSelection = 0;
  private meetLastSignal = -Infinity;
  private frameCounts = {remote_mix: 0, self: 0};
  private waitChecks = new Set<() => void>();
  private windows = new Map<string, {number: number; state: "draining" | "renewing" | "recovering" | "ready"; deadline: number | null}>();
  private diagnosticsRequested = new Set<"paused" | "stopped">();
  private retired = false;
  private manualPausePollUntil = 0;
  private manualPausePolledAt = -Infinity;
  private authorizationLost = false;
  private finishState: {epoch: number; deadline: number; acknowledged: boolean; sources: readonly Source[]; resolve(text: string | null): void} | null = null;
  constructor(readonly role: Role, private readonly dependencies: Dependencies) {
    this.view = {stopAcknowledged: false, stopReceipt: null, stopReceiptState: "idle", localAudioRetirement: "unconfirmed", rtcAudioRetirement: "unconfirmed", browserListening: false, interpretation: null, manualPause: null, manualPauseChecking: false, manualAudioRetired: false, earlierSegments: [], earlierSegmentsDropped: 0, liveCaptions: [], liveCaptionDropped: 0, spokenLanguage: "auto", speechTerms: "", speechHintsError: null, costNotice: null, costNotificationState: "unavailable", aiPaused: false, renewalCount: 0, sourceDiscarded: 0, terminalMessage: null, diagnostics: null, diagnosticsState: "idle", meetSource: "remote_mix", meetSourceLocked: false, meetPhase: "idle", captureStatus: {remote_mix: null, self: null}, meetSignalSeen: false, meetIssue: null, sentFrames: {remote_mix: 0, self: 0}, role, session: null, busy: false, connected: false, stopped: false, finishing: false, message: "Checking this local pilot session…", routes: [], fragments: [], sourceFragments: [], sourceDrafts: [], generatedDropped: 0, coverageGaps: [], notes: "", capture: {remote_mix: "idle", self: "idle"}, muted: {translation: true, live: true, live_interpretation: true}, preview: null, auditionComplete: false, auditioning: false, approvalAttempted: false, delivery: null, receipt: "No public attempt", publicTicketReady: false, captureConfirmed: false, budget: null, budgetFresh: false, budgetRefreshing: false};
  }
  snapshot = (): View => this.view;
  /** The existing Pilot owns this private media extension and every Stop path. */
  privateListeningOwner(): PrivateListeningOwner {
    const available = () => this.role === "operator" && !this.retired && !this.authorizationLost
      && !!this.view.session?.providers_enabled && !!this.view.session.continuous && this.current(this.epoch)
      && !this.view.browserListening && !this.view.busy && !this.view.meetSourceLocked && this.view.meetPhase === "idle"
      && Object.values(this.view.capture).every(state => state === "idle")
      && !this.view.routes.some(route => route.state === "starting" || route.state === "ready");
    return {session: () => this.view.session, available,
      current: () => !this.retired && !this.authorizationLost && this.current(this.epoch) && !!this.view.session,
      claim: (displayedEstimate, allocationSeconds) => {
        if (!available() || !Number.isSafeInteger(displayedEstimate) || !this.freshBudget() || displayedEstimate !== privateWindowEstimate(this.view.budget, allocationSeconds)) return false;
        this.budgetRevision++; this.set({browserListening: true, budgetFresh: false, message: "Private listening is selected. Its panel shows current audio, connection and cost status."}); void this.refreshBudget(); return true;},
      connect: async () => {await this.connect(); await this.waitUntil(this.epoch, () => this.lease(), 5000, "The private control stream did not become ready."); this.ensureConnected();},
      release: () => {if (!this.retired) {this.budgetRevision++; this.set({browserListening: false, budgetFresh: false, ...(this.view.browserListening && this.current(this.epoch) ? {message: "Private listening has released the local audio. See its panel for the last result and cost closure."} : {})}); void this.refreshBudget();}},
      subscribe: this.subscribe, authorityLost: () => this.fault("Private listening authorization was lost.", true)};
  }
  subscribe = (listener: () => void): (() => void) => {this.listeners.add(listener); return () => this.listeners.delete(listener);};
  private emit(): void {for (const listener of this.listeners) listener();}
  private set(update: Partial<View>): void {this.view = {...this.view, ...update}; this.emit();}
  private observing(epoch: number): boolean {return epoch === this.epoch && !this.view.stopped;}
  private current(epoch: number): boolean {return this.observing(epoch) && !this.view.finishing && !this.view.aiPaused && !this.view.manualPause;}
  private leaseMilliseconds(): number {return this.role === "operator" && this.view.session?.continuous ? 30_000 : 3000;}
  private lease(): boolean {return !this.view.stopped && this.hello && this.socket?.readyState === 1 && this.dependencies.now() < this.expires && (this.view.manualPause?.local_retired === true || this.dependencies.now() - this.lastHeartbeat < this.leaseMilliseconds());}
  private ensureConnected(): void {if (!this.lease()) throw new RequestError(409);}
  private detach(): void {
    this.hello = false;
    const socket = this.socket; this.socket = null;
    if (socket) {socket.onopen = null; socket.onmessage = null; socket.onclose = null; socket.onerror = null; socket.close();}
  }
  private cleanup(work: () => void): boolean {
    try {work(); return true;} catch {this.cleanupFailed = true; return false;}
  }
  private cleanupMessage(message: string): string {return this.cleanupFailed ? message + " Some local resource cleanup was unconfirmed. Close the pilot page and check the selected devices." : message;}
  private clear(message: string, broadcast: boolean): void {
    if (this.view.stopped) return;
    // Latch before any callbacks, cancellation, audio teardown or notification.
    this.view = {...this.view, stopped: true, spokenLanguage: "auto", speechTerms: "", speechHintsError: null};
    this.epoch++; this.meetSelection++;
    if (this.role === "operator" && !this.retired) {
      let rtc: View["rtcAudioRetirement"] = "unconfirmed";
      this.cleanup(() => {
        const phase = this.privateListening?.snapshot().phase;
        rtc = !this.privateListening || phase === "idle" ? "unused" : phase === "closed" ? "confirmed" : "unconfirmed";
      });
      this.view = {...this.view, rtcAudioRetirement: rtc};
    }
    this.cleanup(() => this.privateListening?.dispose());
    this.frameCounts = {remote_mix: 0, self: 0}; this.meetLastSignal = -Infinity;
    const finish = this.finishState; this.finishState = null; finish?.resolve(null);
    for (const request of this.pending) this.cleanup(() => request.abort()); this.pending.clear();
    this.audioStopFailed = !this.cleanup(() => this.dependencies.audio.stopAll()); this.cleanup(() => this.detach()); this.cleanup(() => this.cancelClock?.()); this.cancelClock = null;
    this.publicTicket = null; this.previewPins.clear(); this.seenDeliveries.clear(); this.attemptedApprovals.clear(); this.routeDeadlines.clear(); this.routePins.clear(); this.closingRoutes.clear();
    this.sourceArchive.clear(); this.sourceDrafts.clear(); this.liveCaptions.clear(); this.windows.clear();
    this.cleanup(() => this.dependencies.costNotifications?.close());
    this.view = {...this.view, manualPause: null, manualPauseChecking: false, manualAudioRetired: false, earlierSegments: [], earlierSegmentsDropped: 0, costNotice: null, costNotificationState: "unavailable", meetPhase: "idle", captureStatus: {remote_mix: null, self: null}, meetSignalSeen: false, sentFrames: {remote_mix: 0, self: 0}, busy: false, connected: false, finishing: false, budget: null, budgetFresh: false, budgetRefreshing: false, message, fragments: [], liveCaptions: [], liveCaptionDropped: 0, sourceFragments: [], sourceDrafts: [], generatedDropped: 0, coverageGaps: [], notes: "", routes: [], preview: null, auditionComplete: false, auditioning: false, delivery: null, publicTicketReady: false, captureConfirmed: false, muted: {translation: true, live: true, live_interpretation: true}, capture: {remote_mix: "closed", self: "closed"}};
    if (broadcast) this.cleanup(() => this.dependencies.broadcastStop());
    this.view = {...this.view, message: this.cleanupMessage(message)};
    this.emit();
    void this.checkLocalAudioRetirement();
  }
  private fault(message = "Connection or authorization was lost. This page is stopped; it cannot resume itself.", retire = false): void {
    if (this.view.stopped || this.retired) return;
    if (this.view.manualPause && !retire) {
      this.cleanup(() => this.detach());
      this.set({connected: false, manualPause: {...this.view.manualPause, state: "closure_unconfirmed", resume_allowed: false}, message: "Interpretation is paused locally. The control connection closed; check service status before resuming."});
      return;
    }
    if (this.view.session?.continuous && this.role === "operator" && !retire) {
      void this.pauseAI(message); this.cleanup(() => this.detach()); this.set({connected: false}); return;
    }
    this.view = {...this.view, terminalMessage: this.view.aiPaused ? this.view.terminalMessage ?? message : message};
    this.clear(message, this.role === "operator");
    void this.readDiagnostics();
    void this.refreshStopReceipt();
  }
  private async readDiagnostics(phase: "paused" | "stopped" = "stopped"): Promise<void> {
    const session = this.view.session;
    const inPhase = () => phase === "stopped" ? this.view.stopped : this.view.aiPaused && !this.view.stopped && !this.view.finishing;
    if (this.role !== "operator" || !session || !inPhase() || this.retired || this.diagnosticsRequested.has(phase)) return;
    // At most one observation per phase. Stop has its own newer epoch and read.
    this.diagnosticsRequested.add(phase);
    const epoch = this.epoch;
    const current = () => !this.retired && inPhase() && this.epoch === epoch && this.view.session === session;
    this.set({diagnosticsState: "loading"});
    if (!current()) return;
    try {
      const bytes = await this.response("/api/diagnostics", undefined, 2048, 5000);
      if (!current()) return;
      const value: unknown = JSON.parse(new TextDecoder("utf-8", {fatal: true}).decode(bytes));
      const diagnostics = parseDiagnostics(value, session.generation);
      this.set({diagnostics, diagnosticsState: "ready"});
    } catch (error) {
      if (!current()) return;
      if (phase === "paused" && error instanceof RequestError && [401, 403].includes(error.status)) {
        this.diagnosticsRequested.add("stopped");
        this.set({diagnostics: null, diagnosticsState: "unavailable"});
        this.fault("Diagnostic authorization was lost. Private data is cleared and this page is stopped.", true);
      } else this.set({diagnosticsState: "unavailable"});
    }
  }
  private async checkLocalAudioRetirement(): Promise<void> {
    const session = this.view.session;
    if (this.role !== "operator" || !session || !this.view.stopped || this.retired || this.checkingAudio || this.view.localAudioRetirement === "confirmed") return;
    this.checkingAudio = true;
    this.set({localAudioRetirement: "checking"});
    let timeout: ReturnType<typeof setTimeout> | undefined;
    let confirmed = false;
    try {
      confirmed = !!this.dependencies.audio.retire && await Promise.race([
        this.dependencies.audio.retire(), new Promise<boolean>(resolve => {timeout = setTimeout(() => resolve(false), 1800);}),
      ]);
    } catch { /* A disposed handle or open chooser cannot confirm retirement. */ }
    finally {clearTimeout(timeout); this.checkingAudio = false;}
    if (!this.retired && this.view.session === session) this.set({localAudioRetirement: confirmed && !this.audioStopFailed ? "confirmed" : "unconfirmed"});
  }

  /** Read-only, private and generation-bound. A delayed result never settles a successor. */
  async refreshStopReceipt(): Promise<void> {
    const session = this.view.session;
    if (this.role !== "operator" || !session || !this.view.stopped || this.retired || this.authorizationLost) return;
    const revision = ++this.stopRead;
    const current = () => revision === this.stopRead && !this.retired && !this.authorizationLost && this.view.session === session;
    this.set({stopReceiptState: "checking"});
    for (let attempt = 0; attempt < 3 && current(); attempt++) {
      try {
        const bytes = await this.response("/api/stop-receipt", undefined, 1024, 1500);
        const receipt = parseStopReceipt(JSON.parse(new TextDecoder("utf-8", {fatal: true}).decode(bytes)), session);
        if (!current()) return;
        const finished = receipt.provider_activity !== "unconfirmed" || attempt === 2;
        this.set({stopReceipt: receipt, stopReceiptState: finished ? "ready" : "checking"});
        if (finished) return;
        await new Promise<void>(resolve => setTimeout(resolve, attempt === 0 ? 350 : 800));
      } catch {
        if (current()) this.set({stopReceiptState: "unavailable", stopReceipt: null});
        return;
      }
    }
  }

  async recheckStopEvidence(): Promise<void> {
    await Promise.all([this.checkLocalAudioRetirement(), this.refreshStopReceipt()]);
  }

  async stop(broadcast = true): Promise<void> {
    if (this.retired) return;
    const session = this.view.session, attempt = ++this.stopAttempt;
    this.clear("Stopped on this page. Private text is cleared. Your meeting app keeps its own mute setting.", broadcast);
    if (!session) return;
    const epoch = this.epoch;
    let acknowledged = false;
    try {
      if (this.role === "operator") {
        const bytes = await this.response("/api/stop", {}, 512, 5000);
        parseStopAck(JSON.parse(new TextDecoder("utf-8", {fatal: true}).decode(bytes)), session);
        acknowledged = true;
      } else {
        const response = await this.dependencies.fetch("/api/public/logout", {method: "POST", mode: "same-origin", credentials: "omit", redirect: "error", cache: "no-store", headers: {"Content-Type": "application/json", "X-Pilot-CSRF": session.csrf}, body: "{}", signal: AbortSignal.timeout(5000)});
        acknowledged = response.ok;
      }
    } catch { /* The local Stop remains in force regardless of delivery. */ }
    if (this.retired || this.epoch !== epoch || this.view.session !== session) return;
    // A positive response to any attempt proves that same session stopped.
    // An older failure contributes nothing, nor can a later failure undo proof.
    if (attempt !== this.stopAttempt && (!acknowledged || this.authorizationLost || this.view.stopAcknowledged)) return;
    acknowledged = this.view.stopAcknowledged || (acknowledged && !this.authorizationLost);
    this.set({stopAcknowledged: acknowledged, message: this.cleanupMessage(acknowledged ? (session.repeatable_sessions ? "Session ended. Start a new session when you are ready." : "Stop recorded by the local server. Start a new operator process for another session.") : "Stopped locally. Server Stop is unconfirmed; keep this page stopped.")});
    void this.refreshStopReceipt();
    await this.readDiagnostics();
  }
  async logout(): Promise<void> {
    this.retired = true;
    this.dependencies.beginLogout?.();
    for (const request of this.pending) this.cleanup(() => request.abort()); this.pending.clear();
    const csrf = this.view.session?.csrf;
    this.clear("Signed out locally. Private text and pending work are cleared.", this.role === "operator");
    this.set({stopReceipt: null, stopAcknowledged: false, stopReceiptState: "idle", localAudioRetirement: "unconfirmed", rtcAudioRetirement: "unconfirmed", session: null, terminalMessage: null, diagnostics: null, diagnosticsState: "idle"});
    if (!csrf) return;
    try {const response = await this.dependencies.fetch(this.role === "operator" ? "/api/logout" : "/api/public/logout", {method: "POST", mode: "same-origin", credentials: "omit", redirect: "error", cache: "no-store", headers: {"Content-Type": "application/json", "X-Pilot-CSRF": csrf}, body: "{}", signal: AbortSignal.timeout(5000)}); if (!response.ok) throw new Error("Session revocation unconfirmed");} catch {this.set({message: "Signed out locally; server session revocation is unconfirmed. Close this pilot process."});}
  }
  dispose(): void {this.retired = true; for (const request of this.pending) this.cleanup(() => request.abort()); this.pending.clear(); this.clear("Page closed.", this.role === "operator"); this.listeners.clear();}

  private async response(path: string, body: unknown | undefined, limit: number, milliseconds = 15_000, signal?: AbortSignal): Promise<ArrayBuffer> {
    const controller = new AbortController(); this.pending.add(controller);
    const cancel = () => controller.abort();
    if (signal?.aborted) controller.abort(); else signal?.addEventListener("abort", cancel, {once: true});
    const timeout = setTimeout(() => controller.abort(), milliseconds);
    try {
      const headers: Record<string, string> = {};
      if (body !== undefined) {headers["Content-Type"] = "application/json"; if (this.view.session) headers["X-Pilot-CSRF"] = this.view.session.csrf;}
      const payload = body === undefined ? undefined : JSON.stringify(body);
      if (payload && new TextEncoder().encode(payload).length > 16_384) throw new RequestError(400);
      const response = await this.dependencies.fetch(path, {method: body === undefined ? "GET" : "POST", credentials: "omit", mode: "same-origin", redirect: "error", cache: "no-store", headers, body: payload, signal: controller.signal});
      if (!response.ok) {if ([401, 403].includes(response.status) && !this.retired && !controller.signal.aborted) {this.authorizationLost = true; this.set({stopReceipt: null, stopReceiptState: "unauthorized"});} throw new RequestError(response.status);}
      const contentType = response.headers.get("Content-Type")?.split(";", 1)[0].trim();
      if (contentType !== (path.endsWith("/audio") || path.startsWith("/api/public/audio/") ? "audio/wav" : "application/json")) throw new Error("Invalid response type");
      const reader = response.body?.getReader(); if (!reader) throw new Error("Missing response");
      let length = 0; const chunks: Uint8Array[] = [];
      try {for (;;) {const {done, value} = await reader.read(); if (controller.signal.aborted) throw new Error("Stopped request"); if (done) break; length += value.byteLength; if (length > limit) throw new Error("Oversize response"); chunks.push(value);}} finally {await reader.cancel().catch(() => {});}
      const result = new Uint8Array(length); let offset = 0; for (const chunk of chunks) {result.set(chunk, offset); offset += chunk.byteLength;}
      return result.buffer;
    } finally {clearTimeout(timeout); signal?.removeEventListener("abort", cancel); this.pending.delete(controller);}
  }
  private async json(path: string, body?: unknown, milliseconds?: number): Promise<unknown> {return JSON.parse(new TextDecoder("utf-8", {fatal: true}).decode(await this.response(path, body, 32_768, milliseconds)));}
  private async operation(work: (epoch: number) => Promise<void>): Promise<void> {
    if (this.view.busy || this.view.stopped || this.view.finishing || this.view.aiPaused || !this.view.session) return;
    const epoch = this.epoch; this.set({busy: true});
    if (!this.current(epoch)) return;
    try {await work(epoch);} catch (error) {
      if (!this.current(epoch)) return;
      if (error instanceof RequestError && [400, 429, 503].includes(error.status)) this.set({message: error.status === 503 ? "This capability is unavailable. No successful action is claimed." : "Request refused or capacity reached. Review the current state before a new action."});
      else this.fault("The action could not be confirmed safely. This page is stopped; no automatic retry or playback.");
    } finally {if (this.current(epoch)) this.set({busy: false});}
  }
  async initialize(ticket: string | null): Promise<void> {
    if (this.view.busy || this.view.stopped || this.view.session) return;
    const epoch = this.epoch; this.set({busy: true});
    if (!this.current(epoch)) return;
    try {
      const path = this.role === "operator" ? "/api/" : "/api/public/";
      const value = await this.json(path+(ticket ? "bootstrap" : "session"), ticket ? {ticket} : undefined);
      if (!this.current(epoch)) return;
      const session = parseSession(value, this.role);
      this.acceptSession(session);
      if (this.current(epoch)) await this.refreshBudget();
    } catch {if (this.current(epoch)) this.set({message: "No valid local session. Open the fresh one-use link supplied by the operator."});}
    finally {if (this.current(epoch)) this.set({busy: false});}
  }
  /** A returned session is installed only into a new, unused client/audio owner. */
  initializeFromSession(value: Session): void {
    if (this.retired || this.view.session || this.view.stopped || this.view.busy) throw new Error("Client already used");
    const session = parseSession(value, this.role);
    if (session.stopped) throw new Error("Cannot install stopped session");
    this.acceptSession(session);
    void this.refreshBudget();
  }
  private acceptSession(session: Session): void {
    const epoch = this.epoch;
    this.expires = session.continuous ? Infinity : this.dependencies.now() + session.expires_in_seconds! * 1000;
    if (session.continuous) this.sourceArchive.continuous();
    const entryMessage = this.role === "public" ? "Ready for approved public playback."
      : !session.providers_enabled ? "Key-free rehearsal. Choose an audio source and check it locally."
      : guidedLiveAvailable(session) ? "Choose your language and start live interpretation."
      : "Guided interpretation requires both source captions and interpreter permission for this session.";
    this.set({session, costNotificationState: this.role === "operator" && session.continuous && session.providers_enabled ? this.notificationStatus() : "unavailable", message: entryMessage});
    if (!this.current(epoch)) return;
    if (session.stopped || session.expires_in_seconds === 0) {
      this.clear(session.repeatable_sessions ? "Session ended. Start a new session when you are ready." : "This local process has stopped or expired. A fresh process is required.", false);
      void this.readDiagnostics(); void this.refreshStopReceipt(); return;
    }
    this.cancelClock = this.dependencies.interval(() => this.tick(), 1000);
    if (this.role === "operator" && session.repeatable_sessions && !session.providers_enabled) void this.readInitialInterpretationPause();
  }
  canCreateSession(): boolean {
    return this.role === "operator" && this.view.session?.repeatable_sessions === true && !this.retired && !this.authorizationLost && !this.cleanupFailed && !this.view.finishing;
  }
  async retireForNewSession(): Promise<boolean> {
    if (!this.canCreateSession() || !this.view.stopped) return false;
    try {return this.dependencies.audio.retire ? await this.dependencies.audio.retire() : true;}
    catch {return false;}
  }
  /** Exact idempotency receipt; this never revives this instance or sends audio. */
  async requestNewSession(requestId: string): Promise<Session> {
    const session = this.view.session;
    if (!this.canCreateSession() || !this.view.stopped || !session || !/^[a-f0-9]{64}$/.test(requestId)) throw new Error("New session unavailable");
    const generation = session.session_generation ?? session.generation;
    const row = object(await this.json("/api/session/new", {previous_generation: generation, request_id: requestId}, 25_000), ["state", "previous_generation", "request_id", "session"]);
    if (!this.canCreateSession() || this.view.session !== session || row.state !== "ready" || row.previous_generation !== generation || row.request_id !== requestId) throw new Error("Session changed");
    const next = parseSession(row.session, "operator");
    if (!next.repeatable_sessions || next.stopped || next.generation <= session.generation || next.csrf === session.csrf) throw new Error("Invalid successor");
    return next;
  }
  private tick(): void {
    if (this.view.stopped) return;
    if (this.dependencies.now() >= this.expires) {this.fault("The local session deadline was reached. All output is stopped."); return;}
    for (const check of [...this.waitChecks]) check();
    if (this.view.stopped) return;
    if (this.view.manualPause?.state === "pausing" && !this.view.manualPauseChecking && this.dependencies.now() < this.manualPausePollUntil && this.dependencies.now() - this.manualPausePolledAt >= 1000) void this.checkInterpretationPause();
    if (this.view.meetPhase === "preview" && this.view.meetSignalSeen && this.dependencies.now() - this.meetLastSignal > 20_000) this.set({meetSignalSeen: false});
    if (this.publicTicket && this.dependencies.now() >= this.publicTicket.expires) {this.publicTicket = null; this.set({publicTicketReady: false});}
    if (this.role === "operator" && !this.view.finishing && !this.view.busy && this.dependencies.now() - this.budgetCheckedAt >= 15_000) void this.refreshBudget();
    let changed = false, meetExpired = false;
    const routes = this.view.routes.map(route => {
      const deadline = this.routeDeadlines.get(route.route_id);
      if (route.state !== "ready" || deadline === undefined) return route;
      const remaining = Math.max(0, Math.ceil((deadline - this.dependencies.now()) / 1000));
      changed ||= remaining !== route.expires_in_seconds;
      if (remaining === 0 && this.view.session?.continuous) {
        const window = this.windows.get(route.route_id);
        if (window && window.deadline === null) window.deadline = deadline + 35_000;
        return {...route, expires_in_seconds: 0};
      }
      if (remaining === 0) {meetExpired ||= this.meetAttempted && route.source === this.view.meetSource; this.routePins.set(route.route_id, {...route, state: "closed", expires_in_seconds: null}); this.routeDeadlines.delete(route.route_id); this.closeSourceItems({route_id: route.route_id}); if (route.route !== "transcription") this.muteNow(privateLane(route)); return {...route, state: "closed" as const, expires_in_seconds: null};}
      return {...route, expires_in_seconds: remaining};
    });
    if (changed && !this.view.stopped) this.set({routes});
    if (this.view.stopped) return;
    if (meetExpired && !this.view.finishing) {void this.stopMeetWithIssue("The test's speech service time limit was reached. Local capture and interpretation stopped."); return;}
    if (!this.view.aiPaused && [...this.windows.values()].some(window => window.deadline !== null && this.dependencies.now() >= window.deadline)) {void this.pauseAI("The next speech window did not become ready in time. AI is paused; captions and notes are retained.");}
    const socket = this.socket;
    if (!socket) return;
    if (this.view.manualPause?.local_retired !== true && this.dependencies.now() - this.lastHeartbeat >= this.leaseMilliseconds()) {this.fault("The local service is no longer responding. Capture and playback are paused; your captions and notes are retained."); return;}
    if (this.finishState && this.dependencies.now() >= this.finishState.deadline) {this.completeFinish("incomplete"); return;}
    if (socket.readyState === 1) {try {socket.send(JSON.stringify({type: "heartbeat", generation: this.view.session!.generation}));} catch {this.fault();}}
  }
  async connect(): Promise<void> {
    await this.operation(async epoch => {
      if (this.socket) return;
      if (this.role === "public") {await this.dependencies.audio.resumePlayback(); if (!this.current(epoch)) return;}
      this.connectSocket(epoch);
    });
  }
  private connectSocket(epoch: number): void {
    if (this.socket || !this.current(epoch)) return;
      this.lastHeartbeat = this.dependencies.now();
      const socket = this.dependencies.socket((this.role === "operator" ? ORIGIN : PUBLIC_ORIGIN).replace("http:", "ws:")+(this.role === "operator" ? "/api/operator/events" : "/api/public/events"));
      this.socket = socket;
      socket.onopen = () => {if (this.observing(epoch) && socket === this.socket) this.tick();};
      socket.onclose = socket.onerror = () => {if (socket === this.socket) this.fault();};
      socket.onmessage = event => {if (!this.observing(epoch) || socket !== this.socket) return; try {if (typeof event.data !== "string") throw new Error("Invalid frame"); this.receive(event.data);} catch {this.fault("Invalid stream data. The pilot is stopped rather than trusting an unknown event.");}};
      this.set({message: "Connecting. No previous audio will replay."});
  }
  private receive(raw: string): void {
    const event = parseEvent(raw, this.role), session = this.view.session!;
    if (event.type === "stopped" && event.generation >= session.generation) {this.fault(this.role === "operator" ? stopMessage(event.reason) : "The operator or server stopped this process. Output has been cleared.", event.reason === "operator_stop"); return;}
    if (event.generation !== session.generation || this.dependencies.now() >= this.expires) throw new Error("Stale stream");
    if (event.type === "hello") {if (this.hello) throw new Error("Duplicate hello"); if (event.stopped) {this.fault(); return;} this.hello = true; this.lastHeartbeat = this.dependencies.now(); this.set({connected: true, message: this.role === "public" ? "Public screen armed. Only a new, separately approved clip may play." : this.view.browserListening ? "Private control stream connected. The private listening panel shows current audio, connection and cost status." : "Private stream connected. Open a route, then explicitly select its audio source."}); return;}
    if (!this.hello || !this.lease()) throw new Error("No current stream lease");
    if (event.type === "heartbeat") {
      this.lastHeartbeat = this.dependencies.now();
      // Acknowledge network events directly; a background tab's timer schedule
      // must not decide whether ordinary pauses count as a lost connection.
      if (session.continuous && this.role === "operator") this.socket?.send(JSON.stringify({type: "heartbeat", generation: session.generation}));
      return;
    }
    if (this.view.aiPaused || this.view.manualPause) return;
    if (event.type === "window") {this.receiveWindow(event); return;}
    if (event.type === "finishing" || event.type === "finished") {
      const finish = this.finishState;
      if (!finish || !this.view.finishing) throw new Error("Unsolicited finish marker");
      if (event.type === "finishing") {
        if (finish.acknowledged) throw new Error("Duplicate finish admission");
        finish.acknowledged = true;
      } else {
        if (!finish.acknowledged) throw new Error("Finish marker preceded admission");
        this.completeFinish(event.source_completion);
      }
      return;
    }
    if (this.view.finishing && (event.type === "preview" || event.type === "receipt")) return;
    if (event.type === "gap") {this.gap(event.source, false, event.code); return;}
    if (event.type === "route") {
      if (this.view.capture[event.source] === "closed" && event.state !== "closed" && event.state !== "failed") return;
      const previous = this.routePins.get(event.route_id);
      if (previous && (previous.source !== event.source || previous.route !== event.route || previous.language !== event.language || previous.live_mode !== event.live_mode || !sameInterpretation(previous.interpretation, event.interpretation))) throw new Error("Changed route identity");
      if (previous && (previous.state === "closed" || previous.state === "failed") && (event.state === "ready" || event.state === "starting")) return;
      if (!previous && this.routePins.size >= 16) throw new Error("Route identity capacity");
      const routes = this.view.routes.filter(route => route.route_id !== event.route_id);
      if ((event.state === "ready" || event.state === "starting") && routes.filter(route => route.state === "ready" || route.state === "starting").length >= 6) throw new Error("Route capacity");
      const {route_id, source, route, live_mode, language, interpretation, state, code, expires_in_seconds} = event;
      const eventEpoch = this.epoch;
      this.invalidateBudget();
      if (!this.observing(eventEpoch)) return;
      if (expires_in_seconds !== null) this.routeDeadlines.set(route_id, this.dependencies.now() + expires_in_seconds * 1000); else this.routeDeadlines.delete(route_id);
      if (state === "closed" || state === "failed") {this.closeSourceItems({route_id}); if (route !== "transcription") this.muteNow(privateLane(event));}
      if (this.view.stopped) return;
      const identity = {route_id, source, route, live_mode, language, ...(interpretation ? {interpretation} : {}), state, code, expires_in_seconds};
      this.routePins.set(route_id, identity);
      const next = [...routes, identity];
      this.set({routes: [...next.filter(item => item.state === "ready" || item.state === "starting"), ...next.filter(item => item.state === "closed" || item.state === "failed").slice(-6)]});
      if (this.current(eventEpoch) && this.meetAttempted && source === this.view.meetSource && (state === "closed" || state === "failed")) {void this.stopMeetWithIssue(providerMessage(code)); return;}
      if (this.current(eventEpoch)) void this.refreshBudget();
      return;
    }
    if (event.type === "transcript" || event.type === "audio" || event.type === "source_draft" || event.type === "live_transcript") {
      if (this.view.finishing && (event.type === "live_transcript" || event.type === "audio" || (event.type === "transcript" && event.provenance !== "source_asr"))) return;
      if (this.view.capture[event.source] === "closed") return;
      const binding = this.view.routes.find(route => route.route_id === event.route_id);
      if (!binding || binding.source !== event.source || binding.route !== event.route) throw new Error("Unknown route");
      if (binding.state !== "ready" || (!session.continuous && this.dependencies.now() >= (this.routeDeadlines.get(binding.route_id) ?? 0))) return;
      if (session.continuous) {
        const window = this.windows.get(event.route_id);
        if (event.window === undefined || !window || event.window > window.number) throw new Error("Missing or unknown speech window");
        if (event.window < window.number || window.state === "renewing" || window.state === "recovering") return;
      } else if (event.window !== undefined) throw new Error("Unexpected speech window");
      if (event.type !== "source_draft" && (!sameInterpretation(binding.interpretation, event.interpretation))) throw new Error("Changed content routing");
      if (event.type === "audio") {
        const lane = privateLane(binding);
        if (!this.view.muted[lane]) {
          this.dependencies.audio.playPrivate(lane, event.pcm, event.sample_rate);
        }
        return;
      }
      if (event.type === "live_transcript") {
        if (!binding.live_mode) throw new Error("Live caption mode missing");
        this.liveCaptions.apply({...event, language: binding.language, live_mode: binding.live_mode, ...(binding.interpretation ? {interpretation: binding.interpretation} : {})});
        this.set({liveCaptions: this.liveCaptions.items(), liveCaptionDropped: this.liveCaptions.discardedCount()}); return;
      }
      if (event.type === "source_draft") {this.sourceDrafts.apply(event); if (!this.view.finishing) this.set({sourceDrafts: this.sourceDrafts.items()}); return;}
      const {route_id, source, route, provenance, text, final, item} = event;
      const scope = event.window === undefined ? {} : {window: event.window};
      if (provenance === "source_asr") {
        this.sourceDrafts.bind(route_id, source, event.window, item!.id);
        const result = this.sourceArchive.apply({...scope, key: ++this.fragmentKey, route_id, source, route, language: binding.language, live_mode: binding.live_mode, ...(binding.interpretation ? {interpretation: binding.interpretation} : {}), provenance, text, final, item});
        if (result === "capacity") {this.gap(source); if (!this.view.stopped) this.set({message: "Source archive capacity reached. The affected source is closed with a coverage gap; existing captions remain available for explicit export."}); return;}
        if (result === "changed") {
          this.set({sourceFragments: this.sourceArchive.items(), sourceDiscarded: this.sourceArchive.discardedCount()});
        } return;
      }
      const fragments = [...this.view.fragments, {...scope, key: ++this.fragmentKey, route_id, source, route, language: binding.language, live_mode: binding.live_mode, ...(binding.interpretation ? {interpretation: binding.interpretation} : {}), provenance, text, final, item}].slice(-MAX_FRAGMENTS);
      let bytes = fragments.reduce((total, fragment) => total + fragment.text.length * 2, 0);
      while (bytes > MAX_TEXT_BYTES && fragments.length) bytes -= fragments.shift()!.text.length * 2;
      this.set({fragments, generatedDropped: this.view.generatedDropped + this.view.fragments.length + 1 - fragments.length}); return;
    }
    if (event.type === "preview") {this.pinPreview(event.preview); return;}
    if (event.type === "receipt") {this.set({receipt: event.state+" · local browser report only; not proof anyone on the call heard it"}); return;}
    if (event.type === "delivery") {void this.deliver(event);}
  }
  sendFrame(source: Source, frame: ArrayBuffer): boolean {
    if (this.role !== "operator" || this.view.aiPaused || this.view.manualPause || !this.lease() || !["starting", "capturing"].includes(this.view.capture[source])) return false;
    // Keep the source sequence continuous while an expired provider route is
    // explicitly renewed. Server dispatch remains restricted to ready routes.
    if (frame.byteLength !== 646 || this.socket!.bufferedAmount > 646 * 8) {this.gap(source); return false;}
    const header = new DataView(frame);
    if (header.getUint8(0) !== 1 || header.getUint8(1) !== (source === "remote_mix" ? 0 : 1)) {this.gap(source); return false;}
    try {this.socket!.send(frame); this.frameCounts[source] = Math.min(Number.MAX_SAFE_INTEGER, this.frameCounts[source] + 1); return true;} catch {this.gap(source); return false;}
  }
  private closeSourceItems(scope: {route_id?: string; source?: Source}): void {
    if (this.view.stopped) return;
    const incomplete = this.sourceDrafts.incompleteSources(scope);
    if (incomplete.length) this.view = {...this.view, coverageGaps: [...new Set([...this.view.coverageGaps, ...incomplete])]};
    this.liveCaptions.close(scope); this.view = {...this.view, liveCaptions: this.liveCaptions.items()};
    this.sourceArchive.close(scope); this.sourceDrafts.clear(scope); this.view = {...this.view, sourceFragments: this.sourceArchive.items(), sourceDrafts: this.sourceDrafts.items()};
  }
  private receiveWindow(event: Extract<Event, {type: "window"}>): void {
    if (!this.view.session?.continuous) throw new Error("Unexpected renewal");
    const epoch = this.epoch;
    const route = this.routePins.get(event.route_id), previous = this.windows.get(event.route_id);
    if (!route || route.source !== event.source || route.route !== event.route || ["closed", "failed"].includes(route.state)) throw new Error("Unbound renewal");
    if (previous && event.window < previous.number) return;
    if (route.route === "live" && event.state !== "ready") {this.liveCaptions.close({route_id: event.route_id}); this.view = {...this.view, liveCaptions: this.liveCaptions.items()};}
    if (event.state === "renewing" && this.sourceDrafts.incompleteSources({route_id: event.route_id}).length) {void this.pauseAI(providerMessage("source_incomplete")); return;}
    if (event.state === "recovering" || event.state === "renewing") {this.sourceDrafts.clear({route_id: event.route_id}); this.view = {...this.view, sourceDrafts: this.sourceDrafts.items()};}
    if (event.state === "recovering") {
      if (event.window !== (previous?.number ?? 1) + 1) throw new Error("Invalid recovery order");
      if (!previous && this.windows.size >= 16) throw new Error("Window identity capacity");
      this.windows.set(event.route_id, {number: event.window, state: "recovering", deadline: this.dependencies.now() + 60_000});
      if (route.route === "transcription") {
        if (!previous) this.sourceArchive.advanceWindow(event.route_id, 1);
        this.sourceArchive.advanceWindow(event.route_id, event.window);
      }
      else this.dependencies.audio.setPrivateMuted(privateLane(route), true);
      if (!this.current(epoch)) return;
      this.set({meetPhase: this.meetAttempted ? "recovering" : this.view.meetPhase,
        sourceFragments: this.sourceArchive.items(), sourceDiscarded: this.sourceArchive.discardedCount(),
        renewalCount: Math.min(Number.MAX_SAFE_INTEGER, this.view.renewalCount + 1),
        coverageGaps: [...new Set([...this.view.coverageGaps, event.source])],
        message: "Reconnecting the speech service. Keep speaking when Listening returns; your selected audio and notes stay in place."});
    } else if (event.state === "draining") {
      if (!previous || previous.state !== "ready" || event.window !== previous.number) throw new Error("Invalid drain order");
      this.windows.set(event.route_id, {...previous, state: "draining", deadline: (this.routeDeadlines.get(event.route_id) ?? this.dependencies.now()) + 35_000});
      this.set({meetPhase: this.meetAttempted ? "recovering" : this.view.meetPhase, message: "Refreshing the speech connection. Your selected audio stays connected."});
    } else if (event.state === "renewing") {
      if (!previous || !["ready", "draining"].includes(previous.state) || event.window !== previous.number + 1) throw new Error("Invalid renewal order");
      this.windows.set(event.route_id, {number: event.window, state: "renewing", deadline: this.dependencies.now() + 35_000});
      if (route.route === "transcription" && this.sourceArchive.advanceWindow(event.route_id, event.window)) {
        void this.pauseAI(providerMessage("source_incomplete")); return;
      }
      // Clear old queued sound without changing the operator's mute preference.
      if (route.route !== "transcription") this.dependencies.audio.setPrivateMuted(privateLane(route), true);
      if (!this.current(epoch)) return;
      this.set({renewalCount: Math.min(Number.MAX_SAFE_INTEGER, this.view.renewalCount + 1), coverageGaps: [...new Set([...this.view.coverageGaps, event.source])], message: "Refreshing a speech connection. Audio during this transition is not processed; the selected audio stays connected."});
    } else {
      if ((!previous && event.window !== 1) || (previous && (previous.number !== event.window || !["renewing", "recovering"].includes(previous.state)))) throw new Error("Invalid ready window");
      if (!previous && this.windows.size >= 16) throw new Error("Window identity capacity");
      if (!previous && route.route === "transcription") this.sourceArchive.advanceWindow(event.route_id, 1);
      this.windows.set(event.route_id, {number: event.window, state: "ready", deadline: null});
      if (route.route !== "transcription" && !this.view.muted[privateLane(route)]) this.dependencies.audio.setPrivateMuted(privateLane(route), false);
      if (!this.current(epoch)) return;
      const pairReady = this.meetAttempted && this.view.capture[this.view.meetSource] === "capturing" && [...this.windows.values()].every(window => window.state === "ready");
      this.set({meetPhase: pairReady ? "listening" : this.view.meetPhase,
        ...(pairReady ? {message: "Listening. Pauses are normal; interpretation continues when speech resumes."} : {})});
    }
    if (!this.current(epoch)) return;
    this.invalidateBudget();
    void this.refreshBudget();
  }
  /** Unexpected continuous failures retire AI only; no retry or history wipe. */
  private async readInitialInterpretationPause(): Promise<void> {
    const session = this.view.session, epoch = this.epoch;
    if (!session?.repeatable_sessions) return;
    try {
      const state = parseInterpretationLifecycle(await this.json("/api/interpretation/state", undefined, 5000), session.generation);
      if (!this.current(epoch) || this.view.session !== session || state.state === "active") return;
      this.view = {...this.view, manualPause: state, manualPauseChecking: true, meetPhase: "paused", capture: {remote_mix: "closed", self: "closed"}};
      this.cleanup(() => this.dependencies.audio.stopAll());
      this.acceptInterpretationPause(state);
      const retired = this.dependencies.audio.retire ? await this.dependencies.audio.retire() : !this.cleanupFailed;
      if (this.observing(epoch) && this.view.session === session && this.view.manualPause) this.set({manualAudioRetired: retired, manualPauseChecking: false});
    } catch (error) {if (this.observing(epoch) && error instanceof RequestError && [401, 403].includes(error.status)) this.fault("Operator authorization was lost.", true);}
  }
  private supportsLanguage(language: Language): boolean {
    return (this.view.session?.repeatable_sessions && this.view.interpretation ? INTERPRETATION_LANGUAGES : LEGACY_LANGUAGES).includes(language);
  }
  setInterpretation(value: Interpretation | null): boolean {
    if (!this.current(this.epoch) || this.role !== "operator" || !this.view.session?.repeatable_sessions || this.view.busy || !["idle", "preview"].includes(this.view.meetPhase) || this.meetAttempted) return false;
    try {this.set({interpretation: value === null ? null : parseInterpretation(value)}); return true;} catch {return false;}
  }
  canPauseInterpretation(): boolean {
    return this.role === "operator" && !!this.view.session?.repeatable_sessions && this.current(this.epoch)
      && (this.meetQuickStart || this.meetAttempted || ["preview", "connecting", "starting_captions", "starting_interpreter", "listening", "recovering"].includes(this.view.meetPhase));
  }
  /** User Pause owns a backend fence; muting is deliberately a different action. */
  async pauseInterpretation(): Promise<void> {
    if (!this.canPauseInterpretation()) return;
    const session = this.view.session!, epoch = this.epoch;
    this.meetSelection++;
    const source = this.view.meetSource;
    // Fence before abort/cleanup callbacks. Keep control heartbeat alive so the
    // authenticated Pause can observe actual provider retirement independently.
    this.view = {...this.view, manualPause: {schema_version: "interpretation-lifecycle.v1", generation: session.generation, state: "pausing", local_retired: false, provider_closed_confirmed: false, resume_allowed: false}, manualPauseChecking: true, manualAudioRetired: false, busy: false, meetPhase: "paused", meetIssue: null, preview: null, auditioning: false, publicTicketReady: false};
    for (const request of this.pending) this.cleanup(() => request.abort()); this.pending.clear();
    this.cleanup(() => this.dependencies.audio.stopAll());
    this.cleanup(() => this.dependencies.costNotifications?.close());
    this.closeSourceItems({}); this.windows.clear(); this.routeDeadlines.clear();
    for (const route of this.view.routes) this.routePins.set(route.route_id, {...route, state: "closed", expires_in_seconds: null});
    this.set({capture: {remote_mix: "closed", self: "closed"}, captureStatus: {remote_mix: null, self: null}, muted: {translation: true, live: true, live_interpretation: true}, routes: this.view.routes.map(route => ({...route, state: "closed", expires_in_seconds: null})), coverageGaps: [...new Set([...this.view.coverageGaps, source])], budgetFresh: false, budgetRefreshing: false, message: "Pausing interpretation. Audio and sound stopped locally; waiting for the service to close paid connections. Captions and notes are retained."});
    const current = () => !this.retired && this.observing(epoch) && this.view.session === session && !!this.view.manualPause;
    const localRetirement = (async () => {
      let retired = false;
      try {retired = this.dependencies.audio.retire ? await this.dependencies.audio.retire() : !this.cleanupFailed;} catch { /* Refuse Resume on uncertainty. */ }
      if (current()) this.set({manualAudioRetired: retired});
    })();
    this.manualPausePollUntil = this.dependencies.now() + 30_000;
    try {
      const result = await this.json("/api/interpretation/pause", {generation: session.generation}, 15_000);
      if (current()) this.acceptInterpretationPause(parseInterpretationLifecycle(result, session.generation));
    } catch (error) {
      if (current() && error instanceof RequestError && [401, 403].includes(error.status)) this.fault("Operator authorization was lost. Paused content is cleared.", true);
      else if (current()) this.set({manualPause: {...this.view.manualPause!, state: "closure_unconfirmed", resume_allowed: false}, message: "Audio stopped locally. Paid connection closure is not confirmed. Check pause status before resuming; earlier processing may still be billed."});
    } finally {await localRetirement; if (current()) this.set({manualPauseChecking: false});}
  }
  private acceptInterpretationPause(state: InterpretationLifecycle): void {
    if (!this.view.manualPause || this.view.stopped) return;
    // A local Pause never grants positive authority from an active snapshot.
    if (state.state === "active") throw new Error("Pause was not admitted");
    this.set({manualPause: state, message: state.state === "paused" ? "Interpretation paused. The service confirmed paid connections closed. Earlier usage may be billed later. Resume and select audio when you are ready." : state.state === "pausing" ? "Pausing interpretation. Waiting for paid connections to close; no new audio is captured." : "Interpretation stopped locally. Provider closure is unconfirmed; Resume remains unavailable."});
  }
  async checkInterpretationPause(): Promise<void> {
    const session = this.view.session, epoch = this.epoch;
    if (!session || !this.view.manualPause || this.view.manualPauseChecking || this.view.stopped || this.retired) return;
    const current = () => !this.retired && this.observing(epoch) && this.view.session === session && !!this.view.manualPause;
    this.manualPausePolledAt = this.dependencies.now(); this.set({manualPauseChecking: true});
    try {
      const result = await this.json("/api/interpretation/state", undefined, 5000);
      if (current()) this.acceptInterpretationPause(parseInterpretationLifecycle(result, session.generation));
      // Cleanup retry only: no device selection, capture or paid request.
      if (current() && !this.view.manualAudioRetired) {
        const retired = this.dependencies.audio.retire ? await this.dependencies.audio.retire() : !this.cleanupFailed;
        if (current()) this.set({manualAudioRetired: retired});
      }
    } catch (error) {
      if (current() && error instanceof RequestError && [401, 403].includes(error.status)) this.fault("Operator authorization was lost. Paused content is cleared.", true);
      else if (current()) this.set({manualPause: {...this.view.manualPause!, state: "closure_unconfirmed", resume_allowed: false}, message: "Pause status could not be confirmed. No audio is captured; use Check pause status to retry."});
    } finally {if (current()) this.set({manualPauseChecking: false});}
  }
  canResumeInterpretation(): boolean {
    return this.canCreateSession() && !this.view.stopped && !this.view.aiPaused && this.view.manualPause?.resume_allowed === true && this.view.manualAudioRetired && !this.view.manualPauseChecking;
  }
  async requestInterpretationResume(requestId: string): Promise<Session> {
    const session = this.view.session;
    if (!this.canResumeInterpretation() || !session || !/^[a-f0-9]{64}$/.test(requestId)) throw new Error("Resume unavailable");
    const generation = session.session_generation ?? session.generation;
    const row = object(await this.json("/api/interpretation/resume", {previous_generation: generation, request_id: requestId}, 25_000), ["state", "previous_generation", "request_id", "session"]);
    if (!this.canResumeInterpretation() || this.view.session !== session || row.state !== "ready" || row.previous_generation !== generation || row.request_id !== requestId) throw new Error("Resume changed");
    const next = parseSession(row.session, "operator");
    if (!next.repeatable_sessions || next.stopped || next.generation <= session.generation || next.csrf === session.csrf) throw new Error("Invalid resume authority");
    return next;
  }
  resumeWorkspace(): ResumeWorkspace {
    if (!this.canResumeInterpretation()) throw new Error("Resume unavailable");
    const text = this.sourceExport(2 * 1024 * 1024);
    const segments = [...this.view.earlierSegments, {generation: this.view.session!.generation, text, items: this.view.sourceFragments.length}];
    let bytes = segments.reduce((sum, segment) => sum + new TextEncoder().encode(segment.text).length, 0), dropped = this.view.earlierSegmentsDropped;
    // Prior paused text is separately bounded; current archive keeps its own
    // established bounds. Eviction is visible and never invents continuity.
    while (segments.length > 8 || bytes > 2 * 1024 * 1024) {const old = segments.shift()!; bytes -= new TextEncoder().encode(old.text).length; dropped++;}
    return Object.freeze({segments: Object.freeze(segments.map(segment => Object.freeze({...segment}))), dropped, notes: this.view.notes, source: this.view.meetSource, interpretation: this.view.interpretation, spokenLanguage: this.view.spokenLanguage, speechTerms: this.view.speechTerms});
  }
  restoreResumeWorkspace(workspace: ResumeWorkspace): void {
    if (!this.view.session || this.view.stopped || this.view.routes.length || this.view.meetPhase !== "idle" || this.view.notes || this.view.earlierSegments.length) throw new Error("Resume workspace already used");
    this.set({earlierSegments: workspace.segments, earlierSegmentsDropped: workspace.dropped, notes: workspace.notes, meetSource: workspace.source, interpretation: workspace.interpretation, spokenLanguage: workspace.spokenLanguage, speechTerms: workspace.speechTerms, message: "Ready to resume. Earlier source captions and notes are retained with a paused gap. Press Start and select audio; nothing from the pause will replay."});
  }
  private async pauseAI(message: string, affectedSource?: Source): Promise<void> {
    if (this.view.stopped || this.view.aiPaused || this.view.manualPause || this.retired) return;
    const session = this.view.session;
    if (!session?.continuous || this.role !== "operator") {await this.stopMeetWithIssue(message); return;}
    const routes = this.view.routes.filter(route => route.state === "ready" || route.state === "starting").slice(0, 6);
    const sources = new Set(routes.map(route => route.source));
    if (affectedSource) sources.add(affectedSource);
    if (this.meetAttempted || this.view.meetPhase !== "idle") sources.add(this.view.meetSource);
    // Fence callbacks before aborting any request or releasing the capture.
    this.sourceDrafts.clear(); this.liveCaptions.close();
    this.view = {...this.view, sourceDrafts: [], liveCaptions: this.liveCaptions.items(), aiPaused: true, busy: false, meetPhase: "paused", meetIssue: message, terminalMessage: message};
    if (this.privateListening?.isActive()) void this.privateListening.stop("Private listening paused with the operator session.");
    for (const request of this.pending) this.cleanup(() => request.abort()); this.pending.clear();
    this.cleanup(() => this.dependencies.audio.stopAll());
    this.cleanup(() => this.dependencies.costNotifications?.close());
    this.sourceArchive.close({});
    this.windows.clear(); this.routeDeadlines.clear();
    for (const route of routes) this.routePins.set(route.route_id, {...route, state: "closed", expires_in_seconds: null});
    this.set({coverageGaps: [...new Set([...this.view.coverageGaps, ...sources])], capture: {remote_mix: "closed", self: "closed"}, captureStatus: {remote_mix: null, self: null}, muted: {translation: true, live: true, live_interpretation: true}, routes: this.view.routes.map(route => ({...route, state: "closed", expires_in_seconds: null})), sourceFragments: this.sourceArchive.items(), sourceDiscarded: this.sourceArchive.discardedCount(), budgetFresh: false, budgetRefreshing: false, message: this.cleanupMessage("AI paused. Captions and notes are retained for review or export. Nothing restarts automatically.")});
    const epoch = this.epoch;
    const same = () => !this.retired && !this.view.stopped && this.epoch === epoch && this.view.session === session;
    if (!same()) return;
    // A source fence also covers an admitted route whose HTTP receipt was lost.
    const closed = await Promise.allSettled([...sources].map(async source => {
      const row = object(await this.json("/api/source-gap", {source}, 5000), ["state", "generation"]);
      if (row.state !== "closed" || row.generation !== session.generation) throw new Error("Unconfirmed route closure");
    }));
    if (same() && closed.some(result => result.status === "rejected")) this.set({message: this.cleanupMessage("AI paused locally; one or more server route closures are unconfirmed. Captions and notes remain available. Use Stop to end this process.")});
    if (same()) void this.readDiagnostics("paused");
  }
  gap(source: Source, notify = true, code?: string): void {
    if (this.view.stopped || this.view.capture[source] === "closed") return;
    if (this.view.session?.continuous && !this.view.finishing) {void this.pauseAI(code ? providerMessage(code) : "The selected audio source has a gap. AI is paused; captions and notes are retained.", source); return;}
    // Mark the source closed before device callbacks can reenter this method.
    this.view = {...this.view, capture: {...this.view.capture, [source]: "closed"}};
    this.closeSourceItems({source});
    this.dependencies.audio.stopCapture(source);
    for (const route of this.view.routes) if (route.source === source && route.route !== "transcription") this.muteNow(privateLane(route));
    if (this.view.stopped) return;
    this.set({routes: this.view.routes.map(route => route.source === source ? {...route, state: "closed"} : route), coverageGaps: this.view.coverageGaps.includes(source) ? this.view.coverageGaps : [...this.view.coverageGaps, source], message: (source === "remote_mix" ? "Remote mix" : "Own microphone")+" has a coverage gap and is closed for this process. No substitute source will start."});
    if (code) this.set({message: providerMessage(code), meetIssue: providerMessage(code)});
    if (source === this.view.meetSource && this.meetAttempted && !this.view.finishing && this.current(this.epoch)) {void this.stopMeetWithIssue(code ? providerMessage(code) : "The selected audio source ended. Captions and interpretation stopped together."); return;}
    if (notify) void this.json("/api/source-gap", {source}).catch(() => {if (!this.view.stopped) this.fault("Source closure could not be confirmed. The pilot is stopped.");});
  }
  async capture(source: Source): Promise<void> {
    await this.operation(async epoch => {
      this.ensureConnected();
      if (this.view.browserListening || this.view.meetPhase !== "idle" || this.role !== "operator" || !this.view.captureConfirmed || this.view.capture[source] !== "idle" || !this.view.routes.some(route => route.source === source && route.state === "ready")) return;
      this.set({capture: {...this.view.capture, [source]: "starting"}});
      if (!this.current(epoch)) return;
      try {await this.dependencies.audio.startCapture(source);} catch (error) {if (this.current(epoch)) {this.gap(source); this.set({message: captureMessage(error instanceof Error ? error.message : "", source)});} return;}
      if (this.current(epoch) && this.snapshot().capture[source] === "starting") this.set({capture: {...this.view.capture, [source]: "capturing"}});
    });
  }
  confirmCapture(confirmed: boolean): void {
    if (this.role !== "operator" || !this.view.session || this.view.stopped || this.view.finishing) return;
    this.set({captureConfirmed: confirmed});
    if (!confirmed && this.view.meetPhase === "preview") this.cancelMeetSetup();
    if (!confirmed && (this.meetAttempted || this.view.meetPhase === "connecting") && !this.view.stopped) {void this.stopMeetWithIssue("Audio agreement was withdrawn. Captions and interpretation stopped."); return;}
    if (!confirmed && !this.view.stopped) for (const source of ["remote_mix", "self"] as const) if (this.view.capture[source] === "starting" || this.view.capture[source] === "capturing") this.gap(source);
  }
  /** Aggregate device observations only; never stores audio or device identities. */
  captureStatus(source: Source, status: CaptureStatus): void {
    if (this.view.stopped || this.view.finishing || this.view.aiPaused || this.view.manualPause || this.role !== "operator") return;
    if (this.view.capture[source] === "idle" && (source !== this.view.meetSource || this.view.meetPhase === "idle" || this.view.meetPhase === "failed")) return;
    if (!Number.isFinite(status.level) || status.level < 0 || status.level > 1 || !Number.isSafeInteger(status.frames) || status.frames < 0 || typeof status.muted !== "boolean") {this.captureGap(source, "invalid_capture_frame"); return;}
    if (source === this.view.meetSource && !status.muted && status.level >= 0.01 && status.frames > 0) this.meetLastSignal = this.dependencies.now();
    this.set({captureStatus: {...this.view.captureStatus, [source]: {...status}}, sentFrames: {...this.frameCounts}, meetSignalSeen: source === this.view.meetSource ? !status.muted && this.dependencies.now() - this.meetLastSignal <= 20_000 : this.view.meetSignalSeen});
  }
  captureGap(source: Source, code: string): void {
    if (this.view.stopped || this.view.finishing || this.view.manualPause) return;
    const message = captureMessage(code, source);
    if (code === "audio_cleanup_unconfirmed") {void this.stopMeetWithIssue(message); return;}
    if (source === this.view.meetSource && (this.meetAttempted || this.view.meetPhase === "connecting")) {void this.stopMeetWithIssue(message); return;}
    if (source === this.view.meetSource && this.view.meetPhase === "preview") {
      this.cancelMeetSetup(message); return;
    }
    this.gap(source); if (!this.view.stopped) this.set({message, meetIssue: message});
  }
  canSetMeetSource(): boolean {
    return this.role === "operator" && !!this.view.session && !this.view.browserListening && this.current(this.epoch) && !this.view.busy && !this.meetQuickStart && !this.meetAttempted && !this.view.meetSourceLocked && this.view.meetPhase === "idle" && Object.values(this.view.capture).every(state => state === "idle") && !this.view.routes.some(route => route.state === "starting" || route.state === "ready");
  }
  /** Explicit source choice never starts capture and is frozen during a session. */
  setMeetSource(source: Source): boolean {
    if (!["remote_mix", "self"].includes(source) || !this.canSetMeetSource()) return false;
    if (this.view.meetSource !== source) this.set({meetSource: source, meetIssue: null, meetSignalSeen: false, message: !guidedLiveAvailable(this.view.session) ? "Select the agreement, then check your chosen audio source locally." : source === "self" ? "Your microphone is selected. Start will ask your browser for microphone access." : "Meeting tab audio is selected. Start will ask you to choose the tab containing your call."});
    return true;
  }
  /** Compatibility entrypoint: this always means tab audio, never microphone. */
  async selectMeetTab(): Promise<void> {
    if (this.view.meetSource !== "remote_mix" && !this.setMeetSource("remote_mix")) return;
    await this.selectMeetAudio();
  }
  async selectMeetAudio(): Promise<void> {
    const source = this.view.meetSource;
    if (this.view.browserListening || this.role !== "operator" || !this.view.session || this.view.stopped || this.view.finishing || this.view.aiPaused || this.view.busy || this.meetAttempted || this.view.meetSourceLocked || !this.view.captureConfirmed || Object.values(this.view.capture).some(state => state !== "idle") || this.view.meetPhase !== "idle" || this.view.routes.some(route => route.state === "starting" || route.state === "ready")) return;
    const epoch = this.epoch, selection = ++this.meetSelection;
    this.meetLastSignal = -Infinity;
    this.set({busy: true, meetSourceLocked: true, meetPhase: "preview", meetIssue: null, meetSignalSeen: false, captureStatus: {...this.view.captureStatus, [source]: null}, message: source === "self" ? "Allow your microphone in your browser. This local check keeps audio on this device." : "Choose the tab containing your call and include its audio. This local check keeps audio on this device."});
    if (!this.current(epoch) || selection !== this.meetSelection) return;
    try {
      // No network await before invoking the browser's required user gesture.
      await this.dependencies.audio.prepareCapture(source);
      if (!this.current(epoch) || selection !== this.meetSelection || !this.view.captureConfirmed) return;
      this.set({message: !guidedLiveAvailable(this.view.session) ? "Local audio check active. Watch the input meter, then release the selected audio." : source === "self" ? "Microphone selected. You can start interpretation even while you are quiet." : "Meeting tab selected. You can start interpretation even while everyone is quiet."});
    } catch (error) {
      if (!this.current(epoch) || selection !== this.meetSelection) return;
      let message = captureMessage(error instanceof Error ? error.message : "", source);
      let released = false;
      try {
        await this.dependencies.audio.cancelPreparedCapture(source);
        released = true;
      } catch (releaseError) {
        if (releaseError instanceof Error && releaseError.message === "capture_selection_pending") message = captureMessage(releaseError.message, source);
        else {if (this.current(epoch)) await this.stopMeetWithIssue(captureMessage("audio_cleanup_unconfirmed", source)); return;}
      }
      if (!this.current(epoch) || selection !== this.meetSelection) return;
      this.set({meetPhase: "idle", meetSourceLocked: !released, meetSignalSeen: false, meetIssue: message, captureStatus: {...this.view.captureStatus, [source]: null}, message});
    } finally {if (this.current(epoch) && selection === this.meetSelection && !this.meetAttempted) this.set({busy: false});}
  }
  cancelMeetSetup(issue?: string): void {
    // A native chooser cannot be dismissed by this page. After the operator
    // dismisses it, this explicit release-only retry can confirm cleanup without
    // opening any device, connecting a socket or admitting another provider.
    const retryRelease = this.view.meetPhase === "idle" && this.view.meetSourceLocked && !this.view.busy;
    if (!this.current(this.epoch) || (this.view.meetPhase !== "preview" && !retryRelease) || this.meetAttempted) return;
    const selection = ++this.meetSelection, epoch = this.epoch, source = this.view.meetSource;
    this.meetLastSignal = -Infinity;
    this.set({busy: true, meetPhase: "idle", meetSignalSeen: false, captureStatus: {...this.view.captureStatus, [source]: null}, meetIssue: issue ?? null, message: issue ?? "Releasing the selected audio. Cancel any browser chooser still open."});
    if (!this.current(epoch)) return;
    const done = (error?: unknown) => {
      if (!this.current(epoch) || selection !== this.meetSelection) return;
      if (error && !(error instanceof Error && error.message === "capture_selection_pending")) {void this.stopMeetWithIssue(captureMessage("audio_cleanup_unconfirmed", source)); return;}
      const message = error ? captureMessage("capture_selection_pending", source) : issue ?? "Local audio check stopped. No provider route was opened. You can select audio again.";
      this.set({busy: false, meetSourceLocked: !!error, meetIssue: error || issue ? message : null, message});
    };
    try {void Promise.resolve(this.dependencies.audio.cancelPreparedCapture(source)).then(() => done(), error => done(error));} catch (error) {done(error);}
  }

  private waitUntil(epoch: number, predicate: () => boolean, milliseconds: number, timeoutMessage: string): Promise<void> {
    const deadline = this.dependencies.now() + milliseconds;
    return new Promise((resolve, reject) => {
      let finished = false;
      const cleanup = () => {clearTimeout(timer); this.listeners.delete(check); this.waitChecks.delete(check);};
      const finish = (error?: Error) => {if (finished) return; finished = true; cleanup(); if (error) reject(error); else resolve();};
      const check = () => {
        try {
          if (!this.current(epoch)) {finish(new SetupError("Startup was cancelled.")); return;}
          if (this.dependencies.now() >= deadline) {finish(new SetupError(timeoutMessage)); return;}
          if (predicate()) finish();
        } catch (error) {finish(error instanceof Error ? error : new SetupError("Startup could not be confirmed."));}
      };
      const timer = setTimeout(() => finish(new SetupError(timeoutMessage)), milliseconds);
      this.listeners.add(check); this.waitChecks.add(check); check();
    });
  }
  private async readyRoute(epoch: number, route_id: string): Promise<void> {
    for (;;) {
      try {
        await this.waitUntil(epoch, () => {
          this.ensureConnected();
          const route = this.routePins.get(route_id);
          if (!route || route.state === "failed" || route.state === "closed") throw new SetupError(providerMessage(route?.code ?? null));
          return route.state === "ready" && (!this.view.session?.continuous || this.windows.get(route_id)?.state === "ready");
        }, 12_000, "The speech service did not become ready in time. Both services have been stopped.");
        return;
      } catch (error) {
        const window = this.windows.get(route_id);
        // Only a recent, validated server recovery extends startup. A silent or
        // terminal server cannot leave this wait alive indefinitely.
        if (!this.current(epoch) || !this.lease() || !this.view.session?.continuous || !window || !["recovering", "renewing"].includes(window.state) || window.deadline === null || this.dependencies.now() >= window.deadline) throw error;
      }
    }
  }
  private async stopMeetWithIssue(message: string): Promise<void> {
    if (this.view.stopped || this.view.finishing) return;
    if (this.view.session?.continuous) {await this.pauseAI(message); return;}
    // Latch the explanation synchronously before Stop detaches the socket or
    // pending HTTP/broadcast completion can replace the operation status.
    this.view = {...this.view, terminalMessage: message, meetIssue: message};
    // Global Stop also covers an admitted route whose HTTP receipt was lost.
    await this.stop();
    if (this.retired) return;
    this.set({meetPhase: "failed"});
  }
  /** One deliberate Start owns the chooser, private sound and provider setup. */
  async startMeetSession(language: Language, displayedReservation: number): Promise<void> {
    const session = this.view.session, epoch = this.epoch;
    if (this.view.interpretation) language = interpretationAnchor(this.view.interpretation);
    if (this.view.browserListening || this.meetQuickStart || !this.current(epoch) || this.role !== "operator" || !session?.providers_enabled || !session.capabilities.source_captions || !session.capabilities.live_interpreter || !this.supportsLanguage(language) || !this.view.captureConfirmed || this.view.busy || this.meetAttempted || !["idle", "preview"].includes(this.view.meetPhase) || (this.view.meetPhase === "idle" && this.view.meetSourceLocked) || Object.values(this.view.capture).some(state => state !== "idle") || this.view.routes.some(route => ["starting", "ready"].includes(route.state))) return;
    // Bind this click to the quote on screen before acquiring browser resources.
    if (!this.freshBudget() || !Number.isSafeInteger(displayedReservation) || displayedReservation !== meetReservation(this.view.budget)) return;
    this.meetQuickStart = true;
    // Invoke Chrome's chooser before any await. Arm sound in the same gesture;
    // merely selecting a source never sends its audio to a provider.
    const choosing = this.view.meetPhase === "idle" ? this.selectMeetAudio() : Promise.resolve();
    const selection = this.meetSelection;
    let playback: Promise<boolean>;
    try {playback = this.dependencies.audio.resumePlayback().then(() => true, () => false);} catch {playback = Promise.resolve(false);}
    void this.enableCostNotifications();
    try {
      await choosing;
      if (!this.current(epoch) || selection !== this.meetSelection || this.view.meetPhase !== "preview" || !this.view.captureConfirmed) return;
      this.set({busy: true, message: "Connecting interpretation…"});
      await this.refreshBudget();
      await this.waitUntil(epoch, () => this.freshBudget(), 5000, "The local service could not confirm availability. Try Start again.");
      if (!this.current(epoch) || selection !== this.meetSelection || this.view.meetPhase !== "preview" || !this.view.captureConfirmed) return;
      const total = meetReservation(this.view.budget);
      if (total === null) throw new SetupError("Captions and interpretation are unavailable with the current local settings.");
      if (total > displayedReservation) throw new SetupError("The estimate increased. Review the new amount and press Start again.");
      this.set({busy: false});
      await this.startMeet(language, displayedReservation, playback);
    } catch (error) {
      if (this.current(epoch) && selection === this.meetSelection && !this.meetAttempted) this.set({meetIssue: error instanceof SetupError ? error.message : "The local service could not prepare interpretation. Try Start again.", message: "Your audio is selected. Try Start again when the local service is available."});
    } finally {
      this.meetQuickStart = false;
      if (this.current(epoch) && selection === this.meetSelection) this.set({busy: false});
    }
  }
  async startMeet(language: Language, maximumReservation: number, preparedPlayback?: Promise<boolean>): Promise<void> {
    if (this.view.interpretation) language = interpretationAnchor(this.view.interpretation);
    if (this.role !== "operator" || !this.view.session || !this.current(this.epoch) || this.view.busy || this.meetAttempted || this.view.meetPhase !== "preview" || !this.view.captureConfirmed || this.view.routes.some(route => route.state === "ready" || route.state === "starting")) return;
    const capabilities = this.view.session.capabilities;
    if (!this.supportsLanguage(language) || !capabilities.source_captions || !capabilities.live_interpreter || !this.view.session.providers_enabled || !this.freshBudget()) return;
    const total = meetReservation(this.view.budget);
    if (total === null || !Number.isSafeInteger(maximumReservation) || maximumReservation < total) return;
    const epoch = this.epoch, source = this.view.meetSource;
    this.set({busy: true, meetPhase: "connecting", meetIssue: null, message: "Connecting captions and interpretation. Local check audio is discarded."});
    if (!this.current(epoch)) return;
    try {
      // Resume playback under this click, before any network wait. Sound remains
      // muted until both routes are Ready and the current input is admitted.
      let playbackReady = false, playbackFailed = false;
      // Observe a bounded promise without losing the synchronous user gesture.
      // Stop also fences a browser resume that settles after this wait ends.
      const playback = preparedPlayback ?? this.dependencies.audio.resumePlayback().then(() => true, () => false);
      void playback.then(ready => {playbackFailed = !ready; playbackReady = true; if (this.current(epoch)) this.emit();}, () => {playbackFailed = true; playbackReady = true; if (this.current(epoch)) this.emit();});
      // Ask under the same click, after audio resume has used its gesture.
      // Permission is optional and never awaited by speech startup.
      void this.enableCostNotifications();
      if (!this.current(epoch)) return;
      await this.waitUntil(epoch, () => {if (playbackFailed) throw new SetupError("Your browser could not enable private interpretation sound."); return playbackReady;}, 5000, "Your browser did not enable private interpretation sound in time.");
      if (!this.current(epoch)) return;
      this.connectSocket(epoch);
      await this.waitUntil(epoch, () => this.lease(), 5000, "The companion could not establish its private connection.");
      if (!this.current(epoch)) return;
      if (!this.view.captureConfirmed || !this.freshBudget()) throw new SetupError("Audio agreement or the available allowance changed during setup. Processing did not start.");
      const currentTotal = meetReservation(this.view.budget), captionsCost = this.view.budget?.routes.find(row => row.route === "transcription")?.quote_microusd;
      if (currentTotal === null || currentTotal > maximumReservation || captionsCost == null) throw new SetupError("The two services no longer fit the displayed allowance. Processing did not start.");
      this.meetAttempted = true;
      this.set({meetPhase: "starting_captions", message: "Starting source captions…"});
      const captions = await this.admitRoute(source, "transcription", language, null, epoch);
      // Reservations remain sequential and nonrefundable, with a fresh quote
      // after the first receipt. Independent provider startup can overlap;
      // neither ready route receives PCM until the entire pair is ready.
      // Promise.all observes both rejections, including a sibling cancelled by
      // Stop after the other branch fails. No admission is retried here.
      const [, interpreter] = await Promise.all([
        this.readyRoute(epoch, captions),
        (async () => {
          await this.waitUntil(epoch, () => this.view.budgetFresh && !this.view.budgetRefreshing && this.dependencies.now() - this.budgetCheckedAt < 15_000, 5000, "The current allowance could not be confirmed after captions were admitted.");
          if (!this.current(epoch)) throw new SetupError("Startup was cancelled.");
          const liveQuote = this.view.budget?.routes.find(row => row.route === "live");
          if (!this.view.captureConfirmed || !this.freshBudget() || !liveQuote?.can_reserve || liveQuote.quote_microusd == null || liveQuote.quote_microusd > maximumReservation - captionsCost) throw new SetupError("Interpretation no longer fits the approved setup. Captions and interpretation have both stopped.");
          this.set({meetPhase: "starting_interpreter", message: "Connecting source captions and live interpretation…"});
          const route = await this.admitRoute(source, "live", language, "interpretation", epoch);
          await this.readyRoute(epoch, route);
          return route;
        })(),
      ]);
      if (this.view.session?.continuous) {
        // A ready sibling may have begun recovery while the other was opening.
        // Never admit the microphone/tab's future PCM until both current windows
        // are Ready together; waiting remains cancellable by Stop or withdrawal.
        while (![captions, interpreter].every(id => this.windows.get(id)?.state === "ready")) {
          await this.readyRoute(epoch, captions);
          await this.readyRoute(epoch, interpreter);
        }
      }
      if (!this.current(epoch)) return;
      if (!this.view.captureConfirmed || ![captions, interpreter].every(id => this.routePins.get(id)?.state === "ready")) throw new SetupError("An input or service stopped during setup. No earlier audio will be replayed.");
      this.ensureConnected();
      this.set({capture: {...this.view.capture, [source]: "starting"}});
      if (!this.current(epoch)) return;
      this.dependencies.audio.activateCapture(source);
      if (!this.current(epoch)) return;
      this.dependencies.audio.setPrivateMuted("live_interpretation", false);
      if (!this.current(epoch)) return;
      this.set({meetPhase: "listening", capture: {...this.view.capture, [source]: "capturing"}, muted: {...this.view.muted, live_interpretation: false}, message: source === "self" ? "Listening to your microphone. Your source captions and private interpretation will appear as you speak." : "Listening to the selected meeting tab. Source captions and private interpretation will appear as the other participants speak."});
    } catch (error) {
      if (!this.current(epoch)) return;
      const message = error instanceof SetupError ? error.message : error instanceof RequestError ? error.status === 429 ? providerMessage("budget_exhausted") : error.status === 503 ? providerMessage("provider_unavailable") : "The server refused or could not confirm this startup." : "Audio or service startup could not be confirmed.";
      await this.stopMeetWithIssue(message);
    } finally {if (this.current(epoch)) this.set({busy: false});}
  }
  private notificationStatus(): CostNotificationState {
    try {return this.dependencies.costNotifications?.status() ?? "unavailable";} catch {return "unavailable";}
  }
  /** Called under a user gesture; permission never controls media authority. */
  async enableCostNotifications(): Promise<void> {
    if (this.role !== "operator" || !this.view.session?.continuous || !this.view.session.providers_enabled || this.retired || this.view.stopped || this.view.finishing || this.view.aiPaused || this.view.manualPause || this.notificationRequested) return;
    const state = this.notificationStatus(), epoch = this.epoch;
    this.notificationRequested = true;
    this.set({costNotificationState: state === "default" ? "requesting" : state});
    if (state !== "default" || !this.current(epoch) || this.retired) return;
    try {
      const result = await this.dependencies.costNotifications!.requestPermission();
      if (this.current(epoch) && !this.retired) this.set({costNotificationState: result});
    } catch {if (this.current(epoch) && !this.retired) this.set({costNotificationState: "unavailable"});}
  }
  async refreshBudget(): Promise<void> {
    if (this.role !== "operator" || !this.view.session || this.view.stopped || this.view.finishing || this.view.aiPaused || this.view.manualPause || this.view.budgetRefreshing) return;
    const epoch = this.epoch, generation = this.view.session.generation, revision = this.budgetRevision;
    this.budgetCheckedAt = this.dependencies.now();
    this.set({budgetRefreshing: true, budgetFresh: false});
    if (!this.current(epoch)) return;
    try {
      const value = await this.json("/api/budget", undefined, 5000);
      if (!this.current(epoch)) return;
      if (revision === this.budgetRevision) {
        const budget = parseBudget(value, generation, this.view.session!.continuous, this.view.session!.provider_vendor);
        const notice = budget.continuous && budget.state === "active" ? this.costMilestones.observe(budget.continuous.warning_threshold_microusd, budget.continuous.allocated_microusd) : null;
        this.set({budget, budgetFresh: true, ...(notice ? {costNotice: notice} : {})});
        // Subscribers may Stop/Finish while rendering the new in-app notice.
        if (notice && this.current(epoch) && !this.retired && this.view.session?.providers_enabled) {
          try {if (this.notificationStatus() === "granted") this.dependencies.costNotifications?.notify(costNoticeText(notice));} catch { /* In-app warning remains visible. */ }
          if (this.current(epoch)) this.set({costNotificationState: this.notificationStatus()});
        }
      }
    } catch (error) {
      if (!this.current(epoch)) return;
      if (error instanceof RequestError && [401, 403].includes(error.status)) this.fault("Budget authorization was lost. This page is stopped.", true);
      else this.set({budgetFresh: false});
    } finally {
      if (this.current(epoch)) {
        this.set({budgetRefreshing: false});
        if (revision !== this.budgetRevision) void this.refreshBudget();
      }
    }
  }
  private invalidateBudget(): void {this.budgetRevision++; this.set({budgetFresh: false});}
  private freshBudget(): boolean {
    if (this.view.budgetFresh && this.dependencies.now() - this.budgetCheckedAt >= 15_000) {
      this.invalidateBudget();
      if (!this.view.stopped && !this.view.finishing) {
        this.set({message: "The allowance quote expired. Review the refreshed reservation, then open the route or prepare speech explicitly."});
        void this.refreshBudget();
      }
      return false;
    }
    return this.view.budgetFresh && !this.view.budgetRefreshing;
  }
  async openRoute(source: Source, route: RouteKind, language: Language, live_mode: LiveMode | null = null): Promise<void> {
    await this.operation(async epoch => {
      this.ensureConnected(); if (this.view.browserListening || this.view.meetPhase !== "idle" || this.role !== "operator" || this.view.capture[source] === "closed" || !this.freshBudget() || !this.view.budget?.routes.find(quote => quote.route === route)?.can_reserve) return;
      const capabilities = this.view.session!.capabilities;
      if (this.view.session!.provider_vendor === "gemini" && !LEGACY_LANGUAGES.includes(language)) return;
      if (route === "live" ? !["assistant", "interpretation"].includes(live_mode ?? "") || !(live_mode === "interpretation" ? capabilities.live_interpreter : capabilities.live_assistant) : live_mode !== null || !(route === "transcription" ? capabilities.source_captions : capabilities.private_translation)) return;
      await this.admitRoute(source, route, language, live_mode, epoch);
    });
  }
  setSpeechHints(language: SpokenLanguage, terms: string): void {
    if (language !== "auto" && !LEGACY_LANGUAGES.includes(language) && !this.view.session?.repeatable_sessions) return;
    if (this.role !== "operator" || this.view.stopped || this.view.finishing || this.view.aiPaused || this.view.busy
      || !["idle", "preview"].includes(this.view.meetPhase) || this.view.routes.some(route => ["starting", "ready"].includes(route.state))) return;
    if (terms.length > 4096 || (this.view.session?.provider_vendor === "gemini" && terms.trim())) return;
    let error: string | null = null;
    try { speechHints(language, terms); } catch (reason) { error = reason instanceof Error ? reason.message : "Check the caption hints."; }
    this.set({spokenLanguage: language, speechTerms: terms, speechHintsError: error});
  }
  private async admitRoute(source: Source, route: RouteKind, language: Language, live_mode: LiveMode | null, epoch: number): Promise<string> {
    if (!this.current(epoch)) throw new SetupError("Startup was cancelled.");
    let hints = route === "transcription" ? speechHints(this.view.spokenLanguage, this.view.speechTerms) : undefined;
    const interpretation = route === "live" && live_mode === "interpretation" && this.meetAttempted ? this.view.interpretation ?? undefined : undefined;
    if (hints && this.meetAttempted && this.view.interpretation) hints = {...hints, languages: this.view.interpretation.mode === "two_way" ? [this.view.interpretation.first, this.view.interpretation.second] : INTERPRETATION_LANGUAGES};
    this.ensureConnected();
      const generation = this.view.session!.generation;
      this.invalidateBudget();
      if (!this.current(epoch)) throw new SetupError("Startup was cancelled.");
      let result: unknown;
      try {result = await this.json("/api/routes", {generation, source, route, language, live_mode, ...(interpretation ? {interpretation} : {}), ...(hints && (hasSpeechHints(hints) || this.view.interpretation) ? {transcription: hints} : {})});}
      finally {if (this.current(epoch)) {this.invalidateBudget(); await this.refreshBudget();}}
      const row = object(result, ["state", "generation", "route_id"]);
      if (!this.current(epoch)) throw new SetupError("Startup was cancelled.");
      if (row.state !== "starting" || row.generation !== generation) throw new Error("Invalid route admission");
      const route_id = identifier(row.route_id);
      const observed = this.routePins.get(route_id);
      if (observed && (observed.source !== source || observed.route !== route || observed.language !== language || observed.live_mode !== live_mode || !sameInterpretation(observed.interpretation, interpretation))) throw new Error("Changed route admission identity");
      if (!observed) {
        if (this.routePins.size >= 16) throw new Error("Route identity capacity");
        const identity: Route = {route_id, source, route, live_mode, language, ...(interpretation ? {interpretation} : {}), state: "starting", code: null, expires_in_seconds: null};
        this.routePins.set(route_id, identity);
        this.set({routes: [...this.view.routes.filter(item => item.state !== "closed" && item.state !== "failed"), identity]});
      }
    return route_id;
  }
  async closeRoute(route_id: string): Promise<void> {
    const route = this.view.routes.find(item => item.route_id === route_id), session = this.view.session;
    if (!route || !session || this.view.stopped || this.closingRoutes.has(route_id) || this.closingRoutes.size >= 6) return;
    const epoch = this.epoch; this.closingRoutes.add(route_id);
    try {
      // Negative controls never wait behind an ordinary preparation request.
      if (route.route !== "transcription") this.muteNow(privateLane(route));
      if (!this.current(epoch)) return;
      this.routePins.set(route_id, {...route, state: "closed", expires_in_seconds: null});
      this.routeDeadlines.delete(route_id);
      this.closeSourceItems({route_id});
      this.set({routes: this.view.routes.map(item => item.route_id === route_id ? {...item, state: "closed", expires_in_seconds: null} : item), message: "Route stopped locally; server closure is pending."});
      if (!this.current(epoch)) return;
      const row = object(await this.json("/api/routes/close", {generation: session.generation, route_id}), ["state", "generation"]);
      if (!this.current(epoch)) return;
      if (row.generation !== session.generation || row.state !== "closed") throw new Error("Invalid close");
      this.set({message: "Selected provider route closed. Other ready routes and source capture remain independent. Its reservation is not refunded."});
      await this.refreshBudget();
    } catch {if (this.current(epoch)) this.fault("Provider route closure could not be confirmed. The pilot is stopped.");}
    finally {this.closingRoutes.delete(route_id);}
  }
  private muteNow(kind: PrivateKind): void {this.muteEpoch[kind]++; this.dependencies.audio.setPrivateMuted(kind, true); this.set({muted: {...this.view.muted, [kind]: true}});}
  async mute(kind: PrivateKind, muted: boolean): Promise<void> {
    if (this.view.stopped || this.view.finishing || this.view.aiPaused || this.view.manualPause) return;
    if (muted) {this.muteNow(kind); return;}
    const epoch = this.epoch, request = ++this.muteEpoch[kind];
    try {await this.dependencies.audio.resumePlayback(); if (!this.current(epoch) || !this.lease() || request !== this.muteEpoch[kind]) return; this.dependencies.audio.setPrivateMuted(kind, false); this.set({muted: {...this.view.muted, [kind]: false}});} catch {if (this.current(epoch)) this.set({message: "Private playback could not be enabled. It remains muted."});}
  }
  playbackFault(kind: PrivateKind | "clip"): void {if (this.view.stopped || this.view.manualPause) return; if (kind === "clip") {this.fault("Clip playback failed or lost its lease. Output is stopped; no automatic replay.");} else {this.muteNow(kind); this.set({message: "Private audio has a playback gap and is muted. Review the route before explicitly enabling again."});}}
  async liveText(route_id: string, text: string): Promise<void> {await this.operation(async epoch => {this.ensureConnected(); if (!textFits(text, LIVE_TEXT_BYTES) || !this.view.routes.some(route => route.route_id === route_id && route.route === "live" && route.live_mode === "assistant" && route.state === "ready")) return; const generation = this.view.session!.generation; const row = object(await this.json("/api/live/text", {generation, route_id, text}), ["state", "generation"]); if (!this.current(epoch)) return; if (row.state !== "accepted" || row.generation !== generation) throw new Error("Invalid text response"); this.set({message: "Private Live context accepted. This is not an approved public statement."});});}
  setNotes(notes: string): void {if (!this.view.stopped && this.role === "operator") this.set({notes: notes.slice(0, 20_000)});}
  private sourceExport(maximumBytes = Infinity): string {
    const rows: string[] = [];
    // Leave a fixed metadata allowance; only whole source rows are evicted.
    // A paused segment larger than the history budget keeps its latest text.
    let bytes = 1024;
    for (let index = this.view.sourceFragments.length - 1; index >= 0; index--) {
      const fragment = this.view.sourceFragments[index];
      const row = `[${fragment.source} · source_asr · ${fragment.final ? "provider final" : fragment.incomplete ? "incomplete at closure" : "provisional"}] ${fragment.text}`;
      const size = new TextEncoder().encode(row).length + 1;
      if (bytes + size > maximumBytes) break;
      rows.push(row); bytes += size;
    }
    rows.reverse();
    const sourceModel = this.view.session?.provider_vendor === "gemini" ? "Dedicated source-caption model: gemini-3.5-transcribe-live.\n" : "";
    return sourceModel + `Retained source fragments: ${rows.length}; source text UTF-8 bytes in the current archive: ${this.sourceArchive.byteLength()}.\nSources with known gaps/closure: ${this.view.coverageGaps.join(", ") || "none reported; completeness unverified"}.\nRolling source items discarded: ${this.sourceArchive.discardedCount()}. Older source fragments omitted for paused-segment retention: ${this.view.sourceFragments.length - rows.length}. Generated interpretation omitted.\n\n` + rows.join("\n");
  }
  exportText(): string | null {
    if (this.view.stopped || this.role !== "operator") return null;
    return "PRIVATE LOCAL PILOT EXPORT\nUnverified source ASR fragments and manual notes; no participant identity or agreement inferred.\nThis is not a recording or a verified complete transcript.\n" + `${this.view.session?.continuous ? `Rolling source retention: at most 10,000 items / 4 MiB; ${this.sourceArchive.discardedCount()} sealed earlier-window items discarded. Earlier meeting content may no longer be available.\n` : ""}`
      + `Earlier paused segments: ${this.view.earlierSegments.length}; ${this.view.earlierSegmentsDropped} older segments discarded (8 segments / 2 MiB bound). Paused intervals were not captured.\n\n`
      + this.view.earlierSegments.map(segment => `EARLIER SEGMENT · generation ${segment.generation} · closed at user Pause; incomplete coverage\n${segment.text}\n\nPAUSED GAP · no audio captured or replayed\n`).join("\n")
      + "\nCURRENT SEGMENT\n" + this.sourceExport() + `\nGenerated fragments omitted from this source export; ${this.view.generatedDropped} older generated fragments were dropped from the rolling private window.\n\nMANUAL DRAFT NOTES\n` + this.view.notes;
  }
  private completeFinish(outcome: FinishCompletion): void {
    const finish = this.finishState;
    if (!finish || !this.observing(finish.epoch)) return;
    if (!this.lease()) {this.fault("Finalization lost its session lease. Stop superseded the pending export."); return;}
    if (this.dependencies.now() >= finish.deadline) outcome = "incomplete";
    this.sourceArchive.close({});
    if (outcome === "complete" && (this.sourceDrafts.incompleteSources().length > 0 || this.sourceArchive.items().some(item => !item.final) || this.view.coverageGaps.length > 0 || this.sourceArchive.discardedCount() > 0 || this.view.earlierSegments.length > 0 || this.view.earlierSegmentsDropped > 0)) outcome = "incomplete";
    this.view = {...this.view, sourceFragments: this.sourceArchive.items(), coverageGaps: outcome === "incomplete" ? [...new Set([...this.view.coverageGaps, ...finish.sources])] : this.view.coverageGaps};
    const text = this.exportText();
    this.finishState = null;
    // Snapshot only after the ordered private marker (or an explicit local
    // incomplete cutoff). Stop clears all private state before resolution.
    void this.stop();
    finish.resolve(text === null ? null : text + "\n\nFINALIZATION\n" + ({complete: "Source-ASR items from PCM frames sent before Finish finalized and their routes confirmed closed. Audio still buffered in the capture worklet at the local cutoff is excluded. Accuracy and total meeting coverage remain unverified.", incomplete: "Source finalization was incomplete or unconfirmed. The final phrase or other source content may be missing.", no_active_asr: "No active source-ASR route remained to finalize. This file contains only the already retained captions and manual notes."}[outcome]) + "\n");
  }
  finishExport(): Promise<string | null> {
    if (this.view.browserListening || this.role !== "operator" || this.view.stopped || this.view.finishing || this.view.manualPause || !this.view.session || !this.lease()) return Promise.resolve(null);
    const epoch = this.epoch, generation = this.view.session.generation;
    let resolve!: (text: string | null) => void;
    const result = new Promise<string | null>(done => {resolve = done;});
    const finish = {epoch, deadline: this.dependencies.now() + 35_000, acknowledged: false, sources: [...new Set(this.view.routes.filter(route => route.route === "transcription" && (route.state === "ready" || route.state === "starting")).map(route => route.source))], resolve};
    this.finishState = finish;
    // Fence positive callbacks before audio teardown can reenter. Keep only
    // the existing private socket/heartbeat alive for the final ASR events.
    this.liveCaptions.close(); this.view = {...this.view, liveCaptions: this.liveCaptions.items()};
    // Hide drafts immediately; retain only the bounded ledger through finalization.
    this.view = {...this.view, sourceDrafts: [], finishing: true, busy: true, preview: null, auditioning: false, auditionComplete: false, publicTicketReady: false, muted: {translation: true, live: true, live_interpretation: true}, capture: {remote_mix: this.view.capture.remote_mix === "closed" ? "closed" : "finishing", self: this.view.capture.self === "closed" ? "closed" : "finishing"}, message: "Capture and sound stopped. Waiting up to 35 seconds for final source captions; Stop cancels this export immediately."};
    this.publicTicket = null;
    for (const request of this.pending) this.cleanup(() => request.abort()); this.pending.clear();
    const audioStopped = this.cleanup(() => this.dependencies.audio.stopAll());
    this.cleanup(() => this.dependencies.costNotifications?.close());
    if (!this.observing(epoch)) return result;
    const publicStopped = this.cleanup(() => this.dependencies.broadcastStop());
    if (!audioStopped || !publicStopped) {void this.stop(); return result;}
    this.emit();
    if (!this.observing(epoch)) return result;
    try {
      // Use the same ordered stream as PCM. A separate HTTP request could
      // overtake audio already sent by this browser. No later PCM is admitted.
      if (!this.lease() || !this.socket || this.socket.bufferedAmount > 646 * 9) throw new Error("Finish stream unavailable");
      this.socket.send(JSON.stringify({type: "finish", generation}));
    } catch {if (this.observing(epoch) && this.finishState === finish) this.completeFinish("incomplete");}
    return result;
  }
  private pinPreview(preview: Preview): void {
    const previous = this.previewPins.get(preview.id);
    if ((previous && !samePreview(previous, preview)) || (!previous && this.previewPins.size >= 8)) throw new Error("Changed preview or capacity");
    this.previewPins.set(preview.id, preview);
    if (!this.view.preview || this.view.preview.id === preview.id) this.set({preview});
  }
  async prepare(text: string, language: Language): Promise<void> {
    await this.operation(async epoch => {
      this.ensureConnected();
      if (this.role !== "operator" || !textFits(text, PUBLIC_TEXT_BYTES) || !this.freshBudget() || !canPrepare(this.view.budget, new TextEncoder().encode(text).length)) return;
      this.dependencies.audio.stopClip();
      this.set({preview: null, auditionComplete: false, auditioning: false, approvalAttempted: false});
      this.invalidateBudget();
      if (!this.current(epoch)) return;
      let result: unknown;
      try {result = await this.json("/api/previews", {generation: this.view.session!.generation, text, language}, 45_000);}
      finally {if (this.current(epoch)) {this.invalidateBudget(); await this.refreshBudget();}}
      if (!this.current(epoch)) return;
      const preview = parsePreview(result, this.view.session!.generation);
      if (preview.text !== text || preview.language !== language || preview.state !== "prepared") throw new Error("Changed prepared words");
      this.pinPreview(preview);
      if (!this.current(epoch)) return;
      this.set({preview, message: "Immutable preview prepared. Review exact words, audition the clip, then acknowledge separately."});
    });
  }
  async audition(): Promise<void> {await this.operation(async epoch => {const preview = this.view.preview; if (!preview || preview.state !== "prepared" || this.attemptedApprovals.has(preview.id)) return; await this.dependencies.audio.resumePlayback(); if (!this.current(epoch)) return; const bytes = await this.response(preview.audio_path, undefined, 4 * 1024 * 1024, 15_000); if (!this.current(epoch)) return; if (!await this.dependencies.verifyClip(bytes, preview)) throw new Error("Clip digest mismatch"); if (!this.current(epoch) || this.view.preview?.id !== preview.id) return; this.set({auditioning: true, auditionComplete: false}); if (!this.current(epoch)) return; await this.dependencies.audio.playClip(bytes, {started: () => {}, ended: () => {if (this.current(epoch) && this.view.preview?.id === preview.id) this.set({auditioning: false, auditionComplete: true});}, failed: () => {if (this.current(epoch)) this.playbackFault("clip");}, isCurrent: () => this.current(epoch) && this.lease() && this.view.preview?.id === preview.id});});}
  async acknowledge(): Promise<void> {await this.operation(async epoch => {const preview = this.view.preview; if (!preview || !this.view.auditionComplete || preview.state !== "prepared") return; const next = parsePreview(await this.json(`/api/previews/${preview.id}/audition`, {generation: preview.generation, digest: preview.digest}), preview.generation); if (!this.current(epoch)) return; if (!samePreview(preview, next) || !next.auditioned || next.state !== "prepared") throw new Error("Changed audition"); this.pinPreview(next);});}
  async approve(): Promise<void> {await this.operation(async epoch => {this.ensureConnected(); const preview = this.view.preview; if (!preview || !preview.auditioned || !this.view.auditionComplete || preview.state !== "prepared" || this.attemptedApprovals.has(preview.id)) return; this.attemptedApprovals.add(preview.id); this.set({approvalAttempted: true, receipt: "Approval attempted once · outcome not yet confirmed. This preview cannot be retried."}); if (!this.current(epoch)) return; const row = object(await this.json(`/api/previews/${preview.id}/approve`, {generation: preview.generation, digest: preview.digest}), ["state", "generation", "delivery_id"]); if (!this.current(epoch)) return; if (row.state !== "offered" || row.generation !== preview.generation) throw new Error("Invalid approval response"); identifier(row.delivery_id); this.set({preview: {...preview, state: "offered"}, receipt: "Offered once · waiting for a local browser receipt. No automatic retry."});});}
  async issuePublicTicket(): Promise<void> {await this.operation(async epoch => {this.ensureConnected(); const row = object(await this.json("/api/public/tickets", {generation: this.view.session!.generation}), ["ticket", "expires_in_seconds", "path", "origin"]); if (!this.current(epoch)) return; if (row.path !== "/public" || row.origin !== PUBLIC_ORIGIN || row.expires_in_seconds !== 120) throw new Error("Invalid viewer ticket"); this.publicTicket = {ticket: identifier(row.ticket), expires: this.dependencies.now() + 120_000}; this.set({publicTicketReady: true, message: "A new one-use public-screen link is ready. Opening it replaces the previous viewer."});});}
  takePublicUrl(): string | null {if (this.view.stopped || this.view.finishing || !this.publicTicket || this.publicTicket.expires <= this.dependencies.now()) return null; const ticket = this.publicTicket.ticket; this.publicTicket = null; this.set({publicTicketReady: false}); return PUBLIC_ORIGIN+"/public#ticket="+ticket;}
  openPublicScreen(): void {const epoch = this.epoch; const url = this.takePublicUrl(); if (!url || !this.current(epoch)) return; try {this.dependencies.openPublicWindow(url);} catch {this.set({message: "The public window did not open. Issue a fresh one-use link and allow this local popup."});}}
  private async reportReceipt(delivery_id: string, state: "accepted" | "playback_started" | "playback_ended" | "failed", epoch: number): Promise<void> {
    if (!this.current(epoch) || !this.lease()) return;
    try {const row = object(await this.json("/api/public/receipt", {generation: this.view.session!.generation, delivery_id, state}), ["state", "generation"]); if (!this.current(epoch)) return; if (row.state !== "recorded" || row.generation !== this.view.session!.generation) throw new Error("Invalid receipt");} catch {if (this.current(epoch)) this.fault("Local playback receipt could not be recorded. The public screen is stopped.");}
  }
  private async deliver(delivery: Delivery): Promise<void> {
    if (this.role !== "public" || this.view.stopped) return;
    if (this.deliveryBusy || this.seenDeliveries.has(delivery.delivery_id) || this.seenDeliveries.size >= 8) {this.fault("Duplicate or overlapping public delivery was refused. Nothing will replay."); return;}
    this.seenDeliveries.add(delivery.delivery_id); this.deliveryBusy = true; const epoch = this.epoch;
    try {
      const bytes = await this.response(delivery.audio_path, undefined, 4 * 1024 * 1024);
      if (!this.current(epoch)) return;
      if (!await this.dependencies.verifyClip(bytes, delivery)) throw new Error("Unverified delivery digest");
      if (!this.current(epoch) || !this.lease()) return;
      this.set({delivery: {...delivery, state: "accepted"}});
      if (!this.current(epoch) || !this.lease()) return;
      await this.reportReceipt(delivery.delivery_id, "accepted", epoch);
      if (!this.current(epoch) || !this.lease()) return;
      let receipts = Promise.resolve();
      const record = (state: "playback_started" | "playback_ended") => {receipts = receipts.then(() => this.reportReceipt(delivery.delivery_id, state, epoch));};
      await this.dependencies.audio.playClip(bytes, {
        started: () => {if (this.current(epoch) && this.lease()) {this.set({delivery: {...delivery, state: "playback_started"}}); record("playback_started");}},
        ended: () => {if (this.current(epoch) && this.lease()) {this.set({delivery: {...delivery, state: "playback_ended"}}); record("playback_ended");}},
        failed: () => {if (this.current(epoch)) {void this.reportReceipt(delivery.delivery_id, "failed", epoch); this.fault("Public playback failed. No automatic retry; prepare a new clip only after review.");}},
        isCurrent: () => this.current(epoch) && this.lease(),
      });
      await receipts;
    } catch {if (this.current(epoch)) this.fault("Approved audio could not be verified or played. The public screen is stopped.");}
    finally {this.deliveryBusy = false;}
  }
}

/** Scrub the capability before React, requests, logs or any other browser work. */
export function takeEntry(location: Pick<Location, "origin" | "pathname" | "hash">, history: Pick<History, "replaceState">): {role: Role; ticket: string | null} {
  const role: Role = location.pathname === "/public" ? "public" : "operator";
  const hash = location.hash;
  history.replaceState(null, "", location.pathname === "/public" ? "/public" : "/");
  if (location.origin !== (role === "operator" ? ORIGIN : PUBLIC_ORIGIN) || !["/", "/public"].includes(location.pathname)) throw new Error("Use the exact local pilot origin");
  if (!hash || hash === "#main") return {role, ticket: null};
  const prefix = role === "operator" ? "#bootstrap=" : "#ticket=";
  if (!hash.startsWith(prefix)) throw new Error("Invalid local access link");
  return {role, ticket: identifier(hash.slice(prefix.length))};
}
