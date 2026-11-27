import assert from "node:assert/strict";
import test from "node:test";
import {canPrepare, estimatedUsd, quoteDuration, meetReservation, parseBudget, plannedCost, ttsReservation, usd} from "../.test-build/cost.js";
import {budgetFixture, disabledBudget, geminiBudget} from "./budget-fixture.mjs";

test("known price catalog yields exact 90-minute economy, interpretation and assistant plans", () => {
  const budget = parseBudget(budgetFixture(), 1);
  const plan = {mode: "economy", meeting_minutes: 90, source_count: 2, translation_streams: 1, live_minutes: 90};
  assert.equal(plannedCost(budget, plan), 3_060_000);
  assert.equal(plannedCost(budget, {...plan, mode: "interpretation"}), 6_120_000);
  assert.equal(usd(plannedCost(budget, {...plan, mode: "interpretation"})), "$6.12");
  assert.equal(plannedCost(budget, {...plan, mode: "assistant"}), 10_620_000);
  assert.equal(usd(plannedCost(budget, {...plan, mode: "assistant"})), "$10.62");
  assert.equal(plannedCost(budget, {...plan, mode: "assistant", translation_streams: 2, live_minutes: 10}), 9_680_000);
  assert.equal(plannedCost(budget, {...plan, meeting_minutes: 1, source_count: 1, live_minutes: 0}), 17000);
  assert.equal(plannedCost(budget, {...plan, mode: "assistant", meeting_minutes: 120, translation_streams: 4, live_minutes: 120}), 26_400_000);
});

test("integer money preserves sub-cent precision and rejects nonfinite, unsafe and fractional plans", () => {
  const budget = parseBudget(budgetFixture(), 1), plan = {mode: "economy", meeting_minutes: 90, source_count: 2, translation_streams: 1, live_minutes: 0};
  assert.equal(estimatedUsd(4_016_650), "$4.02");
  assert.equal(estimatedUsd(10_000), "$0.01");
  assert.equal(quoteDuration(3597), "59 min 57 sec");
  assert.equal(quoteDuration(9), "9 sec");
  assert.equal(quoteDuration(300), "5 min");
  assert.equal(usd(1), "$0.000001"); assert.equal(usd(17000), "$0.017"); assert.equal(usd(1_000_000_000), "$1,000.00");
  for (const value of [-1, 0.1, NaN, Infinity, "100", true, Number.MAX_SAFE_INTEGER + 1]) assert.throws(() => usd(value));
  assert.equal(usd(Number.MAX_SAFE_INTEGER), "$9,007,199,254.740991");
  for (const patch of [{meeting_minutes: 0}, {meeting_minutes: 121}, {meeting_minutes: 1.5}, {meeting_minutes: "90"}, {source_count: 0}, {source_count: 3}, {translation_streams: 0}, {translation_streams: 5}, {live_minutes: -1}, {live_minutes: 91}, {mode: "automatic"}]) assert.throws(() => plannedCost(budget, {...plan, ...patch}));
});

test("strict budget schema rejects wrong generation, claims, accounting and route identities", () => {
  const bad = [value => {value.generation = 2;}, value => {value.version = true;}, value => {value.currency = "JPY";}, value => {value.pricing_version = "other";}, value => {value.invoice_verified = true;}, value => {value.account_hard_cap_proven = true;}, value => {value.key = "unexpected";}, value => {delete value.remaining_seconds;}, value => {value.amounts.reserved_microusd = 1;}, value => {value.amounts.approved_microusd = "20000000";}, value => {value.stream_reservations.used = 17;}, value => {value.uncertain = true;}, value => {value.routes.reverse();}, value => {value.routes[0].model = "gpt-realtime";}, value => {value.routes[0].can_reserve = "true";}, value => {value.routes[0].quote_microusd--;}, value => {value.routes[0].configured = false;}, value => {value.tts.per_input_utf8_byte_microusd = 31;}];
  for (const mutate of bad) {const value = budgetFixture(); mutate(value); assert.throws(() => parseBudget(value, 1));}
  assert.doesNotThrow(() => parseBudget(disabledBudget(), 1));
  assert.equal(canPrepare(parseBudget(disabledBudget(), 1), 1), false);
});

test("fixed pricing version rejects coherently altered rates and quotes", () => {
  const value = budgetFixture(); value.routes[0].per_minute_microusd = 1; value.routes[0].quote_microusd = 10;
  assert.throws(() => parseBudget(value, 1));
});

test("disabled and unavailable catalogs cannot invent an activation allowance", () => {
  for (const state of ["disabled", "unavailable"]) {
    const value = disabledBudget(); value.state = state;
    value.amounts = {approved_microusd: 1_000_000, reserved_microusd: 0, unallocated_microusd: 1_000_000};
    assert.throws(() => parseBudget(value, 1));
  }
});

