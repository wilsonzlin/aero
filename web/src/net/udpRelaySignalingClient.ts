import { WebRtcUdpProxyClient, type UdpProxyEventSink } from "./udpProxy";
import {
  UDP_RELAY_SIGNALING_VERSION,
  parseAnswerResponseJSON,
  parseSignalMessageJSON,
  type Candidate,
  type SessionDescription,
  type SignalMessage,
} from "../shared/udpRelaySignaling";

export type UdpRelaySignalingMode = "ws-trickle" | "http-offer" | "legacy-offer";

export type ConnectUdpRelaySignalingOptions = {
  baseUrl: string;
  authToken?: string;
  mode?: UdpRelaySignalingMode;
};

export async function connectUdpRelaySignaling(
  opts: ConnectUdpRelaySignalingOptions,
): Promise<{ pc: RTCPeerConnection; dc: RTCDataChannel }> {
  const mode = opts.mode ?? "ws-trickle";
  const iceServers = await fetchIceServers(opts.baseUrl, opts.authToken);

  const pc = new RTCPeerConnection({ iceServers });
  const dc = pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 });

  try {
    switch (mode) {
      case "ws-trickle":
        await negotiateWebSocketTrickle(pc, opts.baseUrl, opts.authToken);
        break;
      case "http-offer":
        await negotiateHttpOffer(pc, opts.baseUrl, opts.authToken);
        break;
      case "legacy-offer":
        await negotiateLegacyOffer(pc, opts.baseUrl, opts.authToken);
        break;
      default:
        throw new Error(`unsupported signaling mode: ${String(mode)}`);
    }

    await waitForDataChannelOpen(dc);
    return { pc, dc };
  } catch (err) {
    pc.close();
    throw err;
  }
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
      try {
        dc.close();
      } catch {
        // Ignore.
      }
      pc.close();
    },
  };
}

async function fetchIceServers(baseUrl: string, authToken?: string): Promise<RTCIceServer[]> {
  const url = new URL(baseUrl);
  url.pathname = `${url.pathname.replace(/\/$/, "")}/webrtc/ice`;

  const headers = new Headers();
  if (authToken) headers.set("Authorization", `Bearer ${authToken}`);

  const res = await fetch(url.toString(), { method: "GET", mode: "cors", headers });
  if (!res.ok) {
    throw new Error(`failed to fetch ICE servers (${res.status})`);
  }

  const body: unknown = await res.json();
  if (Array.isArray(body)) return body as RTCIceServer[];
  if (typeof body !== "object" || body === null) return [];

  const iceServers = (body as { iceServers?: unknown }).iceServers;
  if (!Array.isArray(iceServers)) return [];

  // Trust the relay to provide valid RTCIceServer objects.
  return iceServers as RTCIceServer[];
}

function addAuthToUrl(url: URL, authToken?: string): void {
  if (!authToken) return;
  // Forward/compat: different relay builds may expect different keys depending
  // on auth mode (jwt vs api_key). Supplying both is harmless.
  url.searchParams.set("token", authToken);
  url.searchParams.set("apiKey", authToken);
}

function addAuthHeader(headers: Headers, authToken?: string): void {
  if (!authToken) return;
  headers.set("Authorization", `Bearer ${authToken}`);
}

function toWebSocketUrl(baseUrl: string, path: string, authToken?: string): URL {
  const url = new URL(baseUrl);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.pathname = `${url.pathname.replace(/\/$/, "")}${path}`;
  addAuthToUrl(url, authToken);
  return url;
}

function toHttpUrl(baseUrl: string, path: string): URL {
  const url = new URL(baseUrl);
  url.pathname = `${url.pathname.replace(/\/$/, "")}${path}`;
  return url;
}

function waitForIceGatheringComplete(pc: RTCPeerConnection): Promise<void> {
  if (pc.iceGatheringState === "complete") return Promise.resolve();

  return new Promise((resolve) => {
    const onChange = () => {
      if (pc.iceGatheringState !== "complete") return;
      pc.removeEventListener("icegatheringstatechange", onChange);
      resolve();
    };
    pc.addEventListener("icegatheringstatechange", onChange);
  });
}

