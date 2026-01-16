// Defensive URL length limits for remote disk endpoints.
//
// These bounds are primarily intended to avoid pathological CPU/memory behavior when parsing
// attacker-controlled URLs (e.g. `new URL(...)`) or when propagating oversized strings across
// worker boundaries.

export const MAX_REMOTE_URL_LEN = 8 * 1024;
export const MAX_REMOTE_LEASE_ENDPOINT_LEN = 4 * 1024;

