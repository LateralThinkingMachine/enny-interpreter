import assert from 'node:assert/strict';
import test from 'node:test';
import {tabSession} from '../.test-build/session-transport.js';

const operator = 'http://127.0.0.1:5180', viewer = 'http://127.0.0.1:5182';
const first = 'a'.repeat(64), next = 'b'.repeat(64);
const post = {method: 'POST', headers: {'X-Pilot-CSRF': 'local-proof'}};

function harness() {
  const saved = new Map(), requests = [], events = [];
  const storage = {getItem: key => saved.get(key) ?? null, setItem: (key, value) => saved.set(key, value), removeItem: key => saved.delete(key)};
  let reply = () => new Response('{}');
  const ports = {
    storage: () => storage,
    fetch: (url, init) => {requests.push({url, ...init}); return reply();},
    socket: (url, protocols) => {events.push({url, protocols}); return {};},
  };
  return {saved, ports, requests, events,
    reply: fn => {reply = fn;},
    issue: token => {reply = () => new Response('{}', {headers: {'X-Pilot-Session': token}});},
  };
}

test('bootstrap, reads, writes, WebRTC cleanup and event admission use only the selected origin', async () => {
  for (const [role, origin, bootstrap, eventPath] of [
    ['operator', operator, '/api/bootstrap', '/api/operator/events'],
    ['public', viewer, '/api/public/bootstrap', '/api/public/events'],
  ]) {
    const h = harness(), owner = tabSession(role, origin, true, h.ports).openClient();
    h.issue(first);
    await owner.fetch(bootstrap, post);
    assert.equal(h.requests[0].headers.has('authorization'), false);
    const path = role === 'operator' ? '/api/webrtc/stop' : '/api/public/receipt';
    await owner.fetch(path, post);
    await owner.fetch(role === 'operator' ? '/api/budget' : '/api/public/session');
    for (const req of h.requests.slice(1)) {
      assert.equal(req.headers.get('Authorization'), 'Bearer ' + first);
      assert.equal(req.credentials, 'omit');
      assert.equal(req.redirect, 'error');
      assert.equal(req.mode, 'same-origin');
      assert.equal(req.cache, 'no-store');
      assert.equal(new URL(req.url).origin, origin);
    }
    assert.equal(h.requests[1].headers.get('X-Pilot-CSRF'), 'local-proof');
    const ws = origin.replace('http:', 'ws:') + eventPath;
    owner.socket(ws);
    assert.deepEqual(h.events, [{url: ws, protocols: ['pilot.events.v1', 'pilot.auth.' + first]}]);
    assert.equal(h.saved.size, 0);
    const count = h.requests.length;
    await assert.rejects(owner.fetch('https://outside.invalid/api/session'));
    await assert.rejects(owner.fetch('//127.0.0.1:8080/api/session'));
    await assert.rejects(owner.fetch('/api/../elsewhere'));
    assert.throws(() => owner.socket(ws.replace(eventPath, '/other')));
    assert.throws(() => owner.socket(ws.replace(/:\d+\//, ':8080/')));
    assert.equal(h.requests.length, count);
    assert.equal(h.events.length, 1);
  }
});

test('reload restores this tab and role only, and a new ticket supersedes the previous login', async () => {
  const h = harness(), tab = tabSession('operator', operator, true, h.ports);
  h.issue(first);
  await tab.openClient().fetch('/api/bootstrap', post);
  tab.leave();
  assert.equal(h.saved.size, 1);
  const publicTab = tabSession('public', viewer, false, h.ports);
  await assert.rejects(publicTab.openClient().fetch('/api/public/session'));
  assert.equal(h.requests.length, 1);
  const reloaded = tabSession('operator', operator, false, h.ports);
  assert.equal(h.saved.size, 0);
  await reloaded.openClient().fetch('/api/session');
  assert.equal(h.requests.at(-1).headers.get('Authorization'), 'Bearer ' + first);
  reloaded.leave();
  const newLogin = tabSession('operator', operator, true, h.ports);
  await assert.rejects(newLogin.openClient().fetch('/api/session'));
});

test('rotation binds each generation to its credential and a predecessor can retry the same transition', async () => {
  for (const path of ['/api/session/new', '/api/interpretation/resume']) {
    const h = harness(), tab = tabSession('operator', operator, true, h.ports), old = tab.openClient();
    h.issue(first);
    await old.fetch('/api/bootstrap', post);
    h.issue(next);
    await old.fetch(path, post);
    await old.fetch(path, post);
    await old.fetch('/api/stop', post);
    assert.deepEqual(h.requests.slice(1).map(req => req.headers.get('Authorization')), Array(3).fill('Bearer ' + first));
    const fresh = tab.openClient();
    await fresh.fetch('/api/session');
    assert.equal(h.requests.at(-1).headers.get('Authorization'), 'Bearer ' + next);
    old.dispose();
    await assert.rejects(old.fetch('/api/logout', post));
    await fresh.fetch('/api/logout', post);
    assert.equal(h.requests.at(-1).headers.get('Authorization'), 'Bearer ' + next);
    await assert.rejects(fresh.fetch('/api/session'));
    tab.leave();
    assert.equal(h.saved.size, 0);
  }
});

test('pending header cannot restore local access after sign-out or page exit', async () => {
  for (const close of ['logout', 'leave']) {
    const h = harness(), tab = tabSession('operator', operator, true, h.ports), owner = tab.openClient();
    h.issue(first);
    await owner.fetch('/api/bootstrap', post);
    let resolve;
    h.reply(() => new Promise(done => {resolve = done;}));
    const pending = owner.fetch('/api/session/new', post);
    if (close === 'logout') {
      h.reply(() => {throw new Error('Unconfirmed');});
      await assert.rejects(owner.fetch('/api/logout', post));
    } else tab.leave();
    resolve(new Response('{}', {headers: {'X-Pilot-Session': next}}));
    await assert.rejects(pending);
    tab.leave();
    assert.deepEqual([...h.saved.values()], close === 'logout' ? [] : [first]);
  }
});

test('an invalid or missing credential cannot establish a session; blocked storage still allows fresh login', async () => {
  for (const value of ['', 'invalid', 'C'.repeat(64), first + ', ' + next]) {
    const h = harness(), owner = tabSession('operator', operator, true, h.ports).openClient();
    h.issue(value);
    await assert.rejects(owner.fetch('/api/bootstrap', post));
    await assert.rejects(owner.fetch('/api/session'));
  }
  const h = harness();
  h.ports.storage = () => {throw new Error('Unavailable');};
  const tab = tabSession('operator', operator, true, h.ports), owner = tab.openClient();
  h.issue(first);
  await owner.fetch('/api/bootstrap', post);
  await owner.fetch('/api/session');
  assert.equal(h.requests.at(-1).headers.get('Authorization'), 'Bearer ' + first);
  tab.leave();
});

test('a failed public Stop can retry only revocation, without persisting or granting any other access', async () => {
  const h = harness(), tab = tabSession('public', viewer, true, h.ports), owner = tab.openClient();
  h.issue(first);
  await owner.fetch('/api/public/bootstrap', post);
  h.reply(() => {throw new Error('Transient');});
  await assert.rejects(owner.fetch('/api/public/logout', post));
  await assert.rejects(owner.fetch('/api/public/session'));
  assert.throws(() => owner.socket('ws://127.0.0.1:5182/api/public/events'));
  h.reply(() => new Response('{}'));
  await owner.fetch('/api/public/logout', post);
  assert.equal(h.requests.at(-1).headers.get('Authorization'), 'Bearer ' + first);
  await assert.rejects(owner.fetch('/api/public/logout', post));
  assert.equal(h.requests.length, 3);
  tab.leave();
  assert.equal(h.saved.size, 0);
});

test('local sign-out during bootstrap cannot retain an unconfirmed browser session', async () => {
  const h = harness(), tab = tabSession('operator', operator, true, h.ports), owner = tab.openClient();
  let resolve;
  h.reply(() => new Promise(done => {resolve = done;}));
  const request = owner.fetch('/api/bootstrap', post);
  owner.beginLogout();
  resolve(new Response('{}', {headers: {'X-Pilot-Session': first}}));
  await assert.rejects(request);
  tab.leave();
  assert.equal(h.saved.size, 0);
});
