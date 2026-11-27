import assert from "node:assert/strict";
import {webcrypto} from "node:crypto";
import test from "node:test";
import {sessionStopChannel} from "../.test-build/session-stop-channel.js";

const firstSession = "a".repeat(64), nextSession = "b".repeat(64);
const digest = input => webcrypto.subtle.digest("SHA-256", input);
const deferred = () => {let resolve; const promise = new Promise(done => {resolve = done;}); return {promise, resolve};};
function bus() {
  const channels = [], sent = [];
  return {channels, sent, open(name) {
    const channel = {name, closed: false, onmessage: null,
      postMessage(data) {
        sent.push({name, data});
        for (const other of channels) if (other !== channel && !other.closed && other.name === name) other.onmessage?.({data});
      },
      close() {this.closed = true;},
    };
    channels.push(channel); return channel;
  }};
}

test("closing an earlier login cannot stop the fresh login, while same-session siblings receive Stop", async () => {
  const transport = bus(), observed = [], names = new Map();
  // WebCrypto completions may arrive in any order. Bind assertions to owners,
  // while still exercising all three asynchronous bindings concurrently.
  const create = label => sessionStopChannel({digest, open: name => {names.set(label, name); return transport.open(name);}, onStop: () => observed.push(label)});
  const oldPage = create("old"), sibling = create("sibling"), freshPage = create("fresh");
  await Promise.all([oldPage.bind(firstSession), sibling.bind(firstSession), freshPage.bind(nextSession)]);
  oldPage.stop(); oldPage.close();
  assert.deepEqual(observed, ["sibling"]);
  assert.equal(names.get("old"), names.get("sibling"));
  assert.notEqual(names.get("old"), names.get("fresh"));
  assert.match(names.get("old"), /^meeting-call-pilot-local-stop-v2-[a-f0-9]{64}$/);
  assert.ok(!JSON.stringify({names: transport.channels.map(row => row.name), sent: transport.sent}).includes(firstSession));
  assert.deepEqual(transport.sent.map(row => row.data), [{type: "pilot-stop"}]);
  sibling.close(); freshPage.close();
});

test("legacy unscoped broadcasts have no listener in the new session channel", async () => {
  const transport = bus(); let stopped = 0;
  const scoped = sessionStopChannel({digest, open: transport.open, onStop: () => {stopped++;}});
  await scoped.bind(nextSession);
  transport.open("meeting-call-pilot-local-stop-v1").postMessage({type: "pilot-stop"});
  assert.equal(stopped, 0); scoped.close();
});

test("Stop during hashing stays synchronous and emits one fixed negative command when ready", async () => {
  const transport = bus(), pending = deferred(); let hashInput;
  const scoped = sessionStopChannel({digest: input => {hashInput = input; return pending.promise;}, open: transport.open, onStop: () => assert.fail("No inbound Stop")});
  const binding = scoped.bind(firstSession);
  scoped.stop(); scoped.stop();
  assert.equal(transport.channels.length, 0);
  pending.resolve(await digest(hashInput)); await binding;
  assert.deepEqual(transport.sent.map(row => row.data), [{type: "pilot-stop"}]);
  assert.ok(hashInput.every(byte => byte === 0)); scoped.stop();
  assert.equal(transport.sent.length, 1); scoped.close();
});

test("page disposal while hashing cancels the pending signal and cannot create a late channel", async () => {
  const transport = bus(), pending = deferred();
  const scoped = sessionStopChannel({digest: () => pending.promise, open: transport.open, onStop: () => assert.fail("Retired callback")});
  const binding = scoped.bind(firstSession); scoped.stop(); scoped.close(); scoped.close();
  pending.resolve(new ArrayBuffer(32)); await binding; await scoped.bind(nextSession);
  assert.equal(transport.channels.length, 0); assert.equal(transport.sent.length, 0);
});

test("a login binds only once and malformed or positive messages cannot Stop it", async () => {
  const transport = bus(); let stopped = 0;
  const scoped = sessionStopChannel({digest, open: transport.open, onStop: () => {stopped++;}});
  await scoped.bind(firstSession); await scoped.bind(nextSession);
  assert.equal(transport.channels.length, 1);
  const receive = transport.channels[0].onmessage;
  for (const data of [null, "pilot-stop", [{type: "pilot-stop"}], {}, {type: "start"}, {type: "pilot-stop", text: "private"}]) receive({data});
  assert.equal(stopped, 0);
  receive({data: {type: "pilot-stop"}}); receive({data: {type: "pilot-stop"}});
  assert.equal(stopped, 1); assert.equal(transport.sent.length, 0);
  scoped.close(); receive({data: {type: "pilot-stop"}});
  assert.equal(stopped, 1); assert.equal(transport.channels[0].onmessage, null);
});

test("hashing failure, missing APIs and invalid credentials never open a global fallback", async () => {
  for (const hash of [async () => {throw new Error("Unavailable");}, async () => new ArrayBuffer(31)]) {
    const transport = bus();
    const scoped = sessionStopChannel({digest: hash, open: transport.open, onStop: () => assert.fail("Unexpected Stop")});
    await scoped.bind(firstSession); scoped.stop(); scoped.close();
    assert.equal(transport.channels.length, 0);
  }
  const transport = bus(), scoped = sessionStopChannel({digest, open: transport.open, onStop: () => assert.fail("Unexpected Stop")});
  await scoped.bind("not-an-authenticated-secret"); scoped.stop();
  assert.equal(transport.channels.length, 0); scoped.close();
});

test("channel send and close failure cannot throw out of local teardown", async () => {
  let messages = 0, closed = 0;
  const scoped = sessionStopChannel({digest, open: () => ({onmessage: null, postMessage() {messages++; throw new Error("Unavailable");}, close() {closed++; throw new Error("Unavailable");}}), onStop: () => {}});
  await scoped.bind(firstSession); scoped.stop(); scoped.stop(); scoped.close(); scoped.close();
  assert.equal(messages, 1); assert.equal(closed, 1);
});
