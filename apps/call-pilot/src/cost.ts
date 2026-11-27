import {choice, GEMINI_MAX_DURATION_SECONDS, integer, object} from "./protocol.js";
import type {RouteKind} from "./protocol.js";

export type BudgetState = "disabled" | "active" | "finishing" | "stopped" | "expired" | "uncertain" | "unavailable";
export type RouteQuote = Readonly<{route: RouteKind; model: string; per_minute_microusd: number; configured: boolean; quote_seconds: number | null; quote_microusd: number | null; can_reserve: boolean}>;
export type Budget = Readonly<{
  version: 1; generation: number; state: BudgetState; currency: "USD"; pricing_version: "pilot_audio_2026_09_13_v1" | "gemini_preview_reserve_2026_11_27_v1";
  uncertain: boolean | null; amounts: Readonly<{approved_microusd: number; reserved_microusd: number; unallocated_microusd: number}> | null;
  remaining_seconds: number | null;
  continuous?: Readonly<{warning_threshold_microusd: number; allocated_microusd: number; windows_started: number}>; route_duration_seconds: number | null;
  stream_reservations: Readonly<{used: number; limit: number}> | null; tts_reservations: Readonly<{used: number; limit: number}> | null;
  routes: readonly RouteQuote[];
  tts: Readonly<{model: "tts-1-hd" | "unavailable"; configured: boolean; per_input_utf8_byte_microusd: number; max_input_utf8_bytes: number; max_request_microusd: number; can_reserve_maximum: boolean}>;
  invoice_verified: false; account_hard_cap_proven: false;
}>;
const invalid = (): never => {throw new Error("Invalid budget response");};
const bool = (value: unknown): boolean => typeof value === "boolean" ? value : invalid();
const money = (value: unknown): number => integer(value, 0, 1_000_000_000);
const count = (value: unknown, maximum: number) => {
  if (value === null) return null;
  const row = object(value, ["used", "limit"]), used = integer(row.used, 0, maximum), limit = integer(row.limit, 0, maximum);
  if (used > limit) return invalid();
  return {used, limit};
};
export function parseBudget(value: unknown, generation: number, continuousSession?: boolean, vendor?: "gemini"): Budget {
  const hasContinuous = !!value && typeof value === "object" && Object.hasOwn(value, "continuous");
  const row = object(value, ["version", "generation", "state", "currency", "pricing_version", "uncertain", "amounts", "remaining_seconds", "route_duration_seconds", "stream_reservations", "tts_reservations", "routes", "tts", "invoice_verified", "account_hard_cap_proven", ...(hasContinuous ? ["continuous"] : [])]);
  const gemini = vendor === "gemini";
  const schedule = gemini ? "gemini_preview_reserve_2026_11_27_v1" : "pilot_audio_2026_09_13_v1";
  if (gemini && (hasContinuous || continuousSession)) return invalid();
  if (row.version !== 1 || row.generation !== generation || row.currency !== "USD" || row.pricing_version !== schedule || row.invoice_verified !== false || row.account_hard_cap_proven !== false) return invalid();
  const state = choice(row.state, ["disabled", "active", "finishing", "stopped", "expired", "uncertain", "unavailable"] as const);
  const uncertain = row.uncertain === null ? null : bool(row.uncertain);
  let continuous: Budget["continuous"];
  if (hasContinuous && row.continuous !== null) {
    const item = object(row.continuous, ["warning_threshold_microusd", "allocated_microusd", "windows_started"]);
    continuous = {warning_threshold_microusd: integer(item.warning_threshold_microusd), allocated_microusd: integer(item.allocated_microusd, 0), windows_started: integer(item.windows_started, 0)};
  }
  if (continuousSession === false && continuous) return invalid();
  if (continuousSession === true && state === "active" && !continuous) return invalid();
  if (continuous && (row.amounts !== null || row.remaining_seconds !== null)) return invalid();
  let amounts = null;
  if (row.amounts !== null) {
    const amount = object(row.amounts, ["approved_microusd", "reserved_microusd", "unallocated_microusd"]);
    amounts = {approved_microusd: money(amount.approved_microusd), reserved_microusd: money(amount.reserved_microusd), unallocated_microusd: money(amount.unallocated_microusd)};
    if (amounts.reserved_microusd + amounts.unallocated_microusd !== amounts.approved_microusd) return invalid();
  }
  const maxRouteSeconds = gemini ? GEMINI_MAX_DURATION_SECONDS : 3600;
  const remaining_seconds = row.remaining_seconds === null && (continuous || continuousSession === true) ? null : integer(row.remaining_seconds, 0, gemini ? GEMINI_MAX_DURATION_SECONDS : 7200), route_duration_seconds = row.route_duration_seconds === null ? null : integer(row.route_duration_seconds, 20, maxRouteSeconds);
  const stream_reservations = count(row.stream_reservations, 16), tts_reservations = count(row.tts_reservations, 8);
  if (state === "active" && ((!amounts && !continuous) || uncertain !== false || route_duration_seconds === null || !stream_reservations || !tts_reservations)) return invalid();
  if (!Array.isArray(row.routes) || row.routes.length !== 3) return invalid();
  const models = gemini ? ["gemini-3.5-transcribe-live", "unavailable", "gemini-3.5-live-translate-preview"] : ["gpt-live-transcribe", "gpt-realtime-translate", "gpt-live-1"];
  const kinds = ["transcription", "translation", "live"] as const;
  // Validation of this exact pricing version; calculations use decoded server values.
  const rates = gemini ? [100_000, 0, 100_000] : [17_000, 34_000, 50_000];
  const routes = row.routes.map((value, index): RouteQuote => {
    const quote = object(value, ["route", "model", "per_minute_microusd", "configured", "quote_seconds", "quote_microusd", "can_reserve"]);
    if (quote.route !== kinds[index] || quote.model !== models[index]) return invalid();
    const configured = bool(quote.configured), can_reserve = bool(quote.can_reserve);
    const quote_seconds = quote.quote_seconds === null ? null : integer(quote.quote_seconds, 0, maxRouteSeconds), quote_microusd = quote.quote_microusd === null ? null : integer(quote.quote_microusd, 0, gemini ? 1_000_000 : 3_000_000);
    const rate = integer(quote.per_minute_microusd, gemini ? 0 : 1, 1_000_000);
    if (rate !== rates[index] || (quote_seconds !== null && (route_duration_seconds === null || quote_seconds > route_duration_seconds))) return invalid();
    if ((quote_seconds === null) !== (quote_microusd === null) || (quote_seconds !== null && Math.ceil(quote_seconds * rate / 60) !== quote_microusd)) return invalid();
    if (gemini && kinds[index] === "translation" && configured) return invalid();
    if (!configured && (quote_seconds !== null || can_reserve)) return invalid();
    if (can_reserve && (state !== "active" || !configured || (!amounts && !continuous) || quote_seconds === null || quote_seconds <= 0 || (remaining_seconds !== null && quote_seconds > remaining_seconds) || quote_microusd === null || (amounts !== null && quote_microusd > amounts.unallocated_microusd) || !stream_reservations || stream_reservations.used >= stream_reservations.limit)) return invalid();
    return {route: kinds[index], model: models[index], per_minute_microusd: rate, configured, quote_seconds, quote_microusd, can_reserve};
  });
  if (continuous && (route_duration_seconds !== 300 || stream_reservations?.limit !== 2 || routes.some(quote => quote.configured && quote.quote_seconds !== 300))) return invalid();
  const rawTts = object(row.tts, ["model", "configured", "per_input_utf8_byte_microusd", "max_input_utf8_bytes", "max_request_microusd", "can_reserve_maximum"]);
  const ttsModel = gemini ? "unavailable" : "tts-1-hd";
  const ttsRate = gemini ? 0 : 30, ttsBytes = gemini ? 0 : 2000;
  if (rawTts.model !== ttsModel || rawTts.per_input_utf8_byte_microusd !== ttsRate || rawTts.max_input_utf8_bytes !== ttsBytes || rawTts.max_request_microusd !== ttsRate * ttsBytes) return invalid();
  const tts: Budget["tts"] = {model: ttsModel, configured: bool(rawTts.configured), per_input_utf8_byte_microusd: ttsRate, max_input_utf8_bytes: ttsBytes, max_request_microusd: ttsRate * ttsBytes, can_reserve_maximum: bool(rawTts.can_reserve_maximum)};
  if (gemini && (tts.configured || tts.can_reserve_maximum || (tts_reservations && tts_reservations.limit !== 0))) return invalid();
  if (continuous && (tts.configured || tts.can_reserve_maximum)) return invalid();
  if (tts.can_reserve_maximum && (remaining_seconds === 0 || state !== "active" || !tts.configured || !amounts || amounts.unallocated_microusd < 60000 || !tts_reservations || tts_reservations.used >= tts_reservations.limit)) return invalid();
  if (["disabled", "unavailable"].includes(state) && (amounts !== null || uncertain !== null || stream_reservations !== null || tts_reservations !== null || route_duration_seconds !== null || routes.some(quote => quote.configured) || tts.configured)) return invalid();
  return {...(continuous ? {continuous} : {}), version: 1, generation, state, currency: "USD", pricing_version: schedule, uncertain, amounts, remaining_seconds, route_duration_seconds, stream_reservations, tts_reservations, routes, tts, invoice_verified: false, account_hard_cap_proven: false};
}

