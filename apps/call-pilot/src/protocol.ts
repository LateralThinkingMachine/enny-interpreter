import {pilotOrigins} from "./origins.js";

const origins = pilotOrigins(typeof window === "undefined" ? undefined : window.location.origin);
export const ORIGIN = origins.operator;
export const PUBLIC_ORIGIN = origins.public;
export type Role = "operator" | "public";
export type Source = "remote_mix" | "self";
export type Language = "ja" | "en" | "hi" | "es" | "vi";
export type TranslationRule = Readonly<{id:string;source:Language|'auto';target:Language;text:boolean;speech:boolean}>;
export type RulesInterpretation = Readonly<{schema_version:'interpretation-rules.v1';mode:'rules';rules:readonly TranslationRule[]}>;
export type Interpretation = RulesInterpretation | Readonly<{schema_version: "interpretation.v1"; mode: "two_way"; first: Language; second: Language}> | Readonly<{schema_version: "interpretation.v1"; mode: "auto_to_target"; target: Language}>;
export type LiveMode = "assistant" | "interpretation";
export type RouteKind = "transcription" | "translation" | "live";
export type ReceiptState = "offered" | "accepted" | "playback_started" | "playback_ended" | "failed" | "unknown";
export type Session = Readonly<{role: Role; csrf: string; generation: number; stopped: boolean; provider_vendor?: "gemini"; providers_enabled: boolean; continuous: boolean; repeatable_sessions?: boolean; session_generation?: number; expires_in_seconds: number | null; capabilities: Readonly<{source_captions: boolean; private_translation: boolean; live_assistant: boolean; live_interpreter: boolean; prepared_public_speech: boolean}>; limitations: readonly string[]}>;
export type Preview = Readonly<{id: string; generation: number; text: string; language: Language; digest: string; audio_path: string; duration_ms: number; auditioned: boolean; state: "prepared" | ReceiptState}>;
export type Route = Readonly<{route_id: string; source: Source; route: RouteKind; live_mode: LiveMode | null; language: Language; interpretation?: Interpretation; state: "starting" | "ready" | "closed" | "failed"; code: string | null; expires_in_seconds: number | null}>;
export type Delivery = Readonly<{delivery_id: string; text: string; language: Language; digest: string; audio_path: string; duration_ms: number}>;
export type SourceItem = Readonly<{id: string; revision: number; sequence: number}>;
export type SourceDraft = Readonly<{route_id: string; source: Source; route: "transcription"; item_id: string; revision: number; text: string; retired: boolean; window?: number}>;
export type FinishCompletion = "complete" | "incomplete" | "no_active_asr";
export type Transcript = Readonly<{route_id: string; source: Source; route: RouteKind; provenance: "source_asr" | "translation" | "live_input" | "live_output"; text: string; interpretation?: Interpretation; final: boolean; item: SourceItem | null; window?: number}>;
export type LiveTranscript = Readonly<{route_id: string; source: Source; route: "live"; provenance: "live_input" | "live_output"; text: string; interpretation?: Interpretation; start_ms: number; end_ms: number; window?: number}>;
type Generation = Readonly<{generation: number}>;
export type Event = Generation & (
  | {type: "hello"; role: Role; stopped: boolean}
  | {type: "heartbeat"}
  | {type: "finishing"}
  | {type: "finished"; source_completion: FinishCompletion}
  | {type: "stopped"; reason: string}
  | {type: "window"; route_id: string; source: Source; route: RouteKind; window: number; state: "draining" | "renewing" | "ready"}
  | {type: "window"; route_id: string; source: Source; route: RouteKind; window: number; state: "recovering"; code: string; retry_in_seconds: number}
  | ({type: "route"} & Route)
  | ({type: "transcript"} & Transcript)
  | ({type: "source_draft"} & SourceDraft)
  | ({type: "live_transcript"} & LiveTranscript)
  | {type: "audio"; route_id: string; source: Source; route: "translation" | "live"; pcm: Uint8Array; sample_rate: number; interpretation?: Interpretation; window?: number}
  | {type: "preview"; preview: Preview}
  | {type: "receipt"; delivery_id: string; state: ReceiptState; meaning: "local_browser_report_only"}
  | {type: "gap"; source: Source; code: string}
  | ({type: "delivery"} & Delivery)
);

