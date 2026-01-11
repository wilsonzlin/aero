import { describe, expect, it } from "vitest";

import { L2_TUNNEL_TYPE_FRAME, L2_TUNNEL_TYPE_PONG, decodeL2Message, encodeL2Frame, encodePing } from "../shared/l2TunnelProtocol.ts";
import { WebRtcL2TunnelClient } from "./l2Tunnel.ts";

function microtask(): Promise<void> {
  return new Promise((resolve) => queueMicrotask(resolve));
}

class FakeRtcDataChannel {
  binaryType: BinaryType = "arraybuffer";
  bufferedAmount = 0;
  bufferedAmountLowThreshold = 0;
  readyState: RTCDataChannelState = "open";

  onopen: ((ev: Event) => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onclose: ((ev: Event) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onbufferedamountlow: ((ev: Event) => void) | null = null;

  readonly sent: Uint8Array[] = [];

  send(data: string | Blob | ArrayBuffer | ArrayBufferView): void {
    if (typeof data === "string") throw new Error("unexpected string send");
    if (data instanceof Blob) throw new Error("unexpected Blob send");
    const view = data instanceof ArrayBuffer ? new Uint8Array(data) : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    // Copy to avoid aliasing mutations.
    this.sent.push(view.slice());
  }

  close(): void {
    this.readyState = "closed";
    this.onclose?.(new Event("close"));
  }

  emitMessage(payload: Uint8Array): void {
    const buf = payload.buffer.slice(payload.byteOffset, payload.byteOffset + payload.byteLength);
    this.onmessage?.({ data: buf } as MessageEvent);
  }
}

describe("net/l2Tunnel", () => {
  it("forwards FRAME messages and responds to PING", async () => {
    const channel = new FakeRtcDataChannel();
    const events: unknown[] = [];

    const client = new WebRtcL2TunnelClient(channel as unknown as RTCDataChannel, (ev) => events.push(ev), {
      // Keepalive timers would keep the vitest process alive; we close at the end
      // of the test, but set a large interval to avoid flakiness if close breaks.
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    try {
      await microtask(); // allow constructor-scheduled `open`
      expect(events[0]?.type).toBe("open");

      const frame = Uint8Array.of(1, 2, 3, 4);
      client.sendFrame(frame);
      await microtask(); // flush outbound queue

      expect(channel.sent.length).toBe(1);
      const out = decodeL2Message(channel.sent[0]!);
      expect(out.type).toBe(L2_TUNNEL_TYPE_FRAME);
      expect(Array.from(out.payload)).toEqual(Array.from(frame));

      const inboundFrame = Uint8Array.of(9, 8, 7);
      channel.emitMessage(encodeL2Frame(inboundFrame));
      const frameEvent = events.find((e) => (e as { type?: string }).type === "frame") as { frame?: Uint8Array } | undefined;
      expect(frameEvent?.frame && Array.from(frameEvent.frame)).toEqual(Array.from(inboundFrame));

      const pingPayload = new Uint8Array(4);
      new DataView(pingPayload.buffer).setUint32(0, 123, false);
      channel.emitMessage(encodePing(pingPayload));
      await microtask(); // flush pong response

      expect(channel.sent.length).toBe(2);
      const pong = decodeL2Message(channel.sent[1]!);
      expect(pong.type).toBe(L2_TUNNEL_TYPE_PONG);
      expect(Array.from(pong.payload)).toEqual(Array.from(pingPayload));
    } finally {
      client.close();
    }
  });
});