function waitForDataChannelOpen(dc: RTCDataChannel): Promise<void> {
  if (dc.readyState === "open") return Promise.resolve();
  if (dc.readyState === "closed") return Promise.reject(new Error("data channel closed"));

  return new Promise((resolve, reject) => {
    const onOpen = () => {
      cleanup();
      resolve();
    };
    const onClose = () => {
      cleanup();
      reject(new Error("data channel closed"));
    };
    const onError = () => {
      cleanup();
      reject(new Error("data channel error"));
    };
    const cleanup = () => {
      dc.removeEventListener("open", onOpen);
      dc.removeEventListener("close", onClose);
      dc.removeEventListener("error", onError);
    };

    dc.addEventListener("open", onOpen);
    dc.addEventListener("close", onClose);
    dc.addEventListener("error", onError);
  });
}

async function negotiateHttpOffer(pc: RTCPeerConnection, baseUrl: string, authToken?: string): Promise<void> {
  const offer = await pc.createOffer();
  await pc.setLocalDescription(offer);
  await waitForIceGatheringComplete(pc);

  const local = pc.localDescription;
  if (!local?.sdp) throw new Error("missing local description after setting offer");

  const url = toHttpUrl(baseUrl, "/webrtc/offer");
  const headers = new Headers({ "Content-Type": "application/json" });
  addAuthHeader(headers, authToken);

  const res = await fetch(url.toString(), {
    method: "POST",
    mode: "cors",
    headers,
    body: JSON.stringify({ sdp: { type: "offer", sdp: local.sdp } satisfies SessionDescription }),
  });
  const text = await res.text();
  if (!res.ok) {
    throw new Error(`webrtc offer failed (${res.status}): ${text}`);
  }

  const data: unknown = JSON.parse(text);
  const sdp = (data as { sdp?: unknown }).sdp;
  if (typeof sdp !== "object" || sdp === null) {
    throw new Error("invalid /webrtc/offer response: missing sdp");
  }
  const answer = sdp as SessionDescription;
  if (answer.type !== "answer" || typeof answer.sdp !== "string" || answer.sdp.length === 0) {
    throw new Error("invalid /webrtc/offer response: invalid answer SDP");
  }

  await pc.setRemoteDescription(answer);
}

async function negotiateLegacyOffer(pc: RTCPeerConnection, baseUrl: string, authToken?: string): Promise<void> {
  const offer = await pc.createOffer();
  await pc.setLocalDescription(offer);
  await waitForIceGatheringComplete(pc);

  const local = pc.localDescription;
  if (!local?.sdp) throw new Error("missing local description after setting offer");

  const url = toHttpUrl(baseUrl, "/offer");
  const headers = new Headers({ "Content-Type": "application/json" });
  addAuthHeader(headers, authToken);

  const res = await fetch(url.toString(), {
    method: "POST",
    mode: "cors",
    headers,
    body: JSON.stringify({
      version: UDP_RELAY_SIGNALING_VERSION,
      offer: { type: "offer", sdp: local.sdp } satisfies SessionDescription,
    }),
  });
  const text = await res.text();
  if (!res.ok) {
    throw new Error(`legacy offer failed (${res.status}): ${text}`);
  }

  const answer = parseAnswerResponseJSON(text).answer;
  await pc.setRemoteDescription(answer);
}

async function openWebSocket(url: string, protocol?: string): Promise<WebSocket> {
  return await new Promise((resolve, reject) => {
    let ws: WebSocket;
    try {
      ws = protocol ? new WebSocket(url, protocol) : new WebSocket(url);
    } catch (err) {
      reject(err);
      return;
    }

    const onOpen = () => {
      cleanup();
      resolve(ws);
    };
    const onClose = (evt: CloseEvent) => {
      cleanup();
      reject(new Error(`websocket closed (${evt.code}): ${evt.reason}`));
    };
    const onError = () => {
      cleanup();
      reject(new Error("websocket error"));
    };
    const cleanup = () => {
      ws.removeEventListener("open", onOpen);
      ws.removeEventListener("close", onClose);
      ws.removeEventListener("error", onError);
    };

    ws.addEventListener("open", onOpen);
    ws.addEventListener("close", onClose);
    ws.addEventListener("error", onError);
  });
}

function sendSignal(ws: WebSocket, msg: SignalMessage | (SignalMessage & { apiKey?: string })): void {
  ws.send(JSON.stringify(msg));
}

function sendAuth(ws: WebSocket, authToken: string): void {
  // Forward/compat: different relay builds may accept either {token} or {apiKey}
  // depending on auth mode. Supplying both allows the client to remain agnostic.
  ws.send(JSON.stringify({ type: "auth", token: authToken, apiKey: authToken }));
}