test("available route quotes fit current duration and remaining activation", () => {
  for (const change of [value => {value.remaining_seconds = 0;}, value => {value.remaining_seconds = 599;}, value => {value.route_duration_seconds = 599;}]) {
    const value = budgetFixture(); change(value); assert.throws(() => parseBudget(value, 1));
  }
  const value = budgetFixture({remaining_seconds: 21, route_duration_seconds: 20});
  value.routes = value.routes.map(row => ({...row, quote_seconds: 20, quote_microusd: Math.ceil(20 * row.per_minute_microusd / 60)}));
  assert.equal(parseBudget(value, 1).routes[0].quote_microusd, 5667);
  // A provider quote may span a longer lifetime than this runtime. Keep its
  // actual reservation visible, but it cannot be offered as admissible here.
  const shorterRuntime = budgetFixture({remaining_seconds: 599});
  shorterRuntime.routes = shorterRuntime.routes.map(row => ({...row, can_reserve: false}));
  assert.equal(parseBudget(shorterRuntime, 1).routes[0].quote_seconds, 600);
  assert.equal(parseBudget(shorterRuntime, 1).routes[0].quote_microusd, 170000);
  const expired = budgetFixture({remaining_seconds: 0});
  expired.routes = expired.routes.map(row => ({...row, quote_seconds: 0, quote_microusd: 0, can_reserve: false}));
  assert.throws(() => parseBudget(expired, 1)); // TTS maximum still claims admission at zero time.
});

test("TTS affordability is exact UTF-8 byte reservation, not maximum-request-only or character count", () => {
  const text = "あह😀", bytes = new TextEncoder().encode(text).length;
  assert.equal(bytes, 10);
  const value = budgetFixture({amounts: {approved_microusd: 20_000_000, reserved_microusd: 19_999_700, unallocated_microusd: 300}});
  value.routes = value.routes.map(row => ({...row, can_reserve: false})); value.tts.can_reserve_maximum = false;
  const budget = parseBudget(value, 1);
  assert.equal(ttsReservation(budget, bytes), 300); assert.equal(canPrepare(budget, bytes), true); assert.equal(canPrepare(budget, bytes + 1), false);
  assert.equal(ttsReservation(budget, 2000), 60000);
  for (const count of [0, -1, 2001, 0.5, NaN, Infinity, "10"]) assert.equal(ttsReservation(budget, count), null);
  assert.equal(ttsReservation(null, 10), null);
  assert.equal(canPrepare({...budget, remaining_seconds: 0}, bytes), false);
  assert.equal(canPrepare({...budget, state: "uncertain"}, bytes), false);
  assert.equal(canPrepare({...budget, tts_reservations: {used: 8, limit: 8}}, bytes), false);
});

test("Live interpretation uses the exact shared Live rate without adding a dedicated translation route", () => {
  const budget = parseBudget(budgetFixture(), 1), plan = {mode: "live_interpretation", meeting_minutes: 90, source_count: 1, translation_streams: 1, live_minutes: 90, live_streams: 1};
  assert.equal(plannedCost(budget, plan), 6_030_000);
  assert.equal(plannedCost(budget, {...plan, source_count: 2, live_streams: 2}), 12_060_000);
  assert.equal(plannedCost(budget, {...plan, live_minutes: 15}), 2_280_000);
  assert.equal(plannedCost(budget, {...plan, mode: "assistant"}), 9_090_000);
  for (const live_streams of [0, 3, 1.5, NaN, Infinity, "1"]) assert.throws(() => plannedCost(budget, {...plan, live_streams}));
  assert.equal(budget.routes.length, 3); assert.equal(budget.routes[2].model, "gpt-live-1");
});


test("Meet quote requires the combined captions and Live allocation even when each route fits alone", () => {
  const value = budgetFixture({amounts: {approved_microusd: 1_000_000, reserved_microusd: 400_000, unallocated_microusd: 600_000}});
  const budget = parseBudget(value, 1);
  assert.equal(budget.routes.find(row => row.route === "transcription").can_reserve, true);
  assert.equal(budget.routes.find(row => row.route === "live").can_reserve, true);
  assert.equal(meetReservation(budget), null); // 170,000 + 500,000 exceeds the remaining 600,000.
});

test("Meet reservation accepts its exact integer allowance boundary and rejects one microUSD less", () => {
  for (const [remaining, expected] of [[670_000, 670_000], [669_999, null], [670_001, 670_000]]) {
    const value = budgetFixture({amounts: {approved_microusd: 1_000_000, reserved_microusd: 1_000_000 - remaining, unallocated_microusd: remaining}});
    // Dedicated translation and prepared TTS are not part of this two-route intent.
    value.routes[1] = {...value.routes[1], configured: false, quote_seconds: null, quote_microusd: null, can_reserve: false};
    value.tts.configured = false; value.tts.can_reserve_maximum = false;
    assert.equal(meetReservation(parseBudget(value, 1)), expected);
  }
});

