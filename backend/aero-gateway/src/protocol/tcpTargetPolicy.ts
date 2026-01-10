import net from 'node:net';

import type { TcpTarget } from './tcpTarget.js';
import { matchHostnamePattern, normalizeHostname } from '../hostname.js';
import { isPrivateIp } from '../ip.js';

export type TcpTargetPolicyErrorCode =
  | 'ERR_TCP_POLICY_DISALLOWED_PORT'
  | 'ERR_TCP_POLICY_PRIVATE_IP'
  | 'ERR_TCP_POLICY_HOST_NOT_ALLOWED'
  | 'ERR_TCP_POLICY_INVALID_HOSTNAME';

export class TcpTargetPolicyError extends Error {
  override name = 'TcpTargetPolicyError';
  readonly code: TcpTargetPolicyErrorCode;

  constructor(code: TcpTargetPolicyErrorCode, message: string) {
    super(message);
    this.code = code;
  }
}

export interface TcpTargetPolicy {
  blockPrivateIp: boolean;
  portAllowlist?: ReadonlySet<number> | undefined;
  hostAllowlistPatterns?: readonly string[] | undefined;
  resolveHostnameToIps?: ((hostname: string) => readonly string[]) | undefined;
}

export function enforceTcpTargetPolicy(target: TcpTarget, policy: TcpTargetPolicy): TcpTarget {
  if (policy.portAllowlist && !policy.portAllowlist.has(target.port)) {
    throw new TcpTargetPolicyError(
      'ERR_TCP_POLICY_DISALLOWED_PORT',
      `Port ${target.port} is not allowed`,
    );
  }

  const trimmedHost = target.host.trim();
  if (trimmedHost.length === 0) {
    throw new TcpTargetPolicyError('ERR_TCP_POLICY_INVALID_HOSTNAME', 'Host must not be empty');
  }

  const isIpLiteral = net.isIP(trimmedHost) !== 0;
  let normalizedHost = trimmedHost;

  if (!isIpLiteral) {
    const norm = normalizeHostname(trimmedHost);
    if (!norm) {
      throw new TcpTargetPolicyError(
        'ERR_TCP_POLICY_INVALID_HOSTNAME',
        'Invalid hostname',
      );
    }
    normalizedHost = norm;
  }

  if (policy.hostAllowlistPatterns && !isIpLiteral) {
    const allowed = policy.hostAllowlistPatterns.some((pattern) => matchHostnamePattern(normalizedHost, pattern));
    if (!allowed) {
      throw new TcpTargetPolicyError('ERR_TCP_POLICY_HOST_NOT_ALLOWED', 'Target hostname is not allowed');
    }
  }

  if (policy.blockPrivateIp) {
    if (isIpLiteral && isPrivateIp(normalizedHost)) {
      throw new TcpTargetPolicyError('ERR_TCP_POLICY_PRIVATE_IP', 'Target is a private or reserved IP address');
    }

    if (!isIpLiteral && policy.resolveHostnameToIps) {
      const ips = policy.resolveHostnameToIps(normalizedHost);
      if (ips.some((ip) => isPrivateIp(ip))) {
        throw new TcpTargetPolicyError(
          'ERR_TCP_POLICY_PRIVATE_IP',
          'Target hostname resolves to a private or reserved IP address',
        );
      }
    }
  }

  return normalizedHost === target.host ? target : { ...target, host: normalizedHost };
}