async function negotiateWebSocketTrickle(pc: RTCPeerConnection, baseUrl: string, authToken?: string): Promise<void> {
  const wsUrl = toWebSocketUrl(baseUrl, "/webrtc/signal", authToken).toString();

  // Candidate gathering can start immediately after SetLocalDescription; collect
  // all local candidates so we can re-send them if we have to reconnect the
  // signaling WebSocket (e.g. auth message required).
  const localCandidates: Candidate[] = [];

  let activeWs: WebSocket | null = null;
  let offerSent = false;
  let remoteDescriptionSet = false;
  let trickleEnabled = true;

  const remoteCandidateBuffer: Candidate[] = [];
  let currentAttempt: symbol | null = null;

  const onIceCandidate = (evt: RTCPeerConnectionIceEvent) => {
    if (!evt.candidate) return;
    const cand = evt.candidate.toJSON() as Candidate;
    localCandidates.push(cand);
    if (!trickleEnabled || !offerSent || !activeWs || activeWs.readyState !== WebSocket.OPEN) return;
    sendSignal(activeWs, { type: "candidate", candidate: cand });
  };
  pc.addEventListener("icecandidate", onIceCandidate);

  const closeActiveWs = () => {
    currentAttempt = null;
    if (!activeWs) return;
    try {
      activeWs.close();
    } catch {
      // Ignore.
    }
    activeWs = null;
  };

  const onPcState = () => {
    switch (pc.connectionState) {
      case "closed":
      case "failed":
        closeActiveWs();
        pc.removeEventListener("connectionstatechange", onPcState);
        pc.removeEventListener("icecandidate", onIceCandidate);
        break;
    }
  };
  pc.addEventListener("connectionstatechange", onPcState);

  const offer = await pc.createOffer();
  await pc.setLocalDescription(offer);
  const local = pc.localDescription;
  if (!local?.sdp) throw new Error("missing local description after setting offer");

  const protocols: Array<string | undefined> = authToken ? [undefined, authToken] : [undefined];
  const authFirstVariants: boolean[] = authToken ? [false, true] : [false];

  let lastErr: unknown = null;

  const runTyped = async (protocol: string | undefined, authFirst: boolean): Promise<void> => {
    closeActiveWs();
    offerSent = false;
    trickleEnabled = true;
    remoteDescriptionSet = false;
    remoteCandidateBuffer.length = 0;

    const ws = await openWebSocket(wsUrl, protocol);
    activeWs = ws;
    const attemptId = Symbol("udp-relay-ws-typed");
    currentAttempt = attemptId;

    let haveAnswer = false;

    let resolveAnswer!: () => void;
    let rejectAnswer!: (err: unknown) => void;
    const answerPromise = new Promise<void>((resolve, reject) => {
      resolveAnswer = resolve;
      rejectAnswer = reject;
    });
    let answerSettled = false;
    const settleAnswer = (err?: unknown) => {
      if (answerSettled) return;
      answerSettled = true;
      if (err) {
        rejectAnswer(err);
      } else {
        resolveAnswer();
      }
    };

    const onMessage = async (evt: MessageEvent) => {
      if (currentAttempt !== attemptId) return;
      if (typeof evt.data !== "string") return;

      let msg: SignalMessage;
      try {
        msg = parseSignalMessageJSON(evt.data);
      } catch (err) {
        if (!haveAnswer) settleAnswer(err);
        return;
      }

      switch (msg.type) {
        case "answer":
          if (haveAnswer) return;
          try {
            await pc.setRemoteDescription(msg.sdp);
            haveAnswer = true;
            remoteDescriptionSet = true;
            for (const cand of remoteCandidateBuffer) {
              if (cand.candidate === "") continue;
              await pc.addIceCandidate(cand);
            }
            remoteCandidateBuffer.length = 0;
            settleAnswer();
          } catch (err) {
            settleAnswer(err);
          }
          break;
        case "candidate":
          if (msg.candidate.candidate === "") return;
          if (!remoteDescriptionSet) {
            remoteCandidateBuffer.push(msg.candidate);
            return;
          }
          try {
            await pc.addIceCandidate(msg.candidate);
          } catch (err) {
            if (!haveAnswer) settleAnswer(err);
            else pc.close();
          }
          break;
        case "error":
          if (!haveAnswer) {
            settleAnswer(new Error(`signaling error (${msg.code}): ${msg.message}`));
          } else {
            pc.close();
          }
          break;
        case "close":
          if (!haveAnswer) {
            settleAnswer(new Error("signaling closed"));
          } else {
            pc.close();
          }
          break;
        case "auth":
          // Ignore server echoes.
          break;
        case "offer":
          if (!haveAnswer) settleAnswer(new Error("unexpected offer from server"));
          else pc.close();
          break;
      }
    };

    const onClose = (evt: CloseEvent) => {
      if (currentAttempt !== attemptId) return;
      if (!haveAnswer) {
        settleAnswer(new Error(`signaling websocket closed (${evt.code}): ${evt.reason}`));
        return;
      }
      pc.close();
    };

    const onError = () => {
      if (currentAttempt !== attemptId) return;
      if (!haveAnswer) {
        settleAnswer(new Error("signaling websocket error"));
        return;
      }
      pc.close();
    };

    ws.addEventListener("message", onMessage);
    ws.addEventListener("close", onClose);
    ws.addEventListener("error", onError);

    if (authFirst && authToken) {
      sendAuth(ws, authToken);
    }

    const offerMsg: SignalMessage = {
      type: "offer",
      sdp: { type: "offer", sdp: pc.localDescription?.sdp ?? local.sdp },
    };
    sendSignal(ws, offerMsg);
    offerSent = true;

    for (const cand of localCandidates) {
      sendSignal(ws, { type: "candidate", candidate: cand });
    }

    await answerPromise;
  };

  for (const protocol of protocols) {
    for (const authFirst of authFirstVariants) {
      try {
        await runTyped(protocol, authFirst);
        return;
      } catch (err) {
        lastErr = err;
        closeActiveWs();
      }
    }
  }

  // Legacy fallback: some relay builds (and older tests) use versioned offer/answer
  // JSON on /webrtc/signal without trickle ICE. In that mode, candidates must be
  // embedded in the SDP.
  await waitForIceGatheringComplete(pc);

  const runLegacy = async (protocol: string | undefined, authFirst: boolean): Promise<void> => {
    closeActiveWs();
    offerSent = false;
    trickleEnabled = false;
    remoteDescriptionSet = false;
    remoteCandidateBuffer.length = 0;

    const ws = await openWebSocket(wsUrl, protocol);
    activeWs = ws;
    const attemptId = Symbol("udp-relay-ws-legacy");
    currentAttempt = attemptId;

    let haveAnswer = false;

    let resolveAnswer!: () => void;
    let rejectAnswer!: (err: unknown) => void;
    const answerPromise = new Promise<void>((resolve, reject) => {
      resolveAnswer = resolve;
      rejectAnswer = reject;
    });
    let answerSettled = false;
    const settleAnswer = (err?: unknown) => {
      if (answerSettled) return;
      answerSettled = true;
      if (err) {
        rejectAnswer(err);
      } else {
        resolveAnswer();
      }
    };

    const onMessage = async (evt: MessageEvent) => {
      if (currentAttempt !== attemptId) return;
      if (typeof evt.data !== "string") return;
      if (haveAnswer) return;

      try {
        const answer = parseAnswerResponseJSON(evt.data).answer;
        await pc.setRemoteDescription(answer);
        haveAnswer = true;
        remoteDescriptionSet = true;
        settleAnswer();
      } catch (err) {
        settleAnswer(err);
      }
    };

    const onClose = (evt: CloseEvent) => {
      if (currentAttempt !== attemptId) return;
      if (!haveAnswer) {
        settleAnswer(new Error(`signaling websocket closed (${evt.code}): ${evt.reason}`));
        return;
      }
      pc.close();
    };

    const onError = () => {
      if (currentAttempt !== attemptId) return;
      if (!haveAnswer) {
        settleAnswer(new Error("signaling websocket error"));
        return;
      }
      pc.close();
    };

    ws.addEventListener("message", onMessage);
    ws.addEventListener("close", onClose);
    ws.addEventListener("error", onError);

    if (authFirst && authToken) {
      sendAuth(ws, authToken);
    }

    const fullOffer = pc.localDescription;
    if (!fullOffer?.sdp) throw new Error("missing offer SDP after ICE gathering");
    ws.send(
      JSON.stringify({
        version: UDP_RELAY_SIGNALING_VERSION,
        offer: { type: "offer", sdp: fullOffer.sdp } satisfies SessionDescription,
      }),
    );
    offerSent = true;

    await answerPromise;
  };

  for (const protocol of protocols) {
    for (const authFirst of authFirstVariants) {
      try {
        await runLegacy(protocol, authFirst);
        return;
      } catch (err) {
        lastErr = err;
        closeActiveWs();
      }
    }
  }

  throw lastErr instanceof Error ? lastErr : new Error("failed to establish signaling websocket");
}