export type CostMode = "economy" | "interpretation" | "live_interpretation" | "assistant";
export type CostPlan = Readonly<{mode: CostMode; meeting_minutes: number; source_count: number; translation_streams: number; live_streams?: number; live_minutes: number}>;
export function plannedCost(budget: Budget, plan: CostPlan): number {
  const minutes = integer(plan.meeting_minutes, 1, 120), sources = integer(plan.source_count, 1, 2);
  const translations = integer(plan.translation_streams, 1, 4), live = integer(plan.live_minutes, 0, minutes);
  choice(plan.mode, ["economy", "interpretation", "live_interpretation", "assistant"] as const);
  const liveStreams = integer(plan.live_streams ?? 1, 1, 2);
  const rate = (kind: RouteKind) => budget.routes.find(row => row.route === kind)!.per_minute_microusd;
  return minutes * sources * rate("transcription") + (["interpretation", "assistant"].includes(plan.mode) ? minutes * translations * rate("translation") : 0) + (["assistant", "live_interpretation"].includes(plan.mode) ? live * liveStreams * rate("live") : 0);
}
export function ttsReservation(budget: Budget | null, bytes: number): number | null {
  if (!budget || !Number.isSafeInteger(bytes) || bytes <= 0 || bytes > budget.tts.max_input_utf8_bytes) return null;
  return bytes * budget.tts.per_input_utf8_byte_microusd;
}
export function canPrepare(budget: Budget | null, bytes: number): boolean {
  const reservation = ttsReservation(budget, bytes);
  return !!budget && budget.state === "active" && (budget.remaining_seconds === null || budget.remaining_seconds > 0) && budget.tts.configured && budget.uncertain === false && reservation !== null && !!budget.amounts && reservation <= budget.amounts.unallocated_microusd && !!budget.tts_reservations && budget.tts_reservations.used < budget.tts_reservations.limit;
}
export function usd(microusd: number): string {
  integer(microusd, 0);
  const whole = Math.floor(microusd / 1_000_000), fraction = String(microusd % 1_000_000).padStart(6, "0").replace(/0+$/, "").padEnd(2, "0");
  return new Intl.NumberFormat("en-US", {style: "currency", currency: "USD", minimumFractionDigits: 0, maximumFractionDigits: 0}).format(whole) + "." + fraction;
}

