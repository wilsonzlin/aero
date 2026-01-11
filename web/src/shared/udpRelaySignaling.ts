export const UDP_RELAY_SIGNALING_VERSION = 1;

export type SessionDescription = {
  type: 'offer' | 'answer';
  sdp: string;
};

export type Candidate = {
  candidate: string;
  sdpMid?: string | null;
  sdpMLineIndex?: number | null;
  usernameFragment?: string | null;
};

export type OfferRequest = {
  version: typeof UDP_RELAY_SIGNALING_VERSION;
  offer: SessionDescription;
};

export type AnswerResponse = {
  version: typeof UDP_RELAY_SIGNALING_VERSION;
  answer: SessionDescription;
};

export type SignalMessage =
  | {
      type: 'offer';
      sdp: SessionDescription;
    }
  | {
      type: 'answer';
      sdp: SessionDescription;
    }
  | {
      type: 'candidate';
      candidate: Candidate;
    }
  | {
      type: 'close';
    }
  | {
      type: 'error';
      code: string;
      message: string;
    }
  | {
      type: 'auth';
      token: string;
    };

export class UdpRelaySignalingDecodeError extends Error {
  readonly code:
    | 'invalid_json'
    | 'unsupported_version'
    | 'invalid_sdp_type'
    | 'missing_sdp'
    | 'missing_type'
    | 'unsupported_message_type'
    | 'missing_candidate'
    | 'missing_error_code'
    | 'missing_error_message'
    | 'missing_token';

  constructor(code: UdpRelaySignalingDecodeError['code'], message: string) {
    super(message);
    this.code = code;
  }
}

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null;

const assertNoExtraKeys = (obj: Record<string, unknown>, allowed: readonly string[]): void => {
  for (const key of Object.keys(obj)) {
    if (!allowed.includes(key)) {
      throw new UdpRelaySignalingDecodeError('invalid_json', `unexpected field ${key}`);
    }
  }
};

const parseVersion = (v: unknown): typeof UDP_RELAY_SIGNALING_VERSION => {
  if (v !== UDP_RELAY_SIGNALING_VERSION) {
    throw new UdpRelaySignalingDecodeError('unsupported_version', `unsupported version: ${String(v)}`);
  }
  return UDP_RELAY_SIGNALING_VERSION;
};

const parseSessionDescription = (v: unknown, expectedType: SessionDescription['type']): SessionDescription => {
  if (!isRecord(v)) {
    throw new UdpRelaySignalingDecodeError('invalid_json', `expected session description object`);
  }
  assertNoExtraKeys(v, ['type', 'sdp']);
  const type = v.type;
  if (type !== expectedType) {
    throw new UdpRelaySignalingDecodeError('invalid_sdp_type', `expected type=${expectedType} (got ${String(type)})`);
  }
  const sdp = v.sdp;
  if (typeof sdp !== 'string' || sdp.length === 0) {
    throw new UdpRelaySignalingDecodeError('missing_sdp', 'missing sdp');
  }
  return { type: expectedType, sdp };
};

export const parseOfferRequest = (v: unknown): OfferRequest => {
  if (!isRecord(v)) {
    throw new UdpRelaySignalingDecodeError('invalid_json', 'expected object');
  }
  assertNoExtraKeys(v, ['version', 'offer']);
  return {
    version: parseVersion(v.version),
    offer: parseSessionDescription(v.offer, 'offer'),
  };
};

export const parseAnswerResponse = (v: unknown): AnswerResponse => {
  if (!isRecord(v)) {
    throw new UdpRelaySignalingDecodeError('invalid_json', 'expected object');
  }
  assertNoExtraKeys(v, ['version', 'answer']);
  return {
    version: parseVersion(v.version),
    answer: parseSessionDescription(v.answer, 'answer'),
  };
};

