import type {Interpretation, Language} from "./protocol.js";
export const LANGUAGE_LABELS: Readonly<Record<Language, string>> = Object.freeze({en: "English", ja: "Japanese · 日本語", hi: "Hindi · हिन्दी", es: "Spanish · Español", vi: "Vietnamese · Tiếng Việt"});
export const LEGACY_LANGUAGES: readonly Language[] = Object.freeze(["ja", "en", "hi"]);
export const INTERPRETATION_LANGUAGES: readonly Language[] = Object.freeze(["ja", "en", "hi", "es", "vi"]);
export function interpretationLabel(policy: Interpretation | undefined, fallback: Language): string {
  if (!policy) return LANGUAGE_LABELS[fallback];
  return policy.mode === "rules" ? "Custom Translation rules" : policy.mode === "two_way" ? `${LANGUAGE_LABELS[policy.first]} ↔ ${LANGUAGE_LABELS[policy.second]}` : `Auto → ${LANGUAGE_LABELS[policy.target]}`;
}