const invalid = (): never => {throw new Error("Invalid pilot response");};
export function object(value: unknown, keys: readonly string[]): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return invalid();
  const row = value as Record<string, unknown>;
  if (Object.keys(row).length !== keys.length || keys.some(key => !Object.hasOwn(row, key))) return invalid();
  return row;
}
export function string(value: unknown, max = 4096): string {
  if (typeof value !== "string" || Array.from(value).length > max) return invalid();
  return value;
}
export function integer(value: unknown, min = 1, max = Number.MAX_SAFE_INTEGER): number {
  if (!Number.isSafeInteger(value) || (value as number) < min || (value as number) > max) return invalid();
  return value as number;
}
function boolean(value: unknown): boolean {if (typeof value !== "boolean") return invalid(); return value;}
export function choice<T extends string>(value: unknown, values: readonly T[]): T {if (!values.includes(value as T)) return invalid(); return value as T;}
export function identifier(value: unknown): string {const result = string(value, 128); if (!/^[A-Za-z0-9_-]{1,128}$/.test(result)) return invalid(); return result;}
function digest(value: unknown): string {const result = string(value, 64); if (!/^[a-f0-9]{64}$/.test(result)) return invalid(); return result;}
const language = (value: unknown) => choice(value, ["ja", "en", "hi", "es", "vi"] as const);
const source = (value: unknown) => choice(value, ["remote_mix", "self"] as const);
const kind = (value: unknown) => choice(value, ["transcription", "translation", "live"] as const);
const receipt = (value: unknown) => choice(value, ["offered", "accepted", "playback_started", "playback_ended", "failed", "unknown"] as const);
function audioPath(value: unknown, prefix: string, id: string): string {if (value !== prefix+id+(prefix === "/api/previews/" ? "/audio" : "")) return invalid(); return value as string;}
function boundedCode(value: unknown): string {const code = string(value, 64); if (!/^[a-z_]+$/.test(code)) return invalid(); return code;}

export function parseInterpretation(value: unknown): Interpretation {
  if (!value || typeof value !== "object") return invalid();
  if ((value as Record<string,unknown>).mode === 'rules') {
    const row=object(value,['schema_version','mode','rules']);
    if(row.schema_version!=='interpretation-rules.v1'||!Array.isArray(row.rules)||row.rules.length>30)return invalid();
    const ids=new Set<string>(), pairs=new Set<string>();
    const rules=row.rules.map(item=>{
      const rule=object(item,['id','source','target','text','speech']);
      const id=string(rule.id,11), source=rule.source==='auto'?'auto' as const:language(rule.source),target=language(rule.target);
      if(!/^rule-[1-9][0-9]{0,5}$/.test(id)||ids.has(id)||pairs.has(`${source}:${target}`)||source===target||typeof rule.text!=='boolean'||typeof rule.speech!=='boolean')return invalid();
      ids.add(id);pairs.add(`${source}:${target}`);return {id,source,target,text:rule.text,speech:rule.speech};
    });return {schema_version:'interpretation-rules.v1',mode:'rules',rules};
  }
  const pair = (value as Record<string, unknown>).mode === "two_way";
  const row = object(value, ["schema_version", "mode", ...(pair ? ["first", "second"] : ["target"])]);
  if (row.schema_version !== "interpretation.v1") return invalid();
  if (pair) {
    const first = language(row.first), second = language(row.second);
    if (first === second) return invalid();
    return Object.freeze({schema_version: "interpretation.v1", mode: "two_way", first, second});
  }
  if (row.mode !== "auto_to_target") return invalid();
  return Object.freeze({schema_version: "interpretation.v1", mode: "auto_to_target", target: language(row.target)});
}
export function interpretationAnchor(value:Interpretation):Language {return value.mode==='rules'?(value.rules[0]?.target??'ja'):value.mode==='two_way'?value.first:value.target;}
export function sameInterpretation(a?: Interpretation, b?: Interpretation): boolean {
  if (!a || !b) return a === b;
  if(a.mode==='rules'||b.mode==='rules')return a.mode==='rules'&&b.mode==='rules'&&JSON.stringify(a.rules)===JSON.stringify(b.rules);
  return a.mode === "two_way" ? b.mode === "two_way" && a.first === b.first && a.second === b.second : b.mode === "auto_to_target" && a.target === b.target;
}
export type InterpretationLifecycle = Readonly<{schema_version: "interpretation-lifecycle.v1"; generation: number; state: "active" | "pausing" | "paused" | "closure_unconfirmed"; local_retired: boolean; provider_closed_confirmed: boolean; resume_allowed: boolean}>;
export function parseInterpretationLifecycle(value: unknown, generation: number): InterpretationLifecycle {
  const row = object(value, ["schema_version", "generation", "state", "local_retired", "provider_closed_confirmed", "resume_allowed"]);
  if (row.schema_version !== "interpretation-lifecycle.v1" || row.generation !== generation) return invalid();
  const state = choice(row.state, ["active", "pausing", "paused", "closure_unconfirmed"] as const);
  const local_retired = boolean(row.local_retired), provider_closed_confirmed = boolean(row.provider_closed_confirmed), resume_allowed = boolean(row.resume_allowed);
  if (resume_allowed !== (state === "paused" && local_retired && provider_closed_confirmed)) return invalid();
  if (state === "active" && (local_retired || provider_closed_confirmed)) return invalid();
  return {schema_version: "interpretation-lifecycle.v1", generation, state, local_retired, provider_closed_confirmed, resume_allowed};
}

