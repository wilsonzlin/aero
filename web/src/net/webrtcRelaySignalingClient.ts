import {
  UDP_RELAY_SIGNALING_VERSION,
  parseAnswerResponseJSON,
  parseSignalMessageJSON,
  type Candidate,
  type SessionDescription,
  type SignalMessage,
} from "../shared/udpRelaySignaling";
import { readJsonResponseWithLimit, readTextResponseWithLimit } from "../storage/response_json";
import { unrefBestEffort } from "../unrefSafe";
import { dcIsClosedSafe, dcIsOpenSafe, pcCloseSafe } from "./rtcSafe";
import { wsCloseSafe, wsIsOpenSafe, wsSendSafe } from "./wsSafe.ts";

export type RelaySignalingMode = "ws-trickle" | "http-offer" | "legacy-offer";

export type ConnectRelaySignalingOptions = {
  baseUrl: string;
  authToken?: string;
  mode?: RelaySignalingMode;
};

const DEFAULT_ICE_GATHER_TIMEOUT_MS = 10_000;
const DEFAULT_DATA_CHANNEL_OPEN_TIMEOUT_MS = 30_000;
const DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS = 10_000;
const DEFAULT_HTTP_TIMEOUT_MS = 20_000;
const DEFAULT_SIGNALING_ANSWER_TIMEOUT_MS = 10_000;

// WebRTC signaling endpoints should return small JSON payloads. Cap response size to avoid
// pathological allocations if a relay/gateway is misconfigured or attacker-controlled.
const MAX_SIGNALING_RESPONSE_BYTES = 1024 * 1024; // 1 MiB

function sanitizeSignalingErrorCode(code: string): string {
  const trimmed = code.trim();
  if (!trimmed) return "unknown";
  // Keep error strings short and token-ish to avoid reflecting arbitrary server text.
  const safe = trimmed.replace(/[^A-Za-z0-9._-]/g, "_");
  return safe.length > 64 ? safe.slice(0, 64) : safe;
}

export async function connectRelaySignaling(
  opts: ConnectRelaySignalingOptions,
  createDataChannel: (pc: RTCPeerConnection) => RTCDataChannel,
): Promise<{ pc: RTCPeerConnection; dc: RTCDataChannel }> {
  const mode = opts.mode ?? "ws-trickle";
  const iceServers = await fetchIceServers(opts.baseUrl, opts.authToken);

  const pc = new RTCPeerConnection({ iceServers });
  let dc: RTCDataChannel;

  try {
    dc = createDataChannel(pc);

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
    pcCloseSafe(pc);
    throw err;
  }
}

const isAbortError = (err: unknown): boolean => err instanceof Error && err.name === "AbortError";

async function fetchWithTimeout(input: string, init: RequestInit, timeoutMs: number): Promise<Response> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  unrefBestEffort(timer);
  try {
    return await fetch(input, { ...init, signal: controller.signal });
  } finally {
    clearTimeout(timer);
  }
}

async function fetchIceServers(baseUrl: string, authToken?: string): Promise<RTCIceServer[]> {
  const url = toHttpUrl(baseUrl, "/webrtc/ice");

  const headers = new Headers();
  addAuthHeader(headers, authToken);

  let res: Response;
  try {
    // Treat ICE discovery as non-cacheable even on the client side. The relay
    // already returns `Cache-Control: no-store`, but setting this option makes
    // the intent explicit and avoids relying on intermediaries preserving
    // response headers.
    res = await fetchWithTimeout(
      url.toString(),
      { method: "GET", mode: "cors", headers, cache: "no-store" },
      DEFAULT_HTTP_TIMEOUT_MS,
    );
  } catch (err) {
    if (isAbortError(err)) {
      throw new Error("failed to fetch ICE servers (timeout)");
    }
    throw err;
  }
  if (!res.ok) {
    throw new Error(`failed to fetch ICE servers (${res.status})`);
  }

  const body: unknown = await readJsonResponseWithLimit(res, {
    maxBytes: MAX_SIGNALING_RESPONSE_BYTES,
    label: "webrtc ice servers response",
  });
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
  // Forward/compat: support both jwt and api_key relay configurations without
  // forcing the caller to specify an auth mode.
  headers.set("Authorization", `Bearer ${authToken}`);
  headers.set("X-API-Key", authToken);
}

type RelayUrlTransport = "http" | "ws";

