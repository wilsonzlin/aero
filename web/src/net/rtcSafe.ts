export type RtcDataChannelSendData = string | ArrayBuffer | ArrayBufferView | Blob;

export function dcSendSafe(dc: RTCDataChannel | null | undefined, data: RtcDataChannelSendData): boolean {
  if (!dc || typeof dc.send !== "function") return false;
  if (dc.readyState !== "open") return false;
  try {
    // Some lib.dom versions are overly strict about accepted send() types. Runtime accepts
    // the same shapes we already use (ArrayBuffer, ArrayBufferView, string, Blob).
    (dc.send as (data: unknown) => void)(data);
    return true;
  } catch {
    return false;
  }
}

export function dcCloseSafe(dc: RTCDataChannel | null | undefined): void {
  if (!dc || typeof dc.close !== "function") return;
  try {
    dc.close();
  } catch {
    // ignore
  }
}

export function pcCloseSafe(pc: RTCPeerConnection | null | undefined): void {
  if (!pc || typeof pc.close !== "function") return;
  try {
    pc.close();
  } catch {
    // ignore
  }
}
