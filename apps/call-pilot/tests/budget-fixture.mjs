// Synthetic metadata only. Importing this fixture never enables a provider.
export function budgetFixture(overrides = {}) {
  const result = {
    version: 1, generation: 1, state: "active", currency: "USD", pricing_version: "pilot_audio_2026_09_13_v1",
    uncertain: false, amounts: {approved_microusd: 20_000_000, reserved_microusd: 0, unallocated_microusd: 20_000_000},
    remaining_seconds: 5400, route_duration_seconds: 600,
    stream_reservations: {used: 0, limit: 16}, tts_reservations: {used: 0, limit: 8},
    routes: [
      {route: "transcription", model: "gpt-live-transcribe", per_minute_microusd: 17000},
      {route: "translation", model: "gpt-realtime-translate", per_minute_microusd: 34000},
      {route: "live", model: "gpt-live-1", per_minute_microusd: 50000},
    ].map(row => ({...row, configured: true, quote_seconds: 600, quote_microusd: row.per_minute_microusd * 10, can_reserve: true})),
    tts: {model: "tts-1-hd", configured: true, per_input_utf8_byte_microusd: 30, max_input_utf8_bytes: 2000, max_request_microusd: 60000, can_reserve_maximum: true},
    invoice_verified: false, account_hard_cap_proven: false,
  };
  return {...result, ...overrides};
}

export function disabledBudget() {
  const value = budgetFixture({state: "disabled", uncertain: null, amounts: null, remaining_seconds: 0, route_duration_seconds: null, stream_reservations: null, tts_reservations: null});
  value.routes = value.routes.map(row => ({...row, configured: false, quote_seconds: null, quote_microusd: null, can_reserve: false}));
  value.tts = {...value.tts, configured: false, can_reserve_maximum: false};
  return value;
}

export function geminiBudget() {
  const budget = budgetFixture({remaining_seconds: 600, pricing_version: "gemini_preview_reserve_2026_11_27_v1", tts_reservations: {used: 0, limit: 0}});
  budget.routes = [
    {route: "transcription", model: "gemini-3.5-transcribe-live", per_minute_microusd: 100_000, configured: true, quote_seconds: 600, quote_microusd: 1_000_000, can_reserve: true},
    {route: "translation", model: "unavailable", per_minute_microusd: 0, configured: false, quote_seconds: null, quote_microusd: null, can_reserve: false},
    {route: "live", model: "gemini-3.5-live-translate-preview", per_minute_microusd: 100_000, configured: true, quote_seconds: 600, quote_microusd: 1_000_000, can_reserve: true},
  ];
  budget.tts = {model: "unavailable", configured: false, per_input_utf8_byte_microusd: 0, max_input_utf8_bytes: 0, max_request_microusd: 0, can_reserve_maximum: false};
  return budget;
}
