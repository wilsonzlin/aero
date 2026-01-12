// Test environment setup for Node-based unit tests.
//
// The repository's tests are written against the browser WebSocket API
// (`new WebSocket(...)`, `addEventListener`, etc). Node provides a built-in
// WebSocket implementation via undici, but newer Node versions can change
// behaviour and resource usage characteristics.
//
// For deterministic unit tests (and to avoid tying test stability to the
// Node/undici WebSocket implementation), we force the global WebSocket
// constructor to the `ws` library implementation.
import WebSocket from "ws";

// eslint-disable-next-line no-global-assign
globalThis.WebSocket = WebSocket;