test("Meet intent requires two unused stream admissions, including at the exact admission boundary", () => {
  for (const [used, limit, expected] of [[0, 2, 670_000], [14, 16, 670_000], [15, 16, null], [16, 16, null], [0, 1, null], [0, 0, null]]) {
    const value = budgetFixture({stream_reservations: {used, limit}});
    if (used === limit) value.routes = value.routes.map(row => ({...row, can_reserve: false}));
    assert.equal(meetReservation(parseBudget(value, 1)), expected);
  }
});

test("Meet reservation refuses absent, disabled, uncertain, expired and nonactive allowance state", () => {
  assert.equal(meetReservation(null), null);
  assert.equal(meetReservation(parseBudget(disabledBudget(), 1)), null);
  const unavailable = disabledBudget(); unavailable.state = "unavailable";
  assert.equal(meetReservation(parseBudget(unavailable, 1)), null);
  for (const state of ["uncertain", "finishing", "stopped", "expired"]) {
    const value = budgetFixture({state, uncertain: state === "uncertain"});
    value.routes = value.routes.map(row => ({...row, can_reserve: false})); value.tts.can_reserve_maximum = false;
    assert.equal(meetReservation(parseBudget(value, 1)), null);
  }
  const budget = parseBudget(budgetFixture(), 1);
  for (const patch of [{uncertain: true}, {uncertain: null}, {remaining_seconds: 0}, {amounts: null}, {stream_reservations: null}]) assert.equal(meetReservation({...budget, ...patch}), null);
});

test("Meet reservation requires configured admissible quotes for both exact required route kinds", () => {
  for (const index of [0, 2]) for (const patch of [
    {configured: false, quote_seconds: null, quote_microusd: null, can_reserve: false},
    {can_reserve: false},
    {quote_seconds: null, quote_microusd: null, can_reserve: false},
  ]) {
    const value = budgetFixture(); value.routes[index] = {...value.routes[index], ...patch};
    assert.equal(meetReservation(parseBudget(value, 1)), null);
  }
  const budget = parseBudget(budgetFixture(), 1);
  for (const missing of ["transcription", "live"]) assert.equal(meetReservation({...budget, routes: budget.routes.filter(row => row.route !== missing)}), null);
});

test("invalid wire quotes and admission counters cannot reach a usable Meet reservation", () => {
  for (const mutate of [
    value => {value.routes[0].quote_microusd = -1;},
    value => {value.routes[2].quote_microusd = 500000.5;},
    value => {value.routes[2].quote_microusd = Number.MAX_SAFE_INTEGER;},
    value => {value.routes[0].quote_microusd = NaN;},
    value => {value.routes[2].quote_microusd = "500000";},
    value => {value.routes[2].quote_seconds = null;},
    value => {value.stream_reservations = {used: 17, limit: 16};},
    value => {value.stream_reservations = {used: 0, limit: 1.5};},
    value => {value.amounts.unallocated_microusd = Infinity;},
    value => {value.routes[2].model = "gpt-realtime-translate";},
  ]) {
    const value = budgetFixture(); mutate(value);
    assert.throws(() => meetReservation(parseBudget(value, 1)));
  }
});

test("finite Gemini quotes bind to the operator vendor and admit only its two configured streams", () => {
  const raw = geminiBudget();
  const parsed = parseBudget(raw, 1, false, "gemini");
  assert.equal(meetReservation(parsed), 2_000_000);
  assert.equal(canPrepare(parsed, 1), false);
  assert.throws(() => parseBudget(raw, 1, false));
  assert.throws(() => parseBudget(budgetFixture(), 1, false, "gemini"));
  assert.throws(() => parseBudget(raw, 1, true, "gemini"));
  for (const change of [
    value => {value.routes[0].model = "gemini-3.5-live-translate-preview";},
    value => {value.routes[0].quote_microusd--;},
    value => {value.routes[1].configured = true;},
    value => {value.tts.configured = true;},
    value => {value.tts_reservations.limit = 1;},
  ]) {
    const value = geminiBudget();
    change(value);
    assert.throws(() => parseBudget(value, 1, false, "gemini"));
  }
});

test("Gemini snapshot and reservations stop at ten minutes while OpenAI retains its approved duration", () => {
  const valid = parseBudget(geminiBudget(), 1, false, "gemini");
  assert.equal(valid.remaining_seconds, 600);
  assert.equal(valid.routes[2].quote_seconds, 600);
  for (const key of ["remaining_seconds", "route_duration_seconds"]) {
    assert.throws(() => parseBudget({...geminiBudget(), [key]: 601}, 1, false, "gemini"));
  }
  const excessiveQuote = geminiBudget();
  excessiveQuote.routes[0] = {...excessiveQuote.routes[0], quote_seconds: 601, quote_microusd: 1_001_667};
  assert.throws(() => parseBudget(excessiveQuote, 1, false, "gemini"));
  const legacy = budgetFixture({remaining_seconds: 601, route_duration_seconds: 601});
  legacy.routes[0] = {...legacy.routes[0], quote_seconds: 601, quote_microusd: 170284};
  assert.equal(parseBudget(legacy, 1, false).routes[0].quote_seconds, 601);
});