/** Round a start estimate up to whole cents without changing its admission amount. */
export function estimatedUsd(microusd: number): string {
  const cents = Math.ceil(integer(microusd, 0) / 10_000);
  return new Intl.NumberFormat("en-US", {style: "currency", currency: "USD"}).format(cents / 100);
}

export function quoteDuration(seconds: number): string {
  const minutes = Math.floor(integer(seconds, 0) / 60), remainder = seconds % 60;
  if (!minutes) return `${remainder} sec`;
  return remainder ? `${minutes} min ${remainder} sec` : `${minutes} min`;
}

/** One displayed intent covers two real reservations; server still checks each. */
export function meetReservation(budget: Budget | null): number | null {
  if (!budget || budget.state !== "active" || budget.uncertain !== false || (budget.remaining_seconds !== null && budget.remaining_seconds <= 0) || (!budget.amounts && !budget.continuous) || !budget.stream_reservations || budget.stream_reservations.limit - budget.stream_reservations.used < 2) return null;
  const captions = budget.routes.find(row => row.route === "transcription"), interpreter = budget.routes.find(row => row.route === "live");
  if (!captions?.configured || !interpreter?.configured || !captions.quote_seconds || captions.quote_seconds !== interpreter.quote_seconds || !captions.can_reserve || !interpreter.can_reserve || captions.quote_microusd == null || interpreter.quote_microusd == null) return null;
  const total = captions.quote_microusd + interpreter.quote_microusd;
  return Number.isSafeInteger(total) && total > 0 && (budget.continuous !== undefined || (!!budget.amounts && total <= budget.amounts.unallocated_microusd)) ? total : null;
}

/** WebRTC reserves the live route for the window plus its setup and closing allowance. */
export function privateWindowEstimate(budget: Budget | null, allocationSeconds?: number | null): number | null {
  if (!budget?.continuous || budget.state !== "active" || budget.pricing_version !== "pilot_audio_2026_09_13_v1" || allocationSeconds !== 335) return null;
  const live = budget.routes.find(row => row.route === "live");
  if (!live?.can_reserve || live.quote_seconds !== 300) return null;
  return Math.ceil(allocationSeconds * live.per_minute_microusd / 60);
}
