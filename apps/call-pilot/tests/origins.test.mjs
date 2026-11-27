import assert from "node:assert/strict";
import {spawnSync} from "node:child_process";
import test from "node:test";
import {pilotOrigins} from "../.test-build/origins.js";

test("origin selection accepts only the two reviewed loopback pairs and freezes the result", () => {
  const standard = {operator: "http://127.0.0.1:5180", public: "http://127.0.0.1:5182"};
  const alternate = {operator: "http://127.0.0.1:5080", public: "http://127.0.0.1:5082"};
  for (const origin of Object.values(alternate)) assert.deepEqual(pilotOrigins(origin), alternate);
  for (const origin of [...Object.values(standard), undefined, "http://localhost:5080", "https://127.0.0.1:5080", "http://127.0.0.1:5081", "http://127.0.0.1:5080/", "http://127.0.0.1:5080.evil.invalid", "null"]) assert.deepEqual(pilotOrigins(origin), standard);
  assert.ok(Object.isFrozen(pilotOrigins(undefined))); assert.ok(Object.isFrozen(pilotOrigins(alternate.operator)));
});

// Each realm imports the actual client, protocol and window link after setting
// its synthetic browser origin. This catches one consumer retaining the old pair.
const realmCheck = `
import assert from "node:assert/strict";
globalThis.window = {location: {origin: process.argv[1]}};
const {ORIGIN, PUBLIC_ORIGIN} = await import("./.test-build/protocol.js");
const {PilotClient, takeEntry} = await import("./.test-build/client.js");
const {publicWindowLink, trustedParentStop} = await import("./.test-build/window-link.js");
const {budgetFixture} = await import("./tests/budget-fixture.mjs");
const alternate = ["http://127.0.0.1:5080", "http://127.0.0.1:5082"].includes(process.argv[1]);
const expectedOperator = "http://127.0.0.1:" + (alternate ? "5080" : "5180");
const expectedPublic = "http://127.0.0.1:" + (alternate ? "5082" : "5182");
const otherOperator = "http://127.0.0.1:" + (alternate ? "5180" : "5080");
const otherPublic = "http://127.0.0.1:" + (alternate ? "5182" : "5082");
assert.equal(ORIGIN, expectedOperator); assert.equal(PUBLIC_ORIGIN, expectedPublic);
const scrubbed = [];
const history = {replaceState: (...args) => scrubbed.push(args)};
for (const [origin, pathname, hash, role] of [[ORIGIN, "/", "#bootstrap=synthetic", "operator"], [PUBLIC_ORIGIN, "/public", "#ticket=synthetic", "public"]]) {
  assert.deepEqual(takeEntry({origin, pathname, hash}, history), {role, ticket: "synthetic"});
  assert.deepEqual(scrubbed.at(-1), [null, "", pathname]);
}
for (const [origin, pathname, hash] of [[otherOperator, "/", "#bootstrap=synthetic"], [otherPublic, "/public", "#ticket=synthetic"], [PUBLIC_ORIGIN, "/", "#bootstrap=synthetic"], [ORIGIN, "/public", "#ticket=synthetic"], [PUBLIC_ORIGIN, "/public", "#bootstrap=synthetic"], ["https://attacker.invalid", "/", "#bootstrap=synthetic"]]) {
  const before = scrubbed.length;
  assert.throws(() => takeEntry({origin, pathname, hash}, history));
  assert.equal(scrubbed.length, before + 1);
  assert.deepEqual(scrubbed.at(-1), [null, "", pathname]);
}
if (![ORIGIN, PUBLIC_ORIGIN].includes(process.argv[1])) {
  const before = scrubbed.length;
  assert.throws(() => takeEntry({origin: process.argv[1], pathname: "/", hash: "#bootstrap=synthetic"}, history));
  assert.equal(scrubbed.length, before + 1);
}

const sent = [], opened = [], parent = {};
const link = publicWindowLink(url => {opened.push(url); return {closed: false, postMessage: (...args) => sent.push(args)};});
link.open(PUBLIC_ORIGIN + "/public#ticket=synthetic"); link.stop();
assert.deepEqual(sent, [[{type: "pilot-stop"}, PUBLIC_ORIGIN]]);
for (const bad of [otherPublic + "/public#ticket=synthetic", ORIGIN + "/public#ticket=synthetic", PUBLIC_ORIGIN + "/public?origin=" + otherPublic + "#ticket=synthetic"]) assert.throws(() => link.open(bad));
assert.equal(opened.length, 1);
assert.equal(trustedParentStop({origin: ORIGIN, source: parent, data: {type: "pilot-stop"}}, parent), true);
for (const origin of [otherOperator, otherPublic, PUBLIC_ORIGIN]) assert.equal(trustedParentStop({origin, source: parent, data: {type: "pilot-stop"}}, parent), false);

const json = value => new Response(JSON.stringify(value), {headers: {"Content-Type": "application/json"}});
for (const role of ["operator", "public"]) {
  const sockets = [], calls = []; let ticketOrigin = PUBLIC_ORIGIN;
  const session = {role, csrf: "synthetic_csrf", generation: 1, stopped: false, providers_enabled: true, expires_in_seconds: 900, capabilities: {source_captions: role === "operator", private_translation: role === "operator", live_assistant: role === "operator", live_interpreter: role === "operator", prepared_public_speech: true}, limitations: ["Synthetic test only"]};
  const client = new PilotClient(role, {
    audio: {stopAll() {}, async resumePlayback() {}}, now: () => 0, interval: () => () => {}, broadcastStop() {},
    fetch: async path => {
      calls.push(path);
      if (path.endsWith("/session")) return json(session);
      if (path === "/api/budget") return json(budgetFixture());
      if (path === "/api/public/tickets") return json({ticket: "synthetic_ticket", expires_in_seconds: 120, path: "/public", origin: ticketOrigin});
      throw new Error("Unexpected synthetic request");
    },
    socket: url => {const socket = {url, readyState: 1, bufferedAmount: 0, onopen: null, onmessage: null, onclose: null, onerror: null, send() {}, close() {}}; sockets.push(socket); return socket;}
  });
  await client.initialize(null); await client.connect();
  assert.equal(sockets.length, 1);
  assert.equal(sockets[0].url, (role === "operator" ? ORIGIN : PUBLIC_ORIGIN).replace("http:", "ws:") + "/api/" + role + "/events");
  sockets[0].onmessage({data: JSON.stringify({type: "hello", generation: 1, role, stopped: false})});
  assert.equal(client.snapshot().connected, true);
  if (role === "operator") {
    ticketOrigin = PUBLIC_ORIGIN; await client.issuePublicTicket();
    assert.equal(client.takePublicUrl(), PUBLIC_ORIGIN + "/public#ticket=synthetic_ticket");
    ticketOrigin = otherPublic; await client.issuePublicTicket();
    assert.equal(client.takePublicUrl(), null); assert.equal(client.snapshot().publicTicketReady, false);
    assert.equal(client.snapshot().stopped, true);
  } else assert.equal(calls.includes("/api/budget"), false);
  client.dispose();
}
`;

for (const origin of ["http://127.0.0.1:5080", "http://127.0.0.1:5082", "http://127.0.0.1:5180", "http://127.0.0.1:5182", "https://attacker.invalid"]) {
  test(`actual browser-origin imports keep entry, sockets, tickets and window Stop in one pair: ${origin}`, () => {
    const result = spawnSync(process.execPath, ["--input-type=module", "--eval", realmCheck, origin], {cwd: new URL("../", import.meta.url), encoding: "utf8", timeout: 10_000});
    assert.equal(result.error, undefined); assert.equal(result.signal, null);
    assert.equal(result.status, 0, result.stderr || result.stdout);
  });
}
