import {ORIGIN, PUBLIC_ORIGIN} from "./protocol.js";

type PublicWindow = {postMessage(message: unknown, targetOrigin: string): void; closed: boolean};
/** One negative-only WindowProxy; no text, audio, credentials or positive commands. */
export function publicWindowLink(open: (url: string) => PublicWindow | null) {
  let child: PublicWindow | null = null;
  function stop(): void {
    const target = child; child = null;
    if (target && !target.closed) {try {target.postMessage({type: "pilot-stop"}, PUBLIC_ORIGIN);} catch { /* Server/liveness teardown remains independently active. */ }}
  }
  return {stop, open(url: string): void {
    const parsed = new URL(url);
    if (parsed.origin !== PUBLIC_ORIGIN || parsed.pathname !== "/public" || parsed.search || !/^#ticket=[A-Za-z0-9_-]{1,128}$/.test(parsed.hash)) throw new Error("Invalid public window link");
    stop();
    child = open(url);
    if (!child) throw new Error("Public window blocked");
  }};
}

export function trustedParentStop(event: {origin: string; source: unknown; data: unknown}, parent: unknown): boolean {
  if (!parent || event.source !== parent || event.origin !== ORIGIN || !event.data || typeof event.data !== "object" || Array.isArray(event.data)) return false;
  const data = event.data as Record<string, unknown>;
  return Object.keys(data).length === 1 && data.type === "pilot-stop";
}
