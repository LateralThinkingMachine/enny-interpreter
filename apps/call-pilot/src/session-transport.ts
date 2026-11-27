import {pilotOrigins} from './origins.js';
import type {SocketPort} from './ports.js';
import type {Role} from './protocol.js';

type StoragePort = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
type Ports = {
  fetch: typeof fetch;
  socket(url: string, protocols: string[]): SocketPort;
  storage(): StoragePort;
};

export const EVENT_PROTOCOL = 'pilot.events.v1';

function validToken(value: string | null): value is string {
  return value !== null && value.length === 64 && /^[a-f0-9]+$/.test(value);
}

/** Keep credentials out of render state, the device owners and the URL. A page
 * normally holds its credential only in memory. On navigation it parks it in
 * this tab's origin-scoped storage for reload, and consumes it on entry. */
export function tabSession(role: Role, origin: string, freshTicket: boolean, ports: Ports) {
  if (pilotOrigins(origin)[role] !== origin) throw new Error('Invalid session origin');
  const storageKey = 'pilot.session.v1.' + role;
  const prefix = role === 'operator' ? '/api/' : '/api/public/';
  const eventPath = role === 'operator' ? '/api/operator/events' : '/api/public/events';
  const eventUrl = origin.replace('http:', 'ws:') + eventPath;
  let latest: string | null = null;
  let leaving = false;
  try {
    const storage = ports.storage();
    const saved = storage.getItem(storageKey);
    storage.removeItem(storageKey);
    if (!freshTicket && validToken(saved)) latest = saved;
  } catch { /* A fresh login still works without browser storage. */ }

  return {
    openClient() {
      let bound = latest;
      let successor: string | null = null;
      let retired = false;
      let signingOut = false;
      let revocation: string | null = null;

      function beginLogout() {
        if (retired || signingOut) return;
        signingOut = true;
        revocation = successor ?? bound;
        if (latest === revocation || latest === bound) latest = null;
        bound = successor = null;
      }

      const fetchApi: typeof fetch = async (input, init) => {
        // All application calls use root-relative API paths, including clips.
        if (leaving || retired || typeof input !== 'string' || !/^\/api\/[A-Za-z0-9_/-]+$/.test(input)) throw new Error('Invalid local API request');
        const method = init?.method ?? 'GET';
        if (method !== 'GET' && method !== 'POST') throw new Error('Invalid local API method');
        const bootstrap = method === 'POST' && input === prefix + 'bootstrap';
        const logout = method === 'POST' && input === prefix + 'logout';
        const rollover = role === 'operator' && method === 'POST' && (input === '/api/session/new' || input === '/api/interpretation/resume');
        if (logout) beginLogout();
        else if (signingOut || ((bootstrap || rollover) && latest !== bound && latest !== successor)) throw new Error('Local session changed');
        const token = logout ? revocation : bound;
        const expected = latest;
        const headers = new Headers(init?.headers);
        headers.delete('Authorization');
        if (!bootstrap) {
          if (!token) throw new Error('Local session unavailable');
          headers.set('Authorization', 'Bearer ' + token);
        }
        const response = await ports.fetch(origin + input, {...init, headers, method, credentials: 'omit', mode: 'same-origin', redirect: 'error', cache: 'no-store'});
        // A failed public Stop can retry only this revocation. Its in-memory
        // credential is never returned to ordinary calls or persisted on leave.
        if (logout && (response.ok || response.status === 401)) revocation = null;
        if ((bootstrap || rollover) && response.ok) {
          const issued = response.headers.get('X-Pilot-Session');
          if (!validToken(issued) || retired || signingOut || leaving || init?.signal?.aborted || latest !== expected) throw new Error('Local session changed');
          latest = issued;
          // A delayed request from the predecessor keeps its old authority.
          // Only an explicit logout can revoke the pending successor.
          if (bootstrap) bound = issued;
          else successor = issued;
        }
        return response;
      };

      return {
        fetch: fetchApi,
        beginLogout,
        socket(url: string): SocketPort {
          if (retired || leaving || !bound || url !== eventUrl) throw new Error('Local event session unavailable');
          return ports.socket(eventUrl, [EVENT_PROTOCOL, 'pilot.auth.' + bound]);
        },
        dispose() { retired = true; bound = successor = revocation = null; },
      };
    },
    leave() {
      if (leaving) return;
      leaving = true;
      try {
        if (latest) ports.storage().setItem(storageKey, latest);
        else ports.storage().removeItem(storageKey);
      } catch { /* The operator can reopen with a fresh launcher session. */ }
      latest = null;
    },
  };
}
