const DEFAULT_ORIGINS = Object.freeze({operator: "http://127.0.0.1:5180", public: "http://127.0.0.1:5182"});
const ALTERNATE_ORIGINS = Object.freeze({operator: "http://127.0.0.1:5080", public: "http://127.0.0.1:5082"});

/** Select only a reviewed pair from the browser's own origin, never a URL parameter. */
export function pilotOrigins(browserOrigin: string | undefined): Readonly<{operator: string; public: string}> {
  if (browserOrigin === ALTERNATE_ORIGINS.operator || browserOrigin === ALTERNATE_ORIGINS.public) return ALTERNATE_ORIGINS;
  // Unknown origins still reach takeEntry, which scrubs the fragment before
  // rejecting them. Throwing while importing would leave an access link visible.
  return DEFAULT_ORIGINS;
}
