import assert from "node:assert/strict";
import test from "node:test";
import {publicWindowLink, trustedParentStop} from "../.test-build/window-link.js";

test("parent retains only its exact public child and sends only fixed negative command", () => {
  const sent = [], urls = []; const first = {closed: false, postMessage: (...args) => sent.push(args)};
  const link = publicWindowLink(url => {urls.push(url); return first;});
  link.open("http://127.0.0.1:5182/public#ticket=one_use"); link.stop(); link.stop();
  assert.deepEqual(sent, [[{type: "pilot-stop"}, "http://127.0.0.1:5182"]]); assert.equal(urls.length, 1);
  for (const bad of ["http://127.0.0.1:5180/public#ticket=one_use", "https://evil.invalid/public#ticket=one_use", "http://127.0.0.1:5182/api/session#ticket=one_use", "http://127.0.0.1:5182/public?leak=one_use#ticket=one_use"]) assert.throws(() => link.open(bad));
  assert.equal(urls.length, 1);
});

test("public accepts Stop only from captured parent reference and exact operator origin", () => {
  const parent = {}, event = {source: parent, origin: "http://127.0.0.1:5180", data: {type: "pilot-stop"}};
  assert.equal(trustedParentStop(event, parent), true);
  assert.equal(trustedParentStop({...event, source: {}}, parent), false); assert.equal(trustedParentStop({...event, origin: "http://127.0.0.1:5182"}, parent), false); assert.equal(trustedParentStop({...event, origin: "https://evil.invalid"}, parent), false);
  assert.equal(trustedParentStop({...event, data: {type: "pilot-stop", text: "private data"}}, parent), false); assert.equal(trustedParentStop({...event, data: {type: "play"}}, parent), false); assert.equal(trustedParentStop(event, null), false);
});
