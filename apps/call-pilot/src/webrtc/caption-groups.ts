import type {CaptionEntry, CaptionParticipant} from "./caption-data.js";

export const CAPTION_GROUP_LIMITS = Object.freeze({fragments: 256, characters: 32768, spanMs: 60000, gapMs: 2000, tokenGapMs: 8000, paragraphCharacters: 600, paragraphSentences: 3, pageSize: 200});
type Ending = "open" | "punctuation" | "boundary" | "limit" | "gap";
export type CaptionGroup = Readonly<{
  sequence: number; window: number; configuration_revision?: number; voice_generation?: number; provenance: CaptionEntry["provenance"]; model: CaptionEntry["model"];
  text: string; start_ms: number; end_ms: number; participants: readonly CaptionParticipant[];
  originals: readonly CaptionEntry[]; ending: Ending; continuation: boolean; earlierWordsUnavailable: boolean; partlyUnidentified: boolean;
}>;
type MutableGroup = Omit<{-readonly [Key in keyof CaptionGroup]: CaptionGroup[Key]}, "originals"> & {originals: CaptionEntry[]; knownParticipants: readonly CaptionParticipant[]; hasUnknown: boolean; sentenceBreaks: number};
const sentencePunctuation = /[.!?。！？][\s"'”’）)\]】」』]*$/u;
const punctuationContinuation = /^[\s.!?。！？…"'”’）)\]】」』]*$/u;
const abbreviatedPeriod = /(?:\b(?:Mr|Mrs|Ms|Dr|Prof|Sr|Jr|St|vs|etc)|\b[A-Za-z])\.$/iu;
function sameParticipants(left: readonly CaptionParticipant[], right: readonly CaptionParticipant[]): boolean {
  return left.length === right.length && left.every((person, index) => person.id === right[index].id && person.name === right[index].name && person.email === right[index].email);
}

/** A reading projection only: exact delivered text, never a source final or an audit rewrite.
 * Only the most recent group in each input/output lane can grow. Approximate timestamps
 * bound paragraph size; they never sort words or assign a speaker to missing evidence.
 */
export function groupCaptionEntries(entries: readonly CaptionEntry[]): readonly CaptionGroup[] {
  const groups: MutableGroup[] = [];
  const lanes = new Map<CaptionEntry["provenance"], MutableGroup>();
  const firstLanes = new Set<CaptionEntry["provenance"]>();
  const lanesAfterGap = new Set<CaptionEntry["provenance"]>();
  const truncatedStart = (entries[0]?.sequence ?? 1) > 1;
  let previousSequence: number | null = null;
  for (const entry of entries) {
    const missingEntries = previousSequence !== null && entry.sequence !== previousSequence + 1;
    if (missingEntries) {
      for (const group of lanes.values()) if (group.ending === "open") group.ending = "gap";
      lanes.clear();
      lanesAfterGap.add("gpt_live_input"); lanesAfterGap.add("gpt_live_output");
    }
    previousSequence = entry.sequence;
    const last = lanes.get(entry.provenance);
    // Unknown fragments may share a reading paragraph with one consistent named
    // contributor, but then the entire paragraph is labelled partly unidentified.
    // Two distinct known identities or changing overlap evidence always separate.
    const compatibleEvidence = last !== undefined && (last.knownParticipants.length <= 1 && entry.participants.length <= 1
      ? last.knownParticipants.length === 0 || entry.participants.length === 0 || sameParticipants(last.knownParticipants, entry.participants)
      : !last.hasUnknown && sameParticipants(last.knownParticipants, entry.participants));
    const sameBinding = last !== undefined && last.window === entry.window && last.configuration_revision === entry.configuration_revision && last.voice_generation === entry.voice_generation && last.model === entry.model && compatibleEvidence;
    // A separately delivered closing quote or punctuation belongs to its preceding
    // sentence. A decimal split as "20." then "50" must also remain literal.
    const suffix = punctuationContinuation.test(entry.text) || /\d\.$/u.test(last?.text ?? "") && /^\d/u.test(entry.text);
    const sentenceEnded = last?.ending === "punctuation" && !suffix && !abbreviatedPeriod.test(last.text);
    // Punctuation is a reading hint, not a provider final. Nearby sentences can
    // share one progressive paragraph; the soft limit applies only between them.
    const paragraphEnded = sentenceEnded && (last.text.length >= CAPTION_GROUP_LIMITS.paragraphCharacters || last.sentenceBreaks + 1 >= CAPTION_GROUP_LIMITS.paragraphSentences);
    // Uneven provider intervals can straddle words or an unfinished clause.
    // Leading spaces and combining marks are literal text, not a language or
    // turn boundary. A separately delivered mark still belongs to its base text.
    // Keep natural text together for up to eight seconds; punctuation boundaries
    // and bracketed annotations retain the shorter reading-paragraph break.
    const unfinishedContinuation = last !== undefined && !sentenceEnded && !/^\s*[(【[]/u.test(entry.text) && !/[[(][^\])]*$/u.test(last.text)
      && /[\p{L}\p{N}]/u.test(last.text) && /^[\s\p{L}\p{N}\p{M}。、，’'-]/u.test(entry.text);
    const nearby = last !== undefined && entry.start_ms - last.end_ms <= (unfinishedContinuation || suffix ? CAPTION_GROUP_LIMITS.tokenGapMs : CAPTION_GROUP_LIMITS.gapMs);
    const withinLimits = last !== undefined && last.originals.length < CAPTION_GROUP_LIMITS.fragments && last.text.length + entry.text.length <= CAPTION_GROUP_LIMITS.characters && Math.max(last.end_ms, entry.end_ms) - Math.min(last.start_ms, entry.start_ms) <= CAPTION_GROUP_LIMITS.spanMs;
    if (last && sameBinding && !paragraphEnded && withinLimits && nearby) {
      if (sentenceEnded) last.sentenceBreaks++;
      last.text += entry.text;
      last.start_ms = Math.min(last.start_ms, entry.start_ms); last.end_ms = Math.max(last.end_ms, entry.end_ms);
      last.originals.push(entry);
      if (last.knownParticipants.length === 0 && entry.participants.length > 0) last.knownParticipants = entry.participants;
      last.hasUnknown ||= entry.provenance === "gpt_live_input" && entry.participants.length === 0;
      last.partlyUnidentified = last.hasUnknown && last.knownParticipants.length > 0;
      last.participants = last.hasUnknown ? [] : last.knownParticipants;
      last.ending = sentencePunctuation.test(last.text) ? "punctuation" : "open";
      continue;
    }
    const continuation = Boolean(last && sameBinding && !sentenceEnded && !withinLimits && nearby);
    if (last?.ending === "open") last.ending = continuation ? "limit" : "boundary";
    const group: MutableGroup = {
      sequence: entry.sequence, window: entry.window, ...(entry.configuration_revision !== undefined ? {configuration_revision: entry.configuration_revision, voice_generation: entry.voice_generation} : {}), provenance: entry.provenance, model: entry.model,
      text: entry.text, start_ms: entry.start_ms, end_ms: entry.end_ms,
      participants: entry.participants, originals: [entry], ending: sentencePunctuation.test(entry.text) ? "punctuation" : "open",
      continuation, earlierWordsUnavailable: lanesAfterGap.has(entry.provenance) || truncatedStart && !firstLanes.has(entry.provenance), partlyUnidentified: false,
      knownParticipants: entry.participants, hasUnknown: entry.provenance === "gpt_live_input" && entry.participants.length === 0, sentenceBreaks: 0,
    };
    lanesAfterGap.delete(entry.provenance); firstLanes.add(entry.provenance); groups.push(group); lanes.set(entry.provenance, group);
  }
  return groups;
}

/** Exact display equivalence for React's row memo. Shared immutable originals
 * permit an unchanged paragraph to skip rendering; every visible boundary and
 * participant projection still participates in the comparison. No history cache.
 */
export function sameCaptionGroup(left: CaptionGroup, right: CaptionGroup): boolean {
  return left === right || left.sequence === right.sequence && left.window === right.window && left.configuration_revision === right.configuration_revision && left.voice_generation === right.voice_generation && left.provenance === right.provenance && left.model === right.model
    && left.text === right.text && left.start_ms === right.start_ms && left.end_ms === right.end_ms
    && left.ending === right.ending && left.continuation === right.continuation && left.earlierWordsUnavailable === right.earlierWordsUnavailable && left.partlyUnidentified === right.partlyUnidentified
    && sameParticipants(left.participants, right.participants) && left.originals.length === right.originals.length && left.originals.every((entry, index) => entry === right.originals[index]);
}
