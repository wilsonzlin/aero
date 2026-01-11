import { describe, expect, it } from "vitest";

import { createUsbProxyRingBuffer, UsbProxyRing } from "./usb_proxy_ring";
import { subscribeUsbProxyCompletionRing } from "./usb_proxy_ring_dispatcher";

describe("usb/usb_proxy_ring_dispatcher", () => {
  it("does not steal completions during re-subscribe with SharedArrayBuffer wrapper clones", async () => {
    const sab = createUsbProxyRingBuffer(256);
    const sabClone = structuredClone(sab);

    const seenA: number[] = [];
    const seenB: number[] = [];

    let unsubscribeA = subscribeUsbProxyCompletionRing(sab, (c) => seenA.push(c.id));
    let unsubscribeB = subscribeUsbProxyCompletionRing(sab, (c) => seenB.push(c.id));

    // Flush the initial microtask drains (ring is empty).
    await Promise.resolve();

    // Simulate a completion arriving while a `usb.ringAttach` resend is in flight:
    // the ring is populated first, then subscribers detach/reattach sequentially.
    const ring = new UsbProxyRing(sab);
    expect(ring.pushCompletion({ kind: "bulkIn", id: 42, status: "success", data: Uint8Array.of(1) })).toBe(true);

    // First runtime processes the new `usb.ringAttach` payload and re-subscribes using the cloned SAB wrapper.
    unsubscribeA();
    unsubscribeA = subscribeUsbProxyCompletionRing(sabClone, (c) => seenA.push(c.id));

    // Second runtime processes the same event next and re-subscribes.
    unsubscribeB();
    unsubscribeB = subscribeUsbProxyCompletionRing(sabClone, (c) => seenB.push(c.id));

    // The completion should be delivered to both subscribers (postMessage-like fanout).
    await Promise.resolve();

    expect(seenA).toEqual([42]);
    expect(seenB).toEqual([42]);

    unsubscribeA();
    unsubscribeB();
  });
});

