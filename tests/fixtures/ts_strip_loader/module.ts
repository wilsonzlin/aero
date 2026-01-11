export const answer = 42;

// Mirror the pattern used by AudioWorklet/worker modules: a default export that
// remains usable when imported through Vite-style `?worker&url` specifiers.
export default import.meta.url;
