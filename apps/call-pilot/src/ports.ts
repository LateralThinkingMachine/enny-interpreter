import type {PrivateKind} from "./audio/framing.js";
export type {CaptureStatus} from "./audio/index.js";
import type {Language, Source} from "./protocol.js";
export interface AudioPort {
  prepareCapture(source: Source): Promise<void>;
  activateCapture(source: Source): void;
  cancelPreparedCapture(source: Source): Promise<void>;
  startCapture(source: Source): Promise<void>;
  stopCapture(source: Source): void;
  stopAll(): void;
  /** Optional for synchronous test ports; browser engines must confirm bounded retirement. */
  retire?(): Promise<boolean>;
  resumePlayback(): Promise<void>;
  setPrivateMuted(kind: PrivateKind, muted: boolean): void;
  playPrivate(kind: PrivateKind, pcm: Uint8Array, rate: number): void;
  playClip(bytes: ArrayBuffer, callbacks: {started(): void; ended(): void; failed(): void; isCurrent(): boolean}): Promise<void>;
  stopClip(): void;
}
export interface SocketPort {
  readyState: number; bufferedAmount: number;
  onopen: (() => void) | null; onclose: (() => void) | null; onerror: (() => void) | null;
  onmessage: ((event: {data: unknown}) => void) | null;
  send(data: string | ArrayBuffer): void;
  close(): void;
}
export type CostNotificationState = "default" | "granted" | "denied" | "unavailable";
export interface CostNotificationPort {
  status(): CostNotificationState;
  requestPermission(): Promise<CostNotificationState>;
  notify(message: string): void;
  close(): void;
}
export interface Dependencies {
  costNotifications?: CostNotificationPort;
  beginLogout?(): void;
  fetch: typeof fetch; socket(url: string): SocketPort; audio: AudioPort;
  now(): number; interval(callback: () => void, milliseconds: number): () => void;
  broadcastStop(): void; openPublicWindow(url: string): void;
  verifyClip(bytes: ArrayBuffer, clip: {digest: string; text: string; language: Language}): Promise<boolean>;
}