// The finite Gemini source-preview service rejects longer sessions or routes.
export const GEMINI_MAX_DURATION_SECONDS = 600;

export function parseSession(value: unknown, expected: Role): Session {
  const extended = !!value && typeof value === "object" && Object.hasOwn(value, "continuous");
  const repeatable = !!value && typeof value === "object" && Object.hasOwn(value, "repeatable_sessions");
  const specifiedVendor = !!value && typeof value === "object" && Object.hasOwn(value, "provider_vendor");
  const stableGeneration = !!value && typeof value === "object" && Object.hasOwn(value, "session_generation");
  const row = object(value, ["role", "csrf", "generation", "stopped", "providers_enabled", "expires_in_seconds", "capabilities", "limitations", ...(extended ? ["continuous"] : []), ...(specifiedVendor ? ["provider_vendor"] : []), ...(repeatable ? ["repeatable_sessions"] : []), ...(stableGeneration ? ["session_generation"] : [])]);
  const continuous = extended ? boolean(row.continuous) : false;
  if (continuous !== (row.expires_in_seconds === null)) return invalid();
  if (row.role !== expected) return invalid();
  const repeatable_sessions = repeatable ? boolean(row.repeatable_sessions) : false;
  if (repeatable_sessions && (!continuous || expected !== "operator")) return invalid();
  const cap = object(row.capabilities, ["source_captions", "private_translation", "live_assistant", "live_interpreter", "prepared_public_speech"]);
  const capabilities = {source_captions: boolean(cap.source_captions), private_translation: boolean(cap.private_translation), live_assistant: boolean(cap.live_assistant), live_interpreter: boolean(cap.live_interpreter), prepared_public_speech: boolean(cap.prepared_public_speech)};
  if (specifiedVendor && (row.provider_vendor !== "gemini" || continuous || expected !== "operator" || capabilities.private_translation || capabilities.live_assistant || capabilities.prepared_public_speech)) return invalid();
  const providers_enabled = boolean(row.providers_enabled);
  if ((!providers_enabled && Object.values(capabilities).some(Boolean)) || (expected === "public" && (capabilities.source_captions || capabilities.private_translation || capabilities.live_assistant || capabilities.live_interpreter))) return invalid();
  if (!Array.isArray(row.limitations) || row.limitations.length > 20) return invalid();
  const session_generation = stableGeneration ? integer(row.session_generation, 1, integer(row.generation)) : undefined;
  if (session_generation !== undefined && (!repeatable_sessions || (!row.stopped && session_generation !== row.generation))) return invalid();
  const csrf = identifier(row.csrf);
  return {...(specifiedVendor ? {provider_vendor: "gemini" as const} : {}), role: expected, csrf, generation: integer(row.generation), stopped: boolean(row.stopped), providers_enabled, continuous, repeatable_sessions, ...(session_generation === undefined ? {} : {session_generation}), expires_in_seconds: continuous ? null : integer(row.expires_in_seconds, 0, specifiedVendor ? GEMINI_MAX_DURATION_SECONDS : 7200), capabilities, limitations: row.limitations.map(item => string(item, 1024))};
}

export function parsePreview(value: unknown, generation: number): Preview {
  const row = object(value, ["id", "generation", "text", "language", "digest", "audio_path", "duration_ms", "auditioned", "state"]);
  if (row.generation !== generation) return invalid();
  const id = identifier(row.id), text = string(row.text, 2000);
  if (!textFits(text, PUBLIC_TEXT_BYTES)) return invalid();
  return {id, generation, text, language: language(row.language), digest: digest(row.digest), audio_path: audioPath(row.audio_path, "/api/previews/", id), duration_ms: integer(row.duration_ms, 1, 300_000), auditioned: boolean(row.auditioned), state: choice(row.state, ["prepared", "offered", "accepted", "playback_started", "playback_ended", "failed", "unknown"] as const)};
}
export function samePreview(a: Preview, b: Preview): boolean {return a.id === b.id && a.generation === b.generation && a.digest === b.digest && a.text === b.text && a.language === b.language && a.audio_path === b.audio_path && a.duration_ms === b.duration_ms;}