const parseCandidate = (v: unknown): Candidate => {
  if (!isRecord(v)) {
    throw new UdpRelaySignalingDecodeError('missing_candidate', 'missing candidate');
  }
  assertNoExtraKeys(v, ['candidate', 'sdpMid', 'sdpMLineIndex', 'usernameFragment']);

  const cand = v.candidate;
  if (typeof cand !== 'string') {
    throw new UdpRelaySignalingDecodeError('missing_candidate', 'missing candidate.candidate');
  }

  const sdpMid = v.sdpMid;
  if (sdpMid !== undefined && sdpMid !== null && typeof sdpMid !== 'string') {
    throw new UdpRelaySignalingDecodeError('invalid_json', 'invalid candidate.sdpMid');
  }

  const sdpMLineIndex = v.sdpMLineIndex;
  if (sdpMLineIndex !== undefined && sdpMLineIndex !== null) {
    if (!Number.isInteger(sdpMLineIndex) || sdpMLineIndex < 0 || sdpMLineIndex > 0xffff) {
      throw new UdpRelaySignalingDecodeError('invalid_json', 'invalid candidate.sdpMLineIndex');
    }
  }

  const usernameFragment = v.usernameFragment;
  if (usernameFragment !== undefined && usernameFragment !== null && typeof usernameFragment !== 'string') {
    throw new UdpRelaySignalingDecodeError('invalid_json', 'invalid candidate.usernameFragment');
  }

  return {
    candidate: cand,
    sdpMid: sdpMid === undefined ? undefined : (sdpMid as string | null),
    sdpMLineIndex: sdpMLineIndex === undefined ? undefined : (sdpMLineIndex as number | null),
    usernameFragment: usernameFragment === undefined ? undefined : (usernameFragment as string | null),
  };
};

export const parseSignalMessage = (v: unknown): SignalMessage => {
  if (!isRecord(v)) {
    throw new UdpRelaySignalingDecodeError('invalid_json', 'expected object');
  }

  const t = v.type;
  if (typeof t !== 'string' || t.length === 0) {
    throw new UdpRelaySignalingDecodeError('missing_type', 'missing type');
  }

  switch (t) {
    case 'offer':
      assertNoExtraKeys(v, ['type', 'sdp']);
      return { type: 'offer', sdp: parseSessionDescription(v.sdp, 'offer') };
    case 'answer':
      assertNoExtraKeys(v, ['type', 'sdp']);
      return { type: 'answer', sdp: parseSessionDescription(v.sdp, 'answer') };
    case 'candidate':
      assertNoExtraKeys(v, ['type', 'candidate']);
      return { type: 'candidate', candidate: parseCandidate(v.candidate) };
    case 'close':
      assertNoExtraKeys(v, ['type']);
      return { type: 'close' };
    case 'error': {
      assertNoExtraKeys(v, ['type', 'code', 'message']);
      const code = v.code;
      if (typeof code !== 'string' || code.length === 0) {
        throw new UdpRelaySignalingDecodeError('missing_error_code', 'missing error code');
      }
      const message = v.message;
      if (typeof message !== 'string' || message.length === 0) {
        throw new UdpRelaySignalingDecodeError('missing_error_message', 'missing error message');
      }
      return { type: 'error', code, message };
    }
    case 'auth': {
      // Backwards/forward compatibility: accept either {token} or {apiKey}.
      assertNoExtraKeys(v, ['type', 'token', 'apiKey']);
      const token = v.token;
      const apiKey = v.apiKey;
      const cred =
        typeof token === 'string' && token.length > 0
          ? token
          : typeof apiKey === 'string' && apiKey.length > 0
            ? apiKey
            : null;
      if (!cred) {
        throw new UdpRelaySignalingDecodeError('missing_token', 'missing token');
      }
      return { type: 'auth', token: cred };
    }
    default:
      throw new UdpRelaySignalingDecodeError('unsupported_message_type', `unsupported type: ${t}`);
  }
};

export const parseOfferRequestJSON = (text: string): OfferRequest => {
  try {
    return parseOfferRequest(JSON.parse(text));
  } catch (err) {
    if (err instanceof UdpRelaySignalingDecodeError) throw err;
    throw new UdpRelaySignalingDecodeError('invalid_json', 'invalid json');
  }
};

export const parseAnswerResponseJSON = (text: string): AnswerResponse => {
  try {
    return parseAnswerResponse(JSON.parse(text));
  } catch (err) {
    if (err instanceof UdpRelaySignalingDecodeError) throw err;
    throw new UdpRelaySignalingDecodeError('invalid_json', 'invalid json');
  }
};

export const parseSignalMessageJSON = (text: string): SignalMessage => {
  try {
    return parseSignalMessage(JSON.parse(text));
  } catch (err) {
    if (err instanceof UdpRelaySignalingDecodeError) throw err;
    throw new UdpRelaySignalingDecodeError('invalid_json', 'invalid json');
  }
};
