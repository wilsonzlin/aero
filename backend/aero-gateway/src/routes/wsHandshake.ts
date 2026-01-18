import type { Duplex } from "node:stream";

export { writeWebSocketHandshake } from "./wsHandshakeResponse.js";

// Ensure this file remains a module in TS output.
export type _WsHandshakeDuplex = Duplex;

