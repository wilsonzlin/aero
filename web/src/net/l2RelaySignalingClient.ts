import {
  L2_TUNNEL_DATA_CHANNEL_LABEL,
  WebRtcL2TunnelClient,
  assertL2TunnelDataChannelSemantics,
  createL2TunnelDataChannel,
  type L2TunnelClientOptions,
  type L2TunnelSink,
} from "./l2Tunnel";
import { connectRelaySignaling, type ConnectRelaySignalingOptions } from "./webrtcRelaySignalingClient";

export type ConnectL2RelaySignalingOptions = ConnectRelaySignalingOptions & {
  /**
   * Advanced: additional RTCDataChannel init options. The helper enforces the
   * L2 tunnel's reliability + ordering policy (see below).
   */
  l2ChannelOptions?: RTCDataChannelInit;
};

// `docs/adr/0013-networking-l2-tunnel.md` + `docs/l2-tunnel-protocol.md` require
// the L2 tunnel DataChannel to be reliable (no partial reliability settings).
// Unordered delivery is permitted; our current project policy is unordered
// reliable delivery (see `createL2TunnelDataChannel`).
function createL2DataChannel(pc: RTCPeerConnection, opts?: RTCDataChannelInit): RTCDataChannel {
  if (!opts) return createL2TunnelDataChannel(pc);
  if (opts.maxRetransmits !== undefined || opts.maxPacketLifeTime !== undefined) {
    throw new Error("L2 relay DataChannel must be reliable (do not set maxRetransmits/maxPacketLifeTime)");
  }
  const channel = pc.createDataChannel(L2_TUNNEL_DATA_CHANNEL_LABEL, { ...opts, ordered: false });
  assertL2TunnelDataChannelSemantics(channel);
  return channel;
}

export async function connectL2RelaySignaling(
  opts: ConnectL2RelaySignalingOptions,
): Promise<{ pc: RTCPeerConnection; dc: RTCDataChannel }> {
  return await connectRelaySignaling(opts, (pc) => createL2DataChannel(pc, opts.l2ChannelOptions));
}

export type ConnectL2RelayOptions = ConnectL2RelaySignalingOptions & {
  sink: L2TunnelSink;
  tunnelOptions?: L2TunnelClientOptions;
};

export async function connectL2Relay(
  opts: ConnectL2RelayOptions,
): Promise<{ l2: WebRtcL2TunnelClient; pc: RTCPeerConnection; close: () => void }> {
  const { pc, dc } = await connectL2RelaySignaling(opts);
  const l2 = new WebRtcL2TunnelClient(dc, opts.sink, opts.tunnelOptions);
  return {
    l2,
    pc,
    close: () => {
      try {
        l2.close();
      } catch {
        // Ignore.
      }
      pc.close();
    },
  };
}
