import type {Language} from "./protocol.js";
export type Preferences = Readonly<{version: 1; language: Language}>;
export const defaultPreferences = (): Preferences => ({version: 1, language: "en"});
export const PREFERENCES_KEY = "real-time-interpretation.preferences.v1";
export interface PreferenceStore {read(): Preferences; write(value: Preferences): boolean}
export function parsePreferences(value: unknown): Preferences {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid preferences");
  const row = value as Record<string, unknown>;
  if (Object.keys(row).sort().join(",") !== "language,version" || row.version !== 1 || !["en", "ja", "hi", "es", "vi"].includes(row.language as string)) throw new Error("Invalid preferences");
  return {version: 1, language: row.language as Language};
}
/** Only the language preference persists. Storage is never needed to start. */
export function browserPreferences(storage: () => Pick<Storage, "getItem" | "setItem">): PreferenceStore {
  return {
    read() {try {const raw = storage().getItem(PREFERENCES_KEY); return raw && raw.length <= 256 ? parsePreferences(JSON.parse(raw)) : defaultPreferences();} catch {return defaultPreferences();}},
    write(value) {try {storage().setItem(PREFERENCES_KEY, JSON.stringify(parsePreferences(value))); return true;} catch {return false;}},
  };
}
