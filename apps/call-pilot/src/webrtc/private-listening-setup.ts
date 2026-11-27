import {parseInterpretation, type Interpretation, type Language} from '../protocol.js';

export type PrivateLanguageSelection = Readonly<{mode: Interpretation['mode']; target: Language; first: Language; second: Language}>;

/** The setup form creates the existing closed contract. The controller makes
 * its own immutable copy before claiming audio or opening a permission picker. */
export function privateListeningPolicy(selection: PrivateLanguageSelection): Interpretation {
  if (selection.mode === 'two_way') return parseInterpretation({schema_version: 'interpretation.v1', mode: selection.mode, first: selection.first, second: selection.second});
  if (selection.mode === 'auto_to_target') return parseInterpretation({schema_version: 'interpretation.v1', mode: selection.mode, target: selection.target});
  throw new Error('Unsupported private interpretation mode');
}
