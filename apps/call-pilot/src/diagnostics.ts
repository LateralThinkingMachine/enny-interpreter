import {choice, integer, object} from "./protocol.js";
import type {RouteKind, Source} from "./protocol.js";

const stopCodes = ["operator_stop", "operator_disconnected", "deadline", "backpressure", "invalid_frame", "process_shutdown", "unknown"] as const;
const issueCodes = ["source_gap", "source_incomplete", "sequence_gap", "budget_exhausted", "backpressure", "provider_closed", "provider_connection_lost", "source_input_interrupted", "live_closed_requested", "live_expired", "live_content_closed", "live_remote_hangup", "live_connection_lost", "provider_protocol", "provider_unavailable", "deadline", "text_limit", "stream_limit", "input_audio_limit", "output_audio_limit", "input_pacing", "speech_boundary_limit", "source_ledger_limit", "transcript_item_limit", "usage_mismatch", "source_response_deadline", "protocol_capacity_limit", "unknown"] as const;
export type AsrUsageDiagnostic = Readonly<{submitted_ms: number; reported_ms: number | null; allowed_ms: number}>;
export type Diagnostics = Readonly<{
  version: 1; generation: number; stopped: boolean;
  stop: Readonly<{code: typeof stopCodes[number]; elapsed_seconds: number}> | null;
  last_issue: Readonly<{code: typeof issueCodes[number]; source: Source; route: RouteKind | null; elapsed_seconds: number; asr_usage?: AsrUsageDiagnostic}> | null;
}>;

/** Separate, fixed metadata only. Never accept provider payloads or identifiers. */
export function parseDiagnostics(value: unknown, minimumGeneration: number): Diagnostics {
  const row = object(value, ["version", "generation", "stopped", "stop", "last_issue"]);
  if (row.version !== 1 || typeof row.stopped !== "boolean") throw new Error("Invalid diagnostic response");
  const generation = integer(row.generation, minimumGeneration);
  let stop: Diagnostics["stop"] = null, last_issue: Diagnostics["last_issue"] = null;
  if (row.stop !== null) {
    const entry = object(row.stop, ["code", "elapsed_seconds"]);
    stop = {code: choice(entry.code, stopCodes), elapsed_seconds: integer(entry.elapsed_seconds, 0)};
  }
  if (row.stopped !== (stop !== null)) throw new Error("Inconsistent diagnostic response");
  if (row.last_issue !== null) {
    const hasUsage = typeof row.last_issue === "object" && Object.hasOwn(row.last_issue, "asr_usage");
    const entry = object(row.last_issue, hasUsage ? ["code", "source", "route", "elapsed_seconds", "asr_usage"] : ["code", "source", "route", "elapsed_seconds"]);
    last_issue = {code: choice(entry.code, issueCodes), source: choice(entry.source, ["remote_mix", "self"] as const), route: entry.route === null ? null : choice(entry.route, ["transcription", "translation", "live"] as const), elapsed_seconds: integer(entry.elapsed_seconds, 0)};
    if (hasUsage) {
      if (last_issue.code !== "usage_mismatch" || last_issue.route !== "transcription") throw new Error("Invalid diagnostic response");
      const usage = object(entry.asr_usage, ["submitted_ms", "reported_ms", "allowed_ms"]);
      const submitted_ms = integer(usage.submitted_ms, 0, 120_000), allowed_ms = integer(usage.allowed_ms, 1000, 121_000);
      const reported_ms = usage.reported_ms === null ? null : integer(usage.reported_ms, 0);
      if (allowed_ms !== submitted_ms + 1000 || (reported_ms !== null && reported_ms <= allowed_ms)) throw new Error("Invalid diagnostic response");
      last_issue = {...last_issue, asr_usage: {submitted_ms, reported_ms, allowed_ms}};
    }
    // Stop and issue times are sampled on concurrent paths. They are bounded
    // observations, not a total order; never reject useful metadata over skew.
  }
  return {version: 1, generation, stopped: row.stopped, stop, last_issue};
}

export function stopMessage(code: string): string {
  const messages: Record<string, string> = {
    operator_stop: "The local server recorded a Stop request from the operator page.",
    operator_disconnected: "The operator connection closed or lost its heartbeat. The server stopped processing.",
    deadline: "The approved local session time limit was reached.",
    backpressure: "Audio or event delivery could not keep up. The server stopped processing.",
    invalid_frame: "The server rejected an invalid audio frame and stopped processing.",
    process_shutdown: "The local process received a shutdown signal.",
  };
  return Object.hasOwn(messages, code) ? messages[code] : "The server stopped processing, but a specific reason is unavailable.";
}

export function elapsedLabel(seconds: number): string {
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s after the local process started`;
}
