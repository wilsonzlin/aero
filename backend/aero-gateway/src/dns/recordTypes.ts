export const DNS_RECORD_TYPES = {
  A: 1,
  CNAME: 5,
  AAAA: 28,
} as const;

const SUPPORTED_RECORD_TYPES = new Set<number>([
  DNS_RECORD_TYPES.A,
  DNS_RECORD_TYPES.AAAA,
  DNS_RECORD_TYPES.CNAME,
]);

const RECORD_TYPE_BY_NAME = new Map<string, number>([
  ['A', DNS_RECORD_TYPES.A],
  ['AAAA', DNS_RECORD_TYPES.AAAA],
  ['CNAME', DNS_RECORD_TYPES.CNAME],
]);

const SUPPORTED_RECORD_TYPE_NAMES = ['A', 'AAAA', 'CNAME'] as const;

export function parseDnsRecordType(value: string): number {
  const trimmed = value.trim();
  if (!trimmed) {
    throw new Error('DNS record type cannot be empty');
  }

  let isNumeric = true;
  for (let i = 0; i < trimmed.length; i += 1) {
    const c = trimmed.charCodeAt(i);
    if (c < 0x30 /* '0' */ || c > 0x39 /* '9' */) {
      isNumeric = false;
      break;
    }
  }
  if (isNumeric) {
    const parsed = Number.parseInt(trimmed, 10);
    if (!Number.isFinite(parsed) || parsed <= 0 || parsed > 0xffff) {
      throw new Error('Invalid DNS record type number');
    }
    if (!SUPPORTED_RECORD_TYPES.has(parsed)) {
      throw new Error(`Unsupported DNS record type (supported: ${SUPPORTED_RECORD_TYPE_NAMES.join(', ')})`);
    }
    return parsed;
  }

  const mapped = RECORD_TYPE_BY_NAME.get(trimmed.toUpperCase());
  if (mapped === undefined) {
    throw new Error(`Unsupported DNS record type (supported: ${SUPPORTED_RECORD_TYPE_NAMES.join(', ')})`);
  }
  return mapped;
}
