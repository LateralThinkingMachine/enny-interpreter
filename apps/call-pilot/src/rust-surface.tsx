import {useLayoutEffect, useRef} from "react";
import type {ReactNode} from "react";

interface SurfaceHandle {
  update(json: string): void;
  dispose(): void;
  free(): void;
}
type Action = (action: string, value: string) => unknown;
type Mount = (element: HTMLElement, kind: string, json: string, dispatch: Action) => SurfaceHandle;
let mount: Mount | undefined;

export function installRustSurfaces(render: Mount) {
  mount = render;
}

/** Each panel sends only its display fields to WASM. The existing client owns all
 * credentials, streams and permissions. Gesture callbacks must stay synchronous. */
export function RustSurface({kind, state, dispatch, fallback}: {kind: string; state: object; dispatch?: Action; fallback: ReactNode}) {
  const container = useRef<HTMLDivElement>(null);
  const handle = useRef<SurfaceHandle | undefined>(undefined);
  const previous = useRef<string | null>(null);
  const latest = useRef({state, dispatch});
  useLayoutEffect(() => { latest.current = {state, dispatch}; });
  useLayoutEffect(() => {
    if (!mount || !container.current) return;
    const json = JSON.stringify(latest.current.state);
    const view = mount(container.current, kind, json, (name, value) => latest.current.dispatch?.(name, value));
    handle.current = view;
    previous.current = json;
    return () => {
      handle.current = undefined;
      previous.current = null;
      try { view.dispose(); } finally { view.free(); }
    };
  }, [kind]);
  useLayoutEffect(() => {
    if (!handle.current) return;
    const json = JSON.stringify(state);
    if (previous.current !== json) {
      handle.current.update(json);
      previous.current = json;
    }
  }, [state]);
  // Static renderer callers have no DOM. Browser entry initializes Rust before
  // mounting this tree and reports initialization failures on the entry page.
  if (typeof document === "undefined") return <>{fallback}</>;
  return <div data-rust-ui={kind} className="rust-surface" ref={container}/>;
}
