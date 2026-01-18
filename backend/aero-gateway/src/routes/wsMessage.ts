import { concat2, tryReadWsFrame, type WsFrame } from "./wsFrame.js";

export type WsMessageHandler = (opcode: number, payload: Buffer) => void;

export type WsMessageReceiverOptions = Readonly<{
  maxMessageBytes: number;
  onMessage: WsMessageHandler;
  onClose: () => void;
  sendWsFrame: (opcode: number, payload: Buffer) => void;
  closeWithProtocolError: () => void;
  closeWithMessageTooLarge: () => void;
}>;

export class WsMessageReceiver {
  private readonly opts: WsMessageReceiverOptions;

  private buffer: Buffer = Buffer.alloc(0);

  private fragmentedOpcode: number | null = null;
  private fragmentedChunks: Buffer[] = [];
  private fragmentedBytes = 0;

  private closed = false;

  constructor(opts: WsMessageReceiverOptions) {
    this.opts = opts;
  }

  push(data: Buffer): void {
    if (this.closed) return;
    this.buffer = this.buffer.length === 0 ? data : concat2(this.buffer, data);
    this.drain();
  }

  private drain(): void {
    while (!this.closed) {
      const parsed = tryReadWsFrame(this.buffer, this.opts.maxMessageBytes);
      if (!parsed) return;
      this.buffer = parsed.remaining;
      this.handleFrame(parsed.frame);
    }
  }

  private handleFrame(frame: WsFrame): void {
    // RFC 6455: control frames (close/ping/pong) must not be fragmented and must have payload <= 125 bytes.
    if ((frame.opcode === 0x8 || frame.opcode === 0x9 || frame.opcode === 0xA) && (!frame.fin || frame.payload.length > 125)) {
      this.closed = true;
      this.opts.closeWithProtocolError();
      return;
    }

    switch (frame.opcode) {
      case 0x0: {
        // Continuation
        if (this.fragmentedOpcode === null) {
          this.closed = true;
          this.opts.closeWithProtocolError();
          return;
        }
        this.fragmentedChunks.push(frame.payload);
        this.fragmentedBytes += frame.payload.length;
        if (this.fragmentedBytes > this.opts.maxMessageBytes) {
          this.closed = true;
          this.opts.closeWithMessageTooLarge();
          return;
        }
        if (frame.fin) {
          const payload = Buffer.concat(this.fragmentedChunks, this.fragmentedBytes);
          const opcode = this.fragmentedOpcode;
          this.fragmentedOpcode = null;
          this.fragmentedChunks = [];
          this.fragmentedBytes = 0;
          this.opts.onMessage(opcode, payload);
        }
        return;
      }
      case 0x1:
      case 0x2: {
        // Text / Binary
        if (this.fragmentedOpcode !== null) {
          this.closed = true;
          this.opts.closeWithProtocolError();
          return;
        }
        if (frame.fin) {
          this.opts.onMessage(frame.opcode, frame.payload);
          return;
        }
        this.fragmentedOpcode = frame.opcode;
        this.fragmentedChunks = [frame.payload];
        this.fragmentedBytes = frame.payload.length;
        if (this.fragmentedBytes > this.opts.maxMessageBytes) {
          this.closed = true;
          this.opts.closeWithMessageTooLarge();
          return;
        }
        return;
      }
      case 0x8: {
        // Close
        this.closed = true;
        this.opts.sendWsFrame(0x8, frame.payload);
        this.opts.onClose();
        return;
      }
      case 0x9: {
        // Ping
        this.opts.sendWsFrame(0xA, frame.payload);
        return;
      }
      case 0xA: {
        // Pong
        return;
      }
      default: {
        this.closed = true;
        this.opts.closeWithProtocolError();
      }
    }
  }
}

