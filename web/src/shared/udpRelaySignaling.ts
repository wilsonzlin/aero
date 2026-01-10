export const UDP_RELAY_SIGNALING_VERSION = 1;

export type SessionDescription = {
  type: 'offer' | 'answer';
  sdp: string;
};

export type OfferRequest = {
  version: typeof UDP_RELAY_SIGNALING_VERSION;
  offer: SessionDescription;
};

export type AnswerResponse = {
  version: typeof UDP_RELAY_SIGNALING_VERSION;
  answer: SessionDescription;
};

export class UdpRelaySignalingDecodeError extends Error {
  readonly code: 'invalid_json' | 'unsupported_version' | 'invalid_sdp_type' | 'missing_sdp';

  constructor(code: UdpRelaySignalingDecodeError['code'], message: string) {
    super(message);
    this.code = code;
  }
}

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null;

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
  return {
    version: parseVersion(v.version),
    offer: parseSessionDescription(v.offer, 'offer'),
  };
};

export const parseAnswerResponse = (v: unknown): AnswerResponse => {
  if (!isRecord(v)) {
    throw new UdpRelaySignalingDecodeError('invalid_json', 'expected object');
  }
  return {
    version: parseVersion(v.version),
    answer: parseSessionDescription(v.answer, 'answer'),
  };
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

