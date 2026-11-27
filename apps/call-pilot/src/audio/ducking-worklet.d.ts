export interface DuckingSettings {
  version: 'private-ducking.v1';
  enabled: boolean;
  originalVolume: number;
  translatedVolume: number;
  originalMuted: boolean;
  translatedMuted: boolean;
}
export const DUCKING_PROFILE: Readonly<{version: 'private-ducking.v1'; attackMs: number; releaseMs: number; holdMs: number; volumeRampMs: number; thresholdOn: number; thresholdOff: number; duckGain: number; headroom: number}>;
export const DEFAULT_DUCKING_SETTINGS: Readonly<DuckingSettings>;
export function validateDuckingSettings(settings: unknown): Readonly<DuckingSettings>;
export class DuckingMixer {
  constructor(sampleRate: number, settings?: DuckingSettings);
  readonly settings: Readonly<DuckingSettings>;
  readonly active: boolean;
  readonly ducking: boolean;
  readonly frames: number;
  updateSettings(settings: DuckingSettings): void;
  render(original: Float32Array[], translated: Float32Array[], output: Float32Array[], translationEnabled?: boolean): boolean;
}
