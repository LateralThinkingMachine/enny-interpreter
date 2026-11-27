import {choice, integer, object, type Session} from "./protocol.js";

export type StopReceipt = Readonly<{
  version: 1; session_generation: number; generation: number;
  provider_activity: "none_admitted" | "close_observed" | "unconfirmed";
  reserved_microusd: number | null; invoice: "unavailable";
}>;

function initialGeneration(session: Session): number {
  return integer(session.session_generation ?? (session.stopped ? session.generation - 1 : session.generation), 1);
}

export function parseStopAck(value: unknown, session: Session): void {
  const row = object(value, ["state", "generation"]);
  if (row.state !== "stopped" || integer(row.generation) !== initialGeneration(session) + 1) throw new Error("Invalid Stop acknowledgment");
}

export function parseStopReceipt(value: unknown, session: Session): StopReceipt {
  const row = object(value, ["version", "session_generation", "generation", "provider_activity", "reserved_microusd", "invoice"]);
  if (session.role !== "operator" || row.version !== 1 || row.invoice !== "unavailable" || row.session_generation !== initialGeneration(session)
      || integer(row.generation) !== row.session_generation + 1) throw new Error("Invalid Stop receipt");
  const activity = choice(row.provider_activity, ["none_admitted", "close_observed", "unconfirmed"] as const);
  // A finite receipt is bounded by the maximum native activation allowance.
  const reserved = row.reserved_microusd === null ? null : integer(row.reserved_microusd, 0, 1_000_000_000);
  if ((activity === "none_admitted" && reserved !== 0) || (activity === "close_observed" && (reserved === null || reserved === 0))) {
    throw new Error("Inconsistent provider reservation");
  }
  return {version: 1, session_generation: row.session_generation, generation: integer(row.generation),
    provider_activity: activity, reserved_microusd: reserved, invoice: "unavailable"};
}