const mapRelayBaseUrlProtocol = (protocol: string, transport: RelayUrlTransport): string => {
  // `fetch()` only supports http(s); relay callers may have a ws(s) relay URL
  // already in hand, so we normalize between the two transports.
  switch (transport) {
    case "http":
      switch (protocol) {
        case "http:":
        case "https:":
          return protocol;
        case "ws:":
          return "http:";
        case "wss:":
          return "https:";
        default:
          throw new Error(`unsupported relay baseUrl scheme for HTTP transport: ${protocol}`);
      }
    case "ws":
      switch (protocol) {
        case "ws:":
        case "wss:":
          return protocol;
        case "http:":
          return "ws:";
        case "https:":
          return "wss:";
        default:
          throw new Error(`unsupported relay baseUrl scheme for WebSocket transport: ${protocol}`);
      }
  }
};

const resolveRelayBaseUrl = (baseUrl: string, transport: RelayUrlTransport): URL => {
  const url = new URL(baseUrl);
  url.protocol = mapRelayBaseUrlProtocol(url.protocol, transport);
  return url;
};

export function toWebSocketUrl(baseUrl: string, path: string): URL {
  const url = resolveRelayBaseUrl(baseUrl, "ws");
  url.pathname = `${url.pathname.replace(/\/$/, "")}${path}`;
  return url;
}

function toWebSocketUrlWithQueryAuth(baseUrl: string, path: string, authToken: string): URL {
  const url = toWebSocketUrl(baseUrl, path);
  addAuthToUrl(url, authToken);
  return url;
}

export function toHttpUrl(baseUrl: string, path: string): URL {
  const url = resolveRelayBaseUrl(baseUrl, "http");
  url.pathname = `${url.pathname.replace(/\/$/, "")}${path}`;
  return url;
}

function waitForIceGatheringComplete(pc: RTCPeerConnection): Promise<void> {
  if (pc.iceGatheringState === "complete") return Promise.resolve();

  return new Promise((resolve) => {
    let settled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const onChange = () => {
      if (pc.iceGatheringState !== "complete") return;
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      pc.removeEventListener("icegatheringstatechange", onChange);
      resolve();
    };

    timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      pc.removeEventListener("icegatheringstatechange", onChange);
      resolve();
    }, DEFAULT_ICE_GATHER_TIMEOUT_MS);
    unrefBestEffort(timer);

    pc.addEventListener("icegatheringstatechange", onChange);
  });
}

