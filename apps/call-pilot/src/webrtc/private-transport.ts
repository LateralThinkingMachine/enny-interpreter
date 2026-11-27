/** Browser transport observations only; never model or end-to-end timing. */
export type PrivateTransportEvidence = Readonly<{
  status: 'pending' | 'observed' | 'unavailable';
  roundTripMs: number | null;
  inboundJitterMs: number | null;
  jitterBufferDelayMs: number | null;
  sampleIntervalMs: number | null;
}>;
export const EMPTY_TRANSPORT_EVIDENCE: PrivateTransportEvidence = Object.freeze({status: 'pending', roundTripMs: null, inboundJitterMs: null, jitterBufferDelayMs: null, sampleIntervalMs: null});
export const UNAVAILABLE_TRANSPORT_EVIDENCE: PrivateTransportEvidence = Object.freeze({...EMPTY_TRANSPORT_EVIDENCE, status: 'unavailable'});
type Row = Record<string, unknown>;
type Baseline = {id: string; timestamp: number; delay: number; emitted: number};
const MAX_ROWS = 256, MAX_INTERVAL_MS = 5_000, MAX_DELAY_MS = 60_000;
function nonnegative(value: unknown): value is number {return typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= Number.MAX_SAFE_INTEGER;}
function identity(value: unknown): value is string {return typeof value === 'string' && value.length > 0 && value.length <= 256;}
function milliseconds(value: unknown): number | null {
  return nonnegative(value) && value <= MAX_DELAY_MS / 1000 ? value * 1000 : null;
}

/** One instance per actual peer. Only one bounded private counter baseline is
 * retained; raw reports, addresses, track labels and media are never stored. */
export class PrivateTransportSampler {
  private previous: Baseline | null = null;
  reset(): void {this.previous = null;}

  sample(report: RTCStatsReport): PrivateTransportEvidence {
    const rows = new Map<string, Row>();
    let inbound: Row | null = null, count = 0;
    for (const value of report.values()) {
      if (++count > MAX_ROWS) {this.reset(); return UNAVAILABLE_TRANSPORT_EVIDENCE;}
      if (!value || typeof value !== 'object') continue;
      const row = value as Row;
      if (!identity(row.id)) continue;
      if (rows.has(row.id)) {this.reset(); return UNAVAILABLE_TRANSPORT_EVIDENCE;}
      rows.set(row.id, row);
      if (row.type === 'inbound-rtp' && (row.kind ?? row.mediaType) === 'audio') {
        // This route owns one translated track. Do not combine unrelated or
        // overlapping SSRC lifetimes into a fabricated delay measurement.
        if (inbound) {this.reset(); return UNAVAILABLE_TRANSPORT_EVIDENCE;}
        inbound = row;
      }
    }
    if (!inbound) {this.reset(); return UNAVAILABLE_TRANSPORT_EVIDENCE;}
    const inboundJitterMs = milliseconds(inbound.jitter);
    const transport = identity(inbound.transportId) ? rows.get(inbound.transportId) : undefined;
    const pair = transport?.type === 'transport' && identity(transport.selectedCandidatePairId) ? rows.get(transport.selectedCandidatePairId) : undefined;
    const roundTripMs = pair?.type === 'candidate-pair' && pair.state === 'succeeded' ? milliseconds(pair.currentRoundTripTime) : null;
    let jitterBufferDelayMs: number | null = null, sampleIntervalMs: number | null = null;
    const {id, timestamp, jitterBufferDelay: delay, jitterBufferEmittedCount: emitted} = inbound;
    if (identity(id) && nonnegative(timestamp) && nonnegative(delay) && nonnegative(emitted) && Number.isSafeInteger(emitted)) {
      const previous = this.previous;
      if (previous?.id === id) {
        const interval = timestamp - previous.timestamp, added = emitted - previous.emitted;
        if (interval > 0 && interval <= MAX_INTERVAL_MS && added > 0 && delay >= previous.delay) {
          jitterBufferDelayMs = milliseconds((delay - previous.delay) / added);
          if (jitterBufferDelayMs !== null) sampleIntervalMs = interval;
        }
      }
      // A repeated cached report has no new interval. Regressing counters or
      // timestamps establish a fresh baseline, never a negative/old estimate.
      this.previous = {id, timestamp, delay, emitted};
    } else this.reset();
    const observed = roundTripMs !== null || inboundJitterMs !== null || jitterBufferDelayMs !== null;
    return Object.freeze({status: observed ? 'observed' : 'unavailable', roundTripMs, inboundJitterMs, jitterBufferDelayMs, sampleIntervalMs});
  }
}
