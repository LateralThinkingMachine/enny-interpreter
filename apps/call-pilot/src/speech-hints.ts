import type {Language} from "./protocol.js";

export type SpeechHints = Readonly<{languages: readonly Language[]; keywords: readonly string[]}>;
export type SpokenLanguage = Language | "auto";
export const DEFAULT_SPEECH_HINTS: SpeechHints = Object.freeze({languages: Object.freeze(["ja", "en", "hi"] as const), keywords: Object.freeze([])});

/** Explicit literal hints only; no transcript, address-book or document inference. */
export function speechHints(language: SpokenLanguage, text: string): SpeechHints {
  if (!["auto", "ja", "en", "hi", "es", "vi"].includes(language)) throw new Error("Choose a supported spoken language.");
  if (text.length > 4096) throw new Error("Use at most 1,024 UTF-8 bytes of terms.");
  const lines = text.split(/\r?\n/);
  if (lines.some(line => Array.from(line).some(char => {
    const point = char.codePointAt(0)!;
    return (point >= 0xd800 && point <= 0xdfff) || /\p{Cc}/u.test(char) || "<>\u200b\u200e\u200f\ufeff".includes(char) || /[\u202a-\u202e\u2066-\u2069]/u.test(char);
  }))) throw new Error("Terms cannot contain invalid text, control characters or angle brackets.");
  const keywords = lines.map(word => word.trim()).filter(Boolean);
  if (keywords.length > 20) throw new Error("Use at most 20 terms, one per line.");
  if (new Set(keywords).size !== keywords.length) throw new Error("Remove repeated terms.");
  let bytes = 0;
  for (const word of keywords) {
    if (Array.from(word).length > 80) throw new Error("Keep each term within 80 characters.");
    bytes += new TextEncoder().encode(word).length;
  }
  if (bytes > 1024) throw new Error("Use at most 1,024 UTF-8 bytes of terms.");
  return {languages: language === "auto" ? DEFAULT_SPEECH_HINTS.languages : [language], keywords};
}
export function hasSpeechHints(hints: SpeechHints): boolean {return hints.languages.length !== 3 || hints.keywords.length > 0;}