function waitForDataChannelOpen(dc: RTCDataChannel): Promise<void> {
  if (dcIsOpenSafe(dc)) return Promise.resolve();
  if (dcIsClosedSafe(dc)) return Promise.reject(new Error("data channel closed"));

  return new Promise((resolve, reject) => {
    let settled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;

    function cleanup() {
      dc.removeEventListener("open", onOpen);
      dc.removeEventListener("close", onClose);
      dc.removeEventListener("error", onError);
    }

    function onOpen() {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      cleanup();
      resolve();
    }

    function onClose() {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      cleanup();
      reject(new Error("data channel closed"));
    }

    function onError() {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      cleanup();
      reject(new Error("data channel error"));
    }

    dc.addEventListener("open", onOpen);
    dc.addEventListener("close", onClose);
    dc.addEventListener("error", onError);

    timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(new Error("data channel open timed out"));
    }, DEFAULT_DATA_CHANNEL_OPEN_TIMEOUT_MS);
    unrefBestEffort(timer);
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

  let res: Response;
  try {
    res = await fetchWithTimeout(
      url.toString(),
      {
        method: "POST",
        mode: "cors",
        headers,
        body: JSON.stringify({ sdp: { type: "offer", sdp: local.sdp } satisfies SessionDescription }),
      },
      DEFAULT_HTTP_TIMEOUT_MS,
    );
  } catch (err) {
    if (isAbortError(err)) {
      throw new Error("webrtc offer failed (timeout)");
    }
    throw err;
  }
  const text = await readTextResponseWithLimit(res, {
    maxBytes: MAX_SIGNALING_RESPONSE_BYTES,
    label: "webrtc offer response",
  });
  if (!res.ok) {
    // Do not reflect response bodies in user-visible errors.
    throw new Error(`webrtc offer failed (${res.status})`);
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

  let res: Response;
  try {
    res = await fetchWithTimeout(
      url.toString(),
      {
        method: "POST",
        mode: "cors",
        headers,
        body: JSON.stringify({
          version: UDP_RELAY_SIGNALING_VERSION,
          offer: { type: "offer", sdp: local.sdp } satisfies SessionDescription,
        }),
      },
      DEFAULT_HTTP_TIMEOUT_MS,
    );
  } catch (err) {
    if (isAbortError(err)) {
      throw new Error("legacy offer failed (timeout)");
    }
    throw err;
  }
  const text = await readTextResponseWithLimit(res, {
    maxBytes: MAX_SIGNALING_RESPONSE_BYTES,
    label: "legacy offer response",
  });
  if (!res.ok) {
    // Do not reflect response bodies in user-visible errors.
    throw new Error(`legacy offer failed (${res.status})`);
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

    let settled = false;
    const settle = (err?: unknown) => {
      if (settled) return;
      settled = true;
      cleanup();
      if (err) {
        reject(err);
      } else {
        resolve(ws);
      }
    };

    const timer = setTimeout(() => {
      wsCloseSafe(ws);
      settle(new Error("websocket connect timed out"));
    }, DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS);
    unrefBestEffort(timer);

    const onOpen = () => {
      clearTimeout(timer);
      settle();
    };
    const onClose = (evt: CloseEvent) => {
      clearTimeout(timer);
      // Close reasons are server-controlled; do not reflect them in user-visible errors.
      settle(new Error(`websocket closed (${evt.code})`));
    };
    const onError = () => {
      clearTimeout(timer);
      settle(new Error("websocket error"));
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
  if (!wsSendSafe(ws, JSON.stringify(msg))) {
    wsCloseSafe(ws);
  }
}

function sendAuth(ws: WebSocket, authToken: string): void {
  // Forward/compat: different relay builds may accept either {token} or {apiKey}
  // depending on auth mode. Supplying both allows the client to remain agnostic.
  if (!wsSendSafe(ws, JSON.stringify({ type: "auth", token: authToken, apiKey: authToken }))) {
    wsCloseSafe(ws);
  }
}

async function negotiateWebSocketTrickle(pc: RTCPeerConnection, baseUrl: string, authToken?: string): Promise<void> {
  const wsUrls: string[] = [toWebSocketUrl(baseUrl, "/webrtc/signal").toString()];
  if (authToken) {
    // Query-string auth is supported for non-browser tooling, but we prefer
    // first-message `{type:"auth"}` to avoid leaking secrets into URLs.
    wsUrls.push(toWebSocketUrlWithQueryAuth(baseUrl, "/webrtc/signal", authToken).toString());
  }

  // Candidate gathering can start immediately after SetLocalDescription; collect
  // all local candidates so we can re-send them if we have to reconnect the
  // signaling WebSocket (e.g. auth message required).
  const localCandidates: Candidate[] = [];
  let localCandidateCursor = 0;

  let activeWs: WebSocket | null = null;
  let offerSent = false;
  let remoteDescriptionSet = false;
  let trickleEnabled = true;

  const remoteCandidateBuffer: Candidate[] = [];
  let currentAttempt: symbol | null = null;

  const flushLocalCandidates = () => {
    if (!trickleEnabled || !offerSent || !activeWs || !wsIsOpenSafe(activeWs)) return;
    while (localCandidateCursor < localCandidates.length) {
      const cand = localCandidates[localCandidateCursor++];
      sendSignal(activeWs, { type: "candidate", candidate: cand });
    }
  };

  const onIceCandidate = (evt: RTCPeerConnectionIceEvent) => {
    if (!evt.candidate) return;
    const cand = evt.candidate.toJSON() as Candidate;
    localCandidates.push(cand);
    flushLocalCandidates();
  };
  pc.addEventListener("icecandidate", onIceCandidate);

  const closeActiveWs = () => {
    currentAttempt = null;
    if (!activeWs) return;
    wsCloseSafe(activeWs);
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
  const authFirstVariants: boolean[] = authToken ? [true, false] : [false];

  let lastErr: unknown = null;

  const runTyped = async (wsUrl: string, protocol: string | undefined, authFirst: boolean): Promise<void> => {
    closeActiveWs();
    offerSent = false;
    trickleEnabled = true;
    localCandidateCursor = 0;
    remoteDescriptionSet = false;
    remoteCandidateBuffer.length = 0;

    const ws = await openWebSocket(wsUrl, protocol);
    activeWs = ws;
    const attemptId = Symbol("udp-relay-ws-typed");
    currentAttempt = attemptId;

    let haveAnswer = false;

    let resolveAnswer!: () => void;
    let rejectAnswer!: (err: unknown) => void;
    let answerTimer: ReturnType<typeof setTimeout> | null = null;
    const answerPromise = new Promise<void>((resolve, reject) => {
      resolveAnswer = resolve;
      rejectAnswer = reject;
    });
    let answerSettled = false;
    const settleAnswer = (err?: unknown) => {
      if (answerSettled) return;
      answerSettled = true;
      if (answerTimer) clearTimeout(answerTimer);
      answerTimer = null;
      if (err) {
        rejectAnswer(err);
      } else {
        resolveAnswer();
      }
    };
    answerTimer = setTimeout(() => {
      settleAnswer(new Error("signaling answer timed out"));
    }, DEFAULT_SIGNALING_ANSWER_TIMEOUT_MS);
    unrefBestEffort(answerTimer);

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
            else pcCloseSafe(pc);
          }
          break;
        case "error":
          if (!haveAnswer) {
            // Do not reflect server-provided error messages into user-visible errors.
            settleAnswer(new Error(`signaling error (${sanitizeSignalingErrorCode(msg.code)})`));
          } else {
            pcCloseSafe(pc);
          }
          break;
        case "close":
          if (!haveAnswer) {
            settleAnswer(new Error("signaling closed"));
          } else {
            pcCloseSafe(pc);
          }
          break;
        case "auth":
          // Ignore server echoes.
          break;
        case "offer":
          if (!haveAnswer) settleAnswer(new Error("unexpected offer from server"));
          else pcCloseSafe(pc);
          break;
      }
    };

    const onClose = (evt: CloseEvent) => {
      if (currentAttempt !== attemptId) return;
      if (!haveAnswer) {
        // Close reasons are server-controlled; do not reflect them in user-visible errors.
        settleAnswer(new Error(`signaling websocket closed (${evt.code})`));
        return;
      }
      pcCloseSafe(pc);
    };

    const onError = () => {
      if (currentAttempt !== attemptId) return;
      if (!haveAnswer) {
        settleAnswer(new Error("signaling websocket error"));
        return;
      }
      pcCloseSafe(pc);
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
    flushLocalCandidates();

    await answerPromise;
  };

  for (const wsUrl of wsUrls) {
    for (const protocol of protocols) {
      for (const authFirst of authFirstVariants) {
        try {
          await runTyped(wsUrl, protocol, authFirst);
          return;
        } catch (err) {
          lastErr = err;
          closeActiveWs();
        }
      }
    }
  }

  // Legacy fallback: some relay builds (and older tests) use versioned offer/answer
  // JSON on /webrtc/signal without trickle ICE. In that mode, candidates must be
  // embedded in the SDP.
  await waitForIceGatheringComplete(pc);

  const runLegacy = async (wsUrl: string, protocol: string | undefined, authFirst: boolean): Promise<void> => {
    closeActiveWs();
    offerSent = false;
    trickleEnabled = false;
    localCandidateCursor = 0;
    remoteDescriptionSet = false;
    remoteCandidateBuffer.length = 0;

    const ws = await openWebSocket(wsUrl, protocol);
    activeWs = ws;
    const attemptId = Symbol("udp-relay-ws-legacy");
    currentAttempt = attemptId;

    let haveAnswer = false;

    let resolveAnswer!: () => void;
    let rejectAnswer!: (err: unknown) => void;
    let answerTimer: ReturnType<typeof setTimeout> | null = null;
    const answerPromise = new Promise<void>((resolve, reject) => {
      resolveAnswer = resolve;
      rejectAnswer = reject;
    });
    let answerSettled = false;
    const settleAnswer = (err?: unknown) => {
      if (answerSettled) return;
      answerSettled = true;
      if (answerTimer) clearTimeout(answerTimer);
      answerTimer = null;
      if (err) {
        rejectAnswer(err);
      } else {
        resolveAnswer();
      }
    };
    answerTimer = setTimeout(() => {
      settleAnswer(new Error("signaling answer timed out"));
    }, DEFAULT_SIGNALING_ANSWER_TIMEOUT_MS);
    unrefBestEffort(answerTimer);

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
        // Close reasons are server-controlled; do not reflect them in user-visible errors.
        settleAnswer(new Error(`signaling websocket closed (${evt.code})`));
        return;
      }
      pcCloseSafe(pc);
    };

    const onError = () => {
      if (currentAttempt !== attemptId) return;
      if (!haveAnswer) {
        settleAnswer(new Error("signaling websocket error"));
        return;
      }
      pcCloseSafe(pc);
    };

    ws.addEventListener("message", onMessage);
    ws.addEventListener("close", onClose);
    ws.addEventListener("error", onError);

    if (authFirst && authToken) {
      sendAuth(ws, authToken);
    }

    const fullOffer = pc.localDescription;
    if (!fullOffer?.sdp) throw new Error("missing offer SDP after ICE gathering");
    if (
      !wsSendSafe(
        ws,
        JSON.stringify({
          version: UDP_RELAY_SIGNALING_VERSION,
          offer: { type: "offer", sdp: fullOffer.sdp } satisfies SessionDescription,
        }),
      )
    ) {
      wsCloseSafe(ws);
      throw new Error("signaling websocket send failed");
    }
    offerSent = true;

    await answerPromise;
  };

  for (const wsUrl of wsUrls) {
    for (const protocol of protocols) {
      for (const authFirst of authFirstVariants) {
        try {
          await runLegacy(wsUrl, protocol, authFirst);
          return;
        } catch (err) {
          lastErr = err;
          closeActiveWs();
        }
      }
    }
  }

  throw lastErr instanceof Error ? lastErr : new Error("failed to establish signaling websocket");
}
