export type CaptionParticipant = Readonly<{id: number; name: string | null; email: string | null}>;
export type CaptionEntry = Readonly<{sequence: number; window: number; configuration_revision?: number; voice_generation?: number; provenance: "gpt_live_input" | "gpt_live_output"; text: string; start_ms: number; end_ms: number; participants: readonly CaptionParticipant[]; model: "gpt-live-1"}>;
export const INTERPRETER_VOICES = ["marin", "quartz", "gleam", "meridian"] as const;
