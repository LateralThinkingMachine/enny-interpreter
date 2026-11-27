import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const html = await readFile(new URL("./meeting-interpreter.html", import.meta.url), "utf8");
const inline = name => html.match(new RegExp(`<${name}>([\\s\\S]*?)</${name}>`))[1];
const browserAdapter = await readFile(new URL("./browser-apis.js", import.meta.url), "utf8");
const makeInterpreter = vm.runInNewContext(browserAdapter + "\nmakeInterpreter", { AbortController });
const tick = () => new Promise(resolve => setImmediate(resolve));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function track(kind, surface) {
  return { kind, readyState: "live", enabled: true, stops: 0,
    getSettings() { return { displaySurface: surface }; },
    addEventListener(event, listener) { this[event] = listener; },
    stop() { this.stops++; this.readyState = "ended"; } };
}
function media(surface = "browser", includeAudio = true) {
  const video = track("video", surface);
  const audio = track("audio");
  return { video, audio, getTracks: () => includeAudio ? [video, audio] : [video],
    getAudioTracks: () => includeAudio ? [audio] : [], getVideoTracks: () => [video] };
}
function setup() {
  const calls = [], lines = [], states = [];
  let rec;
  class Recognition {
    static async available(options) { calls.push(["speechAvailability", options]); return "available"; }
    static async install(options) { calls.push(["install", options]); }
    get processLocally() { return this.local; }
    set processLocally(value) { this.local = value; }
    constructor() { rec = this; }
    start(input) { this.input = input; }
    abort() { calls.push(["abort"]); }
  }
  const model = { async translate(text) { calls.push(["translate", text]); return "local " + text; },
    destroy() { calls.push(["destroy"]); } };
  const selected = media();
  const detached = { contentWindow: {
    document: { featurePolicy: { allowsFeature: () => false } },
    SpeechRecognition: class {
      start() { assert.ok(detached.removed); throw new TypeError("parameter 1 is not of type MediaStreamTrack"); }
    }
  }, remove() { this.removed = true; } };
  const env = { isSecureContext: true, SpeechRecognition: Recognition, setTimeout, clearTimeout,
    document: { createElement: () => detached, body: { append(frame) { frame.removed = false; } } },
    Translator: { async availability() { return "downloadable"; }, async create() { calls.push(["create"]); return model; } },
    navigator: { userAgent: "Chrome/150.0", mediaDevices: {
      async getDisplayMedia() { calls.push(["display"]); return selected; },
      async getUserMedia() { calls.push(["microphone"]); return { ...selected, getTracks: () => [selected.audio], getVideoTracks: () => [] }; }
    } }
  };
  const controller = makeInterpreter(env, {
    state(phase, message) { states.push({ phase, message }); },
    line(source, translated) { lines.push([source, translated]); }
  });
  const choose = mode => controller.check({ mode, from: "en", to: "es" });
  const phase = () => states.at(-1)?.phase;
  const count = action => calls.filter(([key]) => key === action).length;
  return { env, selected, model, controller, choose, phase, count, calls, lines, states, recognition: () => rec };
}
function finalResult(rec, text) {
  const result = Object.assign([{ transcript: text }], { isFinal: true });
  rec.onresult({ resultIndex: 0, results: [result] });
}

