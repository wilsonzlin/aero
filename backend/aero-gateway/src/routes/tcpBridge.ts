import net from "node:net";
import type { Duplex } from "node:stream";

import { socketWritableLengthExceedsCap } from "./socketWritableLength.js";

import { encodeWsClosePayload, encodeWsFrame } from "./wsFrame.js";
import { createGracefulDuplexCloser } from "./wsDuplexClose.js";
import { WsMessageReceiver } from "./wsMessage.js";
import { destroyBestEffort, pauseRequired, resumeRequired, writeCaptureErrorBestEffort } from "./socketSafe.js";

export class WebSocketTcpBridge {
  private readonly wsSocket: Duplex;
  private readonly tcpSocket: net.Socket;
  private readonly maxMessageBytes: number;
  private readonly maxTcpBufferedBytes: number;
  private readonly wsCloser: ReturnType<typeof createGracefulDuplexCloser>;

  private pausedForWsBackpressure = false;
  private closed = false;
  private readonly wsMessages: WsMessageReceiver;

  constructor(
    wsSocket: Duplex,
    tcpSocket: net.Socket,
    opts: Readonly<{ maxMessageBytes: number; maxTcpBufferedBytes: number }>,
  ) {
    this.wsSocket = wsSocket;
    this.tcpSocket = tcpSocket;
    this.maxMessageBytes = opts.maxMessageBytes;
    this.maxTcpBufferedBytes = opts.maxTcpBufferedBytes;
    this.wsCloser = createGracefulDuplexCloser(wsSocket);
    this.wsMessages = new WsMessageReceiver({
      maxMessageBytes: this.maxMessageBytes,
      sendWsFrame: (opcode, payload) => this.sendFrame(opcode, payload),
      onMessage: (opcode, payload) => this.forwardPayload(opcode, payload),
      onClose: () => this.closeGracefully(),
      closeWithProtocolError: () => this.closeWithProtocolError(),
      closeWithMessageTooLarge: () => this.closeWithMessageTooLarge(),
    });
  }

  start(head: Buffer): void {
    if (head.length > 0) this.wsMessages.push(head);

    this.wsSocket.on("data", (data) => {
      this.wsMessages.push(data);
    });
    this.wsSocket.on("error", () => this.destroyNow());
    this.wsSocket.on("close", () => this.destroyNow());
    this.wsSocket.on("end", () => this.destroyNow());
    this.wsSocket.on("drain", () => this.onWsDrain());

    this.tcpSocket.on("data", (data) => {
      this.sendFrame(0x2, data);
    });
    this.tcpSocket.on("error", () => this.destroyNow());
    this.tcpSocket.on("close", () => this.destroyNow());
    this.tcpSocket.on("end", () => this.destroyNow());
  }

  private onWsDrain(): void {
    if (this.closed) return;
    if (!this.pausedForWsBackpressure) return;
    this.pausedForWsBackpressure = false;
    if (!resumeRequired(this.tcpSocket)) {
      this.destroyNow();
    }
  }

  private pauseTcpForWsBackpressure(): void {
    if (this.closed) return;
    if (this.pausedForWsBackpressure) return;
    this.pausedForWsBackpressure = true;
    if (!pauseRequired(this.tcpSocket)) {
      this.destroyNow();
    }
  }

  private forwardPayload(opcode: number, payload: Buffer): void {
    // v1: raw TCP bytes forwarded via binary frames.
    if (opcode === 0x2) {
      const res = writeCaptureErrorBestEffort(this.tcpSocket, payload);
      if (res.err) {
        this.destroyNow();
        return;
      }
      this.enforceTcpBackpressure();
      return;
    }
    this.closeWithUnsupportedData();
  }

  private sendFrame(opcode: number, payload: Buffer): void {
    if (this.closed) return;
    const frame = encodeWsFrame(opcode, payload);
    const res = writeCaptureErrorBestEffort(this.wsSocket, frame);
    if (res.err) {
      this.destroyNow();
      return;
    }
    if (!res.ok) {
      this.pauseTcpForWsBackpressure();
    }
  }

  private closeWithProtocolError(): void {
    // 1002 = protocol error.
    this.sendFrame(0x8, encodeWsClosePayload(1002));
    this.closeGracefully();
  }

  private closeWithMessageTooLarge(): void {
    // 1009 = message too big.
    this.sendFrame(0x8, encodeWsClosePayload(1009));
    this.closeGracefully();
  }

  private closeWithUnsupportedData(): void {
    // 1003 = unsupported data.
    this.sendFrame(0x8, encodeWsClosePayload(1003));
    this.closeGracefully();
  }

  private enforceTcpBackpressure(): void {
    if (!socketWritableLengthExceedsCap(this.tcpSocket, this.maxTcpBufferedBytes)) return;
    // 1011 = internal error (treat runaway buffering as an internal backpressure failure).
    this.sendFrame(0x8, encodeWsClosePayload(1011));
    this.closeGracefully();
  }

  private closeGracefully(): void {
    if (this.closed) return;
    this.closed = true;
    destroyBestEffort(this.tcpSocket);

    // `WsMessageReceiver` writes the close response frame before invoking `onClose()`.
    // Avoid destroying the underlying socket until pending writes have a chance to flush.
    this.wsCloser.endThenDestroy();
  }

  private destroyNow(): void {
    if (this.closed) return;
    this.closed = true;

    this.wsCloser.destroyNow();
    destroyBestEffort(this.tcpSocket);
  }
}

