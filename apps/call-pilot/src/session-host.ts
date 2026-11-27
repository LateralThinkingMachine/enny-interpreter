import type {PilotClient} from "./client.js";
import {parseInterpretation, type Interpretation, type Language} from "./protocol.js";
import {defaultPreferences, parsePreferences} from "./preferences.js";
import type {PreferenceStore, Preferences} from "./preferences.js";

export type OwnedClient = Readonly<{client: PilotClient; dispose(): void}>;
type Snapshot = Readonly<{client: PilotClient; key: number; language: Language; preferences: Preferences; preferencesSaved: boolean; available: boolean; creating: boolean; error: string | null}>;

/** A process may outlive recordings; no stopped client or device owner is reset. */
export class SessionHost {
  private owned: OwnedClient;
  private view: Snapshot;
  private listeners = new Set<() => void>();
  private cancelClient: () => void;
  private disposed = false;
  private epoch = 0;
  private requestId: string | null = null;
  private resumeRequestId: string | null = null;
  constructor(private readonly factory: () => OwnedClient, private readonly nonce: () => string, private readonly preferences?: PreferenceStore) {
    this.owned = factory();
    let choices = defaultPreferences();
    if (this.owned.client.role === "operator") {try {choices = parsePreferences(preferences?.read() ?? choices);} catch { /* Defaults remain usable. */ }}
    this.view = {client: this.owned.client, key: 1, language: choices.language, preferences: choices, preferencesSaved: true, available: false, creating: false, error: null};
    this.cancelClient = this.watch(this.owned.client);
  }
  snapshot = (): Snapshot => this.view;
  subscribe = (listener: () => void): (() => void) => {this.listeners.add(listener); return () => this.listeners.delete(listener);};
  private set(update: Partial<Snapshot>): void {this.view = {...this.view, ...update}; for (const listener of this.listeners) listener();}
  private watch(client: PilotClient): () => void {
    return client.subscribe(() => {
      if (this.disposed || this.owned.client !== client) return;
      const available = client.canCreateSession();
      if (available !== this.view.available) this.set({available});
      if (!available && this.view.creating) {this.epoch++; this.set({creating: false, error: null});}
    });
  }
  private save(update: Partial<Preferences>): void {
    if (this.disposed || this.owned.client.role !== "operator") return;
    let choices: Preferences;
    try {choices = parsePreferences({...this.view.preferences, ...update});} catch {return;}
    let saved: boolean;
    try {saved = this.preferences?.write(choices) ?? true;} catch {saved = false;}
    this.set({language: choices.language, preferences: choices, preferencesSaved: saved});
  }
  setLanguage(language: Language): void {this.save({language});}
  async create(expected: PilotClient): Promise<void> {
    const state = expected.snapshot();
    if (this.disposed || expected !== this.owned.client || this.view.creating || !expected.canCreateSession() || (!state.stopped && !state.aiPaused)) return;
    const epoch = ++this.epoch;
    this.set({creating: true, error: null});
    const current = () => !this.disposed && this.epoch === epoch && this.owned.client === expected && expected.canCreateSession();
    let fresh: OwnedClient | null = null;
    try {
      if (!this.requestId) {
        this.requestId = this.nonce();
        if (!/^[a-f0-9]{64}$/.test(this.requestId)) throw new Error("Invalid request identity");
        await expected.stop();
      }
      if (!current()) return;
      if (!await expected.retireForNewSession()) {
        if (current()) this.set({error: "Audio cleanup is not confirmed. Cancel any open audio chooser, then try New session again. If this continues, close and reopen the companion tab."});
        return;
      }
      if (!current()) return;
      const session = await expected.requestNewSession(this.requestId);
      if (!current()) return;
      fresh = this.factory();
      fresh.client.initializeFromSession(session);
      if (!current()) {fresh.dispose(); fresh = null; return;}
      const previous = this.owned;
      this.cancelClient();
      // Dispose old bindings before publishing the successor. Captured callbacks
      // always belong to their original client and cannot target this.owned.
      previous.dispose();
      this.owned = fresh; fresh = null;
      this.requestId = null;
      this.cancelClient = this.watch(this.owned.client);
      this.set({client: this.owned.client, key: this.view.key + 1, available: this.owned.client.canCreateSession(), creating: false, error: null});
    } catch {
      fresh?.dispose();
      if (current()) this.set({error: "The new session is not confirmed yet. Try New session again to check the same request. Your previous session remains ended."});
    } finally {if (!this.disposed && this.epoch === epoch) this.set({creating: false});}
  }
  /** Resume replaces backend/device authority but preserves bounded, labeled work. */
  async resume(expected: PilotClient, requestedInterpretation?: Interpretation): Promise<void> {
    if (this.disposed || expected !== this.owned.client || this.view.creating || !expected.canResumeInterpretation()) return;
    const interpretation = requestedInterpretation === undefined ? undefined : parseInterpretation(requestedInterpretation);
    const epoch = ++this.epoch;
    this.set({creating: true, error: null});
    const current = () => !this.disposed && this.epoch === epoch && this.owned.client === expected && expected.canResumeInterpretation();
    let fresh: OwnedClient | null = null;
    try {
      if (!this.resumeRequestId) {
        this.resumeRequestId = this.nonce();
        if (!/^[a-f0-9]{64}$/.test(this.resumeRequestId)) throw new Error("Invalid resume identity");
      }
      const session = await expected.requestInterpretationResume(this.resumeRequestId);
      if (!current()) return;
      const workspace = expected.resumeWorkspace();
      fresh = this.factory();
      fresh.client.initializeFromSession(session);
      fresh.client.restoreResumeWorkspace(workspace);
      if (interpretation !== undefined && !fresh.client.setInterpretation(interpretation)) throw new Error("Successor interpretation policy could not be applied");
      if (!current()) {fresh.dispose(); fresh = null; return;}
      const previous = this.owned;
      this.cancelClient(); previous.dispose();
      this.owned = fresh; fresh = null;
      this.resumeRequestId = null; this.requestId = null;
      this.cancelClient = this.watch(this.owned.client);
      this.set({client: this.owned.client, key: this.view.key + 1, available: this.owned.client.canCreateSession(), creating: false, error: null});
    } catch {
      fresh?.dispose();
      if (current()) this.set({error: "Resume is not confirmed. Try Resume again to check the same request. Audio remains stopped and your retained work stays here."});
    } finally {if (!this.disposed && this.epoch === epoch) this.set({creating: false});}
  }
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true; this.epoch++;
    try {this.cancelClient();} finally {try {this.owned.dispose();} finally {this.listeners.clear();}}
  }
}
