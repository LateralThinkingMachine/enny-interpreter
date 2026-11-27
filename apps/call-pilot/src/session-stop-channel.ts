type Channel = Pick<BroadcastChannel, "onmessage" | "postMessage" | "close">;
type Dependencies = {
  digest(input: Uint8Array<ArrayBuffer>): Promise<ArrayBuffer>;
  open(name: string): Channel;
  onStop(): void;
};

const PREFIX = "meeting-call-pilot-local-stop-v2";

/** Best-effort negative signal for one authenticated login; never an authority or retry path. */
export function sessionStopChannel(dependencies: Dependencies) {
  let channel: Channel | null = null;
  let binding = false, closed = false, stopped = false, pendingStop = false;
  function send(): void {
    if (!channel || !pendingStop || closed) return;
    pendingStop = false;
    try {channel.postMessage({type: "pilot-stop"});} catch { /* Local and server Stop remain independent. */ }
  }
  return {
    async bind(csrf: string): Promise<void> {
      if (binding || closed) return;
      binding = true;
      // The server supplies a random 256-bit CSRF. Never place the credential itself on the bus.
      if (!/^[a-f0-9]{64}$/.test(csrf)) return;
      const input = new TextEncoder().encode(`${PREFIX}\0${csrf}`);
      try {
        const digest = new Uint8Array(await dependencies.digest(input));
        if (closed || digest.length !== 32) return;
        const name = `${PREFIX}-${Array.from(digest, byte => byte.toString(16).padStart(2, "0")).join("")}`;
        channel = dependencies.open(name);
        channel.onmessage = event => {
          if (closed || stopped || !event.data || typeof event.data !== "object" || Array.isArray(event.data)) return;
          const data = event.data as Record<string, unknown>;
          if (Object.keys(data).length !== 1 || data.type !== "pilot-stop") return;
          stopped = true;
          dependencies.onStop();
        };
        send();
      } catch { /* Unsupported hashing/channel APIs never broaden the signal to other logins. */ }
      finally {input.fill(0);}
    },
    stop(): void {
      if (closed || stopped) return;
      stopped = true; pendingStop = true;
      send();
    },
    close(): void {
      closed = true; pendingStop = false;
      const owned = channel; channel = null;
      if (owned) {
        owned.onmessage = null;
        try {owned.close();} catch { /* Already retired locally. */ }
      }
    },
  };
}
