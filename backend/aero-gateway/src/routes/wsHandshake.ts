import type { Duplex } from "node:stream";

export { writeWebSocketHandshake } from "../../../../src/ws_handshake_response.js";

// Ensure this file remains a module in TS output.
export type _WsHandshakeDuplex = Duplex;

