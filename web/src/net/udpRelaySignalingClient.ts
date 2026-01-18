import { WebRtcUdpProxyClient, type UdpProxyEventSink } from "./udpProxy";
import {
  connectRelaySignaling,
  type ConnectRelaySignalingOptions,
  type RelaySignalingMode,
} from "./webrtcRelaySignalingClient";
import { dcCloseSafe, pcCloseSafe } from "./rtcSafe";

export type UdpRelaySignalingMode = RelaySignalingMode;

export type ConnectUdpRelaySignalingOptions = ConnectRelaySignalingOptions;

export async function connectUdpRelaySignaling(
  opts: ConnectUdpRelaySignalingOptions,
): Promise<{ pc: RTCPeerConnection; dc: RTCDataChannel }> {
  return await connectRelaySignaling(opts, (pc) => pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 }));
}

export type ConnectUdpRelayOptions = ConnectUdpRelaySignalingOptions & {
  sink: UdpProxyEventSink;
};

export async function connectUdpRelay(
  opts: ConnectUdpRelayOptions,
): Promise<{ udp: WebRtcUdpProxyClient; pc: RTCPeerConnection; close: () => void }> {
  const { pc, dc } = await connectUdpRelaySignaling(opts);
  const udp = new WebRtcUdpProxyClient(dc, opts.sink);
  return {
    udp,
    pc,
    close: () => {
      dcCloseSafe(dc);
      pcCloseSafe(pc);
    },
  };
}