export function parseEvent(raw: string, role: Role): Event {
  if (new TextEncoder().encode(raw).length > 80_000) return invalid();
  const value: unknown = JSON.parse(raw);
  if (!value || typeof value !== "object" || Array.isArray(value)) return invalid();
  const type = (value as Record<string, unknown>).type;
  const common = ["type", "generation"];
  const fields: Record<string, string[]> = {hello: ["role", "stopped"], heartbeat: [], window: ["route_id", "source", "route", "window", "state"], finishing: [], finished: ["source_completion"], stopped: ["reason"], route: ["route_id", "source", "route", "live_mode", "language", "state", "code", "expires_in_seconds"], live_transcript: ["route_id", "source", "route", "provenance", "text", "start_ms", "end_ms"], source_draft: ["route_id", "source", "route", "item_id", "revision", "text", "retired"], transcript: ["route_id", "source", "route", "provenance", "text", "final", "item"], audio: ["route_id", "source", "route", "pcm16_base64", "sample_rate"], preview: ["preview"], receipt: ["delivery_id", "state", "meaning"], gap: ["source", "code"], delivery: ["delivery_id", "text", "language", "digest", "audio_path", "duration_ms"]};
  if (typeof type !== "string" || !Object.hasOwn(fields, type) || (role === "public" && !["hello", "heartbeat", "stopped", "delivery"].includes(type)) || (role === "operator" && type === "delivery")) return invalid();
  const scoped = ["transcript", "audio", "source_draft", "live_transcript"].includes(type) && Object.hasOwn(value, "window");
  const interpreted = ["route", "audio", "transcript", "live_transcript"].includes(type) && Object.hasOwn(value, "interpretation");
  const recovering = type === "window" && (value as Record<string, unknown>).state === "recovering";
  const row = object(value, [...common, ...fields[type], ...(interpreted ? ["interpretation"] : []), ...(scoped ? ["window"] : []), ...(recovering ? ["code", "retry_in_seconds"] : [])]), generation = integer(row.generation);
  const interpretation = interpreted ? parseInterpretation(row.interpretation) : undefined;
  if (interpretation && row.route !== "live") return invalid();
  const routing = interpretation ? {interpretation} : {};
  switch (type) {
    case "hello": if (row.role !== role) return invalid(); return {type, generation, role, stopped: boolean(row.stopped)};
    case "heartbeat": return {type, generation};
    case "window": {
      const binding = {type, generation, route_id: identifier(row.route_id), source: source(row.source), route: kind(row.route), window: integer(row.window)};
      if (recovering) {
        const delay = integer(row.retry_in_seconds, 1, 30);
        if (![1, 2, 4, 30].includes(delay)) return invalid();
        return {...binding, state: "recovering", code: choice(row.code, ["provider_connection_lost", "source_input_interrupted", "live_expired", "live_remote_hangup", "live_connection_lost"] as const), retry_in_seconds: delay};
      }
      return {...binding, state: choice(row.state, ["draining", "renewing", "ready"] as const)};
    }
    case "finishing": return {type, generation};
    case "finished": return {type, generation, source_completion: choice(row.source_completion, ["complete", "incomplete", "no_active_asr"] as const)};
    case "stopped": return {type, generation, reason: boundedCode(row.reason)};
    case "gap": return {type, generation, source: source(row.source), code: boundedCode(row.code)};
    case "route": {
      const route = kind(row.route);
      const live_mode = route === "live" ? choice(row.live_mode, ["assistant", "interpretation"] as const) : null;
      if (route !== "live" && row.live_mode !== null) return invalid();
      if (interpretation && (route !== "live" || live_mode !== "interpretation" || row.language !== interpretationAnchor(interpretation))) return invalid();
      const state = choice(row.state, ["starting", "ready", "closed", "failed"] as const);
      if (state !== "ready" && row.expires_in_seconds !== null) return invalid();
      return {type, generation, route_id: identifier(row.route_id), source: source(row.source), route, live_mode, language: language(row.language), ...routing, state, code: row.code === null ? null : boundedCode(row.code), expires_in_seconds: state === "ready" ? integer(row.expires_in_seconds, 1, 7200) : null};
    }
    case "live_transcript": {
      const text = string(row.text), start_ms = row.start_ms, end_ms = row.end_ms;
      if (row.route !== "live" || !text || text.includes("\0") || new TextEncoder().encode(text).length > 16_384
        || typeof start_ms !== "number" || typeof end_ms !== "number" || !Number.isFinite(start_ms) || !Number.isFinite(end_ms)
        || start_ms < 0 || end_ms < start_ms || end_ms > 3_610_000) return invalid();
      return {type, generation, route_id: identifier(row.route_id), source: source(row.source), route: "live", provenance: choice(row.provenance, ["live_input", "live_output"] as const), text, start_ms, end_ms, ...routing, ...(scoped ? {window: integer(row.window)} : {})};
    }
    case "source_draft": {
      const text = string(row.text), retired = boolean(row.retired);
      if (row.route !== "transcription" || new TextEncoder().encode(text).length > 16_384 || text.includes("\0") || (retired ? text !== "" : text === "")) return invalid();
      return {type, generation, route_id: identifier(row.route_id), source: source(row.source), route: "transcription", item_id: identifier(row.item_id), revision: integer(row.revision, 1, 4096), text, retired, ...(scoped ? {window: integer(row.window)} : {})};
    }
    case "transcript": {
      const route = kind(row.route), provenance = choice(row.provenance, ["source_asr", "live_input", "live_output", "translation"] as const);
      if ((route === "transcription" && provenance !== "source_asr") || (route === "translation" && provenance !== "translation") || (route === "live" && !["live_input", "live_output"].includes(provenance))) return invalid();
      const text = string(row.text), final = boolean(row.final);
      if (new TextEncoder().encode(text).length > 16_384 || (!text && !(provenance === "source_asr" && final))) return invalid();
      let item: SourceItem | null = null;
      if (provenance === "source_asr") {const pin = object(row.item, ["id", "revision", "sequence"]); item = {id: identifier(pin.id), revision: integer(pin.revision, 1, 4096), sequence: integer(pin.sequence, 1, 4096)};}
      else if (row.item !== null) return invalid();
      return {type, generation, route_id: identifier(row.route_id), source: source(row.source), route, provenance, text, final, item, ...routing, ...(scoped ? {window: integer(row.window)} : {})};
    }
    case "audio": {
      const encoded = string(row.pcm16_base64, 64_000);
      if (encoded.length % 4 || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)) return invalid();
      const decoded = atob(encoded);
      if (!decoded.length || decoded.length > 48_000 || decoded.length % 2) return invalid();
      const sample_rate = integer(row.sample_rate, 16000, 24000);
      if (sample_rate !== 16000 && sample_rate !== 24000) return invalid();
      return {type, generation, route_id: identifier(row.route_id), source: source(row.source), route: choice(row.route, ["translation", "live"] as const), pcm: Uint8Array.from(decoded, char => char.charCodeAt(0)), sample_rate, ...routing, ...(scoped ? {window: integer(row.window)} : {})};
    }
    case "preview": return {type, generation, preview: parsePreview(row.preview, generation)};
    case "receipt": if (row.meaning !== "local_browser_report_only") return invalid(); return {type, generation, delivery_id: identifier(row.delivery_id), state: receipt(row.state), meaning: row.meaning};
    case "delivery": {
      const delivery_id = identifier(row.delivery_id), text = string(row.text, 2000);
      if (!textFits(text, PUBLIC_TEXT_BYTES)) return invalid();
      return {type, generation, delivery_id, text, language: language(row.language), digest: digest(row.digest), audio_path: audioPath(row.audio_path, "/api/public/audio/", delivery_id), duration_ms: integer(row.duration_ms, 1, 300_000)};
    }
    default: return invalid();
  }
}

export async function verifyClip(bytes: ArrayBuffer, clip: Pick<Preview, "digest" | "text" | "language">): Promise<boolean> {
  if (bytes.byteLength < 44 || bytes.byteLength > 4 * 1024 * 1024) return false;
  const view = new Uint8Array(bytes);
  if (String.fromCharCode(...view.subarray(0, 4)) !== "RIFF" || String.fromCharCode(...view.subarray(8, 12)) !== "WAVE") return false;
  const hex = (data: ArrayBuffer) => Array.from(new Uint8Array(data), byte => byte.toString(16).padStart(2, "0")).join("");
  const audio_sha256 = hex(await crypto.subtle.digest("SHA-256", bytes));
  const canonical = JSON.stringify({audio_sha256, language: clip.language, text: clip.text});
  return hex(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(canonical))) === clip.digest;
}
import {PUBLIC_TEXT_BYTES, textFits} from "./text.js";
