import net from "node:net";
import type { Duplex } from "node:stream";

import { encodeWsClosePayload, encodeWsFrame } from "./wsFrame.js";
import { WsMessageReceiver } from "./wsMessage.js";

export class WebSocketTcpBridge {
  private readonly wsSocket: Duplex;
  private readonly tcpSocket: net.Socket;
  private readonly maxMessageBytes: number;

  private closed = false;
  private readonly wsMessages: WsMessageReceiver;

  constructor(wsSocket: Duplex, tcpSocket: net.Socket, maxMessageBytes: number) {
    this.wsSocket = wsSocket;
    this.tcpSocket = tcpSocket;
    this.maxMessageBytes = maxMessageBytes;
    this.wsMessages = new WsMessageReceiver({
      maxMessageBytes,
      sendWsFrame: (opcode, payload) => this.sendFrame(opcode, payload),
      onMessage: (opcode, payload) => this.forwardPayload(opcode, payload),
      onClose: () => this.close(),
      closeWithProtocolError: () => this.closeWithProtocolError(),
      closeWithMessageTooLarge: () => this.closeWithMessageTooLarge(),
    });
  }

  start(head: Buffer): void {
    if (head.length > 0) this.wsMessages.push(head);

    this.wsSocket.on("data", (data) => {
      this.wsMessages.push(data);
    });
    this.wsSocket.on("error", () => this.close());
    this.wsSocket.on("close", () => this.close());
    this.wsSocket.on("end", () => this.close());

    this.tcpSocket.on("data", (data) => {
      this.sendFrame(0x2, data);
    });
    this.tcpSocket.on("error", () => this.close());
    this.tcpSocket.on("close", () => this.close());
    this.tcpSocket.on("end", () => this.close());
  }

  private forwardPayload(opcode: number, payload: Buffer): void {
    // v1: raw TCP bytes forwarded via binary frames.
    if (opcode === 0x1) {
      // Text frames are permitted by WebSocket, but Aero's TCP tunnel is binary.
      // Still forward the raw UTF-8 bytes to avoid surprising behaviour.
      this.tcpSocket.write(payload);
      return;
    }
    if (opcode === 0x2) {
      this.tcpSocket.write(payload);
      return;
    }
    this.closeWithProtocolError();
  }

  private sendFrame(opcode: number, payload: Buffer): void {
    if (this.closed) return;
    const frame = encodeWsFrame(opcode, payload);
    this.wsSocket.write(frame);
  }

  private closeWithProtocolError(): void {
    // 1002 = protocol error.
    this.sendFrame(0x8, encodeWsClosePayload(1002));
    this.close();
  }

  private closeWithMessageTooLarge(): void {
    // 1009 = message too big.
    this.sendFrame(0x8, encodeWsClosePayload(1009));
    this.close();
  }

  private close(): void {
    if (this.closed) return;
    this.closed = true;

    this.wsSocket.destroy();
    this.tcpSocket.destroy();
  }
}