test("the single file blocks outgoing connections and allows only its exact inline script and style", () => {
  for (const tag of ["script", "style"]) {
    const digest = createHash("sha256").update(inline(tag)).digest("base64");
    assert.ok(html.includes(`${tag}-src 'sha256-${digest}'`));
  }
  assert.ok(inline("script").includes(browserAdapter));
  assert.match(html, /'wasm-unsafe-eval'/);
  assert.doesNotMatch(html, /'unsafe-eval'|'unsafe-inline'/);
  const encoded = inline("script").match(/openPortableInterpreter\("([A-Za-z0-9+/=]+)"\);/)[1];
  const module = new WebAssembly.Module(Buffer.from(encoded, "base64"));
  assert.ok(WebAssembly.Module.exports(module).some(item => item.name === "mount_portable"));
  assert.ok(html.includes('id="runtime-notices"'));
  assert.match(html, /default-src 'none'/);
  assert.match(html, /connect-src 'none'/);
  assert.doesNotMatch(html, /<(?:link|script|img|iframe)\b[^>]*(?:href|src)=/i);
});
test("checking capabilities causes no model setup or audio capture", async () => {
  const t = setup();
  await t.choose("tab");
  assert.equal(t.phase(), "setup");
  assert.deepEqual(t.calls.map(([name]) => name), ["speechAvailability"]);
  assert.equal(t.calls[0][1].processLocally, true);
});
test("a browser that ignores the explicit track argument cannot begin audio capture", async () => {
  const t = setup();
  const frame = t.env.document.createElement("iframe");
  frame.contentWindow.SpeechRecognition = class {
    start() { assert.ok(frame.removed); throw new DOMException("Detached document", "InvalidStateError"); }
  };
  await t.choose("tab"); await t.controller.prepare(); await t.controller.start("", true);
  assert.equal(t.phase(), "unavailable"); assert.equal(t.count("microphone"), 0); assert.equal(t.count("display"), 0);
});
test("typed text uses the prepared translator without any speech or media API", async () => {
  const t = setup();
  delete t.env.navigator.mediaDevices;
  await t.choose("text"); await t.controller.prepare(); await t.controller.start("hello");
  assert.deepEqual(t.lines, [["hello", "local hello"]]);
  assert.equal(t.count("speechAvailability"), 0); assert.equal(t.count("install"), 0);
});
test("only the selected live tab audio goes to on-device recognition, and Stop releases every resource", async () => {
  const t = setup();
  await t.choose("tab"); await t.controller.prepare(); await t.controller.start("", true);
  assert.equal(t.phase(), "live");
  const rec = t.recognition();
  assert.equal(rec.input, t.selected.audio); assert.equal(rec.processLocally, true);
  assert.equal(t.selected.video.enabled, false); assert.equal(t.count("microphone"), 0);
  finalResult(rec, "hello"); await tick();
  assert.deepEqual(t.lines, [["hello", "local hello"]]);
  t.controller.end();
  assert.equal(t.count("abort"), 1); assert.equal(t.count("destroy"), 1);
  assert.equal(t.selected.audio.stops, 1); assert.equal(t.selected.video.stops, 1);
});
test("microphone mode explicitly requests microphone audio and passes that track to recognition", async () => {
  const t = setup();
  await t.choose("mic"); await t.controller.prepare(); await t.controller.start("", true);
  assert.equal(t.recognition().input, t.selected.audio);
  assert.equal(t.count("microphone"), 1); assert.equal(t.count("display"), 0);
  t.selected.audio.ended();
  assert.equal(t.phase(), "setup"); assert.equal(t.count("abort"), 1);
});
for (const [surface, audio] of [["window", true], ["monitor", true], ["browser", false]]) {
  test(`tab selection with surface=${surface} and audio=${audio} is released without microphone fallback`, async () => {
    const t = setup(), selected = media(surface, audio);
    t.env.navigator.mediaDevices.getDisplayMedia = async () => selected;
    await t.choose("tab"); await t.controller.prepare(); await t.controller.start("", true);
    assert.equal(t.phase(), "setup"); assert.equal(t.count("microphone"), 0); assert.equal(t.recognition(), undefined);
    assert.ok(selected.getTracks().every(item => item.stops === 1));
  });
}
test("a browser picker that resolves after Stop cannot start recognition or retain tracks", async () => {
  const t = setup(), picker = deferred();
  t.env.navigator.mediaDevices.getDisplayMedia = () => picker.promise;
  await t.choose("tab"); await t.controller.prepare();
  const start = t.controller.start("", true); t.controller.end();
  picker.resolve(t.selected); await start;
  assert.equal(t.recognition(), undefined); assert.ok(t.selected.getTracks().every(item => item.stops === 1));
});
test("a cancelled model download is destroyed even when the browser returns it after Stop", async () => {
  const t = setup(), download = deferred();
  t.env.Translator.create = () => download.promise;
  await t.choose("text");
  const prepare = t.controller.prepare(); t.controller.end();
  download.resolve(t.model); await prepare;
  assert.equal(t.count("destroy"), 1); assert.equal(t.phase(), "setup");
});
test("changing the input immediately ends capture and discards queued translations", async () => {
  const t = setup(), translation = deferred();
  t.model.translate = () => translation.promise;
  await t.choose("tab"); await t.controller.prepare(); await t.controller.start("", true);
  finalResult(t.recognition(), "first"); finalResult(t.recognition(), "second");
  await t.choose("text"); translation.resolve("late"); await tick();
  assert.equal(t.selected.audio.stops, 1); assert.deepEqual(t.lines, []);
});
test("speech language downloads require preparation and must actually become available", async () => {
  const t = setup();
  t.env.SpeechRecognition.available = async () => "downloadable";
  await t.choose("tab");
  assert.equal(t.count("install"), 0);
  await t.controller.prepare();
  assert.equal(t.count("install"), 1); assert.equal(t.phase(), "setup"); assert.equal(t.count("display"), 0);
  assert.equal(t.count("create"), 0);
});
test("natural recognition end releases capture promptly and delivers the last translated result", async () => {
  const t = setup(), translation = deferred();
  t.model.translate = () => translation.promise;
  await t.choose("tab"); await t.controller.prepare(); await t.controller.start("", true);
  finalResult(t.recognition(), "last"); t.recognition().onend();
  assert.equal(t.selected.audio.stops, 1); assert.equal(t.phase(), "finishing");
  translation.resolve("translated"); await tick();
  assert.deepEqual(t.lines, [["last", "translated"]]); assert.equal(t.phase(), "setup");
});
test("ending an unavailable session cannot unlock the model setup or capture controls", async () => {
  const t = setup(); t.env.isSecureContext = false;
  await t.choose("tab"); t.controller.end(); await t.controller.prepare();
  assert.equal(t.phase(), "unavailable"); assert.equal(t.count("create"), 0);
});
test("audio modes require explicit agreement even if the caller bypasses the UI", async () => {
  for (const mode of ["mic", "tab"]) {
    const t = setup(); await t.choose(mode); await t.controller.prepare();
    await t.controller.start(); assert.equal(t.phase(), "ready");
    assert.equal(t.count("microphone") + t.count("display"), 0);
  }
});
test("an ended tab video selection cannot begin recognition", async () => {
  const t = setup(); t.selected.video.readyState = "ended";
  await t.choose("tab"); await t.controller.prepare(); await t.controller.start("", true);
  assert.equal(t.phase(), "setup"); assert.equal(t.recognition(), undefined); assert.equal(t.selected.audio.stops, 1);
});
test("the portable file carries the complete repository license", async () => {
  const license = await readFile(new URL("../LICENSE", import.meta.url), "utf8");
  assert.ok(html.includes(license));
});
test("the audio capability probe never calls start without first verifying microphone denial", async () => {
  const t = setup(); const frame = t.env.document.createElement("iframe");
  frame.contentWindow.document.featurePolicy.allowsFeature = () => true;
  let created = false; frame.contentWindow.SpeechRecognition = class { constructor() { created = true; } };
  await t.choose("tab");
  assert.equal(t.phase(), "unavailable"); assert.equal(created, false);
  assert.equal(frame.removed, true); assert.match(frame.allow, /microphone 'none'/);
});
test("speech and translation pack downloads receive separate user activations", async () => {
  const t = setup();
  let status = "downloadable";
  t.env.SpeechRecognition.available = async () => status;
  t.env.SpeechRecognition.install = async () => { status = "available"; return true; };
  await t.choose("tab"); await t.controller.prepare();
  assert.equal(t.phase(), "setup"); assert.equal(t.count("create"), 0);
  await t.controller.prepare(); assert.equal(t.phase(), "ready"); assert.equal(t.count("create"), 1);
});
test("when dictation is unavailable the UI offers only local basic recognition", async () => {
  const t = setup(), queries = [];
  t.env.SpeechRecognition.available = async options => {
    queries.push([options.quality, options.processLocally]); return options.quality === "command" ? "available" : "unavailable";
  };
  await t.choose("tab");
  assert.deepEqual(queries, [["dictation", true], ["command", true]]);
  assert.equal(t.phase(), "setup");
});
test("a browser that hears speech without producing captions gives explicit feedback and releases capture", async () => {
  const t = setup(); let alert, cleared;
  t.env.setTimeout = callback => { alert = callback; return 1; };
  t.env.clearTimeout = id => { cleared = id; };
  await t.choose("tab"); await t.controller.prepare(); await t.controller.start("", true);
  t.recognition().onspeechstart(); alert();
  assert.match(t.states.at(-1).message, /detected speech but has not returned captions/);
  t.recognition().onend();
  assert.match(t.states.at(-1).message, /without returning captions/); assert.equal(cleared, 1);
});
