const app = document.querySelector<HTMLDivElement>('#app');
if (!app) throw new Error('Missing #app element');

const status = {
  crossOriginIsolated: globalThis.crossOriginIsolated,
  sharedArrayBuffer: typeof SharedArrayBuffer !== 'undefined',
  atomics: typeof Atomics !== 'undefined',
};

app.innerHTML = `
  <h1>Aero</h1>
  <p>Cross-origin isolation status (required for WASM threads / SharedArrayBuffer):</p>
  <pre>${JSON.stringify(status, null, 2)}</pre>
`;

